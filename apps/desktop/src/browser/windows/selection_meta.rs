//! Measure the selected image off the main thread, so the status bar can say how big it is.
//!
//! Reading a JPEG header is microseconds locally and a round trip on a NAS, and the browser's
//! whole premise is that a slow filesystem never reaches the main thread. So this is one worker
//! and a channel, the same shape as `browser::grid_listing::FolderLister` and
//! `navigation::preloader`: `std::thread` + `mpsc`, no tokio.
//!
//! One request supersedes the previous. Arrowing through a folder fires a request per cell, and
//! only the file the user settles on is worth a header read; the worker drains the queue and
//! measures the newest path alone. A late answer for a file that is no longer selected is
//! dropped by the main thread, which compares the delivered path against what it is showing.

use std::path::PathBuf;
use std::sync::mpsc;

/// Measures whichever image was asked for most recently. Owned by the browse UI, alive for the
/// window's life.
pub struct SelectionMeasurer {
    request_tx: mpsc::Sender<PathBuf>,
}

impl SelectionMeasurer {
    /// Spawn the worker. It runs until the `Sender` drops, which closes the channel.
    #[must_use]
    pub fn start() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("prvw-browse-measure".into())
            .spawn(move || {
                while let Ok(mut path) = request_rx.recv() {
                    // Coalesce: a fast arrow through a folder queues one per cell, and only the
                    // one the user stopped on is worth reading.
                    while let Ok(newer) = request_rx.try_recv() {
                        path = newer;
                    }
                    let dimensions = crate::previews::metadata::read_dimensions_fast(&path)
                        .map(|d| (d.width, d.height));
                    log::debug!(
                        "Browse selection measured: {} → {dimensions:?}",
                        path.display()
                    );
                    crate::commands::send_command(
                        crate::commands::AppCommand::BrowseSelectionMeasured { path, dimensions },
                    );
                }
                log::debug!("Browse selection measurer exiting");
            })
            .expect("Failed to spawn the browse selection measurer thread");
        Self { request_tx }
    }

    /// Ask for `path`'s pixel size. Fire-and-forget; the answer arrives as
    /// `AppCommand::BrowseSelectionMeasured`.
    pub fn measure(&self, path: PathBuf) {
        if self.request_tx.send(path).is_err() {
            log::warn!("Browse selection measurer is gone — dropping the request");
        }
    }
}
