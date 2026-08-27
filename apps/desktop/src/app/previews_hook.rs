//! Glue between `App`, the preview scheduler, and the QuickLook bridge.
//!
//! Lives here (not in `crate::previews`) because it touches `App` fields —
//! the previews module is platform code and shouldn't know about App.
//!
//! Most of this is portable: the scheduler, the dimension cache, and the
//! window auto-fit that reads it all run everywhere. Only the submission
//! itself is fenced, to the platforms that have a preview generator
//! (`previews::RequestTable`). Where there is none the preview cache simply
//! stays empty, so `display_preview_placeholder` reports "nothing to show"
//! and every caller falls through to `apply_preview_auto_fit`.

use super::App;
use super::shared_state::PreviewEvent;

/// Cap of the event ring buffer. 64 is plenty for humans eyeballing a
/// short navigation session.
const EVENT_RING_CAP: usize = 64;

impl App {
    /// Append a preview-lifecycle event to the ring buffer. Also emits
    /// an info-level log line so `RUST_LOG=info` captures the same
    /// timeline with terminal timestamps.
    pub(crate) fn record_preview_event(&mut self, kind: &'static str, detail: impl Into<String>) {
        let detail = detail.into();
        let ts_ms = self.app_start.elapsed().as_millis() as u64;
        log::info!("preview-event [{ts_ms}ms] {kind}: {detail}");
        let event = PreviewEvent {
            ts_ms,
            kind,
            detail,
        };
        if self.preview_events.len() == EVENT_RING_CAP {
            self.preview_events.pop_front();
        }
        self.preview_events.push_back(event);
    }
}

/// Longest edge of a preview, in points; the display scale turns it into pixels.
///
/// 512 is QuickLook's gallery cache bucket, so folders the user has browsed in Finder's gallery
/// view hit the cache instantly. Going above 1024 effective pixels falls off that cache entirely
/// (quicklookd renders from source each time), so 512 × Retina is the sweet spot. Windows has no
/// bucketing to match, and the number is a good one there too: enough pixels to fill a 4K window
/// softly, few enough that the byte budget holds a useful neighbourhood of them.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const PREVIEW_SIZE_PT: f64 = 512.0;

impl App {
    /// Drain the preview scheduler up to its parallelism cap, submitting a request for each
    /// index it hands back. Called after every event that could free a slot: completion arrival,
    /// pause/resume, folder change, navigation. Only where there's a generator to submit to
    /// (`previews::RequestTable`).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn pump_preview_requests(&mut self) {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor())
            .unwrap_or(2.0);
        let proxy = self.event_loop_proxy.clone();
        let generation = self.previews.generation();
        while let Some((index, request_id)) = self.previews.scheduler.poll_next() {
            let Some(path) = self.previews.path(index).map(|p| p.to_path_buf()) else {
                self.previews.scheduler.mark_failed(index);
                continue;
            };
            self.previews
                .requests
                .submit(crate::previews::request::SubmitRequest {
                    request_id,
                    index,
                    folder_generation: generation,
                    path: &path,
                    size_pt: PREVIEW_SIZE_PT,
                    scale,
                    proxy: proxy.clone(),
                });
        }
    }

    /// Called when navigation lands on a non-cached index. The primary
    /// decode path takes priority; pause the preview scheduler until the
    /// full decode completes.
    pub(crate) fn on_primary_decode_started(&mut self) {
        self.previews.pause();
    }

    /// Called when a primary decode completes (success or failure).
    pub(crate) fn on_primary_decode_settled(&mut self) {
        self.previews.resume();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.pump_preview_requests();
    }

    /// Called after `set_current` changes (navigate_by). Reseeds the
    /// centered queue so the nearest-to-new-current indices get generated
    /// first.
    pub(crate) fn on_preview_current_changed(&mut self, current: usize) {
        self.previews.set_current(current);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.pump_preview_requests();
    }

    /// Upload the cached preview for `index` into the image texture as a
    /// placeholder, then run the same display pipeline a real image would
    /// (auto-fit, initial zoom, EDR state). Returns `true` if a preview was
    /// uploaded. Linear sampling on upscale gives a soft, blurred look
    /// that signals "not the final image."
    ///
    /// The preview stays in the image texture until the full decode arrives
    /// (via `PreloadResponse::Ready`), which calls `set_image` again with
    /// the authoritative pixels.
    pub(crate) fn display_preview_placeholder(&mut self, index: usize) -> bool {
        if self.renderer.is_none() {
            return false;
        }
        let (tw, th, rgba) = {
            let Some(preview) = self.previews.get(index) else {
                return false;
            };
            (preview.width, preview.height, preview.rgba.clone())
        };
        // Source dims are read lazily here — only for the index we're
        // about to display, never for all cached previews. On a network
        // share each header read costs ~200 ms, so this is the
        // difference between a snappy display and a 10 s stall.
        // Falls back to the preview's own dims if the read fails (rare;
        // means the file is unreadable, in which case the full decode
        // will fail too and the user will see an error title).
        let (sw, sh) = self
            .previews
            .source_dimensions(index)
            .map(|d| (d.width, d.height))
            .unwrap_or((tw, th));
        // Same display pipeline the cached-image path uses. Source dims
        // drive the zoom math; the preview is RGBA8 so `is_hdr=false` —
        // the surface flips back to HDR when the primary RAW arrives
        // and `display_from_cache` re-prepares.
        self.prepare_display(sw, sh, false);
        let image = crate::decoding::DecodedImage::from_rgba8(tw, th, rgba);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_image(&image);
        }
        // Drop the previous image's histogram so it doesn't render on top
        // of the new placeholder. The full decode arrives shortly and
        // recomputes the histogram via `display_from_cache`. Computing one
        // for the QL preview is wasted work.
        self.histogram.data = None;
        self.histogram.hover_bin = None;
        self.finalize_display();
        self.placeholder_active = true;
        if let Some(requested_at) = self.request_times.get(&index) {
            let elapsed = requested_at.elapsed().as_millis();
            log::info!("Preview for #{index} displayed after {elapsed}ms");
        }
        self.record_preview_event(
            "placeholder-shown",
            format!("index={index} preview={tw}x{th} source={sw}x{sh}"),
        );
        true
    }

    /// Auto-fit the window based on the source image dimensions of `index`,
    /// read from the file's header without decoding it
    /// (`previews::metadata`). Called on cache miss so the window reaches its
    /// final size before the preview is painted. The full decode will later
    /// call `resize_to_fit_image` again with the authoritative dimensions —
    /// the numbers match, so there's no visible second resize. No-op for a
    /// format no tier can size, which leaves the window where it was until
    /// the decode lands: the behaviour every platform had before the tier.
    pub(crate) fn apply_preview_auto_fit(&mut self, index: usize) {
        if !self.zoom.auto_fit {
            return;
        }
        let Some(dims) = self.previews.source_dimensions(index) else {
            return;
        };
        self.navigation.current_image_size = Some((dims.width, dims.height));
        let offset = self.content_offset_y();
        if let Some(win) = &self.window
            && let Some(size) =
                crate::window::resize_to_fit_image(win, dims.width, dims.height, offset)
        {
            let (pw, ph) = crate::pixels::from_physical_size(size);
            if let Some(renderer) = &mut self.renderer {
                renderer.resize(pw, ph);
                self.zoom.view.update_dimensions(
                    dims.width,
                    dims.height,
                    renderer.logical_width(),
                    renderer.logical_height(),
                );
            }
        }
    }
}
