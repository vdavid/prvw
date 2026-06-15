use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::navigation::SortBy;

/// Identifiers for custom menu actions.
pub struct MenuIds {
    pub about: MenuId,
    pub settings: MenuId,
    pub copy: MenuId,
    pub context_copy: MenuId,
    pub print: MenuId,
    pub context_print: MenuId,
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
    pub histogram: MenuId,
    pub exif_info: MenuId,
    pub sort_by_name: MenuId,
    pub sort_by_date: MenuId,
    pub sort_by_file_type: MenuId,
    pub previous: MenuId,
    pub next: MenuId,
    pub go_to_first: MenuId,
    pub go_to_last: MenuId,
    pub loop_navigation: MenuId,
    pub slideshow_toggle: MenuId,
    pub slideshow_increase_speed: MenuId,
    pub slideshow_decrease_speed: MenuId,
}

/// The menu bar and its action IDs. The `Menu` must be kept alive for the entire app lifetime,
/// otherwise the `MenuChild` objects backing the native NSMenuItems get freed and clicking
/// any menu item crashes (dangling pointer to freed MenuChild).
pub struct AppMenu {
    /// Must stay alive. Dropping this frees the MenuChild backing data.
    pub _menu: Menu,
    /// Right-click context menu, shown via `ContextMenu::show_context_menu_for_nsview`.
    /// Kept alive for the same reason as `_menu`: dropping it frees its MenuChild backing.
    /// The container is only read by the macOS-gated show path (the item IDs are
    /// dispatched cross-platform via `MenuIds`), so it's dead code off macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub context_menu: Menu,
    /// Menu delegates that strip AppKit's auto-injected items. AppKit holds delegates
    /// weakly, so these must live for the app's lifetime.
    #[cfg(target_os = "macos")]
    pub _menu_pruners: Vec<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    pub ids: MenuIds,
    /// Kept so we can update the checkmark from outside (e.g., when settings window toggles it).
    pub auto_fit_item: CheckMenuItem,
    pub enlarge_small_item: CheckMenuItem,
    pub icc_color_management_item: CheckMenuItem,
    pub color_match_item: CheckMenuItem,
    pub relative_colorimetric_item: CheckMenuItem,
    pub histogram_item: CheckMenuItem,
    pub exif_info_item: CheckMenuItem,
    pub sort_by_name_item: CheckMenuItem,
    pub sort_by_date_item: CheckMenuItem,
    pub sort_by_file_type_item: CheckMenuItem,
    pub loop_navigation_item: CheckMenuItem,
    /// Start/Stop slideshow. Kept so the label can flip between "Start
    /// slideshow" and "Stop slideshow" when the slideshow toggles.
    pub slideshow_toggle_item: MenuItem,
}

/// macOS auto-injects text-editing items into any menu it recognizes as "Edit" (Writing
/// Tools, AutoFill, Start Dictation, Emoji & Symbols). Prvw is a viewer with no text input,
/// so none of them belong. Two of them are suppressed by user defaults that AppKit reads
/// when building the menu; the rest (no public toggle as of macOS 15) are stripped after.
#[cfg(target_os = "macos")]
fn suppress_auto_edit_menu_items() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    // SAFETY: standard NSUserDefaults calls on the main thread.
    unsafe {
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        for key in [
            "NSDisabledDictationMenuItem",
            "NSDisabledCharacterPaletteMenuItem",
        ] {
            let k = NSString::from_str(key);
            let _: () = msg_send![defaults, setBool: true, forKey: &*k];
        }
    }
}

/// Build the native menu bar. The caller MUST keep the returned `AppMenu` alive.
pub fn create_menu_bar() -> AppMenu {
    #[cfg(target_os = "macos")]
    suppress_auto_edit_menu_items();

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
    let print = MenuItem::new(
        "Print\u{2026}",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyP)),
    );
    file_menu
        .append_items(&[
            &print,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::close_window(None),
        ])
        .expect("Failed to build file menu");

    // Edit menu. Only Copy — Cut/Paste/Select All make no sense in a viewer, and
    // showing them disabled would look broken.
    let edit_menu = Submenu::new("Edit", true);
    let copy = MenuItem::new(
        "Copy image",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyC)),
    );
    edit_menu
        .append_items(&[&copy])
        .expect("Failed to build edit menu");

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
    // Always enabled: even with auto-fit on, this governs fullscreen (where auto-fit is inert).
    let enlarge_small_images = CheckMenuItem::new(
        "Enlarge small images",
        true,
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
    // Bare-letter shortcuts (F, H, E) are shown cosmetically — padded into the title, like
    // the Navigate arrows — rather than as real accelerators: a bare-letter menu key
    // equivalent is app-global and would hijack typing those letters into the Settings text
    // fields. The bare keys themselves are handled in `input`.
    let fullscreen = MenuItem::new("Fullscreen        F", true, None);
    let refresh = MenuItem::new("Refresh", true, None);
    let histogram =
        CheckMenuItem::new("Histogram        H", true, settings.histogram_visible, None);
    let exif_info = CheckMenuItem::new("Exif info        E", true, settings.exif_visible, None);

    // muda has no native radio group, so the handler enforces "exactly one
    // checked" by re-syncing all three after every SetSortBy.
    let sort_by_submenu = Submenu::new("Sort by", true);
    let sort_by_name =
        CheckMenuItem::new("Name", true, matches!(settings.sort_by, SortBy::Name), None);
    let sort_by_date =
        CheckMenuItem::new("Date", true, matches!(settings.sort_by, SortBy::Date), None);
    let sort_by_file_type = CheckMenuItem::new(
        "File type",
        true,
        matches!(settings.sort_by, SortBy::FileType),
        None,
    );
    sort_by_submenu
        .append_items(&[&sort_by_name, &sort_by_date, &sort_by_file_type])
        .expect("Failed to build sort by submenu");

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
            &histogram,
            &exif_info,
            &sort_by_submenu,
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
    let go_to_first = MenuItem::new(
        "Go to first",
        true,
        Some(Accelerator::new(None, Code::Home)),
    );
    let go_to_last = MenuItem::new("Go to last", true, Some(Accelerator::new(None, Code::End)));
    let loop_navigation =
        CheckMenuItem::new("Loop navigation", true, settings.loop_navigation, None);
    nav_menu
        .append_items(&[
            &previous,
            &next,
            &PredefinedMenuItem::separator(),
            &go_to_first,
            &go_to_last,
            &PredefinedMenuItem::separator(),
            &loop_navigation,
        ])
        .expect("Failed to build navigate menu");

    // Slideshow menu
    let slideshow_menu = Submenu::new("Slideshow", true);
    let slideshow_toggle = MenuItem::new(
        "Start slideshow",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyS)),
    );
    // `]` / `[` are shown cosmetically (padded into the title, like the
    // Navigate arrows) rather than as real accelerators: bare-key menu
    // equivalents are app-global and would hijack typing into Settings text
    // fields. The bare `]` / `[` keys are handled in `input`.
    let slideshow_increase_speed = MenuItem::new("Increase speed      ]", true, None);
    let slideshow_decrease_speed = MenuItem::new("Decrease speed     [", true, None);
    slideshow_menu
        .append_items(&[
            &slideshow_toggle,
            &PredefinedMenuItem::separator(),
            &slideshow_increase_speed,
            &slideshow_decrease_speed,
        ])
        .expect("Failed to build slideshow menu");

    // Help menu. Left empty on purpose: macOS auto-adds its Spotlight-style "Search" field
    // to any menu titled "Help", which is all we want here.
    let help_menu = Submenu::new("Help", true);

    menu.append_items(&[
        &app_menu,
        &file_menu,
        &edit_menu,
        &view_menu,
        &nav_menu,
        &slideshow_menu,
        &help_menu,
    ])
    .expect("Failed to build menu bar");

    #[cfg(target_os = "macos")]
    let menu_pruners;
    #[cfg(target_os = "macos")]
    {
        menu.init_for_nsapp();
        // Strip AppKit's auto-injected items (Edit: Writing Tools/AutoFill/etc.; View:
        // Enter Full Screen) before each open. Must run after `init_for_nsapp`.
        menu_pruners = crate::platform::macos::menu_cleanup::install();
    }

    // Right-click context menu. A separate menu (not part of the menu bar) with its own
    // Copy and Print items; both routes funnel to the same commands via
    // `input::menu_to_command`.
    let context_menu = Menu::new();
    let context_copy = MenuItem::new("Copy image", true, None);
    let context_print = MenuItem::new("Print\u{2026}", true, None);
    context_menu
        .append_items(&[&context_copy, &context_print])
        .expect("Failed to build context menu");

    log::debug!("Menu bar created");

    let auto_fit_id = auto_fit_window.id().clone();
    let enlarge_small_id = enlarge_small_images.id().clone();
    let icc_color_management_id = icc_color_management.id().clone();
    let color_match_id = color_match_display.id().clone();
    let relative_colorimetric_id = relative_colorimetric.id().clone();
    let histogram_id = histogram.id().clone();
    let exif_info_id = exif_info.id().clone();
    let sort_by_name_id = sort_by_name.id().clone();
    let sort_by_date_id = sort_by_date.id().clone();
    let sort_by_file_type_id = sort_by_file_type.id().clone();
    let loop_navigation_id = loop_navigation.id().clone();
    let slideshow_toggle_id = slideshow_toggle.id().clone();
    let slideshow_increase_speed_id = slideshow_increase_speed.id().clone();
    let slideshow_decrease_speed_id = slideshow_decrease_speed.id().clone();

    AppMenu {
        auto_fit_item: auto_fit_window,
        enlarge_small_item: enlarge_small_images,
        icc_color_management_item: icc_color_management,
        color_match_item: color_match_display,
        relative_colorimetric_item: relative_colorimetric,
        histogram_item: histogram,
        exif_info_item: exif_info,
        sort_by_name_item: sort_by_name,
        sort_by_date_item: sort_by_date,
        sort_by_file_type_item: sort_by_file_type,
        loop_navigation_item: loop_navigation,
        slideshow_toggle_item: slideshow_toggle,
        _menu: menu,
        context_menu,
        #[cfg(target_os = "macos")]
        _menu_pruners: menu_pruners,
        ids: MenuIds {
            about: about.id().clone(),
            settings: settings_item.id().clone(),
            copy: copy.id().clone(),
            context_copy: context_copy.id().clone(),
            print: print.id().clone(),
            context_print: context_print.id().clone(),
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
            histogram: histogram_id,
            exif_info: exif_info_id,
            sort_by_name: sort_by_name_id,
            sort_by_date: sort_by_date_id,
            sort_by_file_type: sort_by_file_type_id,
            previous: previous.id().clone(),
            next: next.id().clone(),
            go_to_first: go_to_first.id().clone(),
            go_to_last: go_to_last.id().clone(),
            loop_navigation: loop_navigation_id,
            slideshow_toggle: slideshow_toggle_id,
            slideshow_increase_speed: slideshow_increase_speed_id,
            slideshow_decrease_speed: slideshow_decrease_speed_id,
        },
    }
}

/// Check for pending menu events (non-blocking).
pub fn poll_menu_event() -> Option<MenuEvent> {
    MenuEvent::receiver().try_recv().ok()
}
