//! Glue between `App`, the thumbnail scheduler, and the QuickLook bridge.
//!
//! Lives here (not in `crate::thumbnails`) because it touches `App` fields —
//! the thumbnails module is platform code and shouldn't know about App.

#![cfg(target_os = "macos")]

use super::App;
use super::shared_state::ThumbnailEvent;
use objc2_core_foundation::CGSize;

/// Cap of the event ring buffer. 64 is plenty for humans eyeballing a
/// short navigation session.
const EVENT_RING_CAP: usize = 64;

impl App {
    /// Append a thumbnail-lifecycle event to the ring buffer. Also emits
    /// an info-level log line so `RUST_LOG=info` captures the same
    /// timeline with terminal timestamps.
    pub(crate) fn record_thumb_event(&mut self, kind: &'static str, detail: impl Into<String>) {
        let detail = detail.into();
        let ts_ms = self.app_start.elapsed().as_millis() as u64;
        log::info!("thumb-event [{ts_ms}ms] {kind}: {detail}");
        let event = ThumbnailEvent {
            ts_ms,
            kind,
            detail,
        };
        if self.thumbnail_events.len() == EVENT_RING_CAP {
            self.thumbnail_events.pop_front();
        }
        self.thumbnail_events.push_back(event);
    }
}

/// 512 logical pixels is QuickLook's gallery cache bucket, so folders the
/// user has browsed in Finder's gallery view hit the cache instantly. Going
/// above 1024 effective pixels falls off the cache entirely (quicklookd
/// renders from source each time), so 512 × Retina is the sweet spot.
const THUMB_SIZE: CGSize = CGSize {
    width: 512.0,
    height: 512.0,
};

impl App {
    /// Drain the thumbnail scheduler up to its parallelism cap, submitting
    /// QL requests for each index it hands back. Called after every event
    /// that could free a slot: completion arrival, pause/resume, folder
    /// change, navigation.
    pub(crate) fn pump_thumbnail_requests(&mut self) {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor())
            .unwrap_or(2.0);
        let proxy = self.event_loop_proxy.clone();
        let generation = self.thumbnails.generation();
        while let Some((index, request_id)) = self.thumbnails.scheduler.poll_next() {
            let Some(path) = self.thumbnails.path(index).map(|p| p.to_path_buf()) else {
                self.thumbnails.scheduler.mark_failed(index);
                continue;
            };
            self.thumbnails
                .requests
                .submit(crate::thumbnails::quicklook::SubmitRequest {
                    request_id,
                    index,
                    folder_generation: generation,
                    path: &path,
                    size: THUMB_SIZE,
                    scale,
                    proxy: proxy.clone(),
                });
        }
    }

    /// Called when navigation lands on a non-cached index. The primary
    /// decode path takes priority; pause the thumb scheduler until the
    /// full decode completes.
    pub(crate) fn on_primary_decode_started(&mut self) {
        self.thumbnails.pause();
    }

    /// Called when a primary decode completes (success or failure).
    pub(crate) fn on_primary_decode_settled(&mut self) {
        self.thumbnails.resume();
        self.pump_thumbnail_requests();
    }

    /// Called after `set_current` changes (navigate_by). Reseeds the
    /// centered queue so the nearest-to-new-current indices get generated
    /// first.
    pub(crate) fn on_thumbnail_current_changed(&mut self, current: usize) {
        self.thumbnails.set_current(current);
        self.pump_thumbnail_requests();
    }

    /// Upload the cached thumbnail for `index` into the image texture as a
    /// placeholder, then run the same display pipeline a real image would
    /// (auto-fit, initial zoom, EDR state). Returns `true` if a thumb was
    /// uploaded. Linear sampling on upscale gives a soft, blurred look
    /// that signals "not the final image."
    ///
    /// The thumb stays in the image texture until the full decode arrives
    /// (via `PreloadResponse::Ready`), which calls `set_image` again with
    /// the authoritative pixels.
    pub(crate) fn display_thumbnail_placeholder(&mut self, index: usize) -> bool {
        if self.renderer.is_none() {
            return false;
        }
        let (tw, th, rgba) = {
            let Some(thumb) = self.thumbnails.get(index) else {
                return false;
            };
            (thumb.width, thumb.height, thumb.rgba.clone())
        };
        // Source dims are read lazily here — only for the index we're
        // about to display, never for all cached thumbs. On a network
        // share each ImageIO read costs ~200 ms, so this is the
        // difference between a snappy display and a 10 s stall.
        // Falls back to the thumb's own dims if ImageIO fails (rare;
        // means the file is unreadable, in which case the full decode
        // will fail too and the user will see an error title).
        let (sw, sh) = self
            .thumbnails
            .source_dimensions(index)
            .map(|d| (d.width, d.height))
            .unwrap_or((tw, th));
        // Same display pipeline the cached-image path uses. Source dims
        // drive the zoom math; the thumb is RGBA8 so `is_hdr=false` —
        // the surface flips back to HDR when the primary RAW arrives
        // and `display_from_cache` re-prepares.
        self.prepare_display(sw, sh, false);
        let image = crate::decoding::DecodedImage::from_rgba8(tw, th, rgba);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_image(&image);
        }
        self.finalize_display();
        self.placeholder_active = true;
        if let Some(requested_at) = self.request_times.get(&index) {
            let elapsed = requested_at.elapsed().as_millis();
            log::info!("Thumbnail for #{index} displayed after {elapsed}ms");
        }
        self.record_thumb_event(
            "placeholder-shown",
            format!("index={index} thumb={tw}x{th} source={sw}x{sh}"),
        );
        true
    }

    /// Auto-fit the window based on the source image dimensions of `index`,
    /// if available via ImageIO (metadata-only, no decode). Called on cache
    /// miss so the window reaches its final size before the thumb is
    /// painted. The full decode will later call `resize_to_fit_image`
    /// again with the authoritative dimensions — the numbers match, so
    /// there's no visible second resize.
    pub(crate) fn apply_thumbnail_auto_fit(&mut self, index: usize) {
        if !self.zoom.auto_fit {
            return;
        }
        let Some(dims) = self.thumbnails.source_dimensions(index) else {
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
