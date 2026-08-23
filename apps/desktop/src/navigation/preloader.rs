use crate::commands::AppCommand;
use crate::decoding::{self, DecodedImage, RawPipelineFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

/// Fraction of physical RAM the SDR image cache may hold. 1/64 puts a 32 GB Mac
/// on the 512 MB this budget is tuned for, and it scales the same way
/// `previews` does rather than inventing a second scheme. The ratio matters
/// more than the absolute number once we leave macOS: an 8 GB Windows laptop
/// has no unified memory, so it pays for the GPU-side copies separately.
const SDR_RAM_DIVISOR: usize = 64;

/// One decoded 24 MP image as RGBA8: 24 million pixels at 4 bytes each. The
/// unit both the budget floor and the preload window are expressed in.
///
/// 24 MP because the window has to survive the images that can overrun the
/// cache, not the median one. It's what a current full-frame body shoots, and
/// it's already this repo's reference large image (`color::transform`'s
/// measurements use it, and so does the cross-platform plan's `profiles_match`
/// note). Phone photos, the common case, are a third of it and simply fit more
/// of themselves into the same bytes — the cache charges exact
/// `width * height * bytes_per_pixel`, so a smaller image is never rounded up
/// to this.
///
/// RGBA8 rather than RGBA16F even though HDR RAW decodes are 8 bytes per pixel:
/// [`hdr_memory_budget`] doubles alongside, so the window comes out the same in
/// both modes and only one of them needs a constant.
const LARGE_DECODE_BYTES: usize = 24_000_000 * 4;

/// Farthest the preloader will ever reach on each side of the current image.
/// The live window is [`preload_count`], which is smaller on a machine whose
/// budget won't retain this many.
const MAX_PRELOAD_AHEAD: usize = 2;

/// Floor and ceiling for the RAM-scaled SDR cache budget.
///
/// The floor is three [`LARGE_DECODE_BYTES`]: the image on screen plus one
/// neighbor on each side, the narrowest window that still makes navigation
/// instant whichever way the user turns. Sizing the floor to the window rather
/// than the other way round is the point — a budget that can't hold what the
/// preloader fetches doesn't save memory, it just decodes the same images over
/// and over.
///
/// The ceiling is the 512 MB this budget used to be fixed at, so scaling can
/// only ever shrink the cache: a big machine keeps exactly the behavior it has
/// today, and only small ones get frugal. It holds the full
/// ±[`MAX_PRELOAD_AHEAD`] window with room to spare, and reaching further buys
/// nothing.
const MIN_SDR_MEMORY_BUDGET: usize = 3 * LARGE_DECODE_BYTES;
const MAX_SDR_MEMORY_BUDGET: usize = 512 * 1024 * 1024;

/// SDR cache budget: 1/[`SDR_RAM_DIVISOR`] of physical RAM, clamped. 32 GB and
/// up land on the ceiling, 24 GB on 384 MB, and 16 GB and below on the floor.
/// Every JPEG/PNG/WebP cached image comes out of the same budget. Queried once
/// (RAM doesn't change at runtime).
pub fn sdr_memory_budget() -> usize {
    static BUDGET: OnceLock<usize> = OnceLock::new();
    *BUDGET.get_or_init(|| sdr_budget_for_ram(crate::platform::total_physical_ram_bytes() as usize))
}

/// Pure budget math, split out for testing without depending on host RAM.
fn sdr_budget_for_ram(ram_bytes: usize) -> usize {
    (ram_bytes / SDR_RAM_DIVISOR).clamp(MIN_SDR_MEMORY_BUDGET, MAX_SDR_MEMORY_BUDGET)
}

/// HDR cache budget (Phase 5). Doubled from the SDR path because RAW
/// RGBA16F is 8 bytes per pixel instead of 4, so the same doubling keeps
/// [`preload_count`] identical in both modes. User decision: trade RAM for
/// preload count, because the preload experience is the whole value
/// proposition of the preloader (see `docs/notes/raw-support-phase5.md` for
/// the trade-off note).
pub fn hdr_memory_budget() -> usize {
    sdr_memory_budget() * 2
}

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
    /// The two budgets `set_hdr_mode` switches between, resolved once at
    /// construction so the cache never re-reads host RAM mid-session.
    sdr_budget: usize,
    hdr_budget: usize,
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
        Self::with_budgets(sdr_memory_budget(), hdr_memory_budget())
    }

    fn with_budgets(sdr_budget: usize, hdr_budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
            memory_used: 0,
            memory_budget: sdr_budget,
            sdr_budget,
            hdr_budget,
        }
    }

    /// Switch the cache budget between the SDR and HDR sizes. Called when the
    /// RAW pipeline's `hdr_output` flag or the display's EDR headroom changes.
    /// Evicts LRU entries if the new budget is smaller than the
    /// currently-resident total.
    pub fn set_hdr_mode(&mut self, hdr: bool) {
        let new_budget = if hdr {
            self.hdr_budget
        } else {
            self.sdr_budget
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

    /// Evict a single path from the cache. Used by live folder sync on a `Modify`/delete so a
    /// re-decode reads fresh bytes and a deleted image stops squatting in RAM.
    pub fn remove(&mut self, path: &Path) {
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

    /// Warm an explicit set of paths into the cache, decoding each into a
    /// background task. Cancels in-flight tasks for paths NOT in the new list
    /// (a moved selection no longer wants them), keeps the rest alive, and
    /// queues fresh tasks for paths not yet in flight. Path-keyed and
    /// equal-priority (no cancel-all like `prioritize_target`), so it warms
    /// arbitrary paths WITHOUT disturbing the displayed image or `dir_list`:
    /// the browse selection treats its prospective current image + neighbors
    /// as warm targets while the viewer still shows the previous image.
    ///
    /// `tasks` are `(index, path)` pairs where `index` is purely a label /
    /// shared-state slot — the cache is keyed by path, so a warm decode lands
    /// in the same `image_cache` the viewer reads regardless of the slot.
    /// Re-calling with a new set cancels the paths that dropped out (the
    /// move-cancellation the browse selection needs).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only macOS browse warming calls this
    pub fn warm_paths(&mut self, tasks: Vec<(usize, PathBuf)>, total: usize) {
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
            log::debug!("Cancelled {cancelled_count} stale browse-warm tasks");
        }
        let indices: Vec<usize> = tasks.iter().map(|(i, _)| *i).collect();
        if !indices.is_empty() {
            log::debug!("Warming browse selection: {indices:?}");
        }
        // The first task is the prospective current image, so it should land
        // first; the rest are neighbors. `current_index` for the log offset is
        // the first index (the prospective current).
        let current_index = indices.first().copied().unwrap_or(0);
        for (index, path) in tasks.into_iter() {
            if self.in_flight.contains_key(&path) {
                continue;
            }
            // Warm targets are not displayed yet — no preview placeholder.
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
            // Test affordance: when `PRVW_PREVIEW_HOLD_MS` is set, delay
            // the decode by N ms so the preview placeholder stays
            // visible long enough to inspect via MCP or take a
            // screenshot. Zero cost when unset — `std::env::var`
            // returns `Err`, the `ok()` is `None`, and `unwrap_or(0)`
            // short-circuits the sleep. The sleep polls `cancelled`
            // every 50 ms so a navigation that happens during the
            // hold cancels its task within ~50 ms instead of the
            // whole hold — otherwise queued-and-cancelled tasks
            // each burn the full `hold_ms` before the worker moves
            // on to the current target.
            let hold_ms: u64 = std::env::var("PRVW_PREVIEW_HOLD_MS")
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

/// How many images to preload each side of the current one.
///
/// Derived from the cache budget rather than fixed, so the preloader never
/// fetches more than the cache will retain. A window wider than the budget is
/// worse than a narrow one: each preload evicts the previous one, and once the
/// image on screen becomes the LRU entry it gets evicted by its own neighbors,
/// so every keypress pays for a fresh decode. `previews::generation_radius`
/// derives its radius from its own budget for exactly this reason.
///
/// A window of `n` holds `2n + 1` images, so the budget has to cover the
/// current image plus `2n` neighbors. Read off the SDR budget in both modes:
/// HDR doubles the budget and the per-pixel size together, so the count is the
/// same either way.
pub fn preload_count() -> usize {
    preload_count_for_budget(sdr_memory_budget())
}

/// Pure window math, split out for testing without depending on host RAM.
fn preload_count_for_budget(budget: usize) -> usize {
    (budget.saturating_sub(LARGE_DECODE_BYTES) / (2 * LARGE_DECODE_BYTES))
        .clamp(1, MAX_PRELOAD_AHEAD)
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
        // doubles so RAW previews keep their count.
        let mut cache = ImageCache::new();
        assert_eq!(cache.memory_budget, sdr_memory_budget());
        cache.set_hdr_mode(true);
        assert_eq!(cache.memory_budget, hdr_memory_budget());
        assert_eq!(hdr_memory_budget(), sdr_memory_budget() * 2);
        cache.set_hdr_mode(false);
        assert_eq!(cache.memory_budget, sdr_memory_budget());
    }

    #[test]
    fn cache_shrinks_on_budget_drop() {
        // Switching from HDR mode back to SDR must evict entries that no
        // longer fit the tighter budget. Budgets are pinned so the assertion
        // doesn't depend on how much RAM the machine running the test has.
        const SDR: usize = 512 * 1024 * 1024;
        const HDR: usize = 2 * SDR;
        let mut cache = ImageCache::with_budgets(SDR, HDR);
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
        assert!(cache.memory_used() <= SDR);
        assert!(cache.len() <= 2); // at least one eviction happened
    }

    /// The budget tracks RAM between the floor and the ceiling.
    #[test]
    fn sdr_budget_scales_with_ram() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(sdr_budget_for_ram(24 * gb), 384 * 1024 * 1024);
        assert_eq!(sdr_budget_for_ram(20 * gb), 320 * 1024 * 1024);
        assert_eq!(sdr_budget_for_ram(16 * gb), MIN_SDR_MEMORY_BUDGET);
        assert_eq!(sdr_budget_for_ram(8 * gb), MIN_SDR_MEMORY_BUDGET);
    }

    /// Scaling must never hand out *more* than the budget used to be fixed at.
    /// A 32 GB Mac and a 256 GB Mac Pro both keep today's 512 MB.
    #[test]
    fn sdr_budget_never_exceeds_the_old_fixed_size() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(MAX_SDR_MEMORY_BUDGET, 512 * 1024 * 1024);
        assert_eq!(sdr_budget_for_ram(32 * gb), MAX_SDR_MEMORY_BUDGET);
        assert_eq!(sdr_budget_for_ram(256 * gb), MAX_SDR_MEMORY_BUDGET);
        assert_eq!(sdr_budget_for_ram(usize::MAX), MAX_SDR_MEMORY_BUDGET);
    }

    #[test]
    fn sdr_budget_never_drops_below_the_floor() {
        assert_eq!(sdr_budget_for_ram(0), MIN_SDR_MEMORY_BUDGET);
        assert_eq!(sdr_budget_for_ram(1024), MIN_SDR_MEMORY_BUDGET);
    }

    /// The invariant the whole budget/window pair exists to hold: whatever the
    /// host, the cache must be able to retain everything the preloader fetches.
    /// If this fails, every navigation evicts what the last one decoded.
    #[test]
    fn the_budget_always_holds_the_whole_preload_window() {
        for budget in [
            MIN_SDR_MEMORY_BUDGET,
            MIN_SDR_MEMORY_BUDGET + 1,
            300 * 1024 * 1024,
            384 * 1024 * 1024,
            MAX_SDR_MEMORY_BUDGET,
            usize::MAX,
        ] {
            let resident = (2 * preload_count_for_budget(budget) + 1) * LARGE_DECODE_BYTES;
            assert!(
                resident <= budget,
                "window of {} needs {resident} bytes, budget is {budget}",
                preload_count_for_budget(budget)
            );
        }
        let live = (2 * preload_count() + 1) * LARGE_DECODE_BYTES;
        assert!(live <= sdr_memory_budget(), "on this host: {live} bytes");
    }

    /// The window narrows with the budget, and never past the point where
    /// navigating either way is still warm.
    #[test]
    fn preload_window_follows_the_budget() {
        assert_eq!(preload_count_for_budget(MAX_SDR_MEMORY_BUDGET), 2);
        assert_eq!(preload_count_for_budget(MIN_SDR_MEMORY_BUDGET), 1);
        assert_eq!(preload_count_for_budget(384 * 1024 * 1024), 1);
        assert_eq!(preload_count_for_budget(usize::MAX), MAX_PRELOAD_AHEAD);
        // Below the floor is unreachable through `sdr_budget_for_ram`, but the
        // window still refuses to collapse to zero: a preloader that preloads
        // nothing is worse than a small one.
        assert_eq!(preload_count_for_budget(0), 1);
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
