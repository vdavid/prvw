//! What the browse-mode status bar says.
//!
//! A status bar is a Windows idiom that ACDSee had and macOS has no home for, so it is
//! deliberately Windows-only (`docs/specs/windows-ui-design.md` → "The browse-mode status bar").
//! It also gives the browser somewhere to say "Loading…" that isn't an overlay on top of the
//! tree, which is what macOS has to do.
//!
//! Three panes: how many images the selected folder holds, which one is selected, and how big it
//! is. All of it is a pure function of state, so the copy is asserted from a Mac rather than read
//! off a screenshot.

use std::path::Path;

/// What the status bar is being asked to describe.
#[derive(Debug, Clone, Copy, Default)]
pub struct Status<'a> {
    /// How many images the selected folder holds.
    pub image_count: usize,
    /// The selected image, if the grid has one.
    pub selected: Option<&'a Path>,
    /// The selected image's pixel size, once it has been measured. `None` while the measurement
    /// is in flight, and for a file whose header we can't read.
    pub dimensions: Option<(u32, u32)>,
    /// True while a folder listing or a tree scan the user is waiting on hasn't come back.
    pub loading: bool,
}

/// The three panes' text, left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// How many images are here, or that we're still finding out.
    pub count: String,
    /// The selected file's name. Empty when nothing is selected.
    pub name: String,
    /// The selected file's pixel size. Empty until it's known.
    pub size: String,
}

/// Fill the three panes.
///
/// "Loading…" replaces the count rather than joining it, because a half-counted folder is worse
/// than no count: the number would climb while the user reads it.
#[must_use]
pub fn fields(status: Status<'_>) -> Fields {
    Fields {
        count: if status.loading {
            "Loading…".to_string()
        } else {
            image_count(status.image_count)
        },
        // Split under Windows' rules rather than the host's: `Path::file_name` would hand back
        // the whole of `C:\pics\a.jpg` on a Mac, and this is asserted from a Mac.
        name: status
            .selected
            .and_then(|path| crate::paths::PathPolicy::windows().file_name(path))
            .unwrap_or_default()
            .to_string(),
        size: match status.dimensions {
            // A multiplication sign rather than an x: it's what a camera spec sheet uses, and
            // `docs/style-guide.md` asks for the real character wherever there is one.
            Some((width, height)) => format!("{} × {}", thousands(width), thousands(height)),
            None => String::new(),
        },
    }
}

/// "No images", "1 image", "1,234 images".
fn image_count(count: usize) -> String {
    match count {
        0 => "No images".to_string(),
        1 => "1 image".to_string(),
        many => format!("{} images", thousands(many)),
    }
}

/// A number with thousands separators, which `docs/style-guide.md` asks for. Written out rather
/// than pulled from a crate: one loop against one dependency, on a string that is at most ten
/// digits long.
fn thousands<T: std::fmt::Display>(value: T) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_folder_says_how_many_images_it_holds() {
        let count = |n| {
            fields(Status {
                image_count: n,
                ..Status::default()
            })
            .count
        };
        assert_eq!(count(0), "No images");
        assert_eq!(count(1), "1 image");
        assert_eq!(count(2), "2 images");
        assert_eq!(count(999), "999 images");
        // Thousands separators, per the style guide. A photo library really does reach these.
        assert_eq!(count(1_000), "1,000 images");
        assert_eq!(count(12_345), "12,345 images");
        assert_eq!(count(1_234_567), "1,234,567 images");
    }

    /// A count that climbs while the user reads it is worse than no count, so the scan says so
    /// instead of guessing.
    #[test]
    fn a_folder_still_being_read_says_so_instead_of_counting() {
        let f = fields(Status {
            image_count: 3,
            loading: true,
            ..Status::default()
        });
        assert_eq!(f.count, "Loading…");
    }

    #[test]
    fn the_selection_pane_is_the_file_name_alone() {
        let f = fields(Status {
            image_count: 4,
            selected: Some(Path::new(r"C:\Users\dave\Pictures\Trip\DSC_0042.NEF")),
            dimensions: Some((6048, 4024)),
            loading: false,
        });
        assert_eq!(f.count, "4 images");
        assert_eq!(f.name, "DSC_0042.NEF");
        assert_eq!(f.size, "6,048 × 4,024");
    }

    /// The measurement runs on a worker, so every pane has to read sensibly before it lands —
    /// and for a file whose header we can't read, it never will.
    #[test]
    fn an_unmeasured_selection_leaves_the_size_pane_empty() {
        let f = fields(Status {
            image_count: 4,
            selected: Some(Path::new(r"C:\pics\a.jpg")),
            dimensions: None,
            loading: false,
        });
        assert_eq!(f.name, "a.jpg");
        assert_eq!(f.size, "");
    }

    #[test]
    fn an_empty_folder_leaves_the_selection_panes_empty() {
        let f = fields(Status::default());
        assert_eq!(f.count, "No images");
        assert_eq!(f.name, "");
        assert_eq!(f.size, "");
    }

    /// The status bar is user-visible copy, so it answers to `docs/style-guide.md` the way the
    /// settings dialog's and the About box's do.
    #[test]
    fn the_copy_follows_the_style_guide() {
        let samples = [
            fields(Status::default()),
            fields(Status {
                image_count: 1,
                ..Status::default()
            }),
            fields(Status {
                image_count: 4_200,
                selected: Some(Path::new(r"C:\pics\a.jpg")),
                dimensions: Some((4032, 3024)),
                loading: false,
            }),
            fields(Status {
                loading: true,
                ..Status::default()
            }),
        ];
        for sample in &samples {
            for line in [&sample.count, &sample.name, &sample.size] {
                assert!(!line.contains('\u{2014}'), "em dash in {line:?}");
                let lowercase = line.to_lowercase();
                for banned in ["just ", "simply ", "simple ", "easy "] {
                    assert!(!lowercase.contains(banned), "{banned:?} in {line:?}");
                }
                // Sentence case: nothing after the first word starts with a capital.
                for word in line.split_whitespace().skip(1) {
                    assert!(
                        !word.chars().next().is_some_and(char::is_uppercase),
                        "{word:?} is capitalised mid-label in {line:?}"
                    );
                }
            }
        }
    }
}
