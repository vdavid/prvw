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
    Arc::new(window)
}

/// Build the window title from a file path (filename only, not the full path).
pub fn window_title_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Prvw")
        .to_string()
}

/// Build a window title with position info for navigation.
pub fn window_title_with_position(path: &Path, current: usize, total: usize) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Prvw");
    if total > 1 {
        format!("{name} ({}/{})", current + 1, total)
    } else {
        name.to_string()
    }
}

/// Toggle fullscreen on the window.
pub fn toggle_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        window.set_fullscreen(None);
    } else {
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    }
}

/// Check if the window is currently fullscreen.
pub fn is_fullscreen(window: &Window) -> bool {
    window.fullscreen().is_some()
}
