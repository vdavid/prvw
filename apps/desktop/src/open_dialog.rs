//! The native "Open an image" picker behind File → Open.
//!
//! One implementation for all three platforms, through `rfd`: `NSOpenPanel` on macOS,
//! `IFileOpenDialog` on Windows, and the XDG desktop portal on Linux. The chosen path comes
//! back as `AppCommand::OpenFile`, the same command a Finder double-click sends, so there's one
//! path into "show this image" however the file was named.
//!
//! ## The one rule: no nested message loop
//!
//! A file dialog is modal, and running a modal on the event-loop thread spins a nested message
//! loop inside winit's pump: on Windows that freezes the app, and on macOS `runModal` segfaults
//! on autorelease-pool cleanup (`AGENTS.md`'s gotcha, and the same rule stated for Windows in
//! `docs/specs/windows-ui-design.md`). So [`show`] hands the dialog's future to a worker thread
//! and returns immediately. The picker is UI-modal on macOS (a sheet on the app's window) and
//! modeless on the other two, and none of them blocks the loop.
//!
//! The blocking `rfd::FileDialog` is deliberately unused. Besides running a loop, its Windows
//! implementation calls `CoInitializeEx(COINIT_APARTMENTTHREADED)`, gets `RPC_E_CHANGED_MODE` on
//! a thread that's already MTA, and turns the error into `None`, which is indistinguishable from
//! the user cancelling.

use std::path::PathBuf;

use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;
use crate::decoding;

/// Put the file picker up and send the chosen path back as `AppCommand::OpenFile`. Returns as
/// soon as the dialog is on screen; dismissing it sends nothing.
///
/// `start_in` is where the picker opens, normally the folder of the image on screen. `None`
/// leaves the platform to pick, which is what the empty state wants.
///
/// **Call this on the event-loop thread.** On macOS `rfd` builds the future against the main
/// thread's `NSApplication` and presents the panel as a sheet on the app's front window; only
/// the polling moves off. The future is `Send`, the thread it's polled on only parks.
pub fn show(proxy: EventLoopProxy<AppCommand>, start_in: Option<PathBuf>) {
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Open an image")
        .add_filter("Images", &decoding::supported_extensions());
    if let Some(folder) = start_in {
        dialog = dialog.set_directory(folder);
    }
    let picking = dialog.pick_file();

    std::thread::spawn(move || {
        let Some(file) = pollster::block_on(picking) else {
            log::debug!("Open: the picker was dismissed");
            return;
        };
        let path = file.path().to_path_buf();
        log::info!("Open: picked {}", path.display());
        // The event loop is gone only if the app is already exiting, which is nothing to say.
        let _ = proxy.send_event(AppCommand::OpenFile(path));
    });
}
