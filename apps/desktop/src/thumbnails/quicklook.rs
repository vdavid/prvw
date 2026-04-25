//! `QLThumbnailGenerator` bridge. Submits thumbnail requests; completion
//! blocks forward results to the main thread via `EventLoopProxy::send_event`.
//!
//! ## Life of a request
//!
//! 1. Main thread calls [`submit`] with an index, path, and display scale.
//! 2. We build an `NSURL` + `QLThumbnailGenerationRequest`, retain the
//!    request in a `HashMap` keyed by `request_id` for cancellation, then
//!    call `generateBestRepresentationForRequest:completionHandler:`.
//! 3. `quicklookd` generates the thumb (or hits its cache) and invokes the
//!    completion block on an internal queue.
//! 4. The block converts the `CGImage` to `RGBA8` via a bitmap context and
//!    fires an `AppCommand::ThumbnailReady` (or `::Failed`) through the
//!    cloned `EventLoopProxy`.
//! 5. Main thread's `execute_command` hands the bytes to the thumb cache
//!    and asks the scheduler for the next request.
//!
//! The cache staleness is handled by `quicklookd` internally — its cache
//! key includes the file's mtime, so modified files get fresh thumbs
//! automatically.

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
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use winit::event_loop::EventLoopProxy;

/// A ready thumbnail: raw RGBA8, row-packed (no padding).
pub struct ThumbnailPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Arguments for [`RequestTable::submit`]. Grouped into a struct because
/// the call site otherwise exceeds clippy's `too_many_arguments` threshold
/// and because naming each field makes the submission point readable.
pub struct SubmitRequest<'a> {
    pub request_id: RequestId,
    pub index: usize,
    pub folder_generation: u64,
    pub path: &'a Path,
    pub size: CGSize,
    pub scale: f64,
    pub proxy: EventLoopProxy<AppCommand>,
}

/// In-flight request table. Maps `request_id` to the retained QL request
/// object so we can call `cancelRequest:`. Main-thread only.
pub struct RequestTable {
    entries: HashMap<RequestId, Retained<QLThumbnailGenerationRequest>>,
    generator: Retained<QLThumbnailGenerator>,
}

impl RequestTable {
    pub fn new() -> Self {
        // Shared generator — `sharedGenerator` returns a process-wide
        // singleton. Fine to cache once.
        let generator = unsafe { QLThumbnailGenerator::sharedGenerator() };
        Self {
            entries: HashMap::new(),
            generator,
        }
    }

    /// Submit a thumbnail request. `completion_proxy` is cloned and moved
    /// into the completion block; the block fires exactly once and hands
    /// back either `AppCommand::ThumbnailReady` or `ThumbnailFailed`.
    ///
    /// Must be called from the main thread.
    pub fn submit(&mut self, req: SubmitRequest<'_>) {
        let SubmitRequest {
            request_id,
            index,
            folder_generation,
            path,
            size,
            scale,
            proxy,
        } = req;
        let Some(path_str) = path.to_str() else {
            let _ = proxy.send_event(AppCommand::ThumbnailFailed {
                index,
                request_id,
                folder_generation,
            });
            return;
        };
        unsafe {
            // Build the `NSURL`. `NSURL::fileURLWithPath` runs through
            // Foundation's path normalizer (handles spaces, UTF-8, etc.).
            let ns_path = NSString::from_str(path_str);
            let url: Retained<NSURL> = NSURL::fileURLWithPath(&ns_path);

            let request =
                QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                    QLThumbnailGenerationRequest::alloc(),
                    &url,
                    size,
                    scale,
                    QLThumbnailGenerationRequestRepresentationTypes::All,
                );
            // Keep the request alive for cancellation.
            self.entries.insert(request_id, request.clone());

            // Wrap the proxy + path in shared state the block captures.
            // The block is `Send` because it's called on quicklookd's
            // queue; EventLoopProxy is `Send`.
            let shared = Arc::new(BlockShared {
                proxy,
                index,
                request_id,
                folder_generation,
                path: path.to_path_buf(),
            });

            let block_shared = Arc::clone(&shared);
            let block = RcBlock::new(
                move |rep: *mut QLThumbnailRepresentation, err: *mut NSError| {
                    handle_completion(&block_shared, rep, err);
                },
            );

            self.generator
                .generateBestRepresentationForRequest_completionHandler(&request, &block);
        }
    }

    /// Cancel every in-flight request. Used on folder change.
    pub fn cancel_all(&mut self) {
        let requests: Vec<_> = self.entries.drain().map(|(_, r)| r).collect();
        for req in requests {
            unsafe {
                self.generator.cancelRequest(&req);
            }
        }
    }

    /// Drop the retained request object for an id that just completed.
    /// Prevents a tiny leak and keeps the map a truthful in-flight view.
    pub fn forget(&mut self, request_id: RequestId) {
        self.entries.remove(&request_id);
    }
}

struct BlockShared {
    proxy: EventLoopProxy<AppCommand>,
    index: usize,
    request_id: RequestId,
    folder_generation: u64,
    path: PathBuf,
}

fn handle_completion(
    shared: &Arc<BlockShared>,
    rep: *mut QLThumbnailRepresentation,
    err: *mut NSError,
) {
    if rep.is_null() || !err.is_null() {
        if !err.is_null() {
            let msg = unsafe {
                let ns_err = &*err;
                ns_err.localizedDescription().to_string()
            };
            log::debug!(
                "QLThumbnailGenerator failed for {} (index {}): {msg}",
                shared.path.display(),
                shared.index
            );
        }
        let _ = shared.proxy.send_event(AppCommand::ThumbnailFailed {
            index: shared.index,
            request_id: shared.request_id,
            folder_generation: shared.folder_generation,
        });
        return;
    }
    let pixels = unsafe {
        let rep = &*rep;
        cg_image_to_rgba8(rep)
    };
    match pixels {
        Some(p) => {
            let _ = shared.proxy.send_event(AppCommand::ThumbnailReady {
                index: shared.index,
                request_id: shared.request_id,
                folder_generation: shared.folder_generation,
                width: p.width,
                height: p.height,
                rgba: p.rgba,
            });
        }
        None => {
            let _ = shared.proxy.send_event(AppCommand::ThumbnailFailed {
                index: shared.index,
                request_id: shared.request_id,
                folder_generation: shared.folder_generation,
            });
        }
    }
}

// ── CGImage → RGBA8 conversion ─────────────────────────────────────────

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

/// `kCGImageAlphaPremultipliedLast` (RGBA) | `kCGBitmapByteOrder32Big` on
/// a little-endian machine. The byte-order constants in CGImage.h are
/// indexed 1..=4: 16Little, 32Little, 16Big, 32Big. `32Big` is the one
/// that lays bytes in memory as R, G, B, A — what `wgpu`'s
/// `Rgba8UnormSrgb` expects. `32Little` (the initial implementation here)
/// produced `A, B, G, R` memory order on Apple Silicon, which the sampler
/// then interpreted as pink-tinted, translucent nonsense.
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
        // `CGImage()` returns a `Retained<CGImage>` wrapper; we take its
        // raw pointer for the FFI blit. The `Retained` drops at end of
        // scope and releases.
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
