use muda::{
    AboutMetadata, Menu, MenuEvent, MenuId, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Modifiers as MudaModifiers},
};

/// Identifiers for custom menu actions.
pub struct MenuIds {
    pub zoom_in: MenuId,
    pub zoom_out: MenuId,
    pub actual_size: MenuId,
    pub fit_to_window: MenuId,
    pub fullscreen: MenuId,
    pub previous: MenuId,
    pub next: MenuId,
}

/// Build the native menu bar and return the action IDs for matching events.
pub fn create_menu_bar() -> MenuIds {
    let menu = Menu::new();

    // App menu (macOS puts the first menu under the app name)
    let app_menu = Submenu::new("Prvw", true);
    app_menu
        .append_items(&[
            &PredefinedMenuItem::about(
                None,
                Some(AboutMetadata {
                    name: Some("Prvw".to_string()),
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    ..Default::default()
                }),
            ),
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
    let zoom_in = muda::MenuItem::new(
        "Zoom in",
        true,
        Some(Accelerator::new(
            Some(MudaModifiers::SUPER),
            muda::accelerator::Code::Equal,
        )),
    );
    let zoom_out = muda::MenuItem::new(
        "Zoom out",
        true,
        Some(Accelerator::new(
            Some(MudaModifiers::SUPER),
            muda::accelerator::Code::Minus,
        )),
    );
    let actual_size = muda::MenuItem::new(
        "Actual size",
        true,
        Some(Accelerator::new(
            Some(MudaModifiers::SUPER),
            muda::accelerator::Code::Digit1,
        )),
    );
    let fit_to_window = muda::MenuItem::new(
        "Fit to window",
        true,
        Some(Accelerator::new(
            Some(MudaModifiers::SUPER),
            muda::accelerator::Code::Digit0,
        )),
    );
    let fullscreen = muda::MenuItem::new(
        "Fullscreen",
        true,
        Some(Accelerator::new(
            Some(MudaModifiers::SUPER),
            muda::accelerator::Code::KeyF,
        )),
    );
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
    // Note: we don't register ArrowLeft/ArrowRight as muda accelerators because muda 0.17
    // crashes with ZeroWidth icon error when processing bare arrow key accelerators on macOS.
    // Navigation is handled directly in the keyboard event handler in main.rs instead.
    let nav_menu = Submenu::new("Navigate", true);
    let previous = muda::MenuItem::new("Previous", true, None);
    let next = muda::MenuItem::new("Next", true, None);
    nav_menu
        .append_items(&[&previous, &next])
        .expect("Failed to build navigate menu");

    menu.append_items(&[&app_menu, &file_menu, &view_menu, &nav_menu])
        .expect("Failed to build menu bar");

    // On macOS, init the menu bar as the app menu
    menu.init_for_nsapp();

    log::debug!("Menu bar created");

    MenuIds {
        zoom_in: zoom_in.id().clone(),
        zoom_out: zoom_out.id().clone(),
        actual_size: actual_size.id().clone(),
        fit_to_window: fit_to_window.id().clone(),
        fullscreen: fullscreen.id().clone(),
        previous: previous.id().clone(),
        next: next.id().clone(),
    }
}

/// Check for pending menu events (non-blocking).
pub fn poll_menu_event() -> Option<MenuEvent> {
    MenuEvent::receiver().try_recv().ok()
}
