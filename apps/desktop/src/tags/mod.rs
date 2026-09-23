//! Finder color tags on the image being viewed: toggled by the bare digit keys and the Tags
//! menu, drawn as dots in the window's bottom-left corner.
//!
//! macOS stores tags per file in an extended attribute that Finder and Spotlight read, so a tag
//! set here shows up in Finder's sidebar and its tag search. [`finder`] owns that attribute.
//! Everywhere else there's no such thing: [`read_tags`] answers "no tags" and [`toggle_color`]
//! does nothing, and the parity registries call the feature `NotApplicable` there.
//!
//! [`State`] caches what the file on disk says, per path. The disk is the source of truth: a
//! write is always followed by a re-read ([`State::refresh`]), so the dots show what Finder will
//! show even when the write didn't land.

#[cfg(target_os = "macos")]
mod finder;
pub mod overlay;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use crate::paths::PathPolicy;

/// One of Finder's seven tag colors, in the order Finder's own menus list them. That order is
/// also the digit keys': `1` is red, `7` is gray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl TagColor {
    /// Every color, in Finder's menu order.
    pub const ALL: [TagColor; 7] = [
        TagColor::Red,
        TagColor::Orange,
        TagColor::Yellow,
        TagColor::Green,
        TagColor::Blue,
        TagColor::Purple,
        TagColor::Gray,
    ];

    /// The digit key that toggles this color: its 1-based position in [`TagColor::ALL`].
    pub const fn digit(self) -> char {
        match self {
            TagColor::Red => '1',
            TagColor::Orange => '2',
            TagColor::Yellow => '3',
            TagColor::Green => '4',
            TagColor::Blue => '5',
            TagColor::Purple => '6',
            TagColor::Gray => '7',
        }
    }

    /// The color a digit key toggles, for `"1"` to `"7"`.
    pub fn from_digit(key: &str) -> Option<TagColor> {
        TagColor::ALL
            .into_iter()
            .find(|color| key.len() == 1 && key.starts_with(color.digit()))
    }

    /// The name Finder gives its built-in tag of this color. A tag written under this name is
    /// indistinguishable from one Finder wrote, so it lands in Finder's own sidebar entry.
    pub const fn name(self) -> &'static str {
        match self {
            TagColor::Red => "Red",
            TagColor::Orange => "Orange",
            TagColor::Yellow => "Yellow",
            TagColor::Green => "Green",
            TagColor::Blue => "Blue",
            TagColor::Purple => "Purple",
            TagColor::Gray => "Gray",
        }
    }
}

/// One Finder tag. A custom tag can carry a color too, and a colorless one carries `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub color: Option<TagColor>,
}

/// Whether any of `tags` carries `color`. A custom tag of that color counts, the way it does in
/// Finder: toggling red on a file tagged "Important" (red) removes it rather than adding a
/// second red.
///
/// Its readers are the macOS write path and the Tags menu, so Linux, with neither, never calls
/// it outside the tests.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn has_color(tags: &[Tag], color: TagColor) -> bool {
    tags.iter().any(|tag| tag.color == Some(color))
}

/// A file's tags, straight from disk. A file with no tag attribute has no tags.
///
/// Errors are real ones: a read the filesystem refused, or an attribute that isn't a tag list
/// Finder would recognize. The caller decides whether "no tags" is a safe reading of that.
#[cfg(target_os = "macos")]
pub fn read_tags(path: &Path) -> std::io::Result<Vec<Tag>> {
    finder::read_tags(path)
}

/// No Finder tags off macOS, so every file has none.
#[cfg(not(target_os = "macos"))]
pub fn read_tags(_path: &Path) -> std::io::Result<Vec<Tag>> {
    Ok(Vec::new())
}

/// Toggle `color` on `path`: remove every tag of that color if it has one, add Finder's built-in
/// tag of that color otherwise. Every other tag is left alone. Returns the tags written.
#[cfg(target_os = "macos")]
pub fn toggle_color(path: &Path, color: TagColor) -> std::io::Result<Vec<Tag>> {
    finder::toggle_color(path, color)
}

/// No Finder tags off macOS, so there's nothing to toggle.
#[cfg(not(target_os = "macos"))]
pub fn toggle_color(_path: &Path, _color: TagColor) -> std::io::Result<Vec<Tag>> {
    Ok(Vec::new())
}

/// The tags of every image that's been on screen, as last read from disk.
///
/// Keyed through [`PathPolicy::key`], because a byte-keyed map treats two spellings of one file
/// as two files (see `paths.rs`). Entries are a few bytes each and there's one per image viewed,
/// so nothing evicts them.
#[derive(Default)]
pub struct State {
    by_path: HashMap<OsString, Vec<Tag>>,
}

impl State {
    /// The cached tags for `path`, or `None` when it hasn't been read yet.
    pub fn get(&self, path: &Path) -> Option<&[Tag]> {
        self.by_path
            .get(&PathPolicy::HOST.key(path))
            .map(Vec::as_slice)
    }

    /// Read `path`'s tags unless they're cached already. One `getxattr` the first time an image
    /// comes on screen, nothing after. Returns whether it read.
    pub fn ensure(&mut self, path: &Path) -> bool {
        if self.get(path).is_some() {
            return false;
        }
        self.refresh(path);
        true
    }

    /// Re-read `path`'s tags from disk, replacing whatever was cached. The one call to make when
    /// the tags may have changed underneath us: after our own write, and whenever something else
    /// may have touched the file.
    ///
    /// A read that fails caches no tags and logs why, so the dots never claim a tag the file may
    /// not carry.
    pub fn refresh(&mut self, path: &Path) {
        let tags = read_tags(path).unwrap_or_else(|error| {
            log::warn!("Couldn't read the tags of {}: {error}", path.display());
            Vec::new()
        });
        self.by_path.insert(PathPolicy::HOST.key(path), tags);
    }

    /// Drop what's cached for `path`, so the next [`State::ensure`] reads it again. For a file
    /// that isn't on screen: re-reading it now would be an attribute call nobody is waiting on.
    pub fn forget(&mut self, path: &Path) {
        self.by_path.remove(&PathPolicy::HOST.key(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digit keys follow Finder's menu order, which is the order the Tags menu lists them.
    #[test]
    fn digits_follow_finders_menu_order() {
        let by_digit: Vec<(char, &str)> = TagColor::ALL
            .iter()
            .map(|color| (color.digit(), color.name()))
            .collect();
        assert_eq!(
            by_digit,
            vec![
                ('1', "Red"),
                ('2', "Orange"),
                ('3', "Yellow"),
                ('4', "Green"),
                ('5', "Blue"),
                ('6', "Purple"),
                ('7', "Gray"),
            ]
        );
    }

    #[test]
    fn from_digit_round_trips_and_rejects_everything_else() {
        for color in TagColor::ALL {
            assert_eq!(
                TagColor::from_digit(&color.digit().to_string()),
                Some(color)
            );
        }
        for key in ["0", "8", "9", "11", "", "a", "!"] {
            assert_eq!(TagColor::from_digit(key), None, "{key:?}");
        }
    }

    #[test]
    fn a_custom_tag_of_a_color_counts_as_that_color() {
        let tags = vec![Tag {
            name: "Important".to_string(),
            color: Some(TagColor::Red),
        }];
        assert!(has_color(&tags, TagColor::Red));
        assert!(!has_color(&tags, TagColor::Blue));
    }
}
