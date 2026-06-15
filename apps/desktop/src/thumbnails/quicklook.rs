//! `QLThumbnailGenerator` bridge.
//!
//! ## Threading
//!
//! Submission happens on a **dedicated worker thread**, not the main
//! thread. `NSURL::fileURLWithPath` and `generateBest…` involve enough
//! XPC + path-resolution work that on a slow network share each submit
//! costs ~150 ms. With 7 submissions per pump cycle on the main thread
//! that's a full second of unresponsive UI per cycle — the user sees a
//! blank window for the duration. Punting submission off-main moves
//! that cost where it belongs (a background thread) and leaves the
//! main thread free to render and process input.
//!
//! ## Message flow
//!
//! ```
//! Main thread                    Worker thread                      QL queue (private)
//! ─────────────                  ──────────────                     ──────────────────
//! submit  → Submit(...)      →   recv → create NSURL/request,   →
//!                                   store in entries[id],
//!                                   gen.generateBest(block)
//!                                                                   block runs:
//!                                                                     pixels = …
//!                                                                     pending.push(delivery)
//!                                                                     proxy.send_event(wake)
//!                                                                     forget_tx.send(Forget(id))
//!
//! drain_pending ← drains pending mutex
//!                                ←   recv → entries.remove(id)
//!
//! cancel_all → CancelAll     →   recv → entries.drain().for_each(cancelRequest)
//! ```
//!
//! `entries` (the per-request `Retained<QLThumbnailGenerationRequest>`
//! map) lives on the worker thread, so cross-thread cancellation works
//! without sharing `Retained<…>` (which isn't `Send`-friendly).
//!
//! The completion block runs on QuickLook's internal queue (not our
//! worker) — it pushes the delivery, wakes winit, and fires a `Forget`
//! to the worker so `entries` doesn't grow unbounded over a long
//! folder browse.

use crate::commands::AppCommand;
use crate::thumbnails::scheduler::RequestId;
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_quick_look_thumbnailing::{
    QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes,
    QLThumbnailGenerator, QLThumbnailRepresentation,
};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use winit::event_loop::EventLoopProxy;

/// A ready thumbnail: raw RGBA8, row-packed (no padding).
pub struct ThumbnailPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One result slot delivered to the main thread by a QL completion block.
/// `Ok(pixels)` for a generated thumbnail, `Err` for a failed request.
pub struct Delivery {
    pub index: usize,
    pub request_id: RequestId,
    pub folder_generation: u64,
    pub result: Result<ThumbnailPixels, ()>,
}

/// Arguments for [`RequestTable::submit`].
pub struct SubmitRequest<'a> {
    pub request_id: RequestId,
    pub index: usize,
    pub folder_generation: u64,
    pub path: &'a Path,
    pub size: CGSize,
    pub scale: f64,
    pub proxy: EventLoopProxy<AppCommand>,
}

/// Messages sent from the main thread to the worker thread that owns
/// `entries` and the `QLThumbnailGenerator`.
enum WorkerMsg {
    Submit {
        request_id: RequestId,
        index: usize,
        folder_generation: u64,
        path: PathBuf,
        size: CGSize,
        scale: f64,
        proxy: EventLoopProxy<AppCommand>,
    },
    /// Drop the retained request for this id without cancelling — the
    /// thumbnail completed naturally. Sent by the completion block.
    Forget(RequestId),
    /// Cancel every in-flight request. Used on folder change.
    CancelAll,
}

/// Front-end handle owned by `thumbnails::State` on the main thread.
/// All operations are non-blocking from the main thread's perspective:
/// they just shovel an `mpsc` message to the worker.
pub struct RequestTable {
    submit_tx: mpsc::Sender<WorkerMsg>,
    pending: Arc<Mutex<VecDeque<Delivery>>>,
    _worker: thread::JoinHandle<()>,
}

impl RequestTable {
    pub fn new() -> Self {
        let (submit_tx, submit_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        let pending_for_worker = Arc::clone(&pending);
        let forget_tx = submit_tx.clone();
        let worker = thread::Builder::new()
            .name("prvw-thumbgen".into())
            .spawn(move || worker_loop(submit_rx, pending_for_worker, forget_tx))
            .expect("Failed to spawn thumbnail submission worker");
        Self {
            submit_tx,
            pending,
            _worker: worker,
        }
    }

    /// Send a submission to the worker. Returns immediately —
    /// `to_path_buf` is the only thing that runs on the main thread.
    pub fn submit(&self, req: SubmitRequest<'_>) {
        let _ = self.submit_tx.send(WorkerMsg::Submit {
            request_id: req.request_id,
            index: req.index,
            folder_generation: req.folder_generation,
            path: req.path.to_path_buf(),
            size: req.size,
            scale: req.scale,
            proxy: req.proxy,
        });
    }

    /// Cancel every in-flight request. Used on folder change.
    pub fn cancel_all(&self) {
        let _ = self.submit_tx.send(WorkerMsg::CancelAll);
    }

    /// Drain all queued deliveries. Called from the main-thread handler
    /// for `AppCommand::ThumbnailsAvailable`.
    pub fn drain_pending(&self) -> Vec<Delivery> {
        if let Ok(mut q) = self.pending.lock() {
            q.drain(..).collect()
        } else {
            Vec::new()
        }
    }
}

fn worker_loop(
    rx: mpsc::Receiver<WorkerMsg>,
    pending: Arc<Mutex<VecDeque<Delivery>>>,
    forget_tx: mpsc::Sender<WorkerMsg>,
) {
    // Get the singleton generator on this thread. `sharedGenerator()`
    // is process-wide; the `Retained<>` we keep here is just a local
    // reference that we never need to share.
    let generator = unsafe { QLThumbnailGenerator::sharedGenerator() };
    let mut entries: HashMap<RequestId, Retained<QLThumbnailGenerationRequest>> = HashMap::new();
    log::debug!("Thumbnail submission worker started");
    while let Ok(msg) = rx.recv() {
        match msg {
            WorkerMsg::Submit {
                request_id,
                index,
                folder_generation,
                path,
                size,
                scale,
                proxy,
            } => {
                worker_submit(
                    &generator,
                    &mut entries,
                    &pending,
                    &forget_tx,
                    request_id,
                    index,
                    folder_generation,
                    path,
                    size,
                    scale,
                    proxy,
                );
            }
            WorkerMsg::Forget(id) => {
                entries.remove(&id);
            }
            WorkerMsg::CancelAll => {
                let count = entries.len();
                for (_id, req) in entries.drain() {
                    unsafe {
                        generator.cancelRequest(&req);
                    }
                }
                if count > 0 {
                    log::debug!("Cancelled {count} in-flight thumbnail requests");
                }
            }
        }
    }
    log::debug!("Thumbnail submission worker exiting");
}

#[allow(clippy::too_many_arguments)]
fn worker_submit(
    generator: &Retained<QLThumbnailGenerator>,
    entries: &mut HashMap<RequestId, Retained<QLThumbnailGenerationRequest>>,
    pending: &Arc<Mutex<VecDeque<Delivery>>>,
    forget_tx: &mpsc::Sender<WorkerMsg>,
    request_id: RequestId,
    index: usize,
    folder_generation: u64,
    path: PathBuf,
    size: CGSize,
    scale: f64,
    proxy: EventLoopProxy<AppCommand>,
) {
    let Some(path_str) = path.to_str() else {
        push_delivery(
            pending,
            Delivery {
                index,
                request_id,
                folder_generation,
                result: Err(()),
            },
            &proxy,
        );
        return;
    };
    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url: Retained<NSURL> = NSURL::fileURLWithPath(&ns_path);
        // Request rendered content only (`Thumbnail` + `LowQualityThumbnail`),
        // never `Icon`. `All` would let quicklookd fall back to the generic
        // file-type icon (the gray "DNG"/"RAF" document stamp) for files it
        // can't render — we'd then show that junk icon as the placeholder.
        // Excluding `Icon` means such files return an error instead (→ our
        // `ThumbnailFailed` path, no placeholder), leaving the "Loading…" pill
        // (and, for RAW, our embedded-JPEG preview) to cover the gap.
        let representation_types = QLThumbnailGenerationRequestRepresentationTypes::Thumbnail
            | QLThumbnailGenerationRequestRepresentationTypes::LowQualityThumbnail;
        let request =
            QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                QLThumbnailGenerationRequest::alloc(),
                &url,
                size,
                scale,
                representation_types,
            );
        entries.insert(request_id, request.clone());

        // The block runs on QL's private queue, not our worker. Capture
        // by value (Copy primitives) or clone (Arc, Sender, proxy).
        let pending_for_block = Arc::clone(pending);
        let forget_for_block = forget_tx.clone();
        let proxy_for_block = proxy.clone();
        let log_path = path.clone();
        let block = RcBlock::new(
            move |rep: *mut QLThumbnailRepresentation, err: *mut NSError| {
                // Compiler sees this closure as nested under the outer
                // `unsafe` block above (the lexical site at which it's
                // constructed), so the inner pointer dereferences are
                // already in an unsafe scope. No `unsafe` keyword needed
                // here even though we deref raw pointers — the safety
                // contract is documented at the outer block.
                let result = if rep.is_null() || !err.is_null() {
                    if !err.is_null() {
                        let ns_err = &*err;
                        let msg = ns_err.localizedDescription().to_string();
                        log::debug!(
                            "QLThumbnailGenerator failed for {} (index {}): {msg}",
                            log_path.display(),
                            index
                        );
                    }
                    Err(())
                } else {
                    let rep = &*rep;
                    cg_image_to_rgba8(rep).ok_or(())
                };
                push_delivery(
                    &pending_for_block,
                    Delivery {
                        index,
                        request_id,
                        folder_generation,
                        result,
                    },
                    &proxy_for_block,
                );
                // Tell the worker to drop our entries[request_id] entry.
                // Without this, `entries` grows unbounded as the user
                // browses through a 10k-image folder.
                let _ = forget_for_block.send(WorkerMsg::Forget(request_id));
            },
        );

        generator.generateBestRepresentationForRequest_completionHandler(&request, &block);
    }
}

/// Push a delivery onto the shared queue and wake the main thread
/// **only if the queue was previously empty**. A burst of N completions
/// produces 1–2 user events, not N — so winit's window-event flow
/// (keyboard, redraw) doesn't get starved.
fn push_delivery(
    pending: &Arc<Mutex<VecDeque<Delivery>>>,
    delivery: Delivery,
    proxy: &EventLoopProxy<AppCommand>,
) {
    let was_empty = match pending.lock() {
        Ok(mut q) => {
            let empty = q.is_empty();
            q.push_back(delivery);
            empty
        }
        Err(_) => return,
    };
    if was_empty {
        let _ = proxy.send_event(AppCommand::ThumbnailsAvailable);
    }
}

// ── CGImage → RGBA8 conversion ─────────────────────────────────────────
//
// CGImage / CGColorSpace / CGContext opaque types. Declared locally
// because the `objc2-core-graphics` 0.3 crate doesn't re-export the
// classic `CGBitmapContextCreate` constructor — it only exposes the
// newer block-based `CGBitmapContextCreateAdaptive`, which is overkill
// for a one-shot RGBA8 blit. Linking the symbol directly is stable and
// has been since 10.0.

#[allow(non_camel_case_types)]
type CGImageRef = *const c_void;
#[allow(non_camel_case_types)]
type CGContextRef = *const c_void;
#[allow(non_camel_case_types)]
type CGColorSpaceRef = *const c_void;

/// `kCGImageAlphaPremultipliedLast` (RGBA) | `kCGBitmapByteOrder32Big`.
/// `(4 << 12)` is `kCGBitmapByteOrder32Big`, which lays bytes in memory
/// as R, G, B, A — what `wgpu`'s `Rgba8UnormSrgb` expects.
const BITMAP_INFO_RGBA8_PREMUL: u32 = 1 | (4 << 12);

unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;

    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRectC, image: CGImageRef);
    fn CGContextRelease(ctx: CGContextRef);
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;

    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPointC {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSizeC {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRectC {
    origin: CGPointC,
    size: CGSizeC,
}

fn cg_image_to_rgba8(rep: &QLThumbnailRepresentation) -> Option<ThumbnailPixels> {
    unsafe {
        let cg_image_retained = rep.CGImage();
        let cg_image_ptr: CGImageRef = Retained::as_ptr(&cg_image_retained) as CGImageRef;
        if cg_image_ptr.is_null() {
            return None;
        }
        let width = CGImageGetWidth(cg_image_ptr);
        let height = CGImageGetHeight(cg_image_ptr);
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return None;
        }
        let bytes_per_row = width.checked_mul(4)?;
        let total = bytes_per_row.checked_mul(height)?;
        let mut buffer: Vec<u8> = vec![0; total];

        let color_space = CGColorSpaceCreateDeviceRGB();
        if color_space.is_null() {
            return None;
        }
        let ctx = CGBitmapContextCreate(
            buffer.as_mut_ptr() as *mut c_void,
            width,
            height,
            8,
            bytes_per_row,
            color_space,
            BITMAP_INFO_RGBA8_PREMUL,
        );
        CGColorSpaceRelease(color_space);
        if ctx.is_null() {
            return None;
        }
        let rect = CGRectC {
            origin: CGPointC { x: 0.0, y: 0.0 },
            size: CGSizeC {
                width: width as f64,
                height: height as f64,
            },
        };
        CGContextDrawImage(ctx, rect, cg_image_ptr);
        CGContextRelease(ctx);
        Some(ThumbnailPixels {
            width: width as u32,
            height: height as u32,
            rgba: buffer,
        })
    }
}
