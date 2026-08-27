//! Prvw's own preview generator: the platforms without a `quicklookd`.
//!
//! macOS asks the system for preview pixels ([`super::quicklook`]). Nothing else has an
//! equivalent that answers for every format Prvw opens, so this module makes them itself, on a
//! small worker pool, and hands back the same [`Delivery`] the QuickLook bridge does. Everything
//! above the two — the scheduler, the byte-budget cache, the dim prefetcher, `App`'s pump — is
//! shared.
//!
//! ## Three routes, and which file takes which
//!
//! [`route_for`] decides, and it's pure so a Mac can assert what Windows will do:
//!
//! - **[`Route::EmbeddedRaw`] — every camera RAW, on every platform.** The camera's own JPEG
//!   preview, through `decoding::raw_preview`. Never the shell's.
//! - **[`Route::System`] — everything else, where a system thumbnail cache exists.** On Windows
//!   that's `IShellItemImageFactory`, which reads the same `thumbcache_*.db` Explorer fills, so a
//!   folder the user has already looked at costs a cache read rather than a decode.
//! - **[`Route::Decode`] — everything else, otherwise.** Decode the file and downscale it.
//!
//! ### Decision: RAW never goes to the shell
//!
//! **Why:** two reasons, and either alone would settle it. The shell only renders a RAW when
//! Microsoft's Raw Image Extension is installed, so on a machine without it every RAW in the
//! folder is a blank screen. And when it *is* installed, Microsoft's develop is not Prvw's:
//! white balance, tone, and highlight recovery all differ, so the placeholder would visibly
//! change colour the moment the real develop landed. `decoding::decode_raw_preview` is the
//! **same** call `navigation::preloader` already makes for a RAW cache-miss, so the preview and
//! the quick preview that follows it are the same pixels rather than two guesses.
//!
//! ### Decision: previews are sRGB, never display-managed
//!
//! **Why:** the shell hands back sRGB-ish pixels with no way to ask for anything else, and
//! quicklookd does the same on macOS. Colour-managing only the RAW route would make one route
//! disagree with the other two on a wide-gamut display. A preview is a soft placeholder that the
//! colour-managed full decode replaces within a second, so one rule for all three routes beats
//! per-route exactness.
//!
//! ## Threading
//!
//! A pool, where macOS has a single thread, because the difference is where the work happens:
//! `quicklookd` is out-of-process and asynchronous, so one thread submitting is enough, while
//! every route here is a synchronous decode or shell call that occupies the thread running it.
//! The pool is [`super::max_parallel`] threads, the same number the scheduler will let be
//! in flight at once, so a queued job always has a worker waiting for it.
//!
//! Shape borrowed from [`super::dim_prefetch`]: one `mpsc` channel, its receiver behind a
//! `Mutex` the workers take turns on, and an epoch counter that lets a folder change abandon
//! queued work without a per-job cancellation protocol.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;
use crate::decoding::{self, PixelBuffer, RawPipelineFlags};
use crate::previews::request::{self, Delivery, Pending, PreviewPixels, SubmitRequest};
use crate::previews::scheduler::RequestId;

#[cfg(target_os = "windows")]
use super::shell;

/// Per-worker stack. The default 8 MB times a pool the size of half the cores is a lot of
/// address space for bodies that decode into heap buffers. 2 MB matches `dim_prefetch`.
const WORKER_STACK_SIZE: usize = 2 * 1024 * 1024;

/// Bounds on the pixel size a generator is asked for. The floor keeps a nonsense scale factor
/// from producing a 1-pixel preview; the ceiling is the same 8192 the conversion paths refuse to
/// allocate past, so a bad scale can't turn into a gigabyte of thumbnail.
const MIN_PREVIEW_PX: u32 = 32;
const MAX_PREVIEW_PX: u32 = 8192;

/// A request's longest edge in real pixels: its point size times the display scale, rounded and
/// clamped. Pure, so a Mac can assert what Windows will ask the shell for.
fn request_pixels(size_pt: f64, scale: f64) -> u32 {
    let px = size_pt * scale;
    if !px.is_finite() {
        return MIN_PREVIEW_PX;
    }
    (px.round().clamp(0.0, f64::from(MAX_PREVIEW_PX)) as u32).clamp(MIN_PREVIEW_PX, MAX_PREVIEW_PX)
}

/// Whether this platform has a system thumbnail cache [`Route::System`] can read. Windows does;
/// macOS never reaches here, and Linux's desktop thumbnail spec is a directory of PNGs written
/// by whichever file manager happened to visit the folder, which is not a service we can ask.
const HAS_SYSTEM_THUMBNAILS: bool = cfg!(target_os = "windows");

/// Where one file's preview pixels come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The camera's embedded JPEG preview, via `decoding::decode_raw_preview`.
    EmbeddedRaw,
    /// The system thumbnail cache, given a path spelled the way it will accept
    /// (`paths::shell_path`).
    System(String),
    /// Decode the file ourselves and downscale the result.
    Decode,
}

/// Pick the route for `path`. `system` is whether this platform has a thumbnail service at all,
/// a parameter rather than a `cfg` so a test on any host can ask for either answer.
pub fn route_for(path: &Path, system: bool) -> Route {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if decoding::is_raw_extension(ext) {
        return Route::EmbeddedRaw;
    }
    // A path with no legal shell spelling (too long once de-verbatimed, or a volume-GUID path)
    // isn't one the shell can be given, and those are exactly the deep libraries Prvw is for. So
    // it falls through to our own decode rather than to nothing.
    if system && let Some(shell) = crate::paths::shell_path(path) {
        return Route::System(shell);
    }
    Route::Decode
}

/// A job queued for the pool.
struct Job {
    request_id: RequestId,
    index: usize,
    folder_generation: u64,
    path: PathBuf,
    pixels: u32,
    epoch: u64,
    proxy: EventLoopProxy<AppCommand>,
}

/// Front-end handle owned by `previews::State` on the main thread. Same surface as
/// [`super::quicklook::RequestTable`], so `App` has one call site for both.
pub struct RequestTable {
    job_tx: mpsc::Sender<Job>,
    pending: Pending,
    /// Bumped by [`Self::cancel_all`]. A job whose stamp no longer matches is dropped when a
    /// worker pops it, which is what makes cancellation free.
    epoch: Arc<AtomicU64>,
    _workers: Vec<thread::JoinHandle<()>>,
}

impl RequestTable {
    /// Spawn the pool. `wake` constructs the `AppCommand` a completion fires (only when the
    /// pending queue was empty) to nudge winit's loop into draining; `thread_name` prefixes the
    /// OS thread names.
    pub fn new(wake: fn() -> AppCommand, thread_name: &'static str) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let pending = request::new_pending();
        let epoch = Arc::new(AtomicU64::new(0));
        let count = super::max_parallel();

        let workers: Vec<_> = (0..count)
            .map(|i| {
                let job_rx = Arc::clone(&job_rx);
                let pending = Arc::clone(&pending);
                let epoch = Arc::clone(&epoch);
                thread::Builder::new()
                    .name(format!("{thread_name}-{i}"))
                    .stack_size(WORKER_STACK_SIZE)
                    .spawn(move || worker_loop(&job_rx, &pending, &epoch, wake))
                    .expect("Failed to spawn a preview generation worker")
            })
            .collect();

        log::debug!("Spawned {count} preview generation workers ({thread_name})");
        Self {
            job_tx,
            pending,
            epoch,
            _workers: workers,
        }
    }

    /// Queue a preview. Returns immediately: the path copy is the only work on the main thread.
    pub fn submit(&self, req: SubmitRequest<'_>) {
        let _ = self.job_tx.send(Job {
            request_id: req.request_id,
            index: req.index,
            folder_generation: req.folder_generation,
            path: req.path.to_path_buf(),
            pixels: request_pixels(req.size_pt, req.scale),
            epoch: self.epoch.load(Ordering::Relaxed),
            proxy: req.proxy,
        });
    }

    /// Abandon everything queued. Used on folder change.
    ///
    /// A job already running finishes: a decode has no checkpoint to stop at, and a shell call
    /// is someone else's to abort. It's harmless — every delivery carries the folder generation
    /// it was submitted under, and `execute_command` drops the ones that no longer match.
    pub fn cancel_all(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    /// Drain all queued deliveries. Called from the main-thread handler for the wake command.
    pub fn drain_pending(&self) -> Vec<Delivery> {
        request::drain(&self.pending)
    }
}

fn worker_loop(
    rx: &Arc<Mutex<mpsc::Receiver<Job>>>,
    pending: &Pending,
    epoch: &Arc<AtomicU64>,
    wake: fn() -> AppCommand,
) {
    // The shell route is COM, and COM is per-thread. Entering the apartment here rather than
    // per-request keeps the cost off every preview, and the guard leaves it on the way out.
    #[cfg(target_os = "windows")]
    let _apartment = shell::Apartment::enter();

    loop {
        // Pop under the shared receiver mutex, held only for the `recv` itself: `mpsc::Receiver`
        // isn't `Sync`, and the workers run concurrently once they have a job.
        let job = match rx.lock() {
            Ok(guard) => match guard.recv() {
                Ok(job) => job,
                Err(_) => return, // Sender dropped: the process is exiting.
            },
            Err(_) => return, // Poisoned.
        };

        // Cheap staleness check before touching the disk.
        if job.epoch != epoch.load(Ordering::Relaxed) {
            continue;
        }

        let route = route_for(&job.path, HAS_SYSTEM_THUMBNAILS);
        // Guarded, because two of the three routes run a parser on numbers the file supplies and
        // a corrupt neighbour would otherwise take this worker down for the session
        // (`super::without_panicking` has the full reasoning).
        let result = super::without_panicking("Preview generation", &job.path, || {
            produce(&route, &job.path, job.pixels)
        })
        .ok_or(());
        if result.is_err() {
            log::debug!(
                "No preview for {} (index {}) via {route:?}",
                job.path.display(),
                job.index
            );
        }

        // Re-check: a shell call on a cold cache, or a decode of a 50 MP file, is long enough
        // for the user to have moved on. Dropping it here saves the main thread the wake-up.
        if job.epoch != epoch.load(Ordering::Relaxed) {
            continue;
        }
        request::push_delivery(
            pending,
            Delivery {
                index: job.index,
                request_id: job.request_id,
                folder_generation: job.folder_generation,
                result,
            },
            &job.proxy,
            wake,
        );
    }
}

/// Run one route. `None` means no preview for this file, which leaves the "Loading…" pill to
/// cover the gap — the same outcome a failed QuickLook request produces on macOS.
fn produce(route: &Route, path: &Path, pixels: u32) -> Option<PreviewPixels> {
    match route {
        Route::EmbeddedRaw => {
            let decoded =
                decoding::decode_raw_preview(path, crate::color::srgb_icc_bytes(), false)?;
            downscale(decoded, pixels)
        }
        Route::System(shell_path) => system_thumbnail(shell_path, pixels),
        Route::Decode => {
            // Not cancellable: the epoch check either side of this call is the whole
            // cancellation story here, and `load_image`'s flag would need a per-job `AtomicBool`
            // to say anything more. RAW never reaches this arm, so `raw_flags` can't matter and
            // the SDR headroom keeps the result `Rgba8`.
            let decoded = decoding::load_image(
                path,
                &AtomicBool::new(false),
                crate::color::srgb_icc_bytes(),
                false,
                RawPipelineFlags::default(),
                1.0,
                None,
            )
            .ok()?;
            downscale(decoded, pixels)
        }
    }
}

/// The system thumbnail cache, where there is one. Every platform compiles the arm; only
/// Windows has something behind it.
#[allow(unused_variables)]
fn system_thumbnail(shell_path: &str, pixels: u32) -> Option<PreviewPixels> {
    #[cfg(target_os = "windows")]
    {
        shell::thumbnail(shell_path, pixels)
    }
    #[cfg(not(target_os = "windows"))]
    {
        // `route_for` never returns `System` here, so this is unreachable rather than a gap.
        None
    }
}

/// Fit a decoded image into a `pixels`-square box, keeping its aspect ratio. Already-small
/// images pass through untouched: a preview is a placeholder, and upscaling one would only cost
/// memory to look the same.
fn downscale(decoded: decoding::DecodedImage, pixels: u32) -> Option<PreviewPixels> {
    let PixelBuffer::Rgba8(rgba) = decoded.pixels else {
        // Only a RAW develop with HDR headroom returns half-floats, and neither route asks for
        // one. Worth a line rather than a silent skip if that contract ever changes.
        log::warn!("Preview: got a half-float buffer, which the preview cache can't carry");
        return None;
    };
    let (width, height) = (decoded.width, decoded.height);
    if rgba.len() != width as usize * height as usize * 4 {
        return None;
    }
    let (target_w, target_h) = fit_within(width, height, pixels);
    if (target_w, target_h) == (width, height) {
        return Some(PreviewPixels {
            width,
            height,
            rgba,
        });
    }
    let source = image::RgbaImage::from_raw(width, height, rgba)?;
    // A box filter, slightly softer than a Lanczos resize, which is what a "not final yet"
    // placeholder wants — the same choice `decoding::raw_preview` makes.
    let scaled = image::imageops::thumbnail(&source, target_w, target_h);
    Some(PreviewPixels {
        width: target_w,
        height: target_h,
        rgba: scaled.into_raw(),
    })
}

/// The largest `width × height`-shaped box that fits inside `pixels` square, never enlarging and
/// never collapsing an edge to zero.
fn fit_within(width: u32, height: u32, pixels: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest == 0 || longest <= pixels {
        return (width, height);
    }
    let scale = f64::from(pixels) / f64::from(longest);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// Turn the 32-bit DIB a Win32 thumbnail comes back as into the RGBA8 the renderer uploads.
///
/// Two fixups. GDI lays the channels out B, G, R, A, and the app wants R, G, B, A. And the alpha
/// byte is only meaningful when the source had transparency: a thumbnail composited by a GDI
/// path that predates alpha comes back with every alpha byte zero, which would upload as a fully
/// transparent placeholder — worse than no placeholder at all. So an all-zero alpha channel is
/// read as "this bitmap has no alpha" and forced opaque, while any non-zero byte is taken at
/// face value.
///
/// Windows is the only caller; like the rest of this module it compiles everywhere so the tests
/// below run on any host, which is the only way it gets checked before meeting a Windows box.
pub(super) fn dib_to_rgba8(buffer: &mut [u8]) {
    let mut any_alpha = false;
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        any_alpha |= pixel[3] != 0;
    }
    if !any_alpha {
        for pixel in buffer.as_chunks_mut::<4>().0 {
            pixel[3] = 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    /// RAW is ours on every platform, shell or no shell. This is the decision the module docs
    /// argue for, and the one a "just use the shell for everything" cleanup would undo.
    #[test]
    fn raw_never_goes_to_the_shell() {
        for name in [r"\\?\C:\pics\a.dng", r"\\?\C:\pics\b.ARW", r"/pics/c.cr2"] {
            assert_eq!(route_for(&path(name), true), Route::EmbeddedRaw, "{name}");
            assert_eq!(route_for(&path(name), false), Route::EmbeddedRaw, "{name}");
        }
    }

    /// Everything else takes the shell where there is one, de-verbatimed on the way.
    #[test]
    fn ordinary_files_take_the_shell_when_there_is_one() {
        assert_eq!(
            route_for(&path(r"\\?\C:\pics\a.jpg"), true),
            Route::System(r"C:\pics\a.jpg".to_string())
        );
        assert_eq!(
            route_for(&path(r"\\?\UNC\naspi\photos\b.png"), true),
            Route::System(r"\\naspi\photos\b.png".to_string())
        );
        assert_eq!(route_for(&path(r"\\?\C:\pics\a.jpg"), false), Route::Decode);
    }

    /// A path the shell can't be given decodes instead of going without. Deep libraries on a NAS
    /// are exactly who this matters for, so "no preview" would be the wrong answer.
    #[test]
    fn a_path_the_shell_cannot_take_falls_back_to_decoding() {
        let deep = format!(r"\\?\C:\{}\a.jpg", "d".repeat(300));
        assert_eq!(route_for(&path(&deep), true), Route::Decode);
        // A volume-GUID path has no plain spelling at all.
        let guid = r"\\?\Volume{11111111-2222-3333-4444-555555555555}\pics\a.jpg";
        assert_eq!(route_for(&path(guid), true), Route::Decode);
        // And a reserved DOS device name in the middle of one.
        assert_eq!(
            route_for(&path(r"\\?\C:\pics\NUL.jpg"), true),
            Route::Decode
        );
    }

    /// A file with no extension is nobody's RAW, and still worth asking the shell about — it
    /// reads the content, not the name.
    #[test]
    fn an_extensionless_file_still_reaches_the_shell() {
        assert_eq!(
            route_for(&path(r"C:\pics\scan"), true),
            Route::System(r"C:\pics\scan".to_string())
        );
    }

    /// The two sizes the app asks for, on the scales it meets.
    #[test]
    fn points_times_scale_is_pixels() {
        assert_eq!(request_pixels(512.0, 2.0), 1024); // previews on a Retina Mac
        assert_eq!(request_pixels(512.0, 1.0), 512); // and on a 1× display
        assert_eq!(request_pixels(512.0, 1.5), 768); // a 150% Windows display
        assert_eq!(request_pixels(512.0, 1.25), 640);
    }

    /// A scale factor that arrived wrong can't turn into a useless preview or a huge allocation.
    #[test]
    fn nonsense_scales_are_clamped() {
        assert_eq!(request_pixels(512.0, 0.0), MIN_PREVIEW_PX);
        assert_eq!(request_pixels(512.0, -3.0), MIN_PREVIEW_PX);
        assert_eq!(request_pixels(512.0, 1e9), MAX_PREVIEW_PX);
        assert_eq!(request_pixels(f64::NAN, 2.0), MIN_PREVIEW_PX);
        assert_eq!(request_pixels(f64::INFINITY, 2.0), MIN_PREVIEW_PX);
    }

    #[test]
    fn fit_within_keeps_the_aspect_ratio() {
        assert_eq!(fit_within(4000, 3000, 1024), (1024, 768));
        assert_eq!(fit_within(3000, 4000, 1024), (768, 1024));
        assert_eq!(fit_within(1024, 1024, 1024), (1024, 1024));
    }

    /// A small image is left alone: a placeholder gains nothing from being upscaled into more
    /// bytes of the same picture.
    #[test]
    fn fit_within_never_enlarges() {
        assert_eq!(fit_within(300, 200, 1024), (300, 200));
        assert_eq!(fit_within(0, 0, 1024), (0, 0));
    }

    /// A panorama is the case where rounding could collapse the short edge to nothing, and a
    /// zero-height image is one `image` refuses to build.
    #[test]
    fn fit_within_keeps_both_edges_at_least_one() {
        let (w, h) = fit_within(20000, 15, 1024);
        assert_eq!(w, 1024);
        assert_eq!(h, 1);
    }

    #[test]
    fn dib_channels_are_swapped_into_rgba() {
        // B, G, R, A per pixel, with real alpha.
        let mut buffer = vec![10, 20, 30, 128, 40, 50, 60, 255];
        dib_to_rgba8(&mut buffer);
        assert_eq!(buffer, vec![30, 20, 10, 128, 60, 50, 40, 255]);
    }

    /// The fixup that stops a legacy GDI thumbnail uploading as an invisible placeholder.
    #[test]
    fn a_blank_alpha_channel_is_forced_opaque() {
        let mut buffer = vec![10, 20, 30, 0, 40, 50, 60, 0];
        dib_to_rgba8(&mut buffer);
        assert_eq!(buffer, vec![30, 20, 10, 255, 60, 50, 40, 255]);
    }

    /// One transparent pixel among opaque ones is real transparency, not a blank channel, so it
    /// survives.
    #[test]
    fn real_transparency_survives() {
        let mut buffer = vec![10, 20, 30, 0, 40, 50, 60, 255];
        dib_to_rgba8(&mut buffer);
        assert_eq!(buffer, vec![30, 20, 10, 0, 60, 50, 40, 255]);
    }
}
