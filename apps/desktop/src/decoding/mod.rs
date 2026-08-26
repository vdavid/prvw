//! # Decoding
//!
//! Image format decoders. JPEG via `zune-jpeg` (SIMD); PNG, GIF, WebP, BMP, TIFF via the
//! `image` crate; camera RAW (ARW, CR2, CR3, DNG, NEF, ORF, PEF, RAF, RW2, SRW) via
//! `rawler`. Also extracts the embedded ICC profile (transform lives in `crate::color`).
//!
//! ## Key choices
//!
//! - **`zune-jpeg` for JPEG** — significantly faster than the `image` crate's JPEG path on
//!   Apple Silicon. Used unconditionally for JPEGs.
//! - **`image` crate for everything else non-RAW** — mature, covers the rest.
//! - **`rawler` for RAW** — runs its built-in develop pipeline (demosaic, white balance,
//!   color matrix, sRGB gamma) in one call, parallelised via rayon.
//! - **Cancellation.** `load_image` takes an `AtomicBool`. The RAW path checks
//!   it between pipeline stages; JPEG and generic decodes (single opaque library
//!   calls) run on an abandonable thread via `run_decode_cancellable`, which
//!   frees the caller within ~10 ms of the flag flipping. The preloader uses
//!   this so navigating away aborts in-flight work instead of blocking the
//!   serial worker. Callers that don't need cancellation (startup, settings
//!   refresh) pass a fresh `&AtomicBool::new(false)`.
//!
//! ## Public API
//!
//! - [`DecodedImage`] — pixel buffer plus dimensions, ready for GPU upload.
//!   Pixels are either RGBA8 (every non-RAW format, plus SDR RAW output) or
//!   RGBA16F (RAW output when HDR is active and the display can display it).
//! - [`load_image`] — decode a file to `DecodedImage`, color-managed to a target
//!   ICC profile, with EXIF orientation applied. Always cancellable.
//! - [`decode_raw_preview`] — fast embedded-JPEG preview for a RAW (no develop),
//!   shown as a soft placeholder on a cache-miss while the full decode runs.
//! - [`is_supported_extension`] / [`is_raw_extension`] — format gates used by the
//!   directory scanner and the quick-preview path.

mod dispatch;
mod dng_opcodes;
pub mod exif_metadata;
mod generic;
mod jpeg;
mod orientation;
mod raw;
mod raw_flags;
mod raw_preview;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dispatch::Backend;
use orientation::{apply_orientation_bytes, parse_exif_orientation};

pub use exif_metadata::{ExifMetadata, parse_exif_metadata, parse_raw_exif};
pub use raw_flags::RawPipelineFlags;
// Range constants are re-exported for the Settings → RAW panel, which is
// currently macOS-only. Silence the Linux unused-import warning.
#[cfg_attr(not(target_os = "macos"), allow(unused_imports))]
pub use raw_flags::{
    BASELINE_EXPOSURE_OFFSET_RANGE, CLARITY_AMOUNT_RANGE, CLARITY_RADIUS_RANGE, HDR_GAIN_RANGE,
    MIDTONE_ANCHOR_RANGE, SATURATION_BOOST_RANGE, SHARPEN_AMOUNT_RANGE,
};

/// Pixel-buffer variants. `Rgba8` is `[r, g, b, a, r, g, b, a, …]` in sRGB
/// gamma-encoded bytes — the common case. `Rgba16F` is `[r, g, b, a, r, …]`
/// where every element is the IEEE 754 half-precision float bit pattern
/// stored as `u16` (use the `half` crate to convert to `f32`). Half-float
/// RGBA is only produced by the RAW decoder when HDR output is active.
pub enum PixelBuffer {
    Rgba8(Vec<u8>),
    Rgba16F(Vec<u16>),
}

impl PixelBuffer {
    /// Bytes per pixel for cache-size accounting and GPU row-pitch math.
    /// RGBA8 is 4 bytes per pixel; RGBA16F is 8 bytes per pixel (four u16s).
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            PixelBuffer::Rgba8(_) => 4,
            PixelBuffer::Rgba16F(_) => 8,
        }
    }

    /// Total byte length of the pixel buffer. Multiply `bytes_per_pixel()`
    /// by pixel count and you get this; kept as a helper so callers don't
    /// need to know which variant they have.
    pub fn byte_len(&self) -> usize {
        match self {
            PixelBuffer::Rgba8(v) => v.len(),
            PixelBuffer::Rgba16F(v) => v.len() * 2,
        }
    }

    /// True when the backing storage is RGBA16F (half-float).
    pub fn is_hdr(&self) -> bool {
        matches!(self, PixelBuffer::Rgba16F(_))
    }
}

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: PixelBuffer,
    /// Curated subset of the file's EXIF metadata, used by the EXIF info
    /// overlay. `None` for formats without EXIF (PNG, GIF, WebP without an
    /// EXIF chunk, BMP) and for files where every interesting field came
    /// back empty. Boxed so `DecodedImage` (and `PreloadResponse::Ready`,
    /// which carries it across the channel) doesn't grow by ~360 B per
    /// instance.
    pub exif: Option<Box<ExifMetadata>>,
}

impl DecodedImage {
    /// Build an RGBA8 image with no EXIF attached. Kept around so the JPEG /
    /// PNG / WebP / etc. paths don't have to know the `PixelBuffer` enum
    /// exists. Callers that have EXIF should mutate `.exif` after.
    pub fn from_rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: PixelBuffer::Rgba8(rgba),
            exif: None,
        }
    }

    /// Build an RGBA16F image. Used by `decoding::raw` when HDR output is
    /// active. `half_rgba` is 4 × width × height `u16`s in RGBA order, each
    /// element an IEEE 754 half-precision bit pattern.
    pub fn from_rgba16f(width: u32, height: u32, half_rgba: Vec<u16>) -> Self {
        Self {
            width,
            height,
            pixels: PixelBuffer::Rgba16F(half_rgba),
            exif: None,
        }
    }
}

/// Sink for a decode that completed *after* its caller abandoned it on
/// cancellation. Called once, on the detached decode thread, with the finished
/// image — see [`run_decode_cancellable`]. Preloader-agnostic by design: the
/// decoding layer just hands back the recovered image; the receiver decides
/// whether to keep it (the preloader gates on the cache window) or drop it.
pub type SalvageSink = Box<dyn FnOnce(DecodedImage) + Send>;

/// Decode an image file to a `DecodedImage`, color-managed to the given
/// target ICC profile. JPEGs use zune-jpeg (SIMD-accelerated). RAW files use
/// rawler. Everything else goes through the `image` crate. Applies EXIF
/// orientation correction automatically. Images without an embedded ICC
/// profile are assumed sRGB and transformed to `target_icc`.
///
/// `cancelled` is the cancellation flag. It frees the caller promptly while
/// reading the file (every 64 KB), between RAW pipeline stages, and — for the
/// opaque JPEG / generic decodes — within ~10 ms via the abandonable
/// `run_decode_cancellable`. Pass `&AtomicBool::new(false)` if you don't need
/// cancellation.
///
/// `salvage` (JPEG / generic only) recovers a decode that finishes *after*
/// cancellation instead of discarding it — see [`run_decode_cancellable`] and
/// [`SalvageSink`]. Pass `None` if you don't want the recovered image.
///
/// `edr_headroom` is the peak-white headroom the display can show (use
/// [`crate::color::display_profile::current_edr_headroom`] on macOS). `1.0`
/// means "SDR only — clip highlights at display-white". Anything above
/// `1.0` combined with `raw_flags.hdr_output == true` triggers the
/// `RGBA16F` output path for RAW files.
pub fn load_image(
    path: &Path,
    cancelled: &AtomicBool,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    raw_flags: RawPipelineFlags,
    edr_headroom: f32,
    salvage: Option<SalvageSink>,
) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

    log::debug!("Loading {}", path.display());
    let start = Instant::now();

    let bytes = read_file_cancellable(path, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    let backend = dispatch::pick_backend(ext);
    let result = decode_with(
        backend,
        path,
        filename,
        bytes,
        Some(cancelled),
        target_icc,
        use_relative_colorimetric,
        raw_flags,
        edr_headroom,
        salvage,
    );

    log_result(&result, ext, backend, path, start);
    result
}

/// Check if a file extension is a supported image format.
pub fn is_supported_extension(ext: &str) -> bool {
    dispatch::is_supported_extension(ext)
}

/// Every extension the app opens, for the file picker's filter.
pub fn supported_extensions() -> Vec<&'static str> {
    dispatch::supported_extensions()
}

/// Whether a file extension is a camera RAW format (gate for the quick-preview
/// path — only RAW decodes are slow enough to need it).
pub fn is_raw_extension(ext: &str) -> bool {
    dispatch::is_raw_extension(ext)
}

/// Whether a file extension takes the fast JPEG decode path. The metadata tier
/// reads it so its per-format routing can't drift from the decoder's.
pub fn is_jpeg_extension(ext: &str) -> bool {
    dispatch::is_jpeg_extension(ext)
}

/// Extract the camera's embedded JPEG preview from a RAW file as a soft,
/// downscaled, orientation-corrected placeholder — see [`raw_preview`]. Returns
/// `None` for non-RAW files or RAWs without an embedded preview. Fast (no RAW
/// develop); the preloader shows this instantly on a cache-miss while the full
/// develop runs.
pub fn decode_raw_preview(
    path: &Path,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Option<DecodedImage> {
    raw_preview::decode_raw_preview(path, target_icc, use_relative_colorimetric)
}

/// Dispatch to the chosen backend. JPEG and Generic parse EXIF orientation from the
/// outer file bytes; Raw gets orientation from rawler's decoder metadata instead
/// (rawler always sets `RawImage.orientation` to Normal).
#[allow(clippy::too_many_arguments)] // Internal dispatch; plumbing trumps struct-ifying
fn decode_with(
    backend: Backend,
    path: &Path,
    filename: &str,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    raw_flags: RawPipelineFlags,
    edr_headroom: f32,
    salvage: Option<SalvageSink>,
) -> Result<DecodedImage, String> {
    match backend {
        Backend::Jpeg => {
            let orientation = parse_exif_orientation(&bytes, filename);
            let exif = parse_exif_metadata(&bytes).map(Box::new);
            // The zune-jpeg decode is a single opaque call we can't checkpoint
            // internally, so run it on an abandonable thread instead — see
            // `run_decode_cancellable`. Owned clones (cheap: a few-KB ICC + a
            // path) let the closure be `'static`.
            let owned_path = path.to_path_buf();
            let owned_icc = target_icc.to_vec();
            run_decode_cancellable(cancelled, salvage, move || {
                let mut img =
                    jpeg::decode(&owned_path, bytes, &owned_icc, use_relative_colorimetric)?;
                img.exif = exif;
                Ok(finalize(img, orientation))
            })
        }
        Backend::Generic => {
            let orientation = parse_exif_orientation(&bytes, filename);
            let exif = parse_exif_metadata(&bytes).map(Box::new);
            let owned_path = path.to_path_buf();
            let owned_icc = target_icc.to_vec();
            run_decode_cancellable(cancelled, salvage, move || {
                let mut img =
                    generic::decode(&owned_path, bytes, &owned_icc, use_relative_colorimetric)?;
                img.exif = exif;
                Ok(finalize(img, orientation))
            })
        }
        Backend::Raw => {
            // Try nom-exif first on the outer bytes — DNGs and many camera
            // RAW containers (CR2, NEF) carry a normal EXIF segment
            // alongside the raw payload, and nom-exif gives us the same
            // shape we use for JPEGs. Fall back to rawler's already-parsed
            // metadata for cameras nom-exif doesn't recognise.
            let exif_via_nom = parse_exif_metadata(&bytes);
            let (mut img, orientation, raw_metadata) = raw::decode(
                path,
                bytes,
                cancelled,
                target_icc,
                use_relative_colorimetric,
                raw_flags,
                edr_headroom,
            )?;
            img.exif = exif_via_nom
                .or_else(|| raw_metadata.as_ref().and_then(parse_raw_exif))
                .map(Box::new);
            if orientation != 1 {
                log::debug!("RAW orientation: {orientation} for {filename}");
            }
            Ok(finalize(img, orientation))
        }
    }
}

/// Apply EXIF orientation and update dimensions. Works on both RGBA8 and
/// RGBA16F — the rotation logic is a per-pixel block swap, so it factors
/// across the `bytes_per_pixel()` stride cleanly. For RGBA16F we operate
/// on the underlying `u16` slice directly, treating each "pixel" as a
/// 4-element block (one per RGBA channel).
fn finalize(mut img: DecodedImage, orientation: u16) -> DecodedImage {
    let (old_w, old_h) = (img.width, img.height);
    let (new_w, new_h) = match &mut img.pixels {
        PixelBuffer::Rgba8(bytes) => apply_orientation_bytes(old_w, old_h, bytes, orientation, 4),
        PixelBuffer::Rgba16F(halfs) => {
            orientation::apply_orientation_u16(old_w, old_h, halfs, orientation, 4)
        }
    };
    if (new_w, new_h) != (old_w, old_h) {
        log::debug!(
            "Applied rotation: orientation {orientation} ({old_w}x{old_h} -> {new_w}x{new_h})"
        );
    }
    img.width = new_w;
    img.height = new_h;
    img
}

/// Read a file fully, returning its bytes. Runs on a detached `std::thread`
/// so a slow or wedged `read()` (network drive, SMB share timing out) can
/// never block the caller: when `cancelled` flips, we drop the receiver and
/// return `Err("cancelled")` immediately. The reader thread finishes at its
/// own pace and silently discards its result.
///
/// 64 KB chunks + flag check between chunks — same as before, but now we
/// also abandon the syscall entirely on cancellation. That's the critical
/// difference: `std::fs::File::read` has no timeout, so on a wedged share
/// the old in-thread check was useless until the kernel unblocked.
fn read_file_cancellable(path: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>, String>>(1);
    let path_for_thread = path.to_path_buf();
    let cancelled_for_thread = Arc::new(AtomicBool::new(false));
    // Clone the flag the caller owns into an Arc the thread can poll between
    // chunks. The caller's own reference stays — we check it too after each
    // `recv_timeout` tick.
    let thread_cancelled = Arc::clone(&cancelled_for_thread);

    std::thread::Builder::new()
        .name("prvw-io".into())
        .spawn(move || {
            let result = (|| {
                let mut file = std::fs::File::open(&path_for_thread)
                    .map_err(|e| format!("{}: {e}", crate::paths::for_display(&path_for_thread)))?;
                let size = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
                let mut buf = Vec::with_capacity(size);
                let mut chunk = [0u8; 65536];
                loop {
                    if thread_cancelled.load(Ordering::Relaxed) {
                        return Err::<Vec<u8>, String>("cancelled".into());
                    }
                    let n = file.read(&mut chunk).map_err(|e| {
                        format!("{}: {e}", crate::paths::for_display(&path_for_thread))
                    })?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Ok(buf)
            })();
            // Send may fail if the caller abandoned us — silently drop.
            let _ = tx.send(result);
        })
        .map_err(|e| format!("spawn io thread: {e}"))?;

    // Poll the channel with short timeouts so a caller cancellation is
    // reflected within ~10 ms. The reader thread still reads its own flag,
    // but that's belt-and-suspenders — the abandon-on-cancel is what
    // protects against `std::fs::File::read` blocking on a bad mount.
    use std::time::Duration;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            cancelled_for_thread.store(true, Ordering::Relaxed);
            return Err("cancelled".into());
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("io thread exited without a result".into());
            }
        }
    }
}

/// Run a decode closure on a detached `std::thread`, abandoning it if
/// `cancelled` flips before it finishes. Mirrors [`read_file_cancellable`]:
/// the caller polls a `sync_channel` with 10 ms timeouts, so a navigation
/// that cancels mid-decode frees the serial preload worker within ~10 ms
/// instead of waiting out the whole decode.
///
/// Used for the JPEG and generic (`image` crate) backends, whose decode is a
/// single opaque library call we can't checkpoint internally — unlike the RAW
/// path, which checks `cancelled` between its own pipeline stages and so stops
/// early without wasting CPU. The trade-off here: an abandoned decode burns
/// CPU to completion (bounded, and only on cancellation of a large in-flight
/// decode), in exchange for the worker never blocking on a huge image the user
/// has already navigated past.
///
/// **Salvage.** When the caller has abandoned us but the decode still finishes
/// successfully, the result isn't automatically wasted: if a `salvage` sink was
/// provided, the finished image is handed to it (otherwise it's dropped). The
/// preloader uses this to recover a completed decode into the LRU cache *if*
/// the image is still inside the hot navigation window — turning would-be waste
/// into a speculative prefetch. The sink runs on the detached decode thread, so
/// it must be `Send`; the main thread makes the in-window decision.
///
/// Like the preloader's own worker, the decode runs on a plain OS thread, not
/// a rayon worker, so any internal `par_iter` falls back to the global pool
/// (every core) rather than collapsing onto a single-thread pool.
///
/// When `cancelled` is `None` (callers that never cancel), the closure runs
/// inline with no thread or channel overhead and `salvage` is unused.
fn run_decode_cancellable<F>(
    cancelled: Option<&AtomicBool>,
    salvage: Option<SalvageSink>,
    decode: F,
) -> Result<DecodedImage, String>
where
    F: FnOnce() -> Result<DecodedImage, String> + Send + 'static,
{
    use std::sync::mpsc;
    use std::time::Duration;

    let Some(cancelled) = cancelled else {
        return decode();
    };

    let (tx, rx) = mpsc::sync_channel::<Result<DecodedImage, String>>(1);
    std::thread::Builder::new()
        .name("prvw-decode".into())
        .spawn(move || {
            let result = decode();
            // `send` hands the result to a still-waiting caller. If it fails,
            // the caller already gave up (cancelled) and dropped the receiver
            // — `SendError` hands us our value back. Salvage a successful
            // decode rather than waste it; let failures and errors go.
            if let Err(mpsc::SendError(result)) = tx.send(result)
                && let (Ok(image), Some(sink)) = (result, salvage)
            {
                sink(image);
            }
        })
        .map_err(|e| format!("spawn decode thread: {e}"))?;

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("decode thread exited without a result".into());
            }
        }
    }
}

/// Shared success/failure logging for both entry points.
fn log_result(
    result: &Result<DecodedImage, String>,
    ext: &str,
    backend: Backend,
    path: &Path,
    start: Instant,
) {
    match result {
        Ok(image) => {
            let duration = start.elapsed();
            let decoded_size = format_decoded_size(image.pixels.byte_len());
            let format_name = match backend {
                Backend::Jpeg => "JPEG via zune-jpeg".to_string(),
                Backend::Raw => format!("{} via rawler", ext.to_uppercase()),
                Backend::Generic => ext.to_uppercase(),
            };
            let hdr_label = if image.pixels.is_hdr() { " [HDR]" } else { "" };
            log::info!(
                "Decoded {format_name}{hdr_label}: {}x{} ({decoded_size}) in {}ms",
                image.width,
                image.height,
                duration.as_millis()
            );
        }
        Err(msg) if msg == "cancelled" => {
            log::debug!("Cancelled loading {}", path.display());
        }
        Err(msg) => {
            log::warn!("Decode failed for {}: {msg}", path.display());
        }
    }
}

/// Format a byte count as a compact human-readable string (for example, "47.2 MB").
fn format_decoded_size(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color;
    use std::time::Duration;

    /// `run_decode_cancellable` must free the caller within ~10 ms of the
    /// cancel flag flipping, even though the decode closure runs much longer.
    /// This is the JPEG/generic equivalent of the RAW path's inter-stage
    /// cancellation: the serial preload worker can't afford to block on a
    /// large in-flight decode when the user has already navigated away.
    #[test]
    fn decode_cancellable_returns_promptly_on_cancel() {
        use std::sync::Arc;
        use std::time::Instant;

        let cancelled = Arc::new(AtomicBool::new(false));

        // A decode that takes far longer than our cancel deadline.
        let slow = || {
            std::thread::sleep(Duration::from_secs(1));
            Ok(DecodedImage::from_rgba8(1, 1, vec![0, 0, 0, 255]))
        };

        // Flip the flag shortly after we start waiting on the decode.
        let flipper = {
            let cancelled = Arc::clone(&cancelled);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            })
        };

        let start = Instant::now();
        let result = run_decode_cancellable(Some(&cancelled), None, slow);
        let elapsed = start.elapsed();
        flipper.join().unwrap();

        assert_eq!(
            result.err().as_deref(),
            Some("cancelled"),
            "a cancelled decode must report \"cancelled\""
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "caller should be freed promptly after cancel, took {elapsed:?}"
        );
    }

    /// A decode that finishes *after* the caller abandoned it must be handed to
    /// the salvage sink rather than discarded, so the preloader can recover it
    /// into the cache window instead of wasting the work.
    #[test]
    fn decode_cancellable_salvages_abandoned_result() {
        use std::sync::Arc;
        use std::sync::mpsc;

        let cancelled = Arc::new(AtomicBool::new(false));
        let (salvage_tx, salvage_rx) = mpsc::channel::<DecodedImage>();
        let sink: SalvageSink = Box::new(move |img| {
            let _ = salvage_tx.send(img);
        });

        // Decode finishes well after the cancel deadline, so the caller will
        // have already returned "cancelled" and dropped its receiver.
        let slow = || {
            std::thread::sleep(Duration::from_millis(120));
            Ok(DecodedImage::from_rgba8(7, 3, vec![9u8; 7 * 3 * 4]))
        };

        let flipper = {
            let cancelled = Arc::clone(&cancelled);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                cancelled.store(true, Ordering::Relaxed);
            })
        };

        let result = run_decode_cancellable(Some(&cancelled), Some(sink), slow);
        flipper.join().unwrap();
        assert_eq!(
            result.err().as_deref(),
            Some("cancelled"),
            "the caller still sees \"cancelled\" — salvage is out-of-band"
        );

        // The abandoned decode should arrive on the salvage channel once it
        // completes (give it generous slack over the 120 ms decode).
        let salvaged = salvage_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("abandoned decode should be salvaged, not dropped");
        assert_eq!((salvaged.width, salvaged.height), (7, 3));
    }

    /// When the decode is *not* abandoned, the sink must never fire — the image
    /// goes back to the caller as the normal result.
    #[test]
    fn decode_cancellable_does_not_salvage_on_success() {
        use std::sync::mpsc;

        let cancelled = AtomicBool::new(false);
        let (salvage_tx, salvage_rx) = mpsc::channel::<DecodedImage>();
        let sink: SalvageSink = Box::new(move |img| {
            let _ = salvage_tx.send(img);
        });

        let result = run_decode_cancellable(Some(&cancelled), Some(sink), || {
            Ok(DecodedImage::from_rgba8(1, 1, vec![0, 0, 0, 255]))
        });
        assert!(result.is_ok(), "uncancelled decode returns its image");
        assert!(
            salvage_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "sink must not fire when the decode wasn't abandoned"
        );
    }

    /// The happy path: with no cancellation, the decoded image comes back
    /// intact through the abandonable-thread plumbing.
    #[test]
    fn decode_cancellable_returns_image_when_not_cancelled() {
        let cancelled = AtomicBool::new(false);
        let result = run_decode_cancellable(Some(&cancelled), None, || {
            Ok(DecodedImage::from_rgba8(
                2,
                1,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
            ))
        });
        let img = result.expect("decode should succeed");
        assert_eq!((img.width, img.height), (2, 1));
    }

    /// Phase 6.1 smoke perf: time the full `load_image` path with and
    /// without chroma denoise. Run manually:
    /// `RUST_LOG=info cargo test --release arw_chroma_denoise_perf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn arw_chroma_denoise_perf() {
        use std::time::Instant;

        let target = color::srgb_icc_bytes().to_vec();

        for sample in &[
            "/tmp/raw/sample1.arw",
            "/tmp/raw/sample2.dng",
            "/tmp/raw/sample3.arw",
        ] {
            let path = Path::new(sample);
            if !path.exists() {
                println!("skipping {sample} (not present)");
                continue;
            }

            let flags_off = RawPipelineFlags {
                chroma_denoise: false,
                ..RawPipelineFlags::default()
            };
            let flags_on = RawPipelineFlags::default();

            let noop = AtomicBool::new(false);
            // One warm-up pass each.
            let _ = load_image(path, &noop, &target, false, flags_off, 1.0, None);
            let _ = load_image(path, &noop, &target, false, flags_on, 1.0, None);

            let iters = 3;
            let mut off_ms: u128 = 0;
            for _ in 0..iters {
                let t = Instant::now();
                let _ = load_image(path, &noop, &target, false, flags_off, 1.0, None).unwrap();
                off_ms += t.elapsed().as_millis();
            }
            let mut on_ms: u128 = 0;
            for _ in 0..iters {
                let t = Instant::now();
                let _ = load_image(path, &noop, &target, false, flags_on, 1.0, None).unwrap();
                on_ms += t.elapsed().as_millis();
            }
            println!(
                "{sample} decode avg: off {} ms, on {} ms, delta {} ms",
                off_ms / iters,
                on_ms / iters,
                (on_ms as i128 - off_ms as i128) / iters as i128
            );
        }
    }

    /// End-to-end: `load_image` on the ARW fixture. Verifies dimensions after
    /// orientation, which for sample1 is no-op (orientation 1). `#[ignore]` because
    /// the fixture lives outside the repo. Run with
    /// `cargo test decoding::tests::arw_end_to_end -- --ignored`.
    #[test]
    #[ignore]
    fn arw_end_to_end() {
        let path = Path::new("/tmp/raw/sample1.arw");
        let img = load_image(
            path,
            &AtomicBool::new(false),
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0, // SDR headroom — keep the fixture path RGBA8 for golden diffs
            None,
        )
        .expect("decode failed");
        assert_eq!((img.width, img.height), (5456, 3632));
    }

    /// End-to-end: `load_image` on the DNG fixture. sample2 comes out of rawler
    /// as 3990x3000 but carries EXIF orientation 6 or 8, which swaps dims to
    /// 3000x3990.
    #[test]
    #[ignore]
    fn dng_end_to_end() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let img = load_image(
            path,
            &AtomicBool::new(false),
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0, // SDR headroom — keep the fixture path RGBA8 for golden diffs
            None,
        )
        .expect("decode failed");
        assert_eq!((img.width, img.height), (3000, 3990));
    }

    /// Golden regression test: decode the synthetic Bayer DNG fixture via the
    /// full `load_image` path and compare against a checked-in golden PNG. The
    /// threshold is deliberately tight (mean < 0.5, max < 3.0 in CIE76 Delta-E)
    /// so any pipeline drift caught by Phase 2+ changes will trip this test.
    ///
    /// To regenerate after an intentional output change:
    ///   PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden
    ///
    /// The fixture is a 128x128 uncompressed Bayer RGGB DNG built from a
    /// gradient, checked in under `tests/fixtures/raw/synthetic-bayer-128.dng`
    /// (see `tests/fixtures/raw/licenses.md`).
    #[test]
    fn synthetic_dng_matches_golden() {
        use crate::color::delta_e::delta_e_stats;

        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/raw");
        let raw_path = fixture_dir.join("synthetic-bayer-128.dng");
        let golden_path = fixture_dir.join("synthetic-bayer-128.golden.png");

        let img = load_image(
            &raw_path,
            &AtomicBool::new(false),
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0,
            None,
        )
        .expect("synthetic DNG should decode");
        assert_eq!(
            (img.width, img.height),
            (128, 128),
            "synthetic DNG dimensions drifted"
        );

        let rgba_bytes: &[u8] = match &img.pixels {
            super::PixelBuffer::Rgba8(v) => v.as_slice(),
            super::PixelBuffer::Rgba16F(_) => {
                panic!(
                    "synthetic DNG shouldn't be HDR: hdr_output is off-by-default via default_flags path when no EDR display is available; this fixture test runs SDR-only"
                )
            }
        };

        if std::env::var("PRVW_UPDATE_GOLDENS").ok().as_deref() == Some("1") {
            // RGBA8 -> RGB8 for PNG.
            let mut rgb: Vec<u8> = Vec::with_capacity((img.width * img.height * 3) as usize);
            for chunk in rgba_bytes.chunks_exact(4) {
                rgb.extend_from_slice(&chunk[..3]);
            }
            let buf = image::ImageBuffer::<image::Rgb<u8>, _>::from_raw(img.width, img.height, rgb)
                .expect("RGB buffer size mismatch");
            buf.save(&golden_path).expect("couldn't write golden");
            println!("Updated golden: {}", golden_path.display());
            return;
        }

        let golden = image::open(&golden_path)
            .unwrap_or_else(|e| {
                panic!(
                    "couldn't read golden PNG at {}: {e}. \
                     Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` to create it.",
                    golden_path.display()
                )
            })
            .to_rgb8();
        assert_eq!(
            (golden.width(), golden.height()),
            (img.width, img.height),
            "golden PNG dimensions don't match decoded output"
        );

        // Promote both to RGBA8 so `delta_e_stats` can diff them.
        let actual_rgba = rgba_bytes.to_vec();
        let mut golden_rgba: Vec<u8> =
            Vec::with_capacity((golden.width() * golden.height() * 4) as usize);
        for chunk in golden.as_raw().chunks_exact(3) {
            golden_rgba.extend_from_slice(chunk);
            golden_rgba.push(255);
        }

        let stats = delta_e_stats(&golden_rgba, &actual_rgba);
        // Tolerances: mean < 0.5 catches any gross pipeline drift; max < 3.0
        // tolerates a handful of border pixels that may round differently
        // across macOS versions. Tighten as needed if Phase 2+ introduces
        // deterministic pipelines we want to lock down harder.
        assert!(
            stats.mean < 0.5,
            "mean Delta-E {} exceeds 0.5 (max {}, p95 {}). \
             Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` if this change was intentional.",
            stats.mean,
            stats.max,
            stats.p95
        );
        assert!(
            stats.max < 3.0,
            "max Delta-E {} exceeds 3.0 (mean {}, p95 {}). \
             Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` if this change was intentional.",
            stats.max,
            stats.mean,
            stats.p95
        );
    }
}
