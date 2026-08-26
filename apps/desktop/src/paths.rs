//! What "the same path" means, per platform.
//!
//! `Path`'s own `==` and `starts_with` compare components byte for byte. That is exactly right on
//! macOS and Linux, where every path this app compares has come out of `canonicalize` or an OS
//! enumeration and therefore carries the on-disk spelling. On Windows it is wrong twice over, and
//! neither failure is a compile error, so the whole rule lives here as data plus pure functions
//! that every platform's behaviour can be asserted from any host.
//!
//! ## The two ways Windows disagrees with a byte comparison
//!
//! - **Verbatim prefixes.** `Path::canonicalize` returns `\\?\C:\Users\dave\pics`, so a
//!   `starts_with` against a `C:\` root is false and a `PathBuf::eq` against the argv spelling is
//!   false. A network share canonicalizes to `\\?\UNC\naspi\photos\...`, which likewise fails
//!   against a `\\naspi\photos` root. That one matters here: the photo libraries this viewer is
//!   built for live on a NAS.
//! - **Case.** NTFS is case-insensitive. argv carries whatever the user typed, `canonicalize`
//!   returns the on-disk casing, and `GetLogicalDrives` hands back an uppercase drive letter, so
//!   three spellings of one directory can all be in play at once.
//!
//! ## What this module deliberately does NOT do
//!
//! **It never strips a verbatim prefix from a path on its way to the filesystem.** That prefix is
//! what lifts the 260-character `MAX_PATH` limit, so removing it globally would stop deep photo
//! libraries opening: the exact users who need Prvw most. Normalizing happens only inside a
//! comparison, on a throwaway view of the string, and at the display boundary. The `PathBuf` the
//! app carries around and hands to `File::open` keeps its prefix.
//!
//! The **third** boundary, not yet built: a path handed to a Win32 shell API
//! (`IShellItemImageFactory` in M3, `CF_HDROP` in M5) has to be de-verbatimed, because those
//! reject `\\?\` outright, but **only when the result is still a legal Win32 path**. That means:
//! the prefix is `\\?\<drive>:`, no component is a reserved device name (`CON`, `NUL`, `COM1`, …)
//! or ends in a dot or a space, and the whole path is at most 260 characters. When any of that
//! fails, the shell call has to be given up on rather than fed a path it will mangle. Whoever
//! builds the first such call site owns writing that; [`PathPolicy::display`] is not it, because
//! display always strips (a person reading an error message is never helped by `\\?\`).
//!
//! ## Non-UTF-8 names
//!
//! De-verbatiming and case folding both need text, so a path that isn't valid UTF-8 falls back to
//! `Path`'s byte-wise comparison. Only Linux allows such names, and Linux is the platform whose
//! policy is byte-wise anyway, so the fallback is exact there rather than approximate.

use std::borrow::Cow;
use std::path::Path;

/// The prefix `Path::canonicalize` returns on Windows, and the one that lifts `MAX_PATH`.
const VERBATIM: &str = r"\\?\";

/// A share in verbatim spelling: `\\?\UNC\naspi\photos` is `\\naspi\photos`. Stripping stops
/// before the separator so what's left still reads as rooted, which is all a comparison needs.
/// Windows spells the marker exactly `UNC`, and so does `canonicalize`.
const VERBATIM_UNC: &str = r"\\?\UNC";

/// How one platform decides whether two paths name the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPolicy {
    /// Whether `A.JPG` and `a.jpg` name two different files.
    case_sensitive: bool,
    /// Whether `\` separates components and `\\?\` prefixes exist.
    windows_syntax: bool,
}

impl PathPolicy {
    /// macOS. Case-sensitive on purpose even though the default APFS volume isn't: every path
    /// compared here arrives from `canonicalize` or an AppKit enumeration, both of which return
    /// the on-disk spelling, so folding could only add false positives on the case-sensitive
    /// volumes some developers format.
    pub const fn macos() -> Self {
        Self {
            case_sensitive: true,
            windows_syntax: false,
        }
    }

    /// Linux: case-sensitive filesystems, one separator, no verbatim prefixes. Identical to
    /// macOS today, and named separately so a future difference has somewhere to land and so a
    /// test can say which platform it means.
    pub const fn linux() -> Self {
        Self::macos()
    }

    /// Windows: case-insensitive like NTFS, and aware of `\\?\` and both separators.
    pub const fn windows() -> Self {
        Self {
            case_sensitive: false,
            windows_syntax: true,
        }
    }

    /// The policy for the platform this binary runs on. Names all three on purpose, the way
    /// `parity::Platform::HOST` does: every build then compiles every policy, which is what lets
    /// the tests below assert Windows behaviour from a Mac.
    pub const HOST: Self = if cfg!(target_os = "macos") {
        Self::macos()
    } else if cfg!(target_os = "windows") {
        Self::windows()
    } else {
        Self::linux()
    };

    /// True when `Path`'s own comparison already answers correctly: nothing to strip, no case to
    /// fold. Taking that route keeps macOS and Linux on the fast path and, on Linux, keeps names
    /// that aren't UTF-8 exact rather than approximate.
    const fn byte_wise(self) -> bool {
        self.case_sensitive && !self.windows_syntax
    }

    /// Do these two paths name the same file?
    pub fn same_path(self, a: &Path, b: &Path) -> bool {
        if self.byte_wise() {
            return a == b;
        }
        let (Some(left), Some(right)) = (a.to_str(), b.to_str()) else {
            return a == b;
        };
        if self.rooted(left) != self.rooted(right) {
            return false;
        }
        let mut walked = self.components(left);
        let mut against = self.components(right);
        loop {
            match (walked.next(), against.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if self.same_component(x, y) => {}
                _ => return false,
            }
        }
    }

    /// Is `root` an ancestor of `path`, or `path` itself? Component-wise, so `/photo` is not a
    /// prefix of `/photos/a.jpg`.
    pub fn starts_with(self, path: &Path, root: &Path) -> bool {
        if self.byte_wise() {
            return path.starts_with(root);
        }
        let (Some(under), Some(over)) = (path.to_str(), root.to_str()) else {
            return path.starts_with(root);
        };
        if self.rooted(under) != self.rooted(over) {
            return false;
        }
        let mut walked = self.components(under);
        self.components(over).all(|component| {
            walked
                .next()
                .is_some_and(|c| self.same_component(c, component))
        })
    }

    /// Do these two paths end in the same file name? Cheaper than [`same_path`](Self::same_path)
    /// and the right question when both paths are already known to sit in one directory.
    ///
    /// Splits under this policy's syntax rather than the host's, so a Windows path answers the
    /// same from any machine. A path with no name of its own (a root, or one ending in `..`)
    /// matches only another such path.
    pub fn same_file_name(self, a: &Path, b: &Path) -> bool {
        if self.byte_wise() {
            return a.file_name() == b.file_name();
        }
        let (Some(left), Some(right)) = (a.to_str(), b.to_str()) else {
            return a.file_name() == b.file_name();
        };
        match (self.file_name(left), self.file_name(right)) {
            (Some(x), Some(y)) => self.same_component(x, y),
            (None, None) => true,
            _ => false,
        }
    }

    /// The path as a person should read it, with any `\\?\` taken off. Display only: see the
    /// module docs for why the app's own `PathBuf`s keep the prefix.
    pub fn display(self, path: &Path) -> Cow<'_, str> {
        let text = path.to_string_lossy();
        if !self.windows_syntax {
            return text;
        }
        // A share loses the `UNC` marker but keeps the two separators that name it, which is the
        // one case the stripped tail can't be borrowed as-is.
        if let Some(share) = strip_verbatim_unc(&text) {
            return Cow::Owned(format!(
                r"\\{share}",
                share = share.trim_start_matches('\\')
            ));
        }
        match text {
            Cow::Borrowed(s) => Cow::Borrowed(s.strip_prefix(VERBATIM).unwrap_or(s)),
            Cow::Owned(s) => match s.strip_prefix(VERBATIM) {
                Some(rest) => Cow::Owned(rest.to_string()),
                None => Cow::Owned(s),
            },
        }
    }

    /// The path with any verbatim prefix taken off, ready to split. The share form keeps its
    /// leading separator so it still reads as rooted; see [`VERBATIM_UNC`].
    fn body(self, path: &str) -> &str {
        if !self.windows_syntax {
            return path;
        }
        if let Some(share) = strip_verbatim_unc(path) {
            return share;
        }
        path.strip_prefix(VERBATIM).unwrap_or(path)
    }

    /// Whether the path starts at a filesystem root. Only the presence of a leading separator
    /// counts, never how many: `\\naspi\photos` and the `\naspi\photos` a stripped
    /// `\\?\UNC\naspi\photos` leaves behind are the same share.
    fn rooted(self, path: &str) -> bool {
        let body = self.body(path);
        body.starts_with('/') || (self.windows_syntax && body.starts_with('\\'))
    }

    /// The path's components, with separator runs and `.` dropped the way `Path::components`
    /// drops them.
    fn components(self, path: &str) -> impl DoubleEndedIterator<Item = &str> {
        let windows_syntax = self.windows_syntax;
        self.body(path)
            .split(move |c| c == '/' || (windows_syntax && c == '\\'))
            .filter(|part| !part.is_empty() && *part != ".")
    }

    fn same_component(self, a: &str, b: &str) -> bool {
        if self.case_sensitive {
            return a == b;
        }
        // Folded lazily, char by char, so a 5,000-file folder doesn't allocate per comparison.
        a.chars()
            .flat_map(char::to_lowercase)
            .eq(b.chars().flat_map(char::to_lowercase))
    }

    /// The last component, or `None` when the path names no file of its own.
    fn file_name(self, path: &str) -> Option<&str> {
        self.components(path)
            .next_back()
            .filter(|last| *last != "..")
    }
}

/// The tail of a `\\?\UNC\server\share\…` path, keeping the separator that precedes the server
/// name. `None` for anything else, including a folder that merely starts with those letters.
fn strip_verbatim_unc(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(VERBATIM_UNC)?;
    rest.starts_with('\\').then_some(rest)
}

/// Do these two paths name the same file, by the host platform's rules?
pub fn same_path(a: &Path, b: &Path) -> bool {
    PathPolicy::HOST.same_path(a, b)
}

/// Is `root` an ancestor of `path`, or `path` itself, by the host platform's rules?
pub fn starts_with(path: &Path, root: &Path) -> bool {
    PathPolicy::HOST.starts_with(path, root)
}

/// Do these two paths end in the same file name, by the host platform's rules?
pub fn same_file_name(a: &Path, b: &Path) -> bool {
    PathPolicy::HOST.same_file_name(a, b)
}

/// A path as a person should read it. Use this wherever a path becomes text a user sees.
pub fn for_display(path: &Path) -> Cow<'_, str> {
    PathPolicy::HOST.display(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every platform's policy, so a test can say what each one does from whichever host runs it.
    fn every_platform() -> [(&'static str, PathPolicy); 3] {
        [
            ("macOS", PathPolicy::macos()),
            ("Windows", PathPolicy::windows()),
            ("Linux", PathPolicy::linux()),
        ]
    }

    // ── Verbatim prefixes ────────────────────────────────────────────────────────────────────

    /// The launch path: `canonicalize` hands back `\\?\C:\...` and everything else in the app
    /// carries the plain spelling. Byte comparison says they're different files; NTFS says they
    /// are one file, and it's right.
    #[test]
    fn a_verbatim_path_is_the_plain_path_on_windows() {
        let windows = PathPolicy::windows();
        assert!(windows.same_path(
            Path::new(r"\\?\C:\Users\dave\pics\a.jpg"),
            Path::new(r"C:\Users\dave\pics\a.jpg"),
        ));
        assert!(windows.starts_with(
            Path::new(r"\\?\C:\Users\dave\pics\a.jpg"),
            Path::new(r"C:\Users\dave"),
        ));
        // And the other way round: a plain path under a verbatim root.
        assert!(windows.starts_with(
            Path::new(r"C:\Users\dave\pics\a.jpg"),
            Path::new(r"\\?\C:\Users\dave"),
        ));
    }

    /// Off Windows, `\\?\` is not a prefix, it's a file name. A Unix file really can be called
    /// that, and treating it as a spelling of something else would be a bug.
    #[test]
    fn a_verbatim_prefix_means_nothing_off_windows() {
        for policy in [PathPolicy::macos(), PathPolicy::linux()] {
            assert!(!policy.same_path(Path::new(r"\\?\C:\a.jpg"), Path::new(r"C:\a.jpg")));
        }
    }

    /// The NAS case. `canonicalize` on a share returns `\\?\UNC\server\share\...`, so a
    /// `starts_with` against the `\\server\share` root that browse mode holds fails byte-wise.
    #[test]
    fn a_verbatim_share_is_the_share_it_names() {
        let windows = PathPolicy::windows();
        assert!(windows.same_path(
            Path::new(r"\\?\UNC\naspi\photos\2026\a.jpg"),
            Path::new(r"\\naspi\photos\2026\a.jpg"),
        ));
        assert!(windows.starts_with(
            Path::new(r"\\?\UNC\naspi\photos\2026\a.jpg"),
            Path::new(r"\\naspi\photos"),
        ));
    }

    /// `\\?\UNC` is stripped only when it's the whole component. A verbatim path into a drive
    /// whose first folder happens to start with those letters is left alone.
    #[test]
    fn a_folder_named_uncle_is_not_a_share() {
        let windows = PathPolicy::windows();
        assert!(!windows.same_path(
            Path::new(r"\\?\UNCLE\photos\a.jpg"),
            Path::new(r"\\UNCLE\photos\a.jpg"),
        ));
    }

    // ── Case ─────────────────────────────────────────────────────────────────────────────────

    /// argv carries what the user typed, `canonicalize` returns what's on disk, and
    /// `GetLogicalDrives` uppercases the drive letter. All three name one directory on NTFS.
    #[test]
    fn case_is_ignored_only_on_windows() {
        let windows = PathPolicy::windows();
        assert!(windows.same_path(
            Path::new(r"c:\users\dave\A.JPG"),
            Path::new(r"C:\Users\Dave\a.jpg")
        ));
        assert!(windows.starts_with(
            Path::new(r"C:\Users\Dave\pics"),
            Path::new(r"c:\users\dave")
        ));
        assert!(windows.same_file_name(Path::new(r"C:\x\A.JPG"), Path::new(r"D:\y\a.jpg")));

        for policy in [PathPolicy::macos(), PathPolicy::linux()] {
            assert!(!policy.same_path(
                Path::new("/users/dave/A.JPG"),
                Path::new("/users/dave/a.jpg")
            ));
            assert!(!policy.starts_with(Path::new("/Users/Dave/pics"), Path::new("/users/dave")));
        }
    }

    /// Folding is Unicode-wide, not ASCII-only: NTFS's own uppercase table is, and the photos
    /// this viewer opens have names in more than one language.
    #[test]
    fn case_folding_reaches_past_ascii_on_windows() {
        let windows = PathPolicy::windows();
        assert!(windows.same_path(
            Path::new(r"C:\Bilder\ÅKE.jpg"),
            Path::new(r"C:\bilder\åke.jpg")
        ));
        assert!(windows.same_file_name(Path::new(r"C:\a\ÁGNES.jpg"), Path::new(r"C:\a\ágnes.jpg")));
    }

    // ── Separators and boundaries ────────────────────────────────────────────────────────────

    /// Windows takes both separators, so the two spellings of one path have to compare equal.
    #[test]
    fn both_separators_split_components_on_windows() {
        let windows = PathPolicy::windows();
        assert!(windows.same_path(Path::new(r"C:\Users\dave"), Path::new("C:/Users/dave")));
        assert!(windows.starts_with(Path::new("C:/Users/dave/pics"), Path::new(r"C:\Users\dave")));
    }

    /// The reason this isn't a string prefix test on any platform.
    #[test]
    fn a_prefix_has_to_land_on_a_component_boundary() {
        for (platform, policy) in every_platform() {
            assert!(
                !policy.starts_with(Path::new("/photos/a.jpg"), Path::new("/photo")),
                "{platform}: /photo doesn't contain /photos"
            );
            assert!(
                policy.starts_with(Path::new("/photos/a.jpg"), Path::new("/photos")),
                "{platform}: but /photos does"
            );
        }
    }

    /// A drive root contains everything on its drive, and nothing on another one.
    #[test]
    fn a_drive_root_contains_its_own_drive_only() {
        let windows = PathPolicy::windows();
        assert!(windows.starts_with(Path::new(r"C:\Users\dave"), Path::new(r"C:\")));
        assert!(!windows.starts_with(Path::new(r"D:\Photos"), Path::new(r"C:\")));
        assert!(windows.starts_with(Path::new(r"\\?\D:\Photos\a.jpg"), Path::new(r"D:\")));
    }

    /// Doubled separators and `.` are noise; a path is the same path with or without them. What
    /// isn't noise is the leading separator: a relative path is not the absolute one.
    #[test]
    fn noise_is_ignored_but_a_relative_path_is_not_the_absolute_one() {
        for (platform, policy) in every_platform() {
            assert!(
                policy.same_path(Path::new("/photos//./a.jpg"), Path::new("/photos/a.jpg")),
                "{platform}: repeated separators and `.` don't change a path"
            );
            assert!(
                !policy.same_path(Path::new("photos/a.jpg"), Path::new("/photos/a.jpg")),
                "{platform}: a relative path isn't the absolute one"
            );
            assert!(
                !policy.starts_with(Path::new("photos/a.jpg"), Path::new("/photos")),
                "{platform}: and isn't under it either"
            );
        }
    }

    // ── File names ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn same_file_name_compares_the_last_component_only() {
        for (platform, policy) in every_platform() {
            assert!(
                policy.same_file_name(Path::new("/a/b/x.jpg"), Path::new("/c/d/x.jpg")),
                "{platform}"
            );
            assert!(
                !policy.same_file_name(Path::new("/a/x.jpg"), Path::new("/a/y.jpg")),
                "{platform}"
            );
            // A path with no file name matches nothing but another one.
            assert!(
                !policy.same_file_name(Path::new("/"), Path::new("/a/x.jpg")),
                "{platform}"
            );
            assert!(
                policy.same_file_name(Path::new("/"), Path::new("/")),
                "{platform}"
            );
        }
    }

    /// The trap this module exists to close, in miniature: `Path::file_name` splits with the
    /// HOST's separators, so on a Mac it reads a whole Windows path as one name and a test of the
    /// Windows policy passes for the wrong reason. Splitting under the policy answers the same
    /// from any machine.
    #[test]
    fn a_windows_file_name_splits_the_same_from_any_host() {
        let windows = PathPolicy::windows();
        assert!(windows.same_file_name(Path::new(r"C:\pics\a.jpg"), Path::new(r"D:\other\A.JPG")));
        assert!(!windows.same_file_name(Path::new(r"C:\pics\a.jpg"), Path::new(r"C:\pics\b.jpg")));
        assert!(!windows.same_file_name(Path::new(r"C:\"), Path::new(r"C:\pics\a.jpg")));
        assert!(
            windows.same_file_name(Path::new(r"\\?\C:\pics\a.jpg"), Path::new(r"C:\pics\a.jpg"))
        );
    }

    // ── Display ──────────────────────────────────────────────────────────────────────────────

    /// `\\?\` in a title bar or an error message is noise a person can't act on, and unlike the
    /// shell boundary this strips unconditionally: a 300-character path is still more readable
    /// without it.
    #[test]
    fn display_takes_the_verbatim_prefix_off_on_windows() {
        let windows = PathPolicy::windows();
        assert_eq!(
            windows.display(Path::new(r"\\?\C:\pics\a.jpg")),
            r"C:\pics\a.jpg"
        );
        assert_eq!(
            windows.display(Path::new(r"\\?\UNC\naspi\photos\a.jpg")),
            r"\\naspi\photos\a.jpg"
        );
        let deep = format!(r"\\?\C:\{}\a.jpg", "d".repeat(300));
        assert!(
            !windows.display(Path::new(&deep)).starts_with(r"\\?\"),
            "a path past MAX_PATH still reads better without the prefix"
        );
    }

    #[test]
    fn display_leaves_an_ordinary_path_alone() {
        for (platform, policy) in every_platform() {
            assert_eq!(
                policy.display(Path::new("/photos/a.jpg")),
                "/photos/a.jpg",
                "{platform}"
            );
        }
        assert_eq!(
            PathPolicy::macos().display(Path::new(r"\\?\C:\pics\a.jpg")),
            r"\\?\C:\pics\a.jpg",
            "off Windows that's a file name, not a prefix"
        );
    }

    // ── The host ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_host_policy_matches_the_platform_this_is_built_for() {
        let expected = if cfg!(target_os = "windows") {
            PathPolicy::windows()
        } else {
            PathPolicy::macos()
        };
        assert_eq!(PathPolicy::HOST, expected);
        // The free functions are the host policy, so a call site never picks the wrong one.
        assert!(same_path(Path::new("/a/b"), Path::new("/a/b")));
        assert!(starts_with(Path::new("/a/b"), Path::new("/a")));
        assert!(same_file_name(Path::new("/a/b.jpg"), Path::new("/c/b.jpg")));
        assert_eq!(for_display(Path::new("/a/b.jpg")), "/a/b.jpg");
    }

    /// Linux is the only platform that allows a name that isn't UTF-8, and it's also the one
    /// whose policy is byte-wise, so the text fallback loses nothing there.
    #[test]
    #[cfg(unix)]
    fn a_name_that_isnt_utf8_still_compares() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;
        let odd = PathBuf::from(OsStr::from_bytes(b"/photos/\xff\xfe.jpg"));
        for (platform, policy) in every_platform() {
            assert!(policy.same_path(&odd, &odd.clone()), "{platform}");
            assert!(policy.starts_with(&odd, Path::new("/photos")), "{platform}");
            assert!(
                !policy.same_path(&odd, Path::new("/photos/a.jpg")),
                "{platform}"
            );
        }
    }
}
