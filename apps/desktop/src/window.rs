use crate::pixels::LogicalF64;
use std::path::Path;
use std::sync::Arc;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Fullscreen, Window, WindowAttributes};

const DEFAULT_WIDTH: f64 = 1024.0;
const DEFAULT_HEIGHT: f64 = 768.0;

/// Minimum window dimension (logical pixels) when auto-fitting to image size.
pub const MIN_WINDOW_DIM: f64 = 200.0;

/// Maximum fraction of the monitor's work area to use when auto-fitting.
pub const MAX_SCREEN_FRACTION: f64 = 0.9;

/// Create the application window. Must be called in `resumed()`.
pub fn create_window(event_loop: &ActiveEventLoop, file_path: &Path) -> Arc<Window> {
    let title = window_title_for_path(file_path);

    let attrs = WindowAttributes::default()
        .with_title(title)
        .with_inner_size(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));

    let window = event_loop
        .create_window(attrs)
        .expect("Failed to create window");
    let window = Arc::new(window);

    // Disable macOS tab bar and native fullscreen (we have our own borderless fullscreen).
    // This removes "Show Tab Bar", "Show All Tabs", and the system "Enter Full Screen" from menus.
    #[cfg(target_os = "macos")]
    configure_macos_window(&window);

    window
}

/// Set macOS-specific window properties via NSWindow.
#[cfg(target_os = "macos")]
fn configure_macos_window(window: &Window) {
    use objc2::msg_send;
    use objc2_app_kit::NSWindow;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::AppKit(handle)) => handle,
        _ => return,
    };

    let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
    let ns_window: *const NSWindow = unsafe { msg_send![ns_view, window] };
    if ns_window.is_null() {
        return;
    }

    unsafe {
        let ns_window = &*ns_window;

        // Disable tabbing: removes "Show Tab Bar" and "Show All Tabs" from View menu
        // NSWindowTabbingMode.disallowed = 2
        let _: () = msg_send![ns_window, setTabbingMode: 2i64];

        // Remove native fullscreen from collection behavior.
        // This removes the system "Enter Full Screen" menu item.
        // We keep our own borderless fullscreen via winit (F / Enter / F11).
        let behavior: u64 = msg_send![ns_window, collectionBehavior];
        // NSWindowCollectionBehavior.fullScreenPrimary = 1 << 7 = 128
        let new_behavior = behavior & !(1 << 7);
        let _: () = msg_send![ns_window, setCollectionBehavior: new_behavior];

        // Transparent titlebar: content extends behind the title bar, giving the frosted
        // glass look that apps like Finder and Safari use.
        let _: () = msg_send![ns_window, setTitlebarAppearsTransparent: true];
        // NSWindowStyleMask.fullSizeContentView = 1 << 15 = 32768
        let mask: u64 = msg_send![ns_window, styleMask];
        let _: () = msg_send![ns_window, setStyleMask: mask | (1u64 << 15)];

        // Hide the native title text. The title string is still set (for Mission Control
        // and accessibility) but not drawn — we render our own overlay instead.
        // NSWindowTitleVisibility.hidden = 1
        let _: () = msg_send![ns_window, setTitleVisibility: 1i64];
    }

    log::debug!(
        "Configured macOS window: tabbing disabled, native fullscreen removed, transparent titlebar"
    );
}

/// Build the window title from a file path (filename only, not the full path).
pub fn window_title_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Prvw")
        .to_string()
}

/// Build a window title with position info: `3 / 60 – photo.jpg`
pub fn window_title_with_position(path: &Path, current: usize, total: usize) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Prvw");
    if total > 1 {
        format!("{} / {} \u{2013} {name}", current + 1, total)
    } else {
        name.to_string()
    }
}

/// Build a loading title: `3 / 60 – Loading...`
pub fn window_title_loading(current: usize, total: usize) -> String {
    if total > 1 {
        format!("{} / {} \u{2013} Loading...", current + 1, total)
    } else {
        "Loading...".to_string()
    }
}

/// Toggle fullscreen on the window.
pub fn toggle_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        log::debug!("Fullscreen: borderless -> windowed");
        window.set_fullscreen(None);
    } else {
        log::debug!("Fullscreen: windowed -> borderless");
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    }
}

/// Set fullscreen on or off directly.
pub fn set_fullscreen(window: &Window, on: bool) {
    if on {
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    } else {
        window.set_fullscreen(None);
    }
}

/// Check if the window is currently fullscreen.
pub fn is_fullscreen(window: &Window) -> bool {
    window.fullscreen().is_some()
}

/// Monitor work area in logical pixels.
pub struct MonitorBounds {
    pub x: LogicalF64,
    pub y: LogicalF64,
    pub width: LogicalF64,
    pub height: LogicalF64,
}

impl MonitorBounds {
    /// Get the current monitor's bounds in logical pixels. Returns `None` if no monitor.
    pub fn from_window(window: &Window) -> Option<Self> {
        let scale = window.scale_factor();
        window.current_monitor().map(|m| {
            let pos = m.position().to_logical::<f64>(scale);
            let size = m.size().to_logical::<f64>(scale);
            Self {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
            }
        })
    }

    /// Maximum window size (90% of monitor in each dimension).
    pub fn max_window_size(&self) -> (LogicalF64, LogicalF64) {
        (
            self.width * MAX_SCREEN_FRACTION,
            self.height * MAX_SCREEN_FRACTION,
        )
    }
}

/// Clamp a new window position so it doesn't go MORE off-screen than the old position.
///
/// - `target`: desired (x, y) for the new position
/// - `new_size`: (width, height) of the new outer frame
/// - `old_pos`: (x, y) of the current outer frame
/// - `old_size`: (width, height) of the current outer frame
///
/// Returns the clamped (x, y).
pub fn clamp_to_screen(
    target: (LogicalF64, LogicalF64),
    new_size: (LogicalF64, LogicalF64),
    old_pos: (LogicalF64, LogicalF64),
    old_size: (LogicalF64, LogicalF64),
    bounds: &MonitorBounds,
) -> (LogicalF64, LogicalF64) {
    let off_left = (bounds.x - old_pos.0).max(0.0);
    let off_right = ((old_pos.0 + old_size.0) - (bounds.x + bounds.width)).max(0.0);
    let off_top = (bounds.y - old_pos.1).max(0.0);
    let off_bottom = ((old_pos.1 + old_size.1) - (bounds.y + bounds.height)).max(0.0);

    let min_x = bounds.x - off_left;
    let max_x = bounds.x + bounds.width + off_right - new_size.0;
    let min_y = bounds.y - off_top;
    let max_y = bounds.y + bounds.height + off_bottom - new_size.1;

    let fx = if min_x <= max_x {
        target.0.clamp(min_x, max_x)
    } else {
        (min_x + max_x) / 2.0
    };
    let fy = if min_y <= max_y {
        target.1.clamp(min_y, max_y)
    } else {
        (min_y + max_y) / 2.0
    };
    (fx, fy)
}

/// Resize the window to fit the given image dimensions, then center it on screen.
///
/// Returns the physical size the window was set to, so the caller can update the renderer
/// immediately (without waiting for the async `Resized` event).
///
/// The window size is the image size clamped to:
/// - minimum 200px in each dimension
/// - maximum 90% of the monitor's work area in each dimension
///
/// Returns `None` if the window is fullscreen (no resize performed).
pub fn resize_to_fit_image(
    window: &Window,
    image_width: u32,
    image_height: u32,
) -> Option<PhysicalSize<u32>> {
    if is_fullscreen(window) {
        return None;
    }

    let scale_factor = window.scale_factor();

    // Get the monitor's work area (excluding dock/menu bar)
    let (max_w, max_h) = MonitorBounds::from_window(window)
        .map(|b| b.max_window_size())
        .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));

    // Apply the minimum floor first, then scale down proportionally to fit within the
    // screen cap. Scaling must happen on the un-clamped dimensions to preserve aspect ratio —
    // clamping first would make both axes fit independently, losing the ratio.
    let img_w = (image_width as f64).max(MIN_WINDOW_DIM);
    let img_h = (image_height as f64).max(MIN_WINDOW_DIM);
    let scale = (max_w / img_w).min(max_h / img_h).min(1.0);
    let final_w = (img_w * scale).max(MIN_WINDOW_DIM);
    let final_h = (img_h * scale).max(MIN_WINDOW_DIM);

    let new_size = LogicalSize::new(final_w, final_h);
    let physical_size = new_size.to_physical::<u32>(scale_factor);

    let _ = window.request_inner_size(new_size);

    log::debug!(
        "Auto-fit window: {}x{} image -> {}x{} logical ({}x{} physical)",
        image_width,
        image_height,
        final_w as u32,
        final_h as u32,
        physical_size.width,
        physical_size.height
    );

    // Center the window on the current monitor
    if let Some(bounds) = MonitorBounds::from_window(window) {
        let x = bounds.x + (bounds.width - final_w) / 2.0;
        let y = bounds.y + (bounds.height - final_h) / 2.0;
        window.set_outer_position(LogicalPosition::new(x, y));
    }

    Some(physical_size)
}
