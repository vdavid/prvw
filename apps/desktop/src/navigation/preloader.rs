use crate::commands::AppCommand;
use crate::decoding::{self, DecodedImage, RawPipelineFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

/// SDR cache budget (Phase 4). 512 MB holds ~6 × 20 MP RAW decodes as
/// RGBA8. Every JPEG/PNG/WebP cached image fits the same budget.
const SDR_MEMORY_BUDGET: usize = 512 * 1024 * 1024;

/// HDR cache budget (Phase 5). Doubled from the SDR path because RAW
/// RGBA16F is 8 bytes per pixel instead of 4. With this bumped budget we
/// keep the same ~6 preload count for 20 MP RAWs that SDR had. User
/// decision: trade RAM for preload count, because the preload experience
/// is the whole value proposition of the preloader (see
/// `docs/notes/raw-support-phase5.md` for the trade-off note).
const HDR_MEMORY_BUDGET: usize = 1024 * 1024 * 1024;

const PRELOAD_AHEAD: usize = 2;

// The preloader runs on a single dedicated `std::thread`, not a rayon
// pool. Reason: each RAW decode internally calls `rayon::par_iter` through
// rawler and our own stages, and rayon's `par_iter` inherits the caller's
// pool. If the caller runs on a custom rayon pool with N threads, those
// parallel stages get N threads too — not the global pool's all-cores.
// A plain OS thread isn't a rayon worker, so `par_iter` inside it falls
// back to the global pool (every logical core), matching the main-thread
// sync decode path.
//
// Observed on an M3 Max, 20 MP ARW:
//   main-thread sync decode:   demosaic 61 ms, chroma_nr 64 ms, sharpen 19 ms
//   single-thread rayon pool:  demosaic 403 ms, chroma_nr 510 ms, sharpen 194 ms
//   dedicated std::thread:     same as main-thread sync (~500 ms total)
//
// Serial execution is fine: we only want one decode running at a time so
// the priority-zero task gets full CPU and finishes first. Queueing more
// tasks just makes the priority-zero task share cores for no benefit.

/// Messages sent from the preloader back to the main thread.
pub enum PreloadResponse {
    /// An image was decoded and is ready. `index` is the navigation slot
    /// at submission time (used to match `pending_current` for cache-miss
    /// arrivals); `path` is the cache key the caller inserts under.
    Ready {
        index: usize,
        path: PathBuf,
        image: DecodedImage,
        decode_duration: Duration,
        file_name: String,
    },
    /// An image failed to decode.
    Failed {
        index: usize,
        path: PathBuf,
        reason: String,
    },
    /// The task was cancelled before completing.
    Cancelled {
        #[allow(dead_code)]
        index: usize,
        path: PathBuf,
    },
    /// A JPEG / generic decode that was cancelled mid-flight but finished
    /// anyway (see `decoding::run_decode_cancellable`). The image is offered
    /// back so the main thread can recover it into the cache *if* it's still
    /// inside the hot navigation window, instead of wasting the work. The
    /// main thread drops it otherwise — out-of-window images don't get to
    /// squat in RAM.
    Salvaged {
        index: usize,
        path: PathBuf,
        image: DecodedImage,
        decode_duration: Duration,
        file_name: String,
    },
    /// A RAW file's embedded JPEG preview, extracted before the (slow) full
    /// develop. Sent only for the priority target so the main thread can show
    /// it as a soft placeholder instantly, then swap in the full develop when
    /// `Ready` arrives. Not cached — purely a transient placeholder.
    Preview {
        index: usize,
        path: PathBuf,
        image: DecodedImage,
    },
}

/// LRU cache for decoded images with a memory budget. Keyed by absolute
/// file path so a future re-sort or directory rescan doesn't invalidate
/// the cache.
pub struct ImageCache {
    entries: HashMap<PathBuf, CacheEntry>,
    /// Access order: most recently used at the end.
    access_order: Vec<PathBuf>,
    memory_used: usize,
    memory_budget: usize,
}

pub struct CacheEntry {
    pub image: DecodedImage,
    pub decode_duration: Duration,
    pub file_name: String,
    memory_cost: usize,
}

/// Snapshot of cache state for diagnostics.
pub struct CacheDiagnostics {
    pub total_memory: usize,
    pub memory_budget: usize,
    pub entries: Vec<CacheEntryDiagnostic>,
}

/// Diagnostics for a single cached image.
pub struct CacheEntryDiagnostic {
    pub path: PathBuf,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub memory_bytes: usize,
    pub decode_duration: Duration,
}

/// An image that just got removed from the cache. Returned from
/// `ImageCache::insert` / `retain_only` / `set_hdr_mode` so the caller can
/// log it with context (reason) that the cache doesn't have.
pub struct EvictedEntry {
    pub file_name: String,
    pub memory_cost: usize,
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
            memory_used: 0,
            memory_budget: SDR_MEMORY_BUDGET,
        }
    }

    /// Switch the cache budget between SDR (512 MB) and HDR (1 GB). Called
    /// when the RAW pipeline's `hdr_output` flag or the display's EDR
    /// headroom changes. Evicts LRU entries if the new budget is smaller
    /// than the currently-resident total.
    pub fn set_hdr_mode(&mut self, hdr: bool) {
        let new_budget = if hdr {
            HDR_MEMORY_BUDGET
        } else {
            SDR_MEMORY_BUDGET
        };
        if new_budget == self.memory_budget {
            return;
        }
        log::info!(
            "Cache budget: {} MB -> {} MB",
            self.memory_budget / (1024 * 1024),
            new_budget / (1024 * 1024)
        );
        self.memory_budget = new_budget;
        // Evict LRU entries if the new budget doesn't fit the resident set.
        while self.memory_used > self.memory_budget && !self.access_order.is_empty() {
            let evict = self.access_order[0].clone();
            self.remove(&evict);
        }
    }

    /// Get a cached image by path, updating its LRU position.
    pub fn get(&mut self, path: &Path) -> Option<&DecodedImage> {
        if self.entries.contains_key(path) {
            self.touch(path);
            Some(&self.entries[path].image)
        } else {
            None
        }
    }

    /// Like `get` but doesn't touch the LRU. Useful for read-only
    /// inspection (current EXIF metadata, debug snapshots) where bumping
    /// the access order on every render call would be wrong.
    pub fn peek(&self, path: &Path) -> Option<&DecodedImage> {
        self.entries.get(path).map(|e| &e.image)
    }

    /// Insert a decoded image into the cache, evicting LRU entries if over
    /// budget. Returns any entries the LRU logic had to drop so the caller
    /// can log them (the cache doesn't know the current image, which is
    /// what makes a log line readable).
    pub fn insert(
        &mut self,
        path: PathBuf,
        image: DecodedImage,
        decode_duration: Duration,
        file_name: String,
    ) -> Vec<EvictedEntry> {
        let cost = image_memory_cost(&image);

        // If this single image exceeds the budget, don't cache it
        if cost > self.memory_budget {
            log::warn!(
                "Image {} ({cost} bytes) exceeds cache budget, not caching",
                path.display()
            );
            return Vec::new();
        }

        // Remove existing entry if present
        if self.entries.contains_key(&path) {
            self.remove(&path);
        }

        let mut evicted = Vec::new();
        while self.memory_used + cost > self.memory_budget && !self.access_order.is_empty() {
            let evict_path = self.access_order[0].clone();
            if let Some(e) = self.take_evicted(&evict_path) {
                evicted.push(e);
            } else {
                // Stale entry in `access_order` — defensive break to avoid a loop.
                self.access_order.remove(0);
            }
        }

        self.access_order.push(path.clone());
        self.entries.insert(
            path,
            CacheEntry {
                image,
                decode_duration,
                file_name,
                memory_cost: cost,
            },
        );
        self.memory_used += cost;
        evicted
    }

    /// Return diagnostics snapshot of the cache.
    pub fn diagnostics(&self) -> CacheDiagnostics {
        let mut entries: Vec<CacheEntryDiagnostic> = self
            .entries
            .iter()
            .map(|(path, entry)| CacheEntryDiagnostic {
                path: path.clone(),
                file_name: entry.file_name.clone(),
                width: entry.image.width,
                height: entry.image.height,
                memory_bytes: entry.memory_cost,
                decode_duration: entry.decode_duration,
            })
            .collect();
        entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        CacheDiagnostics {
            total_memory: self.memory_used,
            memory_budget: self.memory_budget,
            entries,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    /// Remove entries outside the hot window around the current position.
    /// Called on every navigation so distant images release their RAM
    /// promptly instead of sitting until the LRU budget pushes them out.
    /// Returns the entries that were dropped so the caller can log them.
    pub fn retain_only(&mut self, keep: &[PathBuf]) -> Vec<EvictedEntry> {
        let to_remove: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|k| !keep.iter().any(|p| p == *k))
            .cloned()
            .collect();
        let mut evicted = Vec::with_capacity(to_remove.len());
        for path in to_remove {
            if let Some(e) = self.take_evicted(&path) {
                evicted.push(e);
            }
        }
        evicted
    }

    /// Remove `path` and return its metadata as an `EvictedEntry`.
    fn take_evicted(&mut self, path: &Path) -> Option<EvictedEntry> {
        let entry = self.entries.remove(path)?;
        self.memory_used = self.memory_used.saturating_sub(entry.memory_cost);
        self.access_order.retain(|p| p != path);
        Some(EvictedEntry {
            file_name: entry.file_name,
            memory_cost: entry.memory_cost,
        })
    }

    /// Remove all entries from the cache (for example, after a display profile change).
    pub fn clear(&mut self) {
        let count = self.entries.len();
        self.entries.clear();
        self.access_order.clear();
        self.memory_used = 0;
        if count > 0 {
            log::debug!("Cache cleared ({count} entries removed)");
        }
    }

    fn touch(&mut self, path: &Path) {
        self.access_order.retain(|p| p != path);
        self.access_order.push(path.to_path_buf());
    }

    fn remove(&mut self, path: &Path) {
        if let Some(entry) = self.entries.remove(path) {
            self.memory_used = self.memory_used.saturating_sub(entry.memory_cost);
            self.access_order.retain(|p| p != path);
        }
    }

    #[cfg(test)]
    fn memory_used(&self) -> usize {
        self.memory_used
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

fn image_memory_cost(image: &DecodedImage) -> usize {
    // Respect whichever `PixelBuffer` variant the decoder produced. RGBA8
    // is 4 bytes per pixel (every non-RAW format + SDR RAW); RGBA16F is 8
    // bytes per pixel (HDR RAW output).
    image.width as usize * image.height as usize * image.pixels.bytes_per_pixel()
}

/// Serial image preloader backed by a dedicated OS thread.
pub struct Preloader {
    /// FIFO queue of decode tasks. The worker thread pops and runs them
    /// one at a time. Tasks queued behind cancelled ones still "consume
    /// their turn" but exit in microseconds via the cancellation flag
    /// check at the start of `load_image`.
    task_tx: mpsc::Sender<Box<dyn FnOnce() + Send + 'static>>,
    response_tx: mpsc::Sender<PreloadResponse>,
    pub response_rx: mpsc::Receiver<PreloadResponse>,
    /// In-flight cancellation tokens keyed by file path. When
    /// `request_neighbor_preload` runs, paths still in the new task list
    /// keep their existing token (so a mid-decode task survives), and
    /// paths no longer in the list have their token flipped (cancelling
    /// that decode).
    in_flight: HashMap<PathBuf, Arc<AtomicBool>>,
    /// ICC profile bytes for the current display (target color space for decoding).
    display_icc: Arc<Vec<u8>>,
    /// Whether to use relative colorimetric rendering intent instead of perceptual.
    use_relative_colorimetric: bool,
    /// Per-stage RAW pipeline toggles. Defaults to `RawPipelineFlags::default()`
    /// (all true). Changed via the Settings → RAW panel; the main thread flushes
    /// the cache and re-requests the current image when this changes.
    raw_flags: RawPipelineFlags,
    /// EDR headroom of the active display (Phase 5). Passed through to
    /// every decode task so the RAW decoder picks between RGBA8 and
    /// RGBA16F output.
    edr_headroom: f32,
    /// Sent after each `PreloadResponse` to wake winit's event loop.
    /// Without this, `ControlFlow::Wait` sleeps until the next OS event
    /// (mouse, key) and a freshly-decoded image can sit unprocessed in
    /// the response channel for seconds while the user stares at the
    /// placeholder. The handler for `AppCommand::PreloaderProgress` is
    /// a no-op — the wake itself is the side effect, because winit
    /// always runs `about_to_wait` after any event, which is where we
    /// poll the response channel.
    event_proxy: EventLoopProxy<AppCommand>,
}

impl Preloader {
    pub fn start(
        display_icc: Vec<u8>,
        use_relative_colorimetric: bool,
        raw_flags: RawPipelineFlags,
        edr_headroom: f32,
        event_proxy: EventLoopProxy<AppCommand>,
    ) -> Self {
        let (task_tx, task_rx) = mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
        std::thread::Builder::new()
            .name("prvw-preload".into())
            .spawn(move || {
                while let Ok(task) = task_rx.recv() {
                    task();
                }
                log::debug!("Preloader worker exiting");
            })
            .expect("Failed to spawn preloader worker thread");

        log::info!("Preloader started (serial, dedicated OS thread)");

        let (response_tx, response_rx) = mpsc::channel();

        Self {
            task_tx,
            response_tx,
            response_rx,
            in_flight: HashMap::new(),
            display_icc: Arc::new(display_icc),
            use_relative_colorimetric,
            raw_flags,
            edr_headroom,
            event_proxy,
        }
    }

    /// Update the display's EDR headroom snapshot used by future decode
    /// tasks. The caller flushes the image cache and re-submits preload
    /// tasks so existing entries (possibly RGBA8-only) don't mix with
    /// fresh RGBA16F ones. Only the macOS `handle_display_changed` path
    /// calls this today; Linux builds would otherwise trip `dead_code`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn set_edr_headroom(&mut self, headroom: f32) {
        self.edr_headroom = headroom;
    }

    /// Update the target display ICC profile (called when the window moves to a different display).
    pub fn set_display_icc(&mut self, icc: Vec<u8>) {
        self.display_icc = Arc::new(icc);
    }

    pub fn set_use_relative_colorimetric(&mut self, value: bool) {
        self.use_relative_colorimetric = value;
    }

    /// Update the RAW pipeline flags. The caller is responsible for flushing the image
    /// cache and resubmitting preload tasks so new decodes run with the new flags.
    pub fn set_raw_flags(&mut self, flags: RawPipelineFlags) {
        self.raw_flags = flags;
    }

    /// Submit the navigation TARGET as the highest-priority decode.
    ///
    /// Cancels every other in-flight task (including alive neighbors
    /// from prior navigations), then queues this target if it's not
    /// already in flight. Use this on cache-miss navigation: the user
    /// is waiting for THIS image, so neighbors should get out of the
    /// way. Stale closures still get pulled off the channel by the
    /// worker, but they exit fast at `load_image`'s cancellation check.
    ///
    /// Submit neighbors via [`request_neighbor_preload`] AFTER the
    /// target arrives, so the FIFO channel stays small during rapid
    /// navigation.
    pub fn prioritize_target(&mut self, target_index: usize, path: PathBuf, total: usize) {
        let mut cancelled_count = 0usize;
        self.in_flight.retain(|p, token| {
            if p == &path {
                true
            } else {
                token.store(true, Ordering::Relaxed);
                cancelled_count += 1;
                false
            }
        });
        if cancelled_count > 0 {
            log::debug!(
                "Cancelled {cancelled_count} in-flight tasks to prioritize target {target_index}"
            );
        }
        if !self.in_flight.contains_key(&path) {
            // This is the user-visible target — request the quick preview.
            self.queue_task(target_index, path, target_index, total, true);
        }
    }

    /// Cancel every in-flight task. Used on a re-sort: the captured
    /// (slot index, path) tuples baked into queued closures still target
    /// the same paths, but the slot index now points at a different file
    /// in the new ordering, so `pending_current` matching in
    /// `poll_preloader` would mis-target. We re-issue under the new slot
    /// after the re-sort completes.
    pub fn cancel_all(&mut self) {
        let count = self.in_flight.len();
        for token in self.in_flight.values() {
            token.store(true, Ordering::Relaxed);
        }
        self.in_flight.clear();
        if count > 0 {
            log::debug!("Cancelled {count} in-flight tasks (re-sort)");
        }
    }

    /// Submit background neighbor preloads. Cancels in-flight tasks for
    /// paths NOT in the new list (they're no longer wanted), keeps
    /// the rest alive, queues fresh tasks for paths not yet in flight.
    /// Doesn't cancel-all like [`prioritize_target`] — neighbors are
    /// equal-priority and shouldn't fight each other.
    pub fn request_neighbor_preload(
        &mut self,
        tasks: Vec<(usize, PathBuf)>,
        current_index: usize,
        total: usize,
    ) {
        let requested: std::collections::HashSet<PathBuf> =
            tasks.iter().map(|(_, p)| p.clone()).collect();
        let mut cancelled_count = 0usize;
        self.in_flight.retain(|p, token| {
            if requested.contains(p) {
                true
            } else {
                token.store(true, Ordering::Relaxed);
                cancelled_count += 1;
                false
            }
        });
        if cancelled_count > 0 {
            log::debug!("Cancelled {cancelled_count} stale neighbor tasks");
        }
        let indices: Vec<usize> = tasks.iter().map(|(i, _)| *i).collect();
        log::debug!("Preloading neighbors: {indices:?}");
        for (index, path) in tasks.into_iter() {
            if self.in_flight.contains_key(&path) {
                continue;
            }
            // Neighbors are background warm-ups, never displayed yet — no preview.
            self.queue_task(index, path, current_index, total, false);
        }
    }

    /// Build the task closure and queue it on the worker channel. Both
    /// `prioritize_target` and `request_neighbor_preload` use this so
    /// the closure body lives in one place.
    fn queue_task(
        &mut self,
        index: usize,
        path: PathBuf,
        current_index: usize,
        total: usize,
        wants_preview: bool,
    ) {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.in_flight.insert(path.clone(), Arc::clone(&cancelled));

        let offset_label = crate::navigation::format_offset(index, current_index);
        let position_label = format!("{}/{}", index + 1, total);

        let tx = self.response_tx.clone();
        let display_icc = Arc::clone(&self.display_icc);
        let use_relative_colorimetric = self.use_relative_colorimetric;
        let raw_flags = self.raw_flags;
        let edr_headroom = self.edr_headroom;
        let event_proxy = self.event_proxy.clone();
        let task = move || {
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            log::debug!("Initiated loading {file_name} ({offset_label}, {position_label})");
            // Quick preview (priority target only): for a RAW, extract the
            // camera's embedded JPEG and show it as a soft placeholder before
            // the ~450 ms develop runs, so a cache-miss isn't a blank wait. Skip
            // if already cancelled (a newer nav superseded us). Only RAW needs
            // this — JPEG/generic decode in tens of ms.
            if wants_preview && !cancelled.load(Ordering::Relaxed) {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                if decoding::is_raw_extension(ext)
                    && let Some(preview) =
                        decoding::decode_raw_preview(&path, &display_icc, use_relative_colorimetric)
                    && !cancelled.load(Ordering::Relaxed)
                {
                    let _ = tx.send(PreloadResponse::Preview {
                        index,
                        path: path.clone(),
                        image: preview,
                    });
                    let _ = event_proxy.send_event(AppCommand::PreloaderProgress);
                }
            }
            // Test affordance: when `PRVW_THUMB_HOLD_MS` is set, delay
            // the decode by N ms so the thumbnail placeholder stays
            // visible long enough to inspect via MCP or take a
            // screenshot. Zero cost when unset — `std::env::var`
            // returns `Err`, the `ok()` is `None`, and `unwrap_or(0)`
            // short-circuits the sleep. The sleep polls `cancelled`
            // every 50 ms so a navigation that happens during the
            // hold cancels its task within ~50 ms instead of the
            // whole hold — otherwise queued-and-cancelled tasks
            // each burn the full `hold_ms` before the worker moves
            // on to the current target.
            let hold_ms: u64 = std::env::var("PRVW_THUMB_HOLD_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if hold_ms > 0 {
                let deadline = Instant::now() + Duration::from_millis(hold_ms);
                while Instant::now() < deadline {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(remaining.min(Duration::from_millis(50)));
                }
            }
            let start = Instant::now();
            // Salvage sink: if this decode is cancelled mid-flight but the
            // detached decode thread finishes anyway, hand the image back as
            // `Salvaged` so the main thread can recover it into the cache
            // window. Runs on that decode thread, so it captures its own
            // clones. Fires only on the abandoned-then-completed path; a
            // normal completion drops it unused. JPEG/generic only — RAW
            // ignores it (it self-cancels between stages and never abandons).
            let salvage_sink: decoding::SalvageSink = {
                let tx = tx.clone();
                let event_proxy = event_proxy.clone();
                let path = path.clone();
                let file_name = file_name.clone();
                let offset_label = offset_label.clone();
                let position_label = position_label.clone();
                Box::new(move |image| {
                    let duration = start.elapsed();
                    log::debug!(
                        "Salvaged {file_name} ({offset_label}, {position_label}) after cancellation in {}ms",
                        duration.as_millis()
                    );
                    let _ = tx.send(PreloadResponse::Salvaged {
                        index,
                        path,
                        image,
                        decode_duration: duration,
                        file_name,
                    });
                    // Same wake rationale as the other responses below.
                    let _ = event_proxy.send_event(AppCommand::PreloaderProgress);
                })
            };
            match decoding::load_image(
                &path,
                &cancelled,
                &display_icc,
                use_relative_colorimetric,
                raw_flags,
                edr_headroom,
                Some(salvage_sink),
            ) {
                Ok(image) => {
                    let duration = start.elapsed();
                    log::debug!(
                        "Fully loaded {file_name} ({offset_label}, {position_label}) in {}ms",
                        duration.as_millis()
                    );
                    let _ = tx.send(PreloadResponse::Ready {
                        index,
                        path,
                        image,
                        decode_duration: duration,
                        file_name,
                    });
                }
                Err(reason) if reason == "cancelled" => {
                    log::debug!("Cancelled loading {file_name} ({offset_label}, {position_label})");
                    let _ = tx.send(PreloadResponse::Cancelled { index, path });
                }
                Err(reason) => {
                    log::warn!(
                        "Failed to load {file_name} ({offset_label}, {position_label}): {reason}"
                    );
                    let _ = tx.send(PreloadResponse::Failed {
                        index,
                        path,
                        reason,
                    });
                }
            }
            // Wake the main event loop so `about_to_wait` runs and
            // `poll_preloader` drains the response we just sent. The
            // mpsc channel by itself doesn't wake winit out of
            // `ControlFlow::Wait`. The event handler is a no-op; the
            // wake itself is the side effect.
            let _ = event_proxy.send_event(AppCommand::PreloaderProgress);
        };

        // Channel is naturally FIFO — execution order matches
        // submission order, which is priority order.
        if self.task_tx.send(Box::new(task)).is_err() {
            log::warn!("Preloader worker is gone — dropping task for [{index}]");
        }
    }

    /// Clear the in-flight tracking for a completed path.
    pub fn mark_complete(&mut self, path: &Path) {
        self.in_flight.remove(path);
    }

    /// Shut down the preloader. Dropping the `task_tx` closes the channel,
    /// the worker thread's `recv()` returns `Err`, and it exits.
    pub fn shutdown(self) {
        drop(self);
    }
}

/// Returns the number of images to preload ahead/behind the current position.
pub fn preload_count() -> usize {
    PRELOAD_AHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(width: u32, height: u32) -> DecodedImage {
        DecodedImage::from_rgba8(width, height, vec![0u8; (width * height * 4) as usize])
    }

    fn make_hdr_image(width: u32, height: u32) -> DecodedImage {
        DecodedImage::from_rgba16f(width, height, vec![0u16; (width * height * 4) as usize])
    }

    fn test_path(index: usize) -> PathBuf {
        PathBuf::from(format!("/tmp/test_{index}.png"))
    }

    fn insert_test_image(cache: &mut ImageCache, index: usize, width: u32, height: u32) {
        cache.insert(
            test_path(index),
            make_image(width, height),
            Duration::from_millis(10),
            format!("test_{index}.png"),
        );
    }

    #[test]
    fn cache_insert_and_get() {
        let mut cache = ImageCache::new();
        insert_test_image(&mut cache, 0, 100, 100);
        assert!(cache.contains(&test_path(0)));
        assert!(cache.get(&test_path(0)).is_some());
        assert_eq!(cache.memory_used(), 100 * 100 * 4);
    }

    #[test]
    fn cache_evicts_lru_when_over_budget() {
        let mut cache = ImageCache::new();
        cache.memory_budget = 100 * 100 * 4 * 3; // Room for 3 images of 100x100

        for i in 0..4 {
            insert_test_image(&mut cache, i, 100, 100);
        }

        // Should have evicted the oldest (index 0)
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains(&test_path(0)));
        assert!(cache.contains(&test_path(1)));
        assert!(cache.contains(&test_path(2)));
        assert!(cache.contains(&test_path(3)));
    }

    #[test]
    fn cache_lru_touch_updates_order() {
        let mut cache = ImageCache::new();
        cache.memory_budget = 100 * 100 * 4 * 3;

        insert_test_image(&mut cache, 0, 100, 100);
        insert_test_image(&mut cache, 1, 100, 100);
        insert_test_image(&mut cache, 2, 100, 100);

        // Touch path 0 so it becomes most recently used
        let _ = cache.get(&test_path(0));

        // Insert a 4th: should evict path 1 (oldest untouched)
        insert_test_image(&mut cache, 3, 100, 100);
        assert!(cache.contains(&test_path(0))); // Was touched, so kept
        assert!(!cache.contains(&test_path(1))); // Evicted
        assert!(cache.contains(&test_path(2)));
        assert!(cache.contains(&test_path(3)));
    }

    #[test]
    fn cache_retain_only() {
        let mut cache = ImageCache::new();
        for i in 0..5 {
            insert_test_image(&mut cache, i, 10, 10);
        }
        cache.retain_only(&[test_path(1), test_path(3)]);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&test_path(1)));
        assert!(cache.contains(&test_path(3)));
    }

    #[test]
    fn cache_rejects_oversized_image() {
        let mut cache = ImageCache::new();
        cache.memory_budget = 100; // Very small
        insert_test_image(&mut cache, 0, 100, 100); // Way over budget
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn diagnostics_reports_all_entries() {
        let mut cache = ImageCache::new();
        insert_test_image(&mut cache, 2, 320, 240);
        insert_test_image(&mut cache, 5, 640, 480);

        let diag = cache.diagnostics();
        assert_eq!(diag.entries.len(), 2);
        // Sorted by file_name: test_2.png, test_5.png
        assert_eq!(diag.entries[0].file_name, "test_2.png");
        assert_eq!(diag.entries[0].width, 320);
        assert_eq!(diag.entries[1].file_name, "test_5.png");
        assert_eq!(diag.total_memory, 320 * 240 * 4 + 640 * 480 * 4);
    }

    #[test]
    fn cache_accounts_f16_at_eight_bytes_per_pixel() {
        // Phase 5: HDR images cost 2× per-pixel bytes. The LRU budgeter
        // has to see that so it doesn't over-subscribe the cache.
        let mut cache = ImageCache::new();
        cache.insert(
            test_path(0),
            make_hdr_image(100, 100),
            Duration::from_millis(10),
            "hdr_0.arw".to_string(),
        );
        assert_eq!(cache.memory_used(), 100 * 100 * 8);
    }

    #[test]
    fn cache_hdr_budget_doubles() {
        // Phase 5: when the preloader is in HDR mode, the cache budget
        // doubles from 512 MB to 1 GB so RAW previews keep their count.
        let mut cache = ImageCache::new();
        assert_eq!(cache.memory_budget, SDR_MEMORY_BUDGET);
        cache.set_hdr_mode(true);
        assert_eq!(cache.memory_budget, HDR_MEMORY_BUDGET);
        cache.set_hdr_mode(false);
        assert_eq!(cache.memory_budget, SDR_MEMORY_BUDGET);
    }

    #[test]
    fn cache_shrinks_on_budget_drop() {
        // Switching from HDR mode back to SDR must evict entries that no
        // longer fit the tighter budget.
        let mut cache = ImageCache::new();
        cache.set_hdr_mode(true);
        // Plant 3 × 200 MB HDR images (fits in 1 GB).
        for i in 0..3 {
            cache.insert(
                PathBuf::from(format!("/tmp/hdr_{i}.arw")),
                make_hdr_image(5000, 5000),
                Duration::from_millis(10),
                format!("hdr_{i}.arw"),
            );
        }
        assert_eq!(cache.len(), 3);
        cache.set_hdr_mode(false); // Drop to 512 MB.
        // Post-drop, the cache must shrink until resident <= budget.
        assert!(cache.memory_used() <= SDR_MEMORY_BUDGET);
        assert!(cache.len() <= 2); // at least one eviction happened
    }

    #[test]
    fn cache_clear() {
        let mut cache = ImageCache::new();
        for i in 0..5 {
            insert_test_image(&mut cache, i, 100, 100);
        }
        assert_eq!(cache.len(), 5);
        assert!(cache.memory_used() > 0);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.memory_used(), 0);
        assert!(!cache.contains(&test_path(0)));
    }
}
