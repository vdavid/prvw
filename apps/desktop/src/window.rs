use std::path::Path;
use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Fullscreen, Window, WindowAttributes};

const DEFAULT_WIDTH: f64 = 1024.0;
const DEFAULT_HEIGHT: f64 = 768.0;

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
