//! The muda-backed menu bar, for the platforms that have a native one.
//!
//! See `menu/mod.rs` for the seam this sits behind and for the one-way flow between settings
//! and menu state.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::commands::AppCommand;
use crate::navigation::SortBy;
use crate::parity::Audit;
use crate::parity::command_keys::CommandParity;
use crate::parity::menu_items::MenuItemKey;
#[cfg(target_os = "macos")]
use crate::parity::{Mismatch, Platform};
use crate::settings::Settings;

/// Builds menu items from registry keys, and remembers what it built.
///
/// Every item goes through here, so an item can't reach a menu without naming the
/// [`MenuItemKey`] it satisfies: the key is where the title comes from, and the pairing it
/// records is how a click finds its command. [`MenuBuilder::finish`] then checks the built set
/// against what the registry says this platform owes, which is the runtime half of the parity
/// guarantee (`parity::menu_items`).
struct MenuBuilder {
    ids: Vec<(MenuId, MenuItemKey)>,
    audit: Audit<MenuItemKey>,
}

impl MenuBuilder {
    fn new() -> Self {
        Self {
            ids: Vec::new(),
            audit: Audit::new(),
        }
    }

    /// A plain item that dispatches a command when clicked.
    fn item(&mut self, key: MenuItemKey, accelerator: Option<Accelerator>) -> MenuItem {
        let item = MenuItem::new(key.title(), true, accelerator);
        self.ids.push((item.id().clone(), key));
        self.audit.record(key);
        item
    }

    /// A checkable item. Built unchecked and enabled; `sync_from_settings` gives it its real
    /// state, so settings-to-menu stays one mapping.
    fn check_item(&mut self, key: MenuItemKey, accelerator: Option<Accelerator>) -> CheckMenuItem {
        let item = CheckMenuItem::new(key.title(), true, false, accelerator);
        self.ids.push((item.id().clone(), key));
        self.audit.record(key);
        item
    }

    /// One of muda's predefined items, which the toolkit and the OS act on themselves. There's
    /// no id to route: `MenuItemKey::command` says `None` for exactly these.
    fn predefined(&mut self, key: MenuItemKey, item: PredefinedMenuItem) -> PredefinedMenuItem {
        self.audit.record(key);
        item
    }

    /// Hand over the id table, after checking the menu against what the registry declared.
    fn finish(self) -> Vec<(MenuId, MenuItemKey)> {
        // Windows joins this when M4 attaches the menu bar (`init_for_hwnd` has no call site
        // yet) and flips its arms in `parity::menu_items` from `Missing` to `Present`. Until
        // then muda builds the items there but nobody can reach them, so checking the built
        // set against the declaration would only report that known gap 30 times over.
        #[cfg(target_os = "macos")]
        {
            let mismatches = self
                .audit
                .mismatches(MenuItemKey::declared(Platform::MacOs));
            for mismatch in &mismatches {
                match mismatch {
                    Mismatch::Declared(key) => log::error!(
                        "Menu parity: macOS declares {} present, but no item was built for it",
                        key.name()
                    ),
                    Mismatch::Undeclared(key, coverage) => log::error!(
                        "Menu parity: an item was built for {}, which macOS declares {}",
                        key.name(),
                        coverage.status()
                    ),
                }
            }
            debug_assert!(
                mismatches.is_empty(),
                "the menu bar and parity::menu_items disagree: {mismatches:?}"
            );
        }
        self.ids
    }
}

/// The menu bar and its action IDs. The `Menu` must be kept alive for the entire app lifetime,
/// otherwise the `MenuChild` objects backing the native menu items get freed and clicking
/// any menu item crashes (dangling pointer to freed MenuChild).
pub struct AppMenu {
    /// Must stay alive. Dropping this frees the MenuChild backing data.
    _menu: Menu,
    /// Right-click context menu, shown via `show_image_context_menu`. Kept alive for the same
    /// reason as `_menu`: dropping it frees its MenuChild backing. Its items dispatch through
    /// the same id table as the menu bar.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    context_menu: Menu,
    /// Menu delegates that strip AppKit's auto-injected items. AppKit holds delegates
    /// weakly, so these must live for the app's lifetime.
    #[cfg(target_os = "macos")]
    _menu_pruners: Vec<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    /// Every custom item's muda id, paired with the registry key it was built from. A click
    /// arrives as an id; this is what turns it back into a key `command_for` can match on.
    ids: Vec<(MenuId, MenuItemKey)>,
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
        MenuItemKey::BrowseToggle.title()
    }
}

/// The Slideshow menu's first item, which starts or stops the slideshow.
fn slideshow_toggle_label(running: bool) -> &'static str {
    if running {
        "Stop slideshow"
    } else {
        MenuItemKey::SlideshowToggle.title()
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
        let Some(key) = self.key_for(id) else {
            // Not one of ours: muda's predefined items and the ones AppKit injects come
            // through here too, and the toolkit has already acted on them.
            log::debug!("Menu: unhandled event {id:?}");
            return None;
        };
        log::debug!("Menu: {}", key.label());
        self.command_for(key)
    }

    /// Which registry item a muda click came from.
    fn key_for(&self, id: &MenuId) -> Option<MenuItemKey> {
        self.ids
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, key)| *key)
    }

    /// What a click on `key` runs, checked against the action the registry says the item is
    /// for. The registry works at the level of a feature and `dispatch` at the level of one
    /// command with its payload (three Sort by items, one `SetSortBy`), so neither can be
    /// derived from the other. This is what keeps them from drifting apart: in a debug build,
    /// an item wired to the wrong command trips the first time it's clicked.
    fn command_for(&self, key: MenuItemKey) -> Option<AppCommand> {
        let command = self.dispatch(key);
        debug_assert_eq!(
            command.as_ref().map(AppCommand::parity_key),
            key.command().map(CommandParity::Action),
            "the {} menu item runs something other than the action it's registered for",
            key.name()
        );
        command
    }

    /// The single table from menu item to command, for the menu bar and the context menu
    /// alike. The keyboard's twin lives in `input::key_to_command`.
    ///
    /// Exhaustive with no `_` arm on purpose: a new menu item can't be built without deciding
    /// what clicking it does. `None` is for the items the toolkit acts on itself.
    fn dispatch(&self, key: MenuItemKey) -> Option<AppCommand> {
        match key {
            MenuItemKey::About => Some(AppCommand::ShowAbout),
            MenuItemKey::Open => Some(AppCommand::ShowOpenDialog),
            MenuItemKey::Settings => Some(AppCommand::ShowSettings),
            MenuItemKey::Copy | MenuItemKey::ContextCopy => Some(AppCommand::CopyImage),
            MenuItemKey::Print | MenuItemKey::ContextPrint => Some(AppCommand::Print),
            MenuItemKey::ZoomIn => Some(AppCommand::ZoomIn),
            MenuItemKey::ZoomOut => Some(AppCommand::ZoomOut),
            MenuItemKey::ActualSize => Some(AppCommand::ActualSize),
            MenuItemKey::FitToWindow => Some(AppCommand::FitToWindow),
            MenuItemKey::Histogram => Some(AppCommand::ToggleHistogram),
            MenuItemKey::ExifInfo => Some(AppCommand::ToggleExifInfo),
            MenuItemKey::LoopNavigation => Some(AppCommand::ToggleLoopNavigation),
            MenuItemKey::SortByName => Some(AppCommand::SetSortBy(SortBy::Name)),
            MenuItemKey::SortByDate => Some(AppCommand::SetSortBy(SortBy::Date)),
            MenuItemKey::SortByFileType => Some(AppCommand::SetSortBy(SortBy::FileType)),
            MenuItemKey::Fullscreen => Some(AppCommand::ToggleFullscreen),
            MenuItemKey::Refresh => Some(AppCommand::Refresh),
            MenuItemKey::Previous => Some(AppCommand::NavigateDebounced(false)),
            MenuItemKey::Next => Some(AppCommand::NavigateDebounced(true)),
            MenuItemKey::GoToFirst => Some(AppCommand::GoToFirst),
            MenuItemKey::GoToLast => Some(AppCommand::GoToLast),
            MenuItemKey::BrowseToggle => Some(AppCommand::ToggleBrowseMode),
            MenuItemKey::SlideshowToggle => Some(AppCommand::ToggleSlideshow),
            MenuItemKey::SlideshowIncreaseSpeed => Some(AppCommand::IncreaseSlideshowSpeed),
            MenuItemKey::SlideshowDecreaseSpeed => Some(AppCommand::DecreaseSlideshowSpeed),

            // A CheckMenuItem auto-toggles on click, so these five carry the item's new state
            // in the command. The other checkable items (Histogram, Exif info, Loop
            // navigation) toggle app state instead, and their checkmark catches up through
            // `sync_from_settings`.
            MenuItemKey::AutoFitWindow => Some(AppCommand::SetAutoFitWindow(
                self.auto_fit_item.is_checked(),
            )),
            MenuItemKey::EnlargeSmallImages => Some(AppCommand::SetEnlargeSmallImages(
                self.enlarge_small_item.is_checked(),
            )),
            MenuItemKey::IccColorManagement => Some(AppCommand::SetIccColorManagement(
                self.icc_color_management_item.is_checked(),
            )),
            MenuItemKey::ColorMatchDisplay => Some(AppCommand::SetColorMatchDisplay(
                self.color_match_item.is_checked(),
            )),
            MenuItemKey::RelativeColorimetric => Some(AppCommand::SetRelativeColorimetric(
                self.relative_colorimetric_item.is_checked(),
            )),

            // Handled by muda and the OS, so there's nothing for the app to run.
            MenuItemKey::Hide
            | MenuItemKey::HideOthers
            | MenuItemKey::ShowAll
            | MenuItemKey::Quit
            | MenuItemKey::CloseWindow => None,
        }
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

    let mut build = MenuBuilder::new();
    let menu = Menu::new();

    // App menu (macOS puts the first menu under the app name)
    let app_menu = Submenu::new("Prvw", true);
    let about = build.item(MenuItemKey::About, None);
    let settings_item = build.item(
        MenuItemKey::Settings,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Comma)),
    );
    app_menu
        .append_items(&[
            &about,
            &settings_item,
            &PredefinedMenuItem::separator(),
            &build.predefined(MenuItemKey::Hide, PredefinedMenuItem::hide(None)),
            &build.predefined(
                MenuItemKey::HideOthers,
                PredefinedMenuItem::hide_others(None),
            ),
            &build.predefined(MenuItemKey::ShowAll, PredefinedMenuItem::show_all(None)),
            &PredefinedMenuItem::separator(),
            &build.predefined(MenuItemKey::Quit, PredefinedMenuItem::quit(None)),
        ])
        .expect("Failed to build app menu");

    // File menu
    let file_menu = Submenu::new("File", true);
    let open = build.item(
        MenuItemKey::Open,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyO)),
    );
    let print = build.item(
        MenuItemKey::Print,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyP)),
    );
    file_menu
        .append_items(&[
            &open,
            &PredefinedMenuItem::separator(),
            &print,
            &PredefinedMenuItem::separator(),
            &build.predefined(
                MenuItemKey::CloseWindow,
                PredefinedMenuItem::close_window(None),
            ),
        ])
        .expect("Failed to build file menu");

    // Edit menu. Only Copy — Cut/Paste/Select All make no sense in a viewer, and
    // showing them disabled would look broken.
    let edit_menu = Submenu::new("Edit", true);
    let copy = build.item(
        MenuItemKey::Copy,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyC)),
    );
    edit_menu
        .append_items(&[&copy])
        .expect("Failed to build edit menu");

    // View menu
    let view_menu = Submenu::new("View", true);
    let zoom_in = build.item(
        MenuItemKey::ZoomIn,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Equal)),
    );
    let zoom_out = build.item(
        MenuItemKey::ZoomOut,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Minus)),
    );
    let actual_size = build.item(
        MenuItemKey::ActualSize,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::Digit0)),
    );
    let fit_to_window = build.item(MenuItemKey::FitToWindow, None);
    // "Auto-fit window" and "Enlarge small images" stay enabled unconditionally: even with
    // auto-fit on, enlarging governs fullscreen, where auto-fit is inert.
    let auto_fit_window = build.check_item(MenuItemKey::AutoFitWindow, None);
    let enlarge_small_images = build.check_item(MenuItemKey::EnlargeSmallImages, None);
    let icc_color_management = build.check_item(
        MenuItemKey::IccColorManagement,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyI,
        )),
    );
    let color_match_display = build.check_item(
        MenuItemKey::ColorMatchDisplay,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyC,
        )),
    );
    let relative_colorimetric = build.check_item(
        MenuItemKey::RelativeColorimetric,
        Some(Accelerator::new(
            Some(Modifiers::SUPER | Modifiers::SHIFT),
            Code::KeyR,
        )),
    );
    // Bare-letter shortcuts (F, H, E) are shown cosmetically — the registry keeps them in the
    // item's title, like the Navigate arrows — rather than as real accelerators: a bare-letter
    // menu key equivalent is app-global and would hijack typing those letters into the
    // Settings text fields. The bare keys themselves are handled in `input`.
    let fullscreen = build.item(MenuItemKey::Fullscreen, None);
    let refresh = build.item(MenuItemKey::Refresh, None);
    let histogram = build.check_item(MenuItemKey::Histogram, None);
    let exif_info = build.check_item(MenuItemKey::ExifInfo, None);

    let sort_by_submenu = Submenu::new("Sort by", true);
    let sort_by_name = build.check_item(MenuItemKey::SortByName, None);
    let sort_by_date = build.check_item(MenuItemKey::SortByDate, None);
    let sort_by_file_type = build.check_item(MenuItemKey::SortByFileType, None);
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
    let browse_toggle = build.item(MenuItemKey::BrowseToggle, None);
    let previous = build.item(MenuItemKey::Previous, None);
    let next = build.item(MenuItemKey::Next, None);
    let go_to_first = build.item(
        MenuItemKey::GoToFirst,
        Some(Accelerator::new(None, Code::Home)),
    );
    let go_to_last = build.item(
        MenuItemKey::GoToLast,
        Some(Accelerator::new(None, Code::End)),
    );
    let loop_navigation = build.check_item(MenuItemKey::LoopNavigation, None);
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
    let slideshow_toggle = build.item(
        MenuItemKey::SlideshowToggle,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyS)),
    );
    // `]` / `[` are shown cosmetically, for the same reason the bare letters above are.
    let slideshow_increase_speed = build.item(MenuItemKey::SlideshowIncreaseSpeed, None);
    let slideshow_decrease_speed = build.item(MenuItemKey::SlideshowDecreaseSpeed, None);
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
    // Copy and Print items; both routes funnel to the same commands via `command_for`.
    let context_menu = Menu::new();
    let context_copy = build.item(MenuItemKey::ContextCopy, None);
    let context_print = build.item(MenuItemKey::ContextPrint, None);
    context_menu
        .append_items(&[&context_copy, &context_print])
        .expect("Failed to build context menu");

    log::debug!("Menu bar created");

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
        ids: build.finish(),
    };

    // The one place settings become menu state. Building the items unchecked and syncing here
    // keeps initial state and every later update on the same code path.
    app_menu.sync_from_settings(&Settings::load());

    Some(app_menu)
}
