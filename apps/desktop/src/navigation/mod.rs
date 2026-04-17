//! Image navigation: directory scanning + background preloading + LRU cache.

pub mod directory;
pub mod preloader;

use crate::diagnostics::NavigationRecord;
use std::collections::VecDeque;

/// Per-feature runtime state owned by `App`.
pub struct State {
    pub dir_list: Option<directory::DirectoryList>,
    pub preloader: Option<preloader::Preloader>,
    pub image_cache: preloader::ImageCache,
    /// Recent navigation records for performance diagnostics (newest last, cap 10).
    pub history: VecDeque<NavigationRecord>,
    /// Current image dimensions — stored so resize can update the view without
    /// needing to hit the cache.
    pub current_image_size: Option<(u32, u32)>,
}

impl State {
    pub fn new() -> Self {
        Self {
            dir_list: None,
            preloader: None,
            image_cache: preloader::ImageCache::new(),
            history: VecDeque::with_capacity(10),
            current_image_size: None,
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
