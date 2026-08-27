//! What a preview request carries, and what comes back — the same shapes on every platform.
//!
//! Two generators fill these in: [`super::quicklook`] hands the work to `quicklookd` on macOS,
//! and [`super::generator`] runs a worker pool of our own everywhere else. Because both speak
//! these types, everything above them (`previews::State`, `App::pump_preview_requests`, the
//! `PreviewsAvailable` arm of `execute_command`) is one un-`cfg`ed code path.
//!
//! ## Size is a point size plus a scale, not a pixel count
//!
//! Both fields ride along because macOS needs them apart: `QLThumbnailGenerationRequest` keys
//! quicklookd's on-disk cache on the pair, and 512 pt at scale 2 hits the same gallery bucket
//! Finder fills, where 1024 pt at scale 1 would miss it. A generator that thinks in pixels
//! multiplies them itself (`generator::request_pixels`).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;
use crate::previews::scheduler::RequestId;

/// A ready preview: raw RGBA8, row-packed (no padding), premultiplied alpha.
pub struct PreviewPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One result slot delivered to the main thread by a generator. `Ok(pixels)` for a generated
/// preview, `Err` for a request that produced nothing.
pub struct Delivery {
    pub index: usize,
    pub request_id: RequestId,
    pub folder_generation: u64,
    pub result: Result<PreviewPixels, ()>,
}

/// Arguments for a generator's `submit`.
pub struct SubmitRequest<'a> {
    pub request_id: RequestId,
    pub index: usize,
    pub folder_generation: u64,
    pub path: &'a Path,
    /// Longest edge of the preview in points. See the module docs for why this isn't pixels.
    pub size_pt: f64,
    /// The display scale those points are measured against (2.0 on a Retina Mac).
    pub scale: f64,
    pub proxy: EventLoopProxy<AppCommand>,
}

/// Deliveries waiting for the main thread to drain them.
pub type Pending = Arc<Mutex<VecDeque<Delivery>>>;

/// An empty delivery queue, shared between a generator's threads and the main one.
pub fn new_pending() -> Pending {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Push a delivery onto the shared queue and wake the main thread **only if the queue was
/// previously empty**. A burst of N completions produces 1–2 user events, not N — so winit's
/// window-event flow (keyboard, redraw) doesn't get starved.
pub fn push_delivery(
    pending: &Pending,
    delivery: Delivery,
    proxy: &EventLoopProxy<AppCommand>,
    wake: fn() -> AppCommand,
) {
    let was_empty = match pending.lock() {
        Ok(mut q) => {
            let empty = q.is_empty();
            q.push_back(delivery);
            empty
        }
        Err(_) => return,
    };
    if was_empty {
        let _ = proxy.send_event(wake());
    }
}

/// Drain everything queued. Called from the main-thread handler for the generator's wake command.
pub fn drain(pending: &Pending) -> Vec<Delivery> {
    match pending.lock() {
        Ok(mut q) => q.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}
