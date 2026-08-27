//! Ask Windows what the browse tree's top-level rows are.
//!
//! The thin half of [`super::roots`]: three Win32 enumerations, handed straight to the pure
//! functions next door. Windows-only, because there is nothing here but Win32.
//!
//! ## Gotcha: `GetVolumeInformationW` blocks on a disconnected network drive
//!
//! **Why:** a mapped drive whose server is gone takes the SMB timeout to answer, which is tens of
//! seconds, and this runs on the event loop's thread. So a remote drive is never asked for its
//! label; it gets its drive-type name instead. Local, removable, and optical drives are asked,
//! because those answer from the volume itself.
//!
//! ## Gotcha: an empty optical drive puts up a system dialog
//!
//! **Why:** touching a drive with no media in it makes Windows show "There is no disk in the
//! drive. Please insert a disk into drive E:", which is the OS's dialog and runs its own message
//! loop. `SetThreadErrorMode(SEM_FAILCRITICALERRORS)` is what Explorer sets to suppress it, and
//! the guard below restores the previous mode on the way out.

use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, PCWSTR};

use crate::browser::tree_model::Root;

use super::roots::{self, DriveKind};

/// The tree's top-level rows: the known folders, then every drive letter.
#[must_use]
pub fn enumerate() -> Vec<Root> {
    roots::build_windows_roots(known_folders(), drives())
}

/// Pictures, Desktop, and Downloads, in that order. A folder that doesn't resolve is left out
/// rather than shown as a row that expands to nothing.
fn known_folders() -> Vec<Root> {
    const FOLDERS: [&GUID; 3] = [&FOLDERID_Pictures, &FOLDERID_Desktop, &FOLDERID_Downloads];
    FOLDERS
        .iter()
        .zip(roots::KNOWN_FOLDER_LABELS)
        .filter_map(|(id, label)| {
            known_folder_path(id).map(|path| Root {
                name: label.to_string(),
                path,
            })
        })
        .collect()
}

/// One known folder's path. The shell allocates the string and we free it, which is the whole
/// contract of `SHGetKnownFolderPath`.
fn known_folder_path(id: &GUID) -> Option<std::path::PathBuf> {
    // SAFETY: a well-known folder id, no access token (the calling user's own profile), and the
    // returned pointer is freed below exactly once.
    let raw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: the shell returned a NUL-terminated wide string.
    let path = unsafe { raw.to_string() }
        .ok()
        .map(std::path::PathBuf::from);
    // SAFETY: the shell allocated it with the COM task allocator, and nothing refers to it now.
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };
    path
}

/// Every drive letter Windows currently has, labelled Explorer's way.
fn drives() -> Vec<Root> {
    // SAFETY: a bitmask read, with no arguments and no failure mode.
    let mask = unsafe { GetLogicalDrives() };
    let _quiet = QuietErrors::enter();
    roots::drives_in_mask(mask)
        .into_iter()
        .map(|letter| {
            let path = roots::drive_root_path(letter);
            let kind = drive_kind(&path);
            // A remote drive is never asked for its label; see the module's first gotcha.
            let label = (kind != DriveKind::Remote)
                .then(|| volume_label(&path))
                .flatten();
            Root {
                name: roots::drive_label(letter, label.as_deref(), kind),
                path,
            }
        })
        .collect()
}

fn drive_kind(path: &std::path::Path) -> DriveKind {
    let wide = wide_path(path);
    // SAFETY: a NUL-terminated path that outlives the call.
    DriveKind::from_win32(unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) })
}

/// A volume's label, or `None` when it has none or can't be read.
fn volume_label(path: &std::path::Path) -> Option<String> {
    let wide = wide_path(path);
    let mut name = [0u16; MAX_PATH as usize + 1];
    // SAFETY: the path outlives the call and the buffer is declared by its own length. Every
    // "out" argument we don't want is `None`.
    let read = unsafe {
        GetVolumeInformationW(
            PCWSTR(wide.as_ptr()),
            Some(&mut name),
            None,
            None,
            None,
            None,
        )
    };
    if read.is_err() {
        return None;
    }
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    let label = String::from_utf16_lossy(&name[..end]);
    (!label.trim().is_empty()).then_some(label)
}

/// Suppresses the system's "there is no disk in the drive" dialog for as long as it is alive.
struct QuietErrors(THREAD_ERROR_MODE);

impl QuietErrors {
    fn enter() -> Self {
        let mut previous = THREAD_ERROR_MODE(0);
        // SAFETY: the out parameter is ours, and the mode is restored on drop.
        let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut previous)) };
        Self(previous)
    }
}

impl Drop for QuietErrors {
    fn drop(&mut self) {
        // SAFETY: restoring the mode this thread had before `enter`.
        let _ = unsafe { SetThreadErrorMode(self.0, None) };
    }
}

/// A path as the NUL-terminated wide string every `W` call here takes.
fn wide_path(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
