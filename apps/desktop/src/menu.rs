use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Identifiers for custom menu actions.
pub struct MenuIds {
    pub about: MenuId,
    pub settings: MenuId,
    pub zoom_in: MenuId,
    pub zoom_out: MenuId,
    pub actual_size: MenuId,
    pub fit_to_window: MenuId,
    pub fullscreen: MenuId,
    pub previous: MenuId,
    pub next: MenuId,
}

/// The menu bar and its action IDs. The `Menu` must be kept alive for the entire app lifetime,
/// otherwise the `MenuChild` objects backing the native NSMenuItems get freed and clicking
/// any menu item crashes (dangling pointer to freed MenuChild).
pub struct AppMenu {
    /// Must stay alive. Dropping this frees the MenuChild backing data.
    pub _menu: Menu,
    pub ids: MenuIds,
}

/// Build the native menu bar. The caller MUST keep the returned `AppMenu` alive.
pub fn create_menu_bar() -> AppMenu {
    let menu = Menu::new();

    // App menu (macOS puts the first menu under the app name)
    let app_menu = Submenu::new("Prvw", true);
    let about = MenuItem::new("About Prvw", true, None);
    let settings = MenuItem::new(
        "Settings\u{2026}",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Comma)),
    );
    app_menu
        .append_items(&[
            &about,
            &settings,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])
        .expect("Failed to build app menu");

    // File menu
    let file_menu = Submenu::new("File", true);
    file_menu
        .append_items(&[&PredefinedMenuItem::close_window(None)])
        .expect("Failed to build file menu");

    // View menu
    let view_menu = Submenu::new("View", true);
    let zoom_in = MenuItem::new("Zoom in", true, None);
    let zoom_out = MenuItem::new("Zoom out", true, None);
    let actual_size = MenuItem::new("Actual size", true, None);
    let fit_to_window = MenuItem::new("Fit to window", true, None);
    let fullscreen = MenuItem::new("Fullscreen", true, None);
    view_menu
        .append_items(&[
            &zoom_in,
            &zoom_out,
            &PredefinedMenuItem::separator(),
            &actual_size,
            &fit_to_window,
            &PredefinedMenuItem::separator(),
            &fullscreen,
        ])
        .expect("Failed to build view menu");

    // Navigate menu
    let nav_menu = Submenu::new("Navigate", true);
    let previous = MenuItem::new("Previous      ←", true, None);
    let next = MenuItem::new("Next            →", true, None);
    nav_menu
        .append_items(&[&previous, &next])
        .expect("Failed to build navigate menu");

    menu.append_items(&[&app_menu, &file_menu, &view_menu, &nav_menu])
        .expect("Failed to build menu bar");

    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    log::debug!("Menu bar created");

    AppMenu {
        _menu: menu,
        ids: MenuIds {
            about: about.id().clone(),
            settings: settings.id().clone(),
            zoom_in: zoom_in.id().clone(),
            zoom_out: zoom_out.id().clone(),
            actual_size: actual_size.id().clone(),
            fit_to_window: fit_to_window.id().clone(),
            fullscreen: fullscreen.id().clone(),
            previous: previous.id().clone(),
            next: next.id().clone(),
        },
    }
}

/// Check for pending menu events (non-blocking).
pub fn poll_menu_event() -> Option<MenuEvent> {
    MenuEvent::receiver().try_recv().ok()
}
