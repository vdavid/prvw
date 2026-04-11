use crate::image_loader::{self, DecodedImage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_MEMORY_BUDGET: usize = 512 * 1024 * 1024; // 512 MB
const PRELOAD_AHEAD: usize = 2;

/// Messages sent from the main thread to the preloader.
pub enum PreloadRequest {
    /// Preload these files (paths with their directory indices for cache tracking).
    Load(Vec<(usize, PathBuf)>),
    /// Shut down the preloader thread.
    Shutdown,
}

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
            self.remove(evict_index);
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

/// Handle to the preloader thread. Owns the sender; the main thread reads from the receiver.
pub struct Preloader {
    request_tx: mpsc::Sender<PreloadRequest>,
    pub response_rx: mpsc::Receiver<PreloadResponse>,
    _handle: thread::JoinHandle<()>,
}

impl Preloader {
    pub fn start() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PreloadRequest>();
        let (response_tx, response_rx) = mpsc::channel::<PreloadResponse>();

        let handle = thread::Builder::new()
            .name("prvw-preloader".to_string())
            .spawn(move || {
                preloader_loop(request_rx, response_tx);
            })
            .expect("Failed to spawn preloader thread");

        Self {
            request_tx,
            response_rx,
            _handle: handle,
        }
    }

    /// Ask the preloader to decode the given files.
    pub fn request_preload(&self, files: Vec<(usize, PathBuf)>) {
        let _ = self.request_tx.send(PreloadRequest::Load(files));
    }

    /// Shut down the preloader thread.
    pub fn shutdown(&self) {
        let _ = self.request_tx.send(PreloadRequest::Shutdown);
    }
}

/// Returns the number of images to preload ahead/behind the current position.
pub fn preload_count() -> usize {
    PRELOAD_AHEAD
}

fn preloader_loop(rx: mpsc::Receiver<PreloadRequest>, tx: mpsc::Sender<PreloadResponse>) {
    loop {
        match rx.recv() {
            Ok(PreloadRequest::Load(files)) => {
                for (index, path) in files {
                    let file_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let start = Instant::now();
                    match image_loader::load_image(&path) {
                        Ok(image) => {
                            let decode_duration = start.elapsed();
                            if tx
                                .send(PreloadResponse::Ready {
                                    index,
                                    image,
                                    decode_duration,
                                    file_name,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(reason) => {
                            log::warn!("Preloader: couldn't decode {}: {reason}", path.display());
                            if tx
                                .send(PreloadResponse::Failed {
                                    index,
                                    path,
                                    reason,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
            Ok(PreloadRequest::Shutdown) | Err(_) => return,
        }
    }
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
}
