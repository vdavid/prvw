use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Identifiers for custom menu actions.
pub struct MenuIds {
    pub about: MenuId,
    pub settings: MenuId,
    pub zoom_in: MenuId,
    pub zoom_out: MenuId,
    pub actual_size: MenuId,
    pub fit_to_window: MenuId,
    pub auto_fit_window: MenuId,
    pub enlarge_small_images: MenuId,
    pub icc_color_management: MenuId,
    pub color_match_display: MenuId,
    pub relative_colorimetric: MenuId,
    pub fullscreen: MenuId,
    pub refresh: MenuId,
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
    /// Kept so we can update the checkmark from outside (e.g., when settings window toggles it).
    pub auto_fit_item: CheckMenuItem,
    pub enlarge_small_item: CheckMenuItem,
    pub icc_color_management_item: CheckMenuItem,
    pub color_match_item: CheckMenuItem,
    pub relative_colorimetric_item: CheckMenuItem,
}

/// Build the native menu bar. The caller MUST keep the returned `AppMenu` alive.
pub fn create_menu_bar() -> AppMenu {
    let menu = Menu::new();

    // App menu (macOS puts the first menu under the app name)
    let app_menu = Submenu::new("Prvw", true);
    let about = MenuItem::new("About Prvw", true, None);
    let settings_item = MenuItem::new(
        "Settings\u{2026}",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Comma)),
    );
    app_menu
        .append_items(&[
            &about,
            &settings_item,
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
    let zoom_in = MenuItem::new(
        "Zoom in",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Equal)),
    );
    let zoom_out = MenuItem::new(
        "Zoom out",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Minus)),
    );
    let actual_size = MenuItem::new(
        "Actual size",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Digit0)),
    );
    let fit_to_window = MenuItem::new("Fit to window", true, None);
    let settings = crate::settings::Settings::load();
    let auto_fit_window =
        CheckMenuItem::new("Auto-fit window", true, settings.auto_fit_window, None);
    // Disabled when auto-fit is on (irrelevant — window matches image anyway)
    let enlarge_enabled = !settings.auto_fit_window;
    let enlarge_small_images = CheckMenuItem::new(
        "Enlarge small images",
        enlarge_enabled,
        settings.enlarge_small_images,
        None,
    );
    let icc_color_management = CheckMenuItem::new(
        "ICC color management",
        true,
        settings.icc_color_management,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyI,
        )),
    );
    // Disabled when ICC color management is off (L2 depends on L1)
    let color_match_enabled = settings.icc_color_management;
    let color_match_display = CheckMenuItem::new(
        "Color match display",
        color_match_enabled,
        settings.color_match_display,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyC,
        )),
    );
    let relative_colorimetric = CheckMenuItem::new(
        "Relative colorimetric",
        settings.icc_color_management,
        settings.use_relative_colorimetric,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyR,
        )),
    );
    let fullscreen = MenuItem::new("Fullscreen", true, None);
    let refresh = MenuItem::new("Refresh", true, None);
    view_menu
        .append_items(&[
            &zoom_in,
            &zoom_out,
            &PredefinedMenuItem::separator(),
            &actual_size,
            &fit_to_window,
            &auto_fit_window,
            &enlarge_small_images,
            &PredefinedMenuItem::separator(),
            &icc_color_management,
            &color_match_display,
            &relative_colorimetric,
            &PredefinedMenuItem::separator(),
            &fullscreen,
            &PredefinedMenuItem::separator(),
            &refresh,
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

    let auto_fit_id = auto_fit_window.id().clone();
    let enlarge_small_id = enlarge_small_images.id().clone();
    let icc_color_management_id = icc_color_management.id().clone();
    let color_match_id = color_match_display.id().clone();
    let relative_colorimetric_id = relative_colorimetric.id().clone();

    AppMenu {
        auto_fit_item: auto_fit_window,
        enlarge_small_item: enlarge_small_images,
        icc_color_management_item: icc_color_management,
        color_match_item: color_match_display,
        relative_colorimetric_item: relative_colorimetric,
        _menu: menu,
        ids: MenuIds {
            about: about.id().clone(),
            settings: settings_item.id().clone(),
            zoom_in: zoom_in.id().clone(),
            zoom_out: zoom_out.id().clone(),
            actual_size: actual_size.id().clone(),
            fit_to_window: fit_to_window.id().clone(),
            auto_fit_window: auto_fit_id,
            enlarge_small_images: enlarge_small_id,
            icc_color_management: icc_color_management_id,
            color_match_display: color_match_id,
            relative_colorimetric: relative_colorimetric_id,
            fullscreen: fullscreen.id().clone(),
            refresh: refresh.id().clone(),
            previous: previous.id().clone(),
            next: next.id().clone(),
        },
    }
}

/// Check for pending menu events (non-blocking).
pub fn poll_menu_event() -> Option<MenuEvent> {
    MenuEvent::receiver().try_recv().ok()
}
