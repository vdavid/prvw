use crate::image_loader::{self, DecodedImage};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::available_parallelism;
use std::time::{Duration, Instant};

const DEFAULT_MEMORY_BUDGET: usize = 512 * 1024 * 1024; // 512 MB
const PRELOAD_AHEAD: usize = 2;

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

impl ImageCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
            memory_used: 0,
            memory_budget: DEFAULT_MEMORY_BUDGET,
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

    /// Insert a decoded image into the cache, evicting LRU entries if over budget.
    pub fn insert(
        &mut self,
        index: usize,
        image: DecodedImage,
        decode_duration: Duration,
        file_name: String,
    ) {
        let cost = image_memory_cost(&image);

        // If this single image exceeds the budget, don't cache it
        if cost > self.memory_budget {
            log::warn!("Image at index {index} ({cost} bytes) exceeds cache budget, not caching");
            return;
        }

        // Remove existing entry if present
        if self.entries.contains_key(&index) {
            self.remove(index);
        }

        // Evict until there's room
        while self.memory_used + cost > self.memory_budget && !self.access_order.is_empty() {
            let evict_index = self.access_order[0];
            if let Some(entry) = self.entries.get(&evict_index) {
                log::debug!(
                    "Cache evicted [{evict_index}] {} ({}), freeing {}",
                    entry.file_name,
                    format_cache_bytes(entry.memory_cost),
                    format_cache_bytes(entry.memory_cost)
                );
            }
            self.remove(evict_index);
        }

        log::debug!(
            "Cache insert [{index}] {file_name} ({}), total: {}",
            format_cache_bytes(cost),
            format_cache_bytes(self.memory_used + cost)
        );

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

    /// Remove entries not in the given set of indices (cleanup after navigation).
    #[allow(dead_code)] // Part of cache API, used as the image set grows
    pub fn retain_only(&mut self, keep: &[usize]) {
        let to_remove: Vec<usize> = self
            .entries
            .keys()
            .filter(|k| !keep.contains(k))
            .copied()
            .collect();
        for index in to_remove {
            self.remove(index);
        }
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
    image.width as usize * image.height as usize * 4
}

/// Format a byte count compactly for cache log messages.
fn format_cache_bytes(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

/// Parallel image preloader backed by a rayon thread pool.
pub struct Preloader {
    pool: rayon::ThreadPool,
    response_tx: mpsc::Sender<PreloadResponse>,
    pub response_rx: mpsc::Receiver<PreloadResponse>,
    /// Indices currently being decoded (prevents duplicate work).
    in_flight: HashSet<usize>,
    /// Cancellation tokens for in-flight tasks.
    cancellation_tokens: Vec<Arc<AtomicBool>>,
    /// ICC profile bytes for the current display (target color space for decoding).
    display_icc: Arc<Vec<u8>>,
}

impl Preloader {
    pub fn start(display_icc: Vec<u8>) -> Self {
        let num_threads = available_parallelism().map(|n| n.get()).unwrap_or(4);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|i| format!("prvw-preload-{i}"))
            .build()
            .expect("Failed to create preloader thread pool");

        log::info!("Preloader started with {num_threads} threads");

        let (response_tx, response_rx) = mpsc::channel();

        Self {
            pool,
            response_tx,
            response_rx,
            in_flight: HashSet::new(),
            cancellation_tokens: Vec::new(),
            display_icc: Arc::new(display_icc),
        }
    }

    /// Update the target display ICC profile (called when the window moves to a different display).
    pub fn set_display_icc(&mut self, icc: Vec<u8>) {
        self.display_icc = Arc::new(icc);
    }

    /// Cancel all in-flight tasks and submit new ones.
    /// Tasks are submitted in priority order: indices earlier in the list get higher priority.
    pub fn request_preload(&mut self, tasks: Vec<(usize, PathBuf)>) {
        // Cancel all existing in-flight tasks
        let cancelled_count = self.cancellation_tokens.len();
        for token in &self.cancellation_tokens {
            token.store(true, Ordering::Relaxed);
        }
        self.cancellation_tokens.clear();
        self.in_flight.clear();

        if cancelled_count > 0 {
            log::debug!("Cancelled {cancelled_count} in-flight tasks");
        }

        let indices: Vec<usize> = tasks.iter().map(|(i, _)| *i).collect();
        log::debug!("Preloading {} images: {indices:?}", tasks.len());

        for (priority, (index, path)) in tasks.into_iter().enumerate() {
            self.in_flight.insert(index);

            let cancelled = Arc::new(AtomicBool::new(false));
            self.cancellation_tokens.push(Arc::clone(&cancelled));

            let tx = self.response_tx.clone();
            let display_icc = Arc::clone(&self.display_icc);
            let task = move || {
                let file_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let start = Instant::now();
                match image_loader::load_image_cancellable(&path, &cancelled, &display_icc) {
                    Ok(image) => {
                        let duration = start.elapsed();
                        log::debug!(
                            "Preloaded [{index}] {file_name} in {}ms",
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
                        log::debug!("Preload cancelled for [{index}] {file_name}");
                        let _ = tx.send(PreloadResponse::Cancelled { index });
                    }
                    Err(reason) => {
                        log::warn!("Preload failed for [{index}] {}: {reason}", path.display());
                        let _ = tx.send(PreloadResponse::Failed {
                            index,
                            path,
                            reason,
                        });
                    }
                }
            };

            // First task (highest priority) gets FIFO scheduling
            if priority == 0 {
                self.pool.spawn_fifo(task);
            } else {
                self.pool.spawn(task);
            }
        }
    }

    /// Clear the in-flight tracking for a completed index.
    pub fn mark_complete(&mut self, index: usize) {
        self.in_flight.remove(&index);
    }

    /// Shut down the preloader (rayon handles thread cleanup on drop).
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
        DecodedImage {
            width,
            height,
            rgba_data: vec![0u8; (width * height * 4) as usize],
        }
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
