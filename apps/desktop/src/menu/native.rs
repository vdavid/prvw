//! The muda-backed menu bar, for the platforms that have a native one.
//!
//! See `menu/mod.rs` for the seam this sits behind and for the one-way flow between settings
//! and menu state.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::commands::AppCommand;
use crate::navigation::SortBy;
use crate::settings::Settings;

/// Identifiers for custom menu actions. Private to this module: `menu_to_command` is the only
/// thing that reads them, so the rest of the app never handles a raw `MenuId`.
struct MenuIds {
    about: MenuId,
    settings: MenuId,
    copy: MenuId,
    context_copy: MenuId,
    print: MenuId,
    context_print: MenuId,
    zoom_in: MenuId,
    zoom_out: MenuId,
    actual_size: MenuId,
    fit_to_window: MenuId,
    auto_fit_window: MenuId,
    enlarge_small_images: MenuId,
    icc_color_management: MenuId,
    color_match_display: MenuId,
    relative_colorimetric: MenuId,
    fullscreen: MenuId,
    refresh: MenuId,
    histogram: MenuId,
    exif_info: MenuId,
    sort_by_name: MenuId,
    sort_by_date: MenuId,
    sort_by_file_type: MenuId,
    browse_toggle: MenuId,
    previous: MenuId,
    next: MenuId,
    go_to_first: MenuId,
    go_to_last: MenuId,
    loop_navigation: MenuId,
    slideshow_toggle: MenuId,
    slideshow_increase_speed: MenuId,
    slideshow_decrease_speed: MenuId,
}

/// The menu bar and its action IDs. The `Menu` must be kept alive for the entire app lifetime,
/// otherwise the `MenuChild` objects backing the native menu items get freed and clicking
/// any menu item crashes (dangling pointer to freed MenuChild).
pub struct AppMenu {
    /// Must stay alive. Dropping this frees the MenuChild backing data.
    _menu: Menu,
    /// Right-click context menu, shown via `show_image_context_menu`. Kept alive for the same
    /// reason as `_menu`: dropping it frees its MenuChild backing. Its items dispatch through
    /// the same `MenuIds` table as the menu bar.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    context_menu: Menu,
    /// Menu delegates that strip AppKit's auto-injected items. AppKit holds delegates
    /// weakly, so these must live for the app's lifetime.
    #[cfg(target_os = "macos")]
    _menu_pruners: Vec<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    ids: MenuIds,
    /// The checkable items, each mirroring one setting. `sync_from_settings` writes them all.
    auto_fit_item: CheckMenuItem,
    enlarge_small_item: CheckMenuItem,
    icc_color_management_item: CheckMenuItem,
    color_match_item: CheckMenuItem,
    relative_colorimetric_item: CheckMenuItem,
    histogram_item: CheckMenuItem,
    exif_info_item: CheckMenuItem,
    sort_by_name_item: CheckMenuItem,
    sort_by_date_item: CheckMenuItem,
    sort_by_file_type_item: CheckMenuItem,
    loop_navigation_item: CheckMenuItem,
    /// Start/Stop slideshow. Kept so `set_slideshow_running` can flip the label.
    slideshow_toggle_item: MenuItem,
    /// Image browser / Image view. Kept so `set_browse_mode` can flip the label.
    browse_toggle_item: MenuItem,
}

/// The Navigate menu's first item. In image mode the action takes you to the browser, in browse
/// mode it takes you back. The bare `Enter` shortcut is shown cosmetically (padded into the
/// title) rather than as a real accelerator: bare-key equivalents are app-global and would
/// hijack typing into text fields. Only image mode advertises it, because in browse mode the
/// focused pane owns Enter (`browser::browse_keydown_command`).
fn browse_toggle_label(browsing: bool) -> &'static str {
    if browsing {
        "Image view"
    } else {
        "Image browser        ⏎"
    }
}

/// The Slideshow menu's first item, which starts or stops the slideshow.
fn slideshow_toggle_label(running: bool) -> &'static str {
    if running {
        "Stop slideshow"
    } else {
        "Start slideshow"
    }
}

impl AppMenu {
    /// Push settings onto every checkmark and every enabled state. Idempotent, and the single
    /// place that maps settings to menu state: `create_menu_bar` ends with it, and every
    /// command that saves a setting calls it right after.
    pub fn sync_from_settings(&self, settings: &Settings) {
        self.auto_fit_item.set_checked(settings.auto_fit_window);
        self.enlarge_small_item
            .set_checked(settings.enlarge_small_images);
        self.icc_color_management_item
            .set_checked(settings.icc_color_management);
        // "Color match display" and "Relative colorimetric" are L2 toggles: they only mean
        // anything with ICC color management (L1) on, so they follow it in and out of enabled.
        self.color_match_item
            .set_checked(settings.color_match_display);
        self.color_match_item
            .set_enabled(settings.icc_color_management);
        self.relative_colorimetric_item
            .set_checked(settings.use_relative_colorimetric);
        self.relative_colorimetric_item
            .set_enabled(settings.icc_color_management);
        self.histogram_item.set_checked(settings.histogram_visible);
        self.exif_info_item.set_checked(settings.exif_visible);
        // muda has no native radio group, so "exactly one checked" is enforced here by writing
        // all three every time.
        self.sort_by_name_item
            .set_checked(matches!(settings.sort_by, SortBy::Name));
        self.sort_by_date_item
            .set_checked(matches!(settings.sort_by, SortBy::Date));
        self.sort_by_file_type_item
            .set_checked(matches!(settings.sort_by, SortBy::FileType));
        self.loop_navigation_item
            .set_checked(settings.loop_navigation);
    }

    /// Flip the Slideshow menu's first item between "Start slideshow" and "Stop slideshow".
    pub fn set_slideshow_running(&self, running: bool) {
        self.slideshow_toggle_item
            .set_text(slideshow_toggle_label(running));
    }

    /// Flip the Navigate menu's first item between "Image browser" and "Image view".
    pub fn set_browse_mode(&self, browsing: bool) {
        self.browse_toggle_item
            .set_text(browse_toggle_label(browsing));
    }

    /// Take the next pending menu click, if any, as an `AppCommand`. Non-blocking. Covers the
    /// menu bar and the context menu alike; both post to the same muda event channel.
    pub fn poll_command(&self) -> Option<AppCommand> {
        let event = MenuEvent::receiver().try_recv().ok()?;
        let id = event.id();

        // A CheckMenuItem auto-toggles on click, so these five read the item's new state and
        // carry it in the command. The other checkable items (Histogram, Exif info, Loop
        // navigation) map to a Toggle command that flips app state instead, so `menu_to_command`
        // handles them and their checkmark catches up through `sync_from_settings`.
        if id == &self.ids.auto_fit_window {
            let enabled = self.auto_fit_item.is_checked();
            log::debug!("Menu: Auto-fit window -> {enabled}");
            return Some(AppCommand::SetAutoFitWindow(enabled));
        }
        if id == &self.ids.enlarge_small_images {
            let enabled = self.enlarge_small_item.is_checked();
            log::debug!("Menu: Enlarge small images -> {enabled}");
            return Some(AppCommand::SetEnlargeSmallImages(enabled));
        }
        if id == &self.ids.icc_color_management {
            let enabled = self.icc_color_management_item.is_checked();
            log::debug!("Menu: ICC color management -> {enabled}");
            return Some(AppCommand::SetIccColorManagement(enabled));
        }
        if id == &self.ids.color_match_display {
            let enabled = self.color_match_item.is_checked();
            log::debug!("Menu: Color match display -> {enabled}");
            return Some(AppCommand::SetColorMatchDisplay(enabled));
        }
        if id == &self.ids.relative_colorimetric {
            let enabled = self.relative_colorimetric_item.is_checked();
            log::debug!("Menu: Relative colorimetric -> {enabled}");
            return Some(AppCommand::SetRelativeColorimetric(enabled));
        }

        let command = menu_to_command(id, &self.ids);
        if command.is_some() {
            log::debug!("Menu event: {id:?}");
        } else {
            log::debug!("Menu: unhandled event {id:?}");
        }
        command
    }

    /// Pop up the right-click context menu at the cursor, over the given `NSView`.
    ///
    /// # Safety
    ///
    /// `ns_view` must be a live `NSView*`.
    #[cfg(target_os = "macos")]
    pub unsafe fn show_image_context_menu(&self, ns_view: *const std::ffi::c_void) {
        use muda::ContextMenu;
        // SAFETY: the caller guarantees a live `NSView*`. A `None` position tells muda to use
        // the current mouse location.
        unsafe {
            self.context_menu
                .show_context_menu_for_nsview(ns_view, None);
        }
    }
}

/// Map a menu item's ID to an `AppCommand`. The single table for both the menu bar and the
/// context menu; the keyboard's twin lives in `input::key_to_command`.
///
/// Returns `None` for the checkable items that carry a value: `poll_command` handles those
/// first, because it has to read the item's freshly-toggled state.
fn menu_to_command(id: &MenuId, ids: &MenuIds) -> Option<AppCommand> {
    if id == &ids.about {
        Some(AppCommand::ShowAbout)
    } else if id == &ids.settings {
        Some(AppCommand::ShowSettings)
    } else if id == &ids.copy || id == &ids.context_copy {
        Some(AppCommand::CopyImage)
    } else if id == &ids.print || id == &ids.context_print {
        Some(AppCommand::Print)
    } else if id == &ids.zoom_in {
        Some(AppCommand::ZoomIn)
    } else if id == &ids.zoom_out {
        Some(AppCommand::ZoomOut)
    } else if id == &ids.actual_size {
        Some(AppCommand::ActualSize)
    } else if id == &ids.fit_to_window {
        Some(AppCommand::FitToWindow)
    } else if id == &ids.histogram {
        Some(AppCommand::ToggleHistogram)
    } else if id == &ids.exif_info {
        Some(AppCommand::ToggleExifInfo)
    } else if id == &ids.loop_navigation {
        Some(AppCommand::ToggleLoopNavigation)
    } else if id == &ids.sort_by_name {
        Some(AppCommand::SetSortBy(SortBy::Name))
    } else if id == &ids.sort_by_date {
        Some(AppCommand::SetSortBy(SortBy::Date))
    } else if id == &ids.sort_by_file_type {
        Some(AppCommand::SetSortBy(SortBy::FileType))
    } else if id == &ids.fullscreen {
        Some(AppCommand::ToggleFullscreen)
    } else if id == &ids.refresh {
        Some(AppCommand::Refresh)
    } else if id == &ids.previous {
        Some(AppCommand::NavigateDebounced(false))
    } else if id == &ids.next {
        Some(AppCommand::NavigateDebounced(true))
    } else if id == &ids.go_to_first {
        Some(AppCommand::GoToFirst)
    } else if id == &ids.go_to_last {
        Some(AppCommand::GoToLast)
    } else if id == &ids.browse_toggle {
        Some(AppCommand::ToggleBrowseMode)
    } else if id == &ids.slideshow_toggle {
        Some(AppCommand::ToggleSlideshow)
    } else if id == &ids.slideshow_increase_speed {
        Some(AppCommand::IncreaseSlideshowSpeed)
    } else if id == &ids.slideshow_decrease_speed {
        Some(AppCommand::DecreaseSlideshowSpeed)
    } else {
        None
    }
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
///
/// Checkable items are built unchecked and enabled; the `sync_from_settings` at the end is what
/// gives them their real state, so there's one mapping from settings to menu, not two.
pub fn create_menu_bar() -> Option<AppMenu> {
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
    // "Auto-fit window" and "Enlarge small images" stay enabled unconditionally: even with
    // auto-fit on, enlarging governs fullscreen, where auto-fit is inert.
    let auto_fit_window = CheckMenuItem::new("Auto-fit window", true, false, None);
    let enlarge_small_images = CheckMenuItem::new("Enlarge small images", true, false, None);
    let icc_color_management = CheckMenuItem::new(
        "ICC color management",
        true,
        false,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyI,
        )),
    );
    let color_match_display = CheckMenuItem::new(
        "Color match display",
        true,
        false,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyC,
        )),
    );
    let relative_colorimetric = CheckMenuItem::new(
        "Relative colorimetric",
        true,
        false,
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
    let histogram = CheckMenuItem::new("Histogram        H", true, false, None);
    let exif_info = CheckMenuItem::new("Exif info        E", true, false, None);

    let sort_by_submenu = Submenu::new("Sort by", true);
    let sort_by_name = CheckMenuItem::new("Name", true, false, None);
    let sort_by_date = CheckMenuItem::new("Date", true, false, None);
    let sort_by_file_type = CheckMenuItem::new("File type", true, false, None);
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
    // Top item swaps the main screen between the image viewer and the browse screen. Its label
    // flips by mode (`set_browse_mode`), like the slideshow Start/Stop item. `Enter` in image
    // mode is handled in `input`.
    let browse_toggle = MenuItem::new(browse_toggle_label(false), true, None);
    let previous = MenuItem::new("Previous      ←", true, None);
    let next = MenuItem::new("Next            →", true, None);
    let go_to_first = MenuItem::new(
        "Go to first",
        true,
        Some(Accelerator::new(None, Code::Home)),
    );
    let go_to_last = MenuItem::new("Go to last", true, Some(Accelerator::new(None, Code::End)));
    let loop_navigation = CheckMenuItem::new("Loop navigation", true, false, None);
    nav_menu
        .append_items(&[
            &browse_toggle,
            &PredefinedMenuItem::separator(),
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
        slideshow_toggle_label(false),
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
    let browse_toggle_id = browse_toggle.id().clone();
    let slideshow_increase_speed_id = slideshow_increase_speed.id().clone();
    let slideshow_decrease_speed_id = slideshow_decrease_speed.id().clone();

    let app_menu = AppMenu {
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
        browse_toggle_item: browse_toggle,
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
            browse_toggle: browse_toggle_id,
            previous: previous.id().clone(),
            next: next.id().clone(),
            go_to_first: go_to_first.id().clone(),
            go_to_last: go_to_last.id().clone(),
            loop_navigation: loop_navigation_id,
            slideshow_toggle: slideshow_toggle_id,
            slideshow_increase_speed: slideshow_increase_speed_id,
            slideshow_decrease_speed: slideshow_decrease_speed_id,
        },
    };

    // The one place settings become menu state. Building the items unchecked and syncing here
    // keeps initial state and every later update on the same code path.
    app_menu.sync_from_settings(&Settings::load());

    Some(app_menu)
}
