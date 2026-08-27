//! `AppCommand` — the unified command vocabulary that drives the app.
//!
//! Keyboard, mouse, menu, QA server, and MCP all map their inputs to these commands.
//! `App::execute_command` is the single place where each command's effect is implemented.
//!
//! Also stores the global `EventLoopProxy<AppCommand>` so non-event-loop code (like
//! AppKit Settings delegates on macOS) can send commands into the main loop without
//! holding a proxy reference.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc;
use winit::event_loop::EventLoopProxy;

use crate::decoding::RawPipelineFlags;
use crate::parity::command_keys::{CommandKey, CommandParity};
use crate::parity::{Coverage, Platform};

/// Global event loop proxy, set once in `resumed()`. Allows non-main-loop code (like the
/// native Settings window delegate) to send commands into the event loop.
static EVENT_LOOP_PROXY: OnceLock<EventLoopProxy<AppCommand>> = OnceLock::new();

/// Store the event loop proxy so it's accessible from native UI delegates.
pub fn set_event_loop_proxy(proxy: EventLoopProxy<AppCommand>) {
    let _ = EVENT_LOOP_PROXY.set(proxy);
}

/// Send a command through the global event loop proxy. Returns false if the proxy
/// hasn't been set or the event loop is closed.
///
/// For the code that runs outside `App`'s reach: macOS's native Settings delegate and its
/// screen-change observer, the Windows message hook, and the Win32 settings dialog. Linux has
/// none of them, and nothing else reaches for it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn send_command(command: AppCommand) -> bool {
    EVENT_LOOP_PROXY
        .get()
        .and_then(|p| p.send_event(command).ok())
        .is_some()
}

/// Clone of the global event loop proxy. Used by the browse grid's thumbnail submission path
/// (`browser::grid` on macOS, `browser::windows::ui::grid` on Windows), which has no `App`
/// reference to borrow the proxy from. Panics if called before `resumed()` set the proxy — the
/// grid only submits after the window exists, so the proxy is always present by then.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn event_loop_proxy() -> EventLoopProxy<AppCommand> {
    EVENT_LOOP_PROXY
        .get()
        .expect("event loop proxy must be set before the grid submits thumbnail requests")
        .clone()
}

impl AppCommand {
    /// What this command means to the parity registries.
    ///
    /// Exhaustive with no `_` arm: a new command has to declare itself a user-facing
    /// [`CommandKey`] or plumbing before it compiles. [`CommandParity::Internal`] is for
    /// worker wakeups, watcher results, and the QA driving hooks the integration tests use
    /// because they can't synthesize a native click. It is not a shortcut for an action that
    /// nobody got around to registering.
    pub fn parity_key(&self) -> CommandParity {
        use CommandParity::{Action, Internal};
        match self {
            // ── Navigation ───────────────────────────────────────────
            AppCommand::Navigate(_) | AppCommand::NavigateDebounced(_) => {
                Action(CommandKey::NextPreviousImage)
            }
            AppCommand::GoToFirst => Action(CommandKey::GoToFirst),
            AppCommand::GoToLast => Action(CommandKey::GoToLast),
            AppCommand::OpenFile(_) | AppCommand::ShowOpenDialog => Action(CommandKey::OpenFile),
            AppCommand::OpenDropped(_) => Action(CommandKey::DropToOpen),
            AppCommand::ToggleLoopNavigation => Action(CommandKey::LoopNavigation),
            AppCommand::SetSortBy(_) => Action(CommandKey::SortBy),
            AppCommand::Refresh => Action(CommandKey::Refresh),

            // ── View ─────────────────────────────────────────────────
            AppCommand::ZoomIn => Action(CommandKey::ZoomIn),
            AppCommand::ZoomOut => Action(CommandKey::ZoomOut),
            AppCommand::SetZoom(_) => Action(CommandKey::SetZoom),
            AppCommand::FitToWindow => Action(CommandKey::FitToWindow),
            AppCommand::ActualSize => Action(CommandKey::ActualSize),
            AppCommand::ToggleFit => Action(CommandKey::ToggleFit),
            AppCommand::ToggleFullscreen | AppCommand::SetFullscreen(_) => {
                Action(CommandKey::Fullscreen)
            }
            AppCommand::SetAutoFitWindow(_) => Action(CommandKey::AutoFitWindow),
            AppCommand::SetEnlargeSmallImages(_) => Action(CommandKey::EnlargeSmallImages),
            AppCommand::SetIccColorManagement(_) => Action(CommandKey::IccColorManagement),
            AppCommand::SetColorMatchDisplay(_) => Action(CommandKey::ColorMatchDisplay),
            AppCommand::SetRelativeColorimetric(_) => Action(CommandKey::RelativeColorimetric),
            AppCommand::SetScrollToZoom(_) => Action(CommandKey::ScrollToZoom),
            AppCommand::SetPreloadNeighbors(_) => Action(CommandKey::PreloadNeighbors),
            AppCommand::SetTitleBar(_) => Action(CommandKey::TitleBar),
            AppCommand::ToggleHistogram => Action(CommandKey::Histogram),
            AppCommand::ToggleExifInfo => Action(CommandKey::ExifInfo),

            // ── Browse mode ──────────────────────────────────────────
            AppCommand::ToggleBrowseMode | AppCommand::EnterImageMode => {
                Action(CommandKey::BrowseMode)
            }
            AppCommand::ToggleBrowseFocus => Action(CommandKey::BrowseFocus),
            AppCommand::BrowseOpenSelected => Action(CommandKey::BrowseOpenSelected),

            // ── Slideshow ────────────────────────────────────────────
            AppCommand::ToggleSlideshow => Action(CommandKey::Slideshow),
            AppCommand::SetSlideshowSeconds(_) => Action(CommandKey::SlideshowSeconds),
            AppCommand::SetSlideshowCrossfade(_) => Action(CommandKey::SlideshowCrossfade),
            AppCommand::SetSlideshowLoop(_) => Action(CommandKey::SlideshowLoop),
            AppCommand::IncreaseSlideshowSpeed | AppCommand::DecreaseSlideshowSpeed => {
                Action(CommandKey::SlideshowSpeed)
            }

            // ── RAW ──────────────────────────────────────────────────
            AppCommand::SetRawPipelineFlags(_) => Action(CommandKey::RawPipelineFlags),
            AppCommand::SetCustomDcpDir(_) => Action(CommandKey::CustomDcpDir),

            // ── App ──────────────────────────────────────────────────
            AppCommand::CopyImage => Action(CommandKey::CopyImage),
            AppCommand::Print => Action(CommandKey::Print),
            AppCommand::ShowAbout => Action(CommandKey::About),
            AppCommand::ShowSettings
            | AppCommand::ShowSettingsSection(_)
            | AppCommand::CloseSettings => Action(CommandKey::Settings),
            AppCommand::Exit => Action(CommandKey::Exit),

            // ── Plumbing: worker results, OS notifications, QA hooks ──
            AppCommand::SetCursorPosition { .. }
            | AppCommand::FolderChanged { .. }
            | AppCommand::ActiveFolderRescanned { .. }
            | AppCommand::WatchedFoldersChanged { .. }
            | AppCommand::SetWindowGeometry { .. }
            | AppCommand::ScrollZoom { .. }
            | AppCommand::SendKey(_)
            | AppCommand::TakeScreenshot(_)
            | AppCommand::Sync(_)
            | AppCommand::PreloaderProgress => Internal,
            AppCommand::DisplayChanged => Internal,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AppCommand::PreviewsAvailable => Internal,
            // The browse screen's own event wiring: tree and grid callbacks, background
            // listing results, and the QA hook that stands in for a native click. Browse mode
            // as a feature is `CommandKey::BrowseMode`.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AppCommand::BrowseSelectFolder(_)
            | AppCommand::BrowseTreeChildrenLoaded { .. }
            | AppCommand::BrowseTreeFolderExpanded(_)
            | AppCommand::BrowseTreeFolderCollapsed(_)
            | AppCommand::BrowseFolderListed { .. }
            | AppCommand::BrowseThumbnailsAvailable
            | AppCommand::BrowseGridSelected(_)
            | AppCommand::BrowseQaSelectGrid(_)
            | AppCommand::BrowseSelectionMeasured { .. } => Internal,
            #[cfg(all(debug_assertions, any(target_os = "macos", target_os = "windows")))]
            AppCommand::GetNativeWindowId(_) => Internal,
            #[cfg(all(debug_assertions, target_os = "macos"))]
            AppCommand::WindowDiagnostics(_)
            | AppCommand::ZoomWindow
            | AppCommand::ClickZoomButton => Internal,
        }
    }

    /// The action behind this command, when the registry says the running platform hasn't
    /// built it. `execute_command` drops such a command and logs the name rather than running
    /// an arm that would half-work.
    ///
    /// This is where per-platform suppression comes from, and it covers every entry point at
    /// once: a key, a menu click, and a QA request all reach the same gate. Enter opens the
    /// image browser on macOS and does nothing on Windows because `CommandKey::BrowseMode`
    /// says `Missing` there, not because anything is `#[cfg]`ed out (M1 step 3 of
    /// `docs/specs/cross-platform-plan.md`); M5 flips that arm by building browse mode.
    ///
    /// It's also what keeps `parity::command_keys` honest. A registry that says `Missing` for
    /// something that works now breaks that thing visibly, and a `Present` that's really a
    /// stub still shows up the moment someone runs it.
    pub fn unimplemented_here(&self) -> Option<CommandKey> {
        match self.parity_key() {
            CommandParity::Action(key) if key.coverage(Platform::HOST) == Coverage::Missing => {
                Some(key)
            }
            _ => None,
        }
    }
}

/// Commands that drive all app behavior. Keyboard, mouse, menu, QA server, and MCP all
/// map their inputs to these commands. `App::execute_command` is the single place where
/// each command's effect is implemented.
///
/// On non-macOS builds, the Settings window (AppKit-gated) never dispatches
/// `SetPreloadNeighbors` / `SetRawPipelineFlags` / `SetCustomDcpDir`. Silence
/// the resulting "variant never constructed" clippy warning here rather than
/// peppering `#[cfg]`s across the enum.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum AppCommand {
    // ── Navigation ───────────────────────────────────────────────────
    /// Navigate forward (true) or backward (false), immediately. Used by
    /// QA / MCP / HTTP so tests see the move without waiting for the
    /// user-debounce window.
    Navigate(bool),
    /// User-initiated navigation (arrow keys, mouse wheel). Queues one
    /// step on the debounce accumulator — a burst of these within
    /// `navigation::NAV_DEBOUNCE` collapses to a single jump.
    NavigateDebounced(bool),
    /// Absolute jump to the first image (Home key, Navigate → Go to first).
    /// No-op when already at index 0 or the directory is empty.
    GoToFirst,
    /// Absolute jump to the last image (End key, Navigate → Go to last).
    /// No-op when already at the last index or the directory is empty.
    GoToLast,
    /// Open a specific file.
    OpenFile(PathBuf),
    /// Put the native file picker up (File → Open…, Cmd/Ctrl+O). The chosen path comes back
    /// as [`AppCommand::OpenFile`]; a dismissed picker sends nothing. See `open_dialog`.
    ShowOpenDialog,

    // ── View ─────────────────────────────────────────────────────────
    /// Zoom in one step (keyboard shortcut).
    ZoomIn,
    /// Zoom out one step (keyboard shortcut).
    ZoomOut,
    /// Set absolute zoom level.
    SetZoom(f32),
    /// Reset zoom to fit the image in the window.
    FitToWindow,
    /// Set zoom to 1:1 pixel mapping.
    ActualSize,
    /// Toggle between fit-to-window and actual size.
    ToggleFit,
    /// Toggle fullscreen mode.
    ToggleFullscreen,
    /// Set fullscreen on or off explicitly.
    SetFullscreen(bool),
    /// Set auto-fit window mode.
    SetAutoFitWindow(bool),
    /// Set enlarge-small-images mode.
    SetEnlargeSmallImages(bool),
    /// Set ICC color management (Level 1: source -> sRGB when color match display is off).
    SetIccColorManagement(bool),
    /// Set color match display mode (Level 2 ICC: source -> display profile).
    SetColorMatchDisplay(bool),
    /// Set rendering intent to relative colorimetric (false = perceptual).
    SetRelativeColorimetric(bool),
    /// Set scroll-to-zoom mode (true = scroll zooms, false = scroll navigates).
    SetScrollToZoom(bool),
    /// Set preload-neighbors mode. When false, the preloader skips adjacent
    /// images so only the currently displayed image consumes decode work.
    /// Useful for benchmarking single-image cold-start decode times.
    SetPreloadNeighbors(bool),
    /// Set title bar mode (true = reserve a strip at the top, false = image fills window).
    SetTitleBar(bool),
    /// Toggle the histogram overlay (View → Histogram, bare H key).
    ToggleHistogram,
    /// Toggle the EXIF info overlay (View → Exif info, bare E key).
    ToggleExifInfo,
    /// Toggle loop navigation (Navigate → Loop navigation, bare L key).
    /// Wraps Next/Previous at the directory boundary when on; halts at
    /// edges when off. Recomputes the preloader's active window after
    /// flipping so wrap-side neighbors warm or get evicted to match.
    ToggleLoopNavigation,
    /// Re-sort the directory list (View → Sort by → {Name | Date | File type}).
    /// The cache survives by path: in-window entries stay, out-of-window
    /// entries get evicted, missing in-window slots get queued for preload.
    SetSortBy(crate::navigation::SortBy),
    /// Update the cached cursor position (used by MCP for hover-readout tests).
    SetCursorPosition { x: f64, y: f64 },

    // ── Browse mode (macOS-only; native AppKit folder tree + preview grid) ──
    /// Swap between the wgpu image viewer and the native browse screen. Fired by
    /// the Navigate → "Image browser" / "Image view" menu item and the Enter key
    /// in image mode. Image → Browse hides the Metal layer and shows the split
    /// view; Browse → Image reverses it.
    ToggleBrowseMode,
    /// Leave browse mode for the image viewer unconditionally. Fired by Esc while browsing (the
    /// focused native pane's `keyDown:` override routes it here) and by Enter on the tree (via
    /// `BrowseOpenSelected`'s fallback).
    EnterImageMode,
    /// Flip browse-mode keyboard focus between the tree and grid panes (Tab from the focused
    /// pane's `keyDown:` override). Updates `browser::State::focused_pane` (the single source of
    /// truth) and syncs the native first responder + emphasis via `apply_focus`. Browse-mode only.
    ToggleBrowseFocus,
    /// A folder was selected in the browse-mode tree. Records it in `browser::State` and logs
    /// how many supported images it holds. Fired by the `NSOutlineView` selection delegate.
    /// (Listing the folder's images in the grid is a later phase.)
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseSelectFolder(PathBuf),
    /// A background scan of `path`'s child directories finished. The data source NEVER reads a
    /// directory on the main thread (a slow SMB share would freeze the whole app), so children
    /// arrive here: the executor stores them in the tree's child cache and tells the outline view
    /// to re-query that node (`reloadItem:reloadChildren:`). Posted by the tree scanner thread via
    /// the global `EventLoopProxy`. `children` is already filtered to subdirectories and sorted.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseTreeChildrenLoaded {
        path: PathBuf,
        children: Vec<PathBuf>,
    },
    /// A tree node was expanded — start watching its folder for subdirectory changes (live folder
    /// sync, Part B). Fired by the `NSOutlineView` `outlineViewItemDidExpand:` delegate after the
    /// node's children load. The executor adds the folder to the tree-watch set so a
    /// `FolderChanged` for it reloads the node's subdirectories. Roots are watched at browse setup,
    /// not via this. Browse-mode only.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseTreeFolderExpanded(PathBuf),
    /// A tree node was collapsed — stop watching its folder (live folder sync, Part B). Fired by
    /// `outlineViewItemDidCollapse:`. Keeps the tree-watch set bounded to what's expanded; roots
    /// stay watched. Browse-mode only.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseTreeFolderCollapsed(PathBuf),
    /// A background folder listing finished. The grid NEVER reads a directory on the main thread (a
    /// slow SMB share would freeze the app), so the selected folder's images arrive here: the
    /// executor populates the grid model + reloads the collection view. Posted by the grid lister
    /// thread via the global `EventLoopProxy`. `images` is unsorted (the grid model sorts).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseFolderListed {
        folder: PathBuf,
        images: Vec<PathBuf>,
    },
    /// One or more grid-thumbnail QL completions are queued (the grid's `quicklook::RequestTable`).
    /// Fired only when the queue was empty (see `quicklook::push_delivery`) so a burst of N
    /// completions sends 1–2 events, not N. The executor drains them, builds `NSImage`s, and
    /// reloads the affected cells.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseThumbnailsAvailable,
    /// The grid selection changed to `index` (native click or programmatic). Records it in the grid
    /// model + `browser::State` for QA/tests. Browse-mode only.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseGridSelected(usize),
    /// Select grid item `index` programmatically, the way a native click would (updates the grid
    /// model's selection so the open path reads the right image, focuses the grid, warms the
    /// selection). QA/test-only: the QA server can't synthesize a native collection-view click, so
    /// this is how integration tests drive grid selection. Browse-mode only.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseQaSelectGrid(usize),
    /// A background header read finished for a browse-mode grid selection: `dimensions` is the
    /// image's pixel size, or `None` for a file whose header we can't read. Feeds the Windows
    /// status bar's size pane. A delivery for a file that is no longer selected is dropped — a
    /// fast arrow through a folder fires one measurement per cell, and a late answer would show
    /// the wrong size beside the right name. Browse-mode only.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BrowseSelectionMeasured {
        /// The file that was measured.
        path: PathBuf,
        /// Its pixel size, orientation applied.
        dimensions: Option<(u32, u32)>,
    },
    /// Open the grid's selected image in image mode (double-click on a grid item, or Enter while
    /// the grid pane is focused). Sets up `navigation` for the selected folder at the chosen index,
    /// displays that image, and switches to image mode so arrow-key nav works afterward.
    BrowseOpenSelected,

    // ── Slideshow ────────────────────────────────────────────────────
    /// Start the slideshow if stopped, stop it if running (Slideshow →
    /// Start/Stop slideshow, or bare `S`).
    ToggleSlideshow,
    /// Set the time-per-image in seconds (Settings → Slideshow slider).
    /// Clamped to `slideshow::MIN_SECONDS..=MAX_SECONDS`.
    SetSlideshowSeconds(u32),
    /// Enable/disable the crossfade transition (Settings → Slideshow).
    SetSlideshowCrossfade(bool),
    /// Enable/disable slideshow looping past the last image (Settings → Slideshow).
    SetSlideshowLoop(bool),
    /// Shorten the time-per-image by one second (Slideshow → Increase speed, `]`).
    IncreaseSlideshowSpeed,
    /// Lengthen the time-per-image by one second (Slideshow → Decrease speed, `[`).
    DecreaseSlideshowSpeed,

    // ── RAW pipeline (Phase 3.7) ─────────────────────────────────────
    /// Replace the RAW pipeline flags wholesale. Used by the Settings → RAW
    /// panel so a single event carries all stage toggles in one update
    /// (plus the "Reset to defaults" button).
    SetRawPipelineFlags(RawPipelineFlags),
    /// Replace the custom DCP directory. `None` clears the override and
    /// falls back to Adobe Camera Raw + the bundled collection.
    SetCustomDcpDir(Option<String>),

    // ── File watching (live folder sync) ─────────────────────────────
    /// A watched folder changed on disk. Posted by the `folder_watch` worker after it
    /// coalesces a ~150 ms burst of raw `notify` events into one event per affected folder.
    /// The consumer re-scans `folder` off the main thread (adds/removes are discovered by the
    /// re-scan, so they aren't listed here) and reloads `modified` (paths flagged `Modify`) so a
    /// re-saved image re-decodes. See `crate::folder_watch`.
    FolderChanged {
        folder: PathBuf,
        modified: Vec<PathBuf>,
    },
    /// A background re-scan of the active folder finished (triggered by `FolderChanged`). The
    /// executor diffs `images` against the live `DirectoryList` and applies adds/removes, the
    /// delete-current navigation, and the "(No images)" empty state. `images` is unsorted (the
    /// diff sorts by the active `SortBy`). Posted by the `folder_watch::RescanLister` worker.
    ActiveFolderRescanned {
        folder: PathBuf,
        images: Vec<PathBuf>,
    },
    /// The `folder_watch` worker applied a watch/unwatch, so the set of folders whose FSEvents
    /// stream is actually live changed. Sorted. Mirrored into shared state as `watched_folders`,
    /// which is the QA barrier for "live sync is armed here". Distinct from `App::watched_folder`
    /// and `watched_tree_folders`, which are what the app has *requested*. See
    /// `crate::folder_watch`.
    WatchedFoldersChanged { folders: Vec<PathBuf> },

    // ── Color management ─────────────────────────────────────────────
    /// The display the window is on may now want a different ICC profile. macOS sends it when the
    /// window changes screen (`NSWindowDidChangeScreenNotification`); Windows sends it on
    /// `WM_DISPLAYCHANGE`, which is a monitor arriving or leaving, a resolution change, or a
    /// profile re-associated in place.
    DisplayChanged,

    /// Files were dropped on the window (or handed over by the QA server's `/drop`). The
    /// executor classifies them with `launch::classify_open_request` and opens what it finds:
    /// an image, a set of them, or a folder. winit reports a drop one path at a time, so
    /// `App` batches the paths and sends this once per drop.
    OpenDropped(Vec<PathBuf>),

    // ── Clipboard ────────────────────────────────────────────────────
    /// Copy the current image to the system clipboard. Writes the original
    /// file's URL plus a bitmap to the pasteboard (Edit → Copy, ⌘C, and the
    /// right-click context menu all map here).
    CopyImage,

    // ── Print ────────────────────────────────────────────────────────
    /// Print the current image via the system print dialog (File → Print,
    /// ⌘P, and the right-click context menu all map here). Loads the original
    /// file and presents an `NSPrintOperation` sheet on the viewer window.
    Print,

    // ── App ──────────────────────────────────────────────────────────
    /// Show the About window.
    ShowAbout,
    /// Show the Settings window (optionally to a specific section).
    ShowSettings,
    /// Switch to a specific Settings section by name (e.g., "general", "file_associations").
    ShowSettingsSection(String),
    /// Close the Settings window.
    CloseSettings,
    /// Exit the application.
    Exit,

    // ── Window ───────────────────────────────────────────────────────
    /// Reposition and/or resize the window. All fields optional.
    SetWindowGeometry {
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
    },

    // ── QA / MCP ─────────────────────────────────────────────────────
    /// Scroll-wheel zoom at a specific cursor position.
    ScrollZoom {
        delta: f32,
        cursor_x: f32,
        cursor_y: f32,
    },
    /// Re-display the current image (re-applies zoom, re-reads from cache/disk).
    Refresh,

    /// Simulate a key press. Key name follows web conventions: "ArrowLeft", "Escape", "f", etc.
    SendKey(String),
    /// Capture a screenshot. The sender receives PNG bytes.
    TakeScreenshot(mpsc::Sender<Vec<u8>>),
    /// Get the operating system's own handle for the main viewer window, for the debug-only
    /// `screenshot_window` MCP tool: the `NSWindow.windowNumber` on macOS, the `HWND` on
    /// Windows. One command rather than two, so the QA layer above it has one shape to talk to.
    /// Sender receives 0 if the window doesn't exist yet.
    #[cfg(all(debug_assertions, any(target_os = "macos", target_os = "windows")))]
    GetNativeWindowId(mpsc::Sender<u64>),
    /// Dump the main window's AppKit view/layer tree (debug-only `GET /window-diagnostics`).
    /// Runs on the event loop because it talks to AppKit.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    WindowDiagnostics(mpsc::Sender<String>),
    /// Perform the native window `zoom:` — what the green traffic light does. Debug-only
    /// driving hook: the QA path can't synthesize a real click on the button.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    ZoomWindow,
    /// Send `performClick:` to the green traffic light itself, so the zoom runs through the
    /// button's own action rather than a direct `zoom:`. Debug-only driving hook.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    ClickZoomButton,
    /// Synchronization barrier — sends () back to confirm all prior commands were processed.
    Sync(mpsc::Sender<()>),

    /// No-op signal sent by the preloader worker thread to wake winit's
    /// event loop so `about_to_wait` runs and `poll_preloader` drains the
    /// response channel. Without this, `ControlFlow::Wait` sleeps until
    /// an OS event arrives, and a freshly-decoded priority-0 image can
    /// sit in the mpsc channel unprocessed for seconds while the user
    /// stares at the placeholder.
    PreloaderProgress,

    // ── Previews (the platforms with a generator) ──────────────────
    /// One or more preview completions are sitting in the generator's
    /// pending queue waiting for the main thread to drain them. The
    /// generator pushes deliveries onto the queue and fires this command
    /// **only when the queue was previously empty** — so a burst of 38
    /// completions sends 1–2 user events, not 38, and keyboard / window
    /// events don't get starved by a high-frequency EventLoopProxy flood.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    PreviewsAvailable,
}
