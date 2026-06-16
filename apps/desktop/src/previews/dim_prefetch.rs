//! Parallel pixel-dimension prefetcher.
//!
//! Reads `(width, height)` (with EXIF orientation applied) for every
//! image in the active preview window, in parallel across 16 worker
//! threads. Populates a shared `HashMap<usize, Dimensions>` that the
//! main thread reads when displaying a preview placeholder.
//!
//! ## Why this exists
//!
//! Without prefetching, the first navigation to any not-yet-visited
//! index pays the cost of a synchronous ImageIO file-header read on the
//! main thread. On a slow network share that's 200 ms – 1.3 s per file
//! — which the user perceives as laggy navigation. By reading dims
//! eagerly in the background in parallel, every navigation finds the
//! dim already cached and the placeholder displays in <5 ms.
//!
//! ## Why 16 threads
//!
//! On a typical macOS SMB connection the per-request cost is round-trip
//! latency, not bandwidth. Reads run in parallel up to the SMB outstanding
//! requests limit (~64 raw operations, divided by ~3 ops per file =
//! ~20 concurrent file reads). Past that, the kernel queues. 16 threads
//! keeps us safely under the SMB ceiling, well above 8 (which is too
//! conservative for SMB), and avoids file-descriptor pressure.
//!
//! Local SSD can handle hundreds; iCloud Drive less. 16 is a good
//! all-rounder.

use crate::previews::metadata::{self, Dimensions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

/// Worker count. See module-level docs for tuning rationale.
const NUM_THREADS: usize = 16;

/// Per-worker stack. Default `std::thread` stack is 8 MB; 16 × 8 MB =
/// 128 MB. Dropping to 2 MB saves 96 MB. Worker bodies do trivial work
/// (file open, header parse, mutex insert) so 2 MB is plenty.
const WORKER_STACK_SIZE: usize = 2 * 1024 * 1024;

pub struct DimPrefetcher {
    job_tx: mpsc::Sender<Job>,
    results: Arc<Mutex<HashMap<usize, Dimensions>>>,
    generation: Arc<AtomicU64>,
    _workers: Vec<thread::JoinHandle<()>>,
}

struct Job {
    index: usize,
    path: PathBuf,
    generation: u64,
}

impl DimPrefetcher {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let results = Arc::new(Mutex::new(HashMap::new()));
        let generation = Arc::new(AtomicU64::new(0));

        let workers: Vec<_> = (0..NUM_THREADS)
            .map(|i| {
                let job_rx = Arc::clone(&job_rx);
                let results = Arc::clone(&results);
                let generation = Arc::clone(&generation);
                thread::Builder::new()
                    .name(format!("prvw-dim-{i}"))
                    .stack_size(WORKER_STACK_SIZE)
                    .spawn(move || worker_loop(job_rx, results, generation))
                    .expect("failed to spawn dim prefetch worker")
            })
            .collect();

        log::debug!("Spawned {NUM_THREADS} dimension-prefetch workers");

        Self {
            job_tx,
            results,
            generation,
            _workers: workers,
        }
    }

    /// Queue a dimension read for `(index, path)`. Worker pool picks it
    /// up. Cheap to call: just an mpsc send.
    pub fn enqueue(&self, index: usize, path: PathBuf) {
        let generation = self.generation.load(Ordering::Relaxed);
        let _ = self.job_tx.send(Job {
            index,
            path,
            generation,
        });
    }

    /// Read the cached dimensions for `index`, or `None` if not yet
    /// prefetched.
    pub fn get(&self, index: usize) -> Option<Dimensions> {
        self.results.lock().ok()?.get(&index).copied()
    }

    /// Manually insert dimensions (used by the main-thread lazy fallback
    /// path so a synchronous read populates the same cache the workers
    /// fill).
    pub fn put(&self, index: usize, dims: Dimensions) {
        if let Ok(mut r) = self.results.lock() {
            r.insert(index, dims);
        }
    }

    /// Drop entries from the cache (called by `State::evict_distant_previews`
    /// so the dim cache shadows the preview cache's retention zone).
    pub fn invalidate(&self, indices: &[usize]) {
        if let Ok(mut r) = self.results.lock() {
            for i in indices {
                r.remove(i);
            }
        }
    }

    /// Bump generation (drops any in-flight stale jobs) and clear cache.
    /// Called on `set_folder`.
    pub fn reset(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut r) = self.results.lock() {
            r.clear();
        }
    }
}

impl Default for DimPrefetcher {
    fn default() -> Self {
        Self::new()
    }
}

fn worker_loop(
    rx: Arc<Mutex<mpsc::Receiver<Job>>>,
    results: Arc<Mutex<HashMap<usize, Dimensions>>>,
    current_gen: Arc<AtomicU64>,
) {
    loop {
        // Pop the next job under the shared receiver mutex. mpsc
        // Receiver isn't Sync, so the standard pattern is wrap-in-mutex.
        // Lock is held only for the recv() call itself — workers
        // concurrently process jobs after release.
        let job = match rx.lock() {
            Ok(guard) => match guard.recv() {
                Ok(j) => j,
                Err(_) => return, // sender dropped (process exiting)
            },
            Err(_) => return, // poisoned
        };

        // Drop stale jobs cheaply before reading the file.
        if job.generation != current_gen.load(Ordering::Relaxed) {
            continue;
        }
        // If another worker already populated this index (or main thread
        // did via the lazy fallback), skip.
        if results
            .lock()
            .ok()
            .is_some_and(|r| r.contains_key(&job.index))
        {
            continue;
        }

        let Some(dims) = metadata::read_dimensions_fast(&job.path) else {
            continue;
        };

        // Re-check generation: the read might have taken hundreds of ms
        // on a slow share, during which the user might have changed
        // folders.
        if job.generation != current_gen.load(Ordering::Relaxed) {
            continue;
        }
        if let Ok(mut r) = results.lock() {
            r.insert(job.index, dims);
        }
    }
}
