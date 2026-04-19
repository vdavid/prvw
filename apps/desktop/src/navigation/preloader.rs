use crate::decoding::{self, DecodedImage, RawPipelineFlags};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

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
    /// An image was decoded and is ready.
    Ready {
        index: usize,
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
    Cancelled { index: usize },
}

/// LRU cache for decoded images with a memory budget.
pub struct ImageCache {
    entries: HashMap<usize, CacheEntry>,
    /// Access order: most recently used at the end.
    access_order: Vec<usize>,
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
    pub index: usize,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub memory_bytes: usize,
    pub decode_duration: Duration,
}

/// An image that just got removed from the cache. Returned from
/// `ImageCache::insert` / `retain_only` / `set_hdr_mode` so the caller can
/// log it with context (relative offset, reason) that the cache doesn't
/// have.
pub struct EvictedEntry {
    pub index: usize,
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
            let evict = self.access_order[0];
            self.remove(evict);
        }
    }

    /// Get a cached image by directory index, updating its LRU position.
    pub fn get(&mut self, index: usize) -> Option<&DecodedImage> {
        if self.entries.contains_key(&index) {
            self.touch(index);
            Some(&self.entries[&index].image)
        } else {
            None
        }
    }

    /// Insert a decoded image into the cache, evicting LRU entries if over
    /// budget. Returns any entries the LRU logic had to drop so the caller
    /// can log them (the cache doesn't know the current image index, which
    /// is what makes a log line readable).
    pub fn insert(
        &mut self,
        index: usize,
        image: DecodedImage,
        decode_duration: Duration,
        file_name: String,
    ) -> Vec<EvictedEntry> {
        let cost = image_memory_cost(&image);

        // If this single image exceeds the budget, don't cache it
        if cost > self.memory_budget {
            log::warn!("Image at index {index} ({cost} bytes) exceeds cache budget, not caching");
            return Vec::new();
        }

        // Remove existing entry if present
        if self.entries.contains_key(&index) {
            self.remove(index);
        }

        let mut evicted = Vec::new();
        while self.memory_used + cost > self.memory_budget && !self.access_order.is_empty() {
            let evict_index = self.access_order[0];
            if let Some(e) = self.take_evicted(evict_index) {
                evicted.push(e);
            } else {
                // Stale entry in `access_order` — defensive break to avoid a loop.
                self.access_order.remove(0);
            }
        }

        self.entries.insert(
            index,
            CacheEntry {
                image,
                decode_duration,
                file_name,
                memory_cost: cost,
            },
        );
        self.access_order.push(index);
        self.memory_used += cost;
        evicted
    }

    /// Return diagnostics snapshot of the cache.
    pub fn diagnostics(&self) -> CacheDiagnostics {
        let mut entries: Vec<CacheEntryDiagnostic> = self
            .entries
            .iter()
            .map(|(&index, entry)| CacheEntryDiagnostic {
                index,
                file_name: entry.file_name.clone(),
                width: entry.image.width,
                height: entry.image.height,
                memory_bytes: entry.memory_cost,
                decode_duration: entry.decode_duration,
            })
            .collect();
        entries.sort_by_key(|e| e.index);
        CacheDiagnostics {
            total_memory: self.memory_used,
            memory_budget: self.memory_budget,
            entries,
        }
    }

    pub fn contains(&self, index: usize) -> bool {
        self.entries.contains_key(&index)
    }

    /// Remove entries outside the hot window around the current position.
    /// Called on every navigation so distant images release their RAM
    /// promptly instead of sitting until the LRU budget pushes them out.
    /// Returns the entries that were dropped so the caller can log them.
    pub fn retain_only(&mut self, keep: &[usize]) -> Vec<EvictedEntry> {
        let to_remove: Vec<usize> = self
            .entries
            .keys()
            .filter(|k| !keep.contains(k))
            .copied()
            .collect();
        let mut evicted = Vec::with_capacity(to_remove.len());
        for index in to_remove {
            if let Some(e) = self.take_evicted(index) {
                evicted.push(e);
            }
        }
        evicted
    }

    /// Remove `index` and return its metadata as an `EvictedEntry`.
    fn take_evicted(&mut self, index: usize) -> Option<EvictedEntry> {
        let entry = self.entries.remove(&index)?;
        self.memory_used = self.memory_used.saturating_sub(entry.memory_cost);
        self.access_order.retain(|&i| i != index);
        Some(EvictedEntry {
            index,
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

    fn touch(&mut self, index: usize) {
        self.access_order.retain(|&i| i != index);
        self.access_order.push(index);
    }

    fn remove(&mut self, index: usize) {
        if let Some(entry) = self.entries.remove(&index) {
            self.memory_used = self.memory_used.saturating_sub(entry.memory_cost);
            self.access_order.retain(|&i| i != index);
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
    /// In-flight cancellation tokens keyed by directory index. When
    /// `request_preload` runs, indices still in the new task list keep their
    /// existing token (so a mid-decode task survives), and indices no longer
    /// in the list have their token flipped (cancelling that decode).
    in_flight: HashMap<usize, Arc<AtomicBool>>,
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
}

impl Preloader {
    pub fn start(
        display_icc: Vec<u8>,
        use_relative_colorimetric: bool,
        raw_flags: RawPipelineFlags,
        edr_headroom: f32,
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
        }
    }

    /// Update the display's EDR headroom snapshot used by future decode
    /// tasks. The caller flushes the image cache and re-submits preload
    /// tasks so existing entries (possibly RGBA8-only) don't mix with
    /// fresh RGBA16F ones.
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

    /// Cancel any in-flight tasks not in `tasks`, leave the rest running,
    /// and queue the new ones. `tasks` is priority-ordered — earlier entries
    /// are more likely to be what the user wants next. The worker thread
    /// pops from the FIFO one at a time, so priority-zero runs first.
    ///
    /// Tasks already in flight for one of the requested indices are NOT
    /// cancelled and NOT resubmitted — their decode continues from wherever
    /// it was. Only indices that disappear from the list get their
    /// cancellation token flipped; their closures still run on the worker
    /// but exit in microseconds when `load_image` checks the flag.
    pub fn request_preload(
        &mut self,
        tasks: Vec<(usize, PathBuf)>,
        current_index: usize,
        total: usize,
    ) {
        let requested: std::collections::HashSet<usize> = tasks.iter().map(|(i, _)| *i).collect();

        // Cancel tokens for indices no longer wanted.
        let mut cancelled_count = 0usize;
        self.in_flight.retain(|index, token| {
            if requested.contains(index) {
                true
            } else {
                token.store(true, Ordering::Relaxed);
                cancelled_count += 1;
                false
            }
        });
        if cancelled_count > 0 {
            log::debug!("Cancelled {cancelled_count} stale in-flight tasks");
        }

        let indices: Vec<usize> = tasks.iter().map(|(i, _)| *i).collect();
        log::debug!("Preloading {} images: {indices:?}", tasks.len());

        for (index, path) in tasks.into_iter() {
            // Already decoding this index — leave it alone.
            if self.in_flight.contains_key(&index) {
                continue;
            }

            let cancelled = Arc::new(AtomicBool::new(false));
            self.in_flight.insert(index, Arc::clone(&cancelled));

            // Human-readable labels captured at submit time so the logs read
            // consistently even if the user navigates mid-decode.
            let offset_label = crate::navigation::format_offset(index, current_index);
            let position_label = format!("{}/{}", index + 1, total);

            let tx = self.response_tx.clone();
            let display_icc = Arc::clone(&self.display_icc);
            let use_relative_colorimetric = self.use_relative_colorimetric;
            let raw_flags = self.raw_flags;
            let edr_headroom = self.edr_headroom;
            let task = move || {
                let file_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                log::debug!("Initiated loading {file_name} ({offset_label}, {position_label})");
                let start = Instant::now();
                match decoding::load_image(
                    &path,
                    &cancelled,
                    &display_icc,
                    use_relative_colorimetric,
                    raw_flags,
                    edr_headroom,
                ) {
                    Ok(image) => {
                        let duration = start.elapsed();
                        log::debug!(
                            "Fully loaded {file_name} ({offset_label}, {position_label}) in {}ms",
                            duration.as_millis()
                        );
                        let _ = tx.send(PreloadResponse::Ready {
                            index,
                            image,
                            decode_duration: duration,
                            file_name,
                        });
                    }
                    Err(reason) if reason == "cancelled" => {
                        log::debug!(
                            "Cancelled loading {file_name} ({offset_label}, {position_label})"
                        );
                        let _ = tx.send(PreloadResponse::Cancelled { index });
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
            };

            // Channel is naturally FIFO — execution order matches
            // submission order, which is priority order.
            if self.task_tx.send(Box::new(task)).is_err() {
                log::warn!("Preloader worker is gone — dropping task for [{index}]");
            }
        }
    }

    /// Clear the in-flight tracking for a completed index.
    pub fn mark_complete(&mut self, index: usize) {
        self.in_flight.remove(&index);
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

    fn insert_test_image(cache: &mut ImageCache, index: usize, width: u32, height: u32) {
        cache.insert(
            index,
            make_image(width, height),
            Duration::from_millis(10),
            format!("test_{index}.png"),
        );
    }

    #[test]
    fn cache_insert_and_get() {
        let mut cache = ImageCache::new();
        insert_test_image(&mut cache, 0, 100, 100);
        assert!(cache.contains(0));
        assert!(cache.get(0).is_some());
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
        assert!(!cache.contains(0));
        assert!(cache.contains(1));
        assert!(cache.contains(2));
        assert!(cache.contains(3));
    }

    #[test]
    fn cache_lru_touch_updates_order() {
        let mut cache = ImageCache::new();
        cache.memory_budget = 100 * 100 * 4 * 3;

        insert_test_image(&mut cache, 0, 100, 100);
        insert_test_image(&mut cache, 1, 100, 100);
        insert_test_image(&mut cache, 2, 100, 100);

        // Touch index 0 so it becomes most recently used
        let _ = cache.get(0);

        // Insert a 4th: should evict index 1 (oldest untouched)
        insert_test_image(&mut cache, 3, 100, 100);
        assert!(cache.contains(0)); // Was touched, so kept
        assert!(!cache.contains(1)); // Evicted
        assert!(cache.contains(2));
        assert!(cache.contains(3));
    }

    #[test]
    fn cache_retain_only() {
        let mut cache = ImageCache::new();
        for i in 0..5 {
            insert_test_image(&mut cache, i, 10, 10);
        }
        cache.retain_only(&[1, 3]);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(1));
        assert!(cache.contains(3));
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
        assert_eq!(diag.entries[0].index, 2);
        assert_eq!(diag.entries[0].width, 320);
        assert_eq!(diag.entries[1].index, 5);
        assert_eq!(diag.total_memory, 320 * 240 * 4 + 640 * 480 * 4);
    }

    #[test]
    fn cache_accounts_f16_at_eight_bytes_per_pixel() {
        // Phase 5: HDR images cost 2× per-pixel bytes. The LRU budgeter
        // has to see that so it doesn't over-subscribe the cache.
        let mut cache = ImageCache::new();
        cache.insert(
            0,
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
                i,
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
        assert!(!cache.contains(0));
    }
}
