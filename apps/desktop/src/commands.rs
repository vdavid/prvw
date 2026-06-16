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

/// Global event loop proxy, set once in `resumed()`. Allows non-main-loop code (like the
/// native Settings window delegate) to send commands into the event loop.
static EVENT_LOOP_PROXY: OnceLock<EventLoopProxy<AppCommand>> = OnceLock::new();

/// Store the event loop proxy so it's accessible from native UI delegates.
pub fn set_event_loop_proxy(proxy: EventLoopProxy<AppCommand>) {
    let _ = EVENT_LOOP_PROXY.set(proxy);
}

/// Send a command through the global event loop proxy. Returns false if the proxy
/// hasn't been set or the event loop is closed.
#[cfg(target_os = "macos")] // Called from native_ui (macOS-only Settings delegate)
pub fn send_command(command: AppCommand) -> bool {
    EVENT_LOOP_PROXY
        .get()
        .and_then(|p| p.send_event(command).ok())
        .is_some()
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
    /// Leave browse mode for the image viewer unconditionally. Fired by Esc / Enter while
    /// browsing — keys flow through winit → `input::browse_key_to_command`, branched by mode,
    /// never the AppKit responder chain (winit keeps the keyboard even while the split view is
    /// up).
    EnterImageMode,
    /// Flip browse-mode keyboard focus between the tree and grid panes (Tab). Updates the
    /// app-tracked `browser::State::focused_pane` and the native highlight. Browse-mode only.
    ToggleBrowseFocus,
    /// A folder was selected in the browse-mode tree. Records it in `browser::State` and logs
    /// how many supported images it holds. Fired by the `NSOutlineView` selection delegate.
    /// (Listing the folder's images in the grid is a later phase.)
    #[cfg(target_os = "macos")]
    BrowseSelectFolder(PathBuf),
    /// Move the browse-mode tree selection (arrow keys). `+1` = Down, `-1` = Up. Drives the
    /// `NSOutlineView` programmatically — winit keeps the keyboard, so the responder chain is
    /// never used. Browse-mode only, ignored unless the tree pane is focused.
    #[cfg(target_os = "macos")]
    BrowseMoveTreeSelection(i32),
    /// Expand (Right arrow) or collapse (Left arrow) the selected browse-mode tree row.
    /// `true` = expand, `false` = collapse. Browse-mode only, tree pane focused.
    #[cfg(target_os = "macos")]
    BrowseExpandTreeSelection(bool),
    /// A background scan of `path`'s child directories finished. The data source NEVER reads a
    /// directory on the main thread (a slow SMB share would freeze the whole app), so children
    /// arrive here: the executor stores them in the tree's child cache and tells the outline view
    /// to re-query that node (`reloadItem:reloadChildren:`). Posted by the tree scanner thread via
    /// the global `EventLoopProxy`. `children` is already filtered to subdirectories and sorted.
    #[cfg(target_os = "macos")]
    BrowseTreeChildrenLoaded {
        path: PathBuf,
        children: Vec<PathBuf>,
    },

    // ── Slideshow ────────────────────────────────────────────────────
    /// Start the slideshow if stopped, stop it if running (Slideshow →
    /// Start/Stop slideshow, ⌘S).
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

    // ── Color management ─────────────────────────────────────────────
    /// The window moved to a different display — re-query the display ICC profile.
    #[cfg(target_os = "macos")]
    DisplayChanged,

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
    /// Get the NSWindow `windowNumber` of the main viewer window, for the debug-only
    /// `screenshot_window` MCP tool that shells out to `screencapture -l`. Sender
    /// receives 0 if the window doesn't exist yet.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    GetWindowNumber(mpsc::Sender<u32>),
    /// Synchronization barrier — sends () back to confirm all prior commands were processed.
    Sync(mpsc::Sender<()>),

    /// No-op signal sent by the preloader worker thread to wake winit's
    /// event loop so `about_to_wait` runs and `poll_preloader` drains the
    /// response channel. Without this, `ControlFlow::Wait` sleeps until
    /// an OS event arrives, and a freshly-decoded priority-0 image can
    /// sit in the mpsc channel unprocessed for seconds while the user
    /// stares at the placeholder.
    PreloaderProgress,

    // ── Previews (macOS-only; QuickLook-backed) ────────────────────
    /// One or more QL preview completions are sitting in the
    /// `previews::State::pending` queue waiting for the main thread to
    /// drain them. The completion block pushes deliveries onto the queue
    /// and fires this command **only when the queue was previously empty** —
    /// so a burst of 38 completions sends 1–2 user events, not 38, and
    /// keyboard / window events don't get starved by a high-frequency
    /// EventLoopProxy flood.
    #[cfg(target_os = "macos")]
    PreviewsAvailable,
}
