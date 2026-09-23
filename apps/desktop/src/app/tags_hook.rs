//! Glue between `App` and `crate::tags`: which image a tag applies to, the toggle, and the
//! one call that brings the dots, the Tags menu, and `/state` back in line with the disk.
//!
//! Lives here (not in `crate::tags`) because it touches `App` fields, the same split as
//! `previews_hook.rs`.

use std::path::{Path, PathBuf};

use super::App;
use crate::tags::{self, Tag, TagColor};

impl App {
    /// The image a tag applies to right now: the one image mode is showing. `None` in browse
    /// mode (the grid's selection is a different image), while the empty state is up, and before
    /// anything opened.
    pub(crate) fn tag_target(&self) -> Option<PathBuf> {
        if self.browser.is_browse() || self.empty_state.is_some() {
            return None;
        }
        let dir = self.navigation.dir_list.as_ref()?;
        Some(dir.current().to_path_buf())
    }

    /// The tags of the image on screen, as last read. `None` when there's no image to tag, or
    /// its tags haven't been read yet (it hasn't painted).
    pub(crate) fn current_tags(&self) -> Option<&[Tag]> {
        if self.browser.is_browse() || self.empty_state.is_some() {
            return None;
        }
        let dir = self.navigation.dir_list.as_ref()?;
        self.tags.get(dir.current())
    }

    /// Read the tags of the image that just painted, unless they're cached. Called from
    /// `finalize_display`, which every painted image passes through, placeholders included.
    /// A fresh read publishes itself, since the path that painted the image may already have
    /// written its state snapshot.
    pub(super) fn load_current_tags(&mut self) {
        if let Some(path) = self.tag_target()
            && self.tags.ensure(&path)
        {
            self.update_shared_state();
        }
    }

    /// Bring everything that shows `path`'s tags up to date: the dots, the Tags menu's
    /// checkmarks, and `/state`. The one call to make whenever a file's tags may have changed,
    /// whoever changed them.
    pub(crate) fn refresh_tags(&mut self, path: &Path) {
        self.note_tags_changed(path);
        self.update_shared_state();
    }

    /// [`Self::refresh_tags`] without publishing the state snapshot, for a caller handling a
    /// batch of files that publishes once at the end. The image on screen is re-read right away
    /// (one attribute call) and redrawn; any other file's cached tags are dropped, so they're read
    /// when it next comes on screen instead of now, on the main thread, for a file nobody is
    /// looking at (a Finder tag spree on a share would otherwise cost a round trip per file).
    pub(super) fn note_tags_changed(&mut self, path: &Path) {
        let on_screen = self
            .tag_target()
            .is_some_and(|target| crate::paths::same_path(&target, path));
        if on_screen {
            self.tags.refresh(path);
            self.request_redraw();
        } else {
            self.tags.forget(path);
        }
    }

    /// Toggle `color` on the image on screen. A write that doesn't land is logged and changes
    /// nothing else: the re-read after it shows what the file really carries.
    pub(super) fn toggle_tag(&mut self, color: TagColor) {
        let Some(path) = self.tag_target() else {
            log::debug!("No image on screen to tag {}", color.name());
            return;
        };
        match tags::toggle_color(&path, color) {
            Ok(written) => log::info!(
                "Tags of {} are now {:?}",
                path.display(),
                written
                    .iter()
                    .map(|tag| tag.name.as_str())
                    .collect::<Vec<_>>()
            ),
            Err(error) => log::warn!(
                "Couldn't toggle the {} tag on {}: {error}",
                color.name(),
                path.display()
            ),
        }
        self.refresh_tags(&path);
    }
}
