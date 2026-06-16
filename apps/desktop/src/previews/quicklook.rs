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
//! ## Size-parameterized: one worker, two request paths
//!
//! This worker is **already** size-parameterized — every submission carries
//! its own `size`/`scale` ([`SubmitRequest`]), so the previews path (requests
//! ~512pt × Retina ≈ 1024px, used as decode placeholders) and the browse
//! grid (Phase 4: requests `browser::thumbnail_cache::GRID_THUMBNAIL_PX`)
//! share this one `QLThumbnailGenerator`-backed worker rather than spinning up
//! a second engine. `QLThumbnailGenerator` *is* Finder's shared `quicklookd`
//! cache, so a second request path is the right design — not a second cache.
//!
//! The one place the two paths diverge is **how the resulting `CGImage` is
//! consumed**: previews blit it to a row-packed RGBA8 `Vec<u8>`
//! ([`cg_image_to_rgba8`]) because they upload it to a wgpu texture. The grid
//! has no wgpu — its cells are `NSImageView`s, so Phase 4 will add a sibling
//! CGImage→`NSImage` consumption path (no Rust pixel copy) reusing this same
//! submission worker. That seam is the `cg_image_to_rgba8` call inside the
//! completion block; everything up to obtaining the `CGImage` is shared. Phase
//! 2 intentionally leaves the live grid request path unbuilt (it needs the
//! event loop + AppKit), shipping only the headless scheduler + cache state
//! the grid will drive; the previews behavior here is unchanged.
//!
//! The completion block runs on QuickLook's internal queue (not our
//! worker) — it pushes the delivery, wakes winit, and fires a `Forget`
//! to the worker so `entries` doesn't grow unbounded over a long
//! folder browse.

use crate::commands::AppCommand;
use crate::previews::scheduler::RequestId;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSError, NSSize, NSString, NSURL};
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

/// A ready preview: raw RGBA8, row-packed (no padding).
pub struct PreviewPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One result slot delivered to the main thread by a QL completion block.
/// `Ok(pixels)` for a generated preview, `Err` for a failed request.
pub struct Delivery {
    pub index: usize,
    pub request_id: RequestId,
    pub folder_generation: u64,
    pub result: Result<PreviewPixels, ()>,
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
    /// preview completed naturally. Sent by the completion block.
    Forget(RequestId),
    /// Cancel every in-flight request. Used on folder change.
    CancelAll,
}

/// Front-end handle owned by `previews::State` on the main thread.
/// All operations are non-blocking from the main thread's perspective:
/// they just shovel an `mpsc` message to the worker.
///
/// The browse grid (`browser::grid`) owns a **second** `RequestTable` — a
/// second request path into the same shared `quicklookd` cache (the
/// `QLThumbnailGenerator` singleton), not a second engine. Both worker threads
/// call `sharedGenerator`, so they hit the same Finder cache; they differ only
/// in the wake command they fire (`wake`) and in how the main thread consumes
/// the delivered RGBA8 (previews blit to a wgpu texture; the grid builds an
/// `NSImage` via [`nsimage_from_rgba8`]).
pub struct RequestTable {
    submit_tx: mpsc::Sender<WorkerMsg>,
    pending: Arc<Mutex<VecDeque<Delivery>>>,
    _worker: thread::JoinHandle<()>,
}

impl RequestTable {
    /// Spawn a submission worker. `wake` constructs the `AppCommand` the completion path fires
    /// (only when the pending queue was empty) to nudge winit's loop into draining —
    /// `|| PreviewsAvailable` for the previews path, `|| BrowseThumbnailsAvailable` for the grid.
    /// It's an `fn` (not a value) because `AppCommand` isn't `Clone` (some variants hold an
    /// `mpsc::Sender`), and the completion path needs a fresh wake event each time. `thread_name`
    /// names the OS worker thread for logs.
    pub fn new(wake: fn() -> AppCommand, thread_name: &'static str) -> Self {
        let (submit_tx, submit_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        let pending_for_worker = Arc::clone(&pending);
        let forget_tx = submit_tx.clone();
        let worker = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || worker_loop(submit_rx, pending_for_worker, forget_tx, wake))
            .expect("Failed to spawn QL submission worker");
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
    /// for `AppCommand::PreviewsAvailable`.
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
    wake: fn() -> AppCommand,
) {
    // Get the singleton generator on this thread. `sharedGenerator()`
    // is process-wide; the `Retained<>` we keep here is just a local
    // reference that we never need to share.
    let generator = unsafe { QLThumbnailGenerator::sharedGenerator() };
    let mut entries: HashMap<RequestId, Retained<QLThumbnailGenerationRequest>> = HashMap::new();
    log::debug!("Preview submission worker started");
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
                    wake,
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
                    log::debug!("Cancelled {count} in-flight preview requests");
                }
            }
        }
    }
    log::debug!("Preview submission worker exiting");
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
    wake: fn() -> AppCommand,
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
            wake,
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
        // `PreviewFailed` path, no placeholder), leaving the "Loading…" pill
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
                    wake,
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
    wake: fn() -> AppCommand,
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
        let _ = proxy.send_event(wake());
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

fn cg_image_to_rgba8(rep: &QLThumbnailRepresentation) -> Option<PreviewPixels> {
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
        Some(PreviewPixels {
            width: width as u32,
            height: height as u32,
            rgba: buffer,
        })
    }
}

// ── RGBA8 → NSImage (the grid's consumption seam) ──────────────────────
//
// The previews path blits the QL `CGImage` to RGBA8 (above) because it
// uploads to a wgpu texture. The browse grid (`browser::grid`) has no wgpu —
// its cells are `NSImageView`s — so it consumes the **same** worker's RGBA8
// `Delivery` and wraps it in an `NSImage` here, on the main thread (`NSImage`
// isn't `Send`, so it can't be built on the QL queue or the worker thread).
// This is the divergence the module-level "Size-parameterized" docs name:
// everything up to the RGBA8 buffer is shared; only this final wrap is
// grid-specific.

/// Build an `NSImage` from a row-packed, premultiplied RGBA8 buffer (`width × height × 4`), as
/// produced by [`PreviewPixels`]. Returns `None` if the buffer is the wrong size or AppKit refuses
/// the bitmap. Must run on the main thread (`mtm`) — `NSImage`/`NSBitmapImageRep` are main-thread
/// AppKit objects.
///
/// We allocate the bitmap rep with `planes = null` so it owns its own backing store, then copy our
/// bytes into `rep.bitmapData()`. That avoids tying the `NSImage`'s lifetime to a Rust `Vec` (the
/// rep would otherwise alias a freed buffer once `rgba` drops). The pixels are premultiplied-alpha
/// RGBA in device-RGB, matching `cg_image_to_rgba8`'s `kCGImageAlphaPremultipliedLast` output.
#[must_use]
pub fn nsimage_from_rgba8(
    width: u32,
    height: u32,
    rgba: &[u8],
    mtm: MainThreadMarker,
) -> Option<Retained<NSImage>> {
    let (w, h) = (width as usize, height as usize);
    let bytes_per_row = w.checked_mul(4)?;
    if rgba.len() != bytes_per_row.checked_mul(h)? || w == 0 || h == 0 {
        return None;
    }
    let _ = mtm; // The rep + image are built below; `mtm` proves we're on the main thread.
    unsafe {
        // planes = null → the rep allocates and owns its backing buffer.
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,                       // bits per sample
            4,                       // samples per pixel (RGBA)
            true,                    // has alpha
            false,                   // not planar (interleaved)
            NSDeviceRGBColorSpace,
            bytes_per_row as isize,
            32,                      // bits per pixel
        )?;
        // Copy our pixels into the rep's own buffer.
        let dst: *mut u8 = rep.bitmapData();
        if dst.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), dst, rgba.len());

        let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(w as f64, h as f64));
        image.addRepresentation(&rep);
        Some(image)
    }
}
