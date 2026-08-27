//! What the Windows browse tree shows at its top level, and what each row is called.
//!
//! macOS has one home folder and a `/Volumes` directory to enumerate. Windows has neither: it has
//! drive letters and a set of known folders, and Explorer's navigation pane leads with the known
//! folders. So `tree_model::build_roots` (home first, then volumes) doesn't fit, and this module
//! is its Windows twin.
//!
//! Everything here is pure and compiles on every platform, so a Mac asserts what the Windows tree
//! will show. The Win32 half — `GetLogicalDrives`, `GetVolumeInformationW`,
//! `SHGetKnownFolderPath` — lives in [`super::shell_roots`] and does nothing but feed these
//! functions.

use std::path::{Path, PathBuf};

use crate::browser::tree_model::Root;
use crate::paths::PathPolicy;

/// The known folders the tree leads with, in the order they appear. Pictures first, because this
/// is a photo viewer and that is where photos are; Desktop and Downloads after it, because those
/// are the other two places a picture lands.
///
/// Labels are ours rather than the shell's localized display name: Prvw's UI is English
/// throughout, and a row called "Bilder" beside menus that say "Image browser" would read as a
/// bug rather than as localization.
pub const KNOWN_FOLDER_LABELS: [&str; 3] = ["Pictures", "Desktop", "Downloads"];

/// What the user's profile folder's row is called. macOS's source list calls the same row "Home"
/// ([`crate::browser::tree_model::build_roots`]), and the two trees agreeing on the word is worth
/// more here than matching Explorer, which shows the account name.
pub const HOME_LABEL: &str = "Home";

/// What `GetDriveTypeW` said a letter is. Carried as data rather than read from Win32 at the
/// point of use, so the labelling below is testable from any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// A hard disk or SSD. `DRIVE_FIXED`.
    Fixed,
    /// A USB stick, an SD card, a floppy. `DRIVE_REMOVABLE`.
    Removable,
    /// A mapped network drive. `DRIVE_REMOTE`.
    Remote,
    /// An optical drive. `DRIVE_CDROM`.
    CdRom,
    /// A RAM disk. `DRIVE_RAMDISK`.
    RamDisk,
    /// `DRIVE_UNKNOWN`, `DRIVE_NO_ROOT_DIR`, or a value Windows adds later.
    Unknown,
}

impl DriveKind {
    /// Read a `GetDriveTypeW` return value. The constants are from `winbase.h` and are stable
    /// back to Windows 2000; an unrecognised one is [`DriveKind::Unknown`] rather than a panic,
    /// because a drive we can't name is still a drive we can list.
    #[must_use]
    pub fn from_win32(value: u32) -> Self {
        match value {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Remote,
            5 => DriveKind::CdRom,
            6 => DriveKind::RamDisk,
            _ => DriveKind::Unknown,
        }
    }

    /// What to call a drive that has no volume label of its own.
    ///
    /// Sentence case per `docs/style-guide.md`, which is the one place this deliberately parts
    /// company with Explorer: Explorer writes "Local Disk (C:)".
    #[must_use]
    pub fn fallback_label(self) -> &'static str {
        match self {
            DriveKind::Fixed | DriveKind::Unknown => "Local disk",
            DriveKind::Removable => "Removable disk",
            DriveKind::Remote => "Network drive",
            DriveKind::CdRom => "CD drive",
            DriveKind::RamDisk => "RAM disk",
        }
    }
}

/// A drive row's label: the volume label when the drive has one, else the drive-type name, and
/// the letter in parentheses either way. `Photos (D:)`, `Local disk (C:)`.
///
/// A blank or whitespace-only volume label counts as none: an unformatted stick reports an empty
/// string, and a row called " (E:)" reads as a bug.
#[must_use]
pub fn drive_label(letter: char, volume_label: Option<&str>, kind: DriveKind) -> String {
    let name = volume_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| kind.fallback_label());
    format!("{name} ({letter}:)")
}

/// The drive letters a `GetLogicalDrives` bitmask names, A first. Bit 0 is A, bit 25 is Z;
/// anything above that isn't a drive letter and is ignored.
#[must_use]
pub fn drives_in_mask(mask: u32) -> Vec<char> {
    (0..26u32)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| char::from_u32('A' as u32 + bit))
        .collect()
}

/// Where a drive letter roots. Spelled `C:\`, never a bare `C:`: the bare form is relative to
/// that drive's current directory rather than to its root, so it would list the wrong folder.
#[must_use]
pub fn drive_root_path(letter: char) -> PathBuf {
    PathBuf::from(format!("{letter}:\\"))
}

/// Assemble the tree's top-level rows: the known folders in [`KNOWN_FOLDER_LABELS`] order, then
/// the user's profile folder, then every drive in letter order.
///
/// Home sits below the three photo folders (this is a photo viewer, and that is where photos are)
/// and above the drives, because it is what most of what anyone opens sits under. Its real work is
/// the reveal walk: [`crate::browser::tree_model::reveal_path_chain`] reveals under the
/// longest-matching root, so a folder in the profile expands two or three levels from Home rather
/// than six from `C:\` — and the walk lists each level it passes, one of which would otherwise be
/// the machine's temp directory.
///
/// Two rows naming the same folder are collapsed to the first, under Windows path rules — a
/// Desktop redirected onto a drive root, or two known folders redirected onto each other, would
/// otherwise appear twice and the tree would expand only one of them.
#[must_use]
pub fn build_windows_roots(
    known_folders: Vec<Root>,
    home: Option<Root>,
    drives: Vec<Root>,
) -> Vec<Root> {
    let policy = PathPolicy::windows();
    let mut roots: Vec<Root> = Vec::with_capacity(known_folders.len() + drives.len() + 1);
    for candidate in known_folders.into_iter().chain(home).chain(drives) {
        if roots
            .iter()
            .any(|existing| policy.same_path(&existing.path, &candidate.path))
        {
            continue;
        }
        roots.push(candidate);
    }
    roots
}

/// The user's profile folder, from `%USERPROFILE%`.
///
/// The environment variable rather than `SHGetKnownFolderPath(FOLDERID_Profile)`, for the reason
/// macOS reads `$HOME`: it is what the session says the profile is, and it is the one a launcher
/// or a test fixture can point somewhere else. The caller falls back to the shell when it's unset.
///
/// Only an absolute path counts — a relative one would name whatever the working directory happens
/// to be. Deliberately **not** checked against the disk: this runs on the event loop's thread, and
/// an enterprise roaming profile on a dead share would block it for the SMB timeout, which is the
/// hang the whole tree is built to avoid. A profile that isn't there simply expands to nothing.
#[must_use]
pub fn home_from_environment(value: Option<&std::ffi::OsStr>) -> Option<Root> {
    let path = PathBuf::from(value?);
    is_absolute_windows_path(&path).then(|| Root {
        name: HOME_LABEL.to_string(),
        path,
    })
}

/// Whether a Windows path names a place on its own rather than one relative to a current
/// directory: a drive with a separator (`C:\`), a share (`\\naspi\home`), or either in the
/// verbatim spelling. A bare `C:` is relative to that drive's current directory and doesn't count.
fn is_absolute_windows_path(path: &Path) -> bool {
    /// The prefix `canonicalize` returns, which `paths.rs` spells the same way.
    const VERBATIM: &str = r"\\?\";
    let Some(text) = path.to_str() else {
        return false;
    };
    // A verbatim path is absolute by definition; that is what the prefix means.
    if text.starts_with(VERBATIM) || text.starts_with(r"\\") || text.starts_with("//") {
        return true;
    }
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('\\' | '/'))
}

/// Whether a directory entry with these `FILE_ATTRIBUTE_*` flags stays out of the tree.
///
/// Hidden and system alike, unconditionally. We deliberately don't read Explorer's "show hidden
/// files" setting: a photo browser showing `AppData` and `System Volume Information` is noise,
/// and skipping both matches Explorer's own default. `FILE_ATTRIBUTE_HIDDEN` is `0x2` and
/// `FILE_ATTRIBUTE_SYSTEM` is `0x4`, from `winnt.h`.
#[must_use]
pub fn hidden_by_attributes(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
    attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str, path: &str) -> Root {
        Root {
            name: name.to_string(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn a_labelled_drive_reads_like_explorer_without_its_title_case() {
        assert_eq!(
            drive_label('D', Some("Photos"), DriveKind::Fixed),
            "Photos (D:)"
        );
        assert_eq!(drive_label('C', None, DriveKind::Fixed), "Local disk (C:)");
        assert_eq!(
            drive_label('E', None, DriveKind::Removable),
            "Removable disk (E:)"
        );
        assert_eq!(
            drive_label('Z', None, DriveKind::Remote),
            "Network drive (Z:)"
        );
        assert_eq!(drive_label('F', None, DriveKind::CdRom), "CD drive (F:)");
    }

    /// An unformatted stick reports an empty volume label rather than none at all, and a row
    /// called " (E:)" reads as a bug.
    #[test]
    fn a_blank_volume_label_falls_back_to_the_drive_type() {
        assert_eq!(
            drive_label('E', Some(""), DriveKind::Removable),
            "Removable disk (E:)"
        );
        assert_eq!(
            drive_label('E', Some("   "), DriveKind::Removable),
            "Removable disk (E:)"
        );
        // A label with padding keeps its own name, trimmed.
        assert_eq!(
            drive_label('E', Some("  Trip  "), DriveKind::Removable),
            "Trip (E:)"
        );
    }

    /// A drive type Windows adds after this was written must still produce a listable row.
    #[test]
    fn an_unrecognised_drive_type_is_still_a_drive() {
        assert_eq!(DriveKind::from_win32(0), DriveKind::Unknown);
        assert_eq!(DriveKind::from_win32(1), DriveKind::Unknown);
        assert_eq!(DriveKind::from_win32(99), DriveKind::Unknown);
        assert_eq!(
            drive_label('G', None, DriveKind::Unknown),
            "Local disk (G:)"
        );
        // The five Windows does name today.
        assert_eq!(DriveKind::from_win32(2), DriveKind::Removable);
        assert_eq!(DriveKind::from_win32(3), DriveKind::Fixed);
        assert_eq!(DriveKind::from_win32(4), DriveKind::Remote);
        assert_eq!(DriveKind::from_win32(5), DriveKind::CdRom);
        assert_eq!(DriveKind::from_win32(6), DriveKind::RamDisk);
    }

    #[test]
    fn the_bitmask_names_letters_a_first() {
        assert_eq!(drives_in_mask(0), Vec::<char>::new());
        // Bit 0 is A, bit 2 is C, bit 25 is Z.
        assert_eq!(drives_in_mask(0b1), vec!['A']);
        assert_eq!(drives_in_mask(0b101), vec!['A', 'C']);
        assert_eq!(drives_in_mask(1 << 25), vec!['Z']);
        // A typical desktop: C and D.
        assert_eq!(drives_in_mask((1 << 2) | (1 << 3)), vec!['C', 'D']);
        // Bits above Z name no drive and are ignored rather than turning into `[`.
        assert_eq!(drives_in_mask(1 << 26), Vec::<char>::new());
        assert_eq!(drives_in_mask(u32::MAX).len(), 26);
    }

    /// `C:` is relative to that drive's current directory, so a row spelled that way would list
    /// whatever the process happened to be sitting in.
    #[test]
    fn a_drive_row_is_rooted() {
        assert_eq!(drive_root_path('C'), PathBuf::from(r"C:\"));
    }

    #[test]
    fn known_folders_lead_then_home_then_drives() {
        let roots = build_windows_roots(
            vec![
                root("Pictures", r"C:\Users\dave\Pictures"),
                root("Desktop", r"C:\Users\dave\Desktop"),
            ],
            Some(root("Home", r"C:\Users\dave")),
            vec![root("Local disk (C:)", r"C:\"), root("Photos (D:)", r"D:\")],
        );
        let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Pictures",
                "Desktop",
                "Home",
                "Local disk (C:)",
                "Photos (D:)"
            ]
        );
    }

    /// What the Home row is really for: the reveal walk expands under the longest-matching root,
    /// so a folder in the profile is two steps from Home rather than six from the drive — and the
    /// walk lists every level it passes, one of which is the machine's temp directory.
    #[test]
    fn a_folder_in_the_profile_reveals_from_home_not_the_drive() {
        use crate::browser::tree_model::reveal_path_chain_under;
        let roots = build_windows_roots(
            Vec::new(),
            Some(root("Home", r"C:\Users\dave")),
            vec![root("Local disk (C:)", r"C:\")],
        );
        let chain = reveal_path_chain_under(
            PathPolicy::windows(),
            &roots,
            Path::new(r"\\?\C:\Users\dave\AppData\Local\Temp\t\pics"),
        )
        .expect("the profile contains it");
        assert_eq!(chain.first(), Some(&PathBuf::from(r"C:\Users\dave")));
        assert_eq!(chain.len(), 6);
    }

    /// `%USERPROFILE%` is what a session (or a test fixture) says the profile is. Only an absolute
    /// path counts; a relative one would name whatever the working directory happens to be.
    #[test]
    fn home_comes_from_the_environment_when_it_names_somewhere_absolute() {
        use std::ffi::OsStr;
        let home = home_from_environment(Some(OsStr::new(r"C:\Users\dave")))
            .expect("a drive path is absolute");
        assert_eq!(home.name, HOME_LABEL);
        assert_eq!(home.path, PathBuf::from(r"C:\Users\dave"));
        // Every absolute spelling Windows has, including the one `canonicalize` returns.
        for spelling in [
            r"\\?\C:\Users\dave",
            r"\\naspi\homes\dave",
            r"\\?\UNC\naspi\homes\dave",
            "C:/Users/dave",
        ] {
            assert!(
                home_from_environment(Some(OsStr::new(spelling))).is_some(),
                "{spelling} is absolute"
            );
        }
        for junk in ["", "dave", r"Users\dave", "C:", "C:dave"] {
            assert!(
                home_from_environment(Some(OsStr::new(junk))).is_none(),
                "{junk:?} names nowhere on its own"
            );
        }
        assert!(home_from_environment(None).is_none());
    }

    /// A redirected known folder can land on a drive root, or on another known folder. Two rows
    /// for one path would expand independently and disagree.
    #[test]
    fn a_folder_named_twice_appears_once() {
        let roots = build_windows_roots(
            vec![
                root("Pictures", r"D:\"),
                // Desktop redirected onto Pictures, and spelled in a different case.
                root("Desktop", r"d:\"),
            ],
            // And a profile redirected onto the same drive root.
            Some(root("Home", r"D:\")),
            vec![root("Photos (D:)", r"D:\")],
        );
        let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Pictures"]);
    }

    /// Every known folder can fail to resolve (a stripped-down Windows install, a redirected
    /// folder on a disconnected share). The tree then shows drives alone rather than nothing.
    #[test]
    fn no_known_folders_still_leaves_the_drives() {
        let roots = build_windows_roots(Vec::new(), None, vec![root("Local disk (C:)", r"C:\")]);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, PathBuf::from(r"C:\"));
    }

    #[test]
    fn hidden_and_system_entries_stay_out_of_the_tree() {
        const READONLY: u32 = 0x1;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        const DIRECTORY: u32 = 0x10;

        assert!(!hidden_by_attributes(DIRECTORY));
        assert!(!hidden_by_attributes(DIRECTORY | READONLY));
        assert!(hidden_by_attributes(DIRECTORY | HIDDEN));
        // `System Volume Information` and `$RECYCLE.BIN` are both hidden AND system.
        assert!(hidden_by_attributes(DIRECTORY | SYSTEM));
        assert!(hidden_by_attributes(DIRECTORY | HIDDEN | SYSTEM));
    }

    /// The labels are user-visible, so they answer to `docs/style-guide.md` the way the settings
    /// dialog's and the About box's copy do.
    #[test]
    fn the_labels_follow_the_style_guide() {
        let kinds = [
            DriveKind::Fixed,
            DriveKind::Removable,
            DriveKind::Remote,
            DriveKind::CdRom,
            DriveKind::RamDisk,
            DriveKind::Unknown,
        ];
        let strings: Vec<String> = kinds
            .iter()
            .map(|kind| kind.fallback_label().to_string())
            .chain(KNOWN_FOLDER_LABELS.iter().map(|s| (*s).to_string()))
            .chain(std::iter::once(HOME_LABEL.to_string()))
            .collect();
        for line in &strings {
            assert!(!line.contains('\u{2014}'), "em dash in {line:?}");
            // Sentence case: only the first word may start with a capital, unless the word is an
            // acronym spelled that way (CD, RAM).
            for word in line.split_whitespace().skip(1) {
                assert!(
                    !word.chars().next().is_some_and(char::is_uppercase)
                        || word.chars().all(|c| !c.is_lowercase()),
                    "{word:?} is capitalised mid-label in {line:?}"
                );
            }
        }
    }
}
