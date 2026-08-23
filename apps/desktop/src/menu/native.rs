//! The muda-backed menu bar, for the platforms that have a native one.
//!
//! See `menu/mod.rs` for the seam this sits behind and for the one-way flow between settings
//! and menu state.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

use crate::commands::AppCommand;
use crate::navigation::SortBy;
#[cfg(target_os = "macos")]
use crate::parity::Mismatch;
use crate::parity::command_keys::CommandParity;
use crate::parity::menu_items::MenuItemKey;
use crate::parity::{Audit, Coverage, Platform};
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

    /// Whether this platform's menus carry `key` at all, straight out of `parity`.
    ///
    /// Two registries answer, and they answer different questions. An item the menu registry
    /// calls `NotApplicable` has no meaning here (hiding an app is a macOS convention). An item
    /// whose action `parity::command_keys` calls `Missing` would be a dead end: clicking it runs
    /// a command `execute_command` drops, so offering it is worse than not having it.
    ///
    /// That's the whole suppression mechanism. Taking a feature off a platform means flipping a
    /// coverage arm and watching `docs/parity.md` move, never adding a `#[cfg]` here. Image
    /// browser off macOS is today's case (M1 step 3 of `docs/specs/cross-platform-plan.md`);
    /// M5 flips it back by building the thing.
    ///
    /// Takes the platform rather than reading `Platform::HOST`, so a Mac can check what
    /// Windows and Linux end up with. The registries carry no `#[cfg]` for the same reason.
    fn offers(key: MenuItemKey, platform: Platform) -> bool {
        if matches!(key.coverage(platform), Coverage::NotApplicable { .. }) {
            return false;
        }
        match key.command() {
            // The items the toolkit acts on itself (Hide, Quit, Close window) have no action
            // to check, so the menu registry's own answer above is the only one.
            None => true,
            Some(command) => command.coverage(platform) != Coverage::Missing,
        }
    }

    /// A plain item that dispatches a command when clicked. `None` where this platform doesn't
    /// offer it, which is what keeps it out of the menu and out of the audit alike.
    fn item(&mut self, key: MenuItemKey, accelerator: Option<Accelerator>) -> Option<MenuItem> {
        if !Self::offers(key, Platform::HOST) {
            return None;
        }
        let item = MenuItem::new(key.title(), true, accelerator);
        self.ids.push((item.id().clone(), key));
        self.audit.record(key);
        Some(item)
    }

    /// A checkable item. Built unchecked and enabled; `sync_from_settings` gives it its real
    /// state, so settings-to-menu stays one mapping.
    fn check_item(
        &mut self,
        key: MenuItemKey,
        accelerator: Option<Accelerator>,
    ) -> Option<CheckMenuItem> {
        if !Self::offers(key, Platform::HOST) {
            return None;
        }
        let item = CheckMenuItem::new(key.title(), true, false, accelerator);
        self.ids.push((item.id().clone(), key));
        self.audit.record(key);
        Some(item)
    }

    /// One of muda's predefined items, which the toolkit and the OS act on themselves. There's
    /// no id to route: `MenuItemKey::command` says `None` for exactly these.
    fn predefined(
        &mut self,
        key: MenuItemKey,
        item: PredefinedMenuItem,
    ) -> Option<PredefinedMenuItem> {
        if !Self::offers(key, Platform::HOST) {
            return None;
        }
        self.audit.record(key);
        Some(item)
    }

    /// Hand over the id table, after checking the menu against what the registry declared.
    fn finish(self) -> Vec<(MenuId, MenuItemKey)> {
        // Windows joins this when M4 attaches the menu bar (`init_for_hwnd` has no call site
        // yet) and flips its arms in `parity::menu_items` from `Missing` to `Present`. Until
        // then nobody can reach the bar there, so checking the built set against the
        // declaration would only report that known gap 30 times over.
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

/// One position in a menu being assembled: an item, or a separator.
///
/// [`Slot::Item`] carries `None` for an item this platform doesn't offer (see
/// [`MenuBuilder::offers`]), which is what lets one menu definition serve every platform.
enum Slot<'a> {
    Item(Option<&'a dyn IsMenuItem>),
    Separator,
}

impl<'a> Slot<'a> {
    /// A slot for whatever [`MenuBuilder::item`] or [`MenuBuilder::check_item`] returned.
    fn of<T: IsMenuItem>(item: &'a Option<T>) -> Self {
        Slot::Item(item.as_ref().map(|item| item as &dyn IsMenuItem))
    }
}

/// Put `slots` into `menu`, skipping the items this platform doesn't offer and the separators
/// they would strand: no leading separator, no trailing one, and no two in a row. A gap where
/// an item used to be reads as a broken menu, so the grouping has to survive the filter.
fn fill(menu: &Submenu, slots: &[Slot<'_>]) {
    let mut separator_pending = false;
    let mut filled = false;
    for slot in slots {
        match slot {
            Slot::Separator => separator_pending = filled,
            Slot::Item(None) => {}
            Slot::Item(Some(item)) => {
                if separator_pending {
                    menu.append(&PredefinedMenuItem::separator())
                        .expect("Failed to append a menu separator");
                    separator_pending = false;
                }
                menu.append(*item).expect("Failed to append a menu item");
                filled = true;
            }
        }
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
    /// `None` on a platform the registry says doesn't offer the item, so every write goes
    /// through [`set_checked`] / [`set_enabled`] rather than testing for it inline.
    auto_fit_item: Option<CheckMenuItem>,
    enlarge_small_item: Option<CheckMenuItem>,
    icc_color_management_item: Option<CheckMenuItem>,
    color_match_item: Option<CheckMenuItem>,
    relative_colorimetric_item: Option<CheckMenuItem>,
    histogram_item: Option<CheckMenuItem>,
    exif_info_item: Option<CheckMenuItem>,
    sort_by_name_item: Option<CheckMenuItem>,
    sort_by_date_item: Option<CheckMenuItem>,
    sort_by_file_type_item: Option<CheckMenuItem>,
    loop_navigation_item: Option<CheckMenuItem>,
    /// Start/Stop slideshow. Kept so `set_slideshow_running` can flip the label.
    slideshow_toggle_item: Option<MenuItem>,
    /// Image browser / Image view. Kept so `set_browse_mode` can flip the label. `None` off
    /// macOS, where browse mode doesn't exist yet (M5).
    browse_toggle_item: Option<MenuItem>,
}

/// Tick or untick an item this platform may not have.
fn set_checked(item: &Option<CheckMenuItem>, checked: bool) {
    if let Some(item) = item {
        item.set_checked(checked);
    }
}

/// Enable or grey out an item this platform may not have.
fn set_enabled(item: &Option<CheckMenuItem>, enabled: bool) {
    if let Some(item) = item {
        item.set_enabled(enabled);
    }
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
        set_checked(&self.auto_fit_item, settings.auto_fit_window);
        set_checked(&self.enlarge_small_item, settings.enlarge_small_images);
        set_checked(
            &self.icc_color_management_item,
            settings.icc_color_management,
        );
        // "Color match display" and "Relative colorimetric" are L2 toggles: they only mean
        // anything with ICC color management (L1) on, so they follow it in and out of enabled.
        set_checked(&self.color_match_item, settings.color_match_display);
        set_enabled(&self.color_match_item, settings.icc_color_management);
        set_checked(
            &self.relative_colorimetric_item,
            settings.use_relative_colorimetric,
        );
        set_enabled(
            &self.relative_colorimetric_item,
            settings.icc_color_management,
        );
        set_checked(&self.histogram_item, settings.histogram_visible);
        set_checked(&self.exif_info_item, settings.exif_visible);
        // muda has no native radio group, so "exactly one checked" is enforced here by writing
        // all three every time.
        set_checked(
            &self.sort_by_name_item,
            matches!(settings.sort_by, SortBy::Name),
        );
        set_checked(
            &self.sort_by_date_item,
            matches!(settings.sort_by, SortBy::Date),
        );
        set_checked(
            &self.sort_by_file_type_item,
            matches!(settings.sort_by, SortBy::FileType),
        );
        set_checked(&self.loop_navigation_item, settings.loop_navigation);
    }

    /// Flip the Slideshow menu's first item between "Start slideshow" and "Stop slideshow".
    pub fn set_slideshow_running(&self, running: bool) {
        if let Some(item) = &self.slideshow_toggle_item {
            item.set_text(slideshow_toggle_label(running));
        }
    }

    /// Flip the Navigate menu's first item between "Image browser" and "Image view". A no-op
    /// where the registry says there's no image browser to switch to.
    pub fn set_browse_mode(&self, browsing: bool) {
        if let Some(item) = &self.browse_toggle_item {
            item.set_text(browse_toggle_label(browsing));
        }
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
                self.auto_fit_item.as_ref()?.is_checked(),
            )),
            MenuItemKey::EnlargeSmallImages => Some(AppCommand::SetEnlargeSmallImages(
                self.enlarge_small_item.as_ref()?.is_checked(),
            )),
            MenuItemKey::IccColorManagement => Some(AppCommand::SetIccColorManagement(
                self.icc_color_management_item.as_ref()?.is_checked(),
            )),
            MenuItemKey::ColorMatchDisplay => Some(AppCommand::SetColorMatchDisplay(
                self.color_match_item.as_ref()?.is_checked(),
            )),
            MenuItemKey::RelativeColorimetric => Some(AppCommand::SetRelativeColorimetric(
                self.relative_colorimetric_item.as_ref()?.is_checked(),
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
    let hide = build.predefined(MenuItemKey::Hide, PredefinedMenuItem::hide(None));
    let hide_others = build.predefined(
        MenuItemKey::HideOthers,
        PredefinedMenuItem::hide_others(None),
    );
    let show_all = build.predefined(MenuItemKey::ShowAll, PredefinedMenuItem::show_all(None));
    let quit = build.predefined(MenuItemKey::Quit, PredefinedMenuItem::quit(None));
    fill(
        &app_menu,
        &[
            Slot::of(&about),
            Slot::of(&settings_item),
            Slot::Separator,
            Slot::of(&hide),
            Slot::of(&hide_others),
            Slot::of(&show_all),
            Slot::Separator,
            Slot::of(&quit),
        ],
    );

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
    let close_window = build.predefined(
        MenuItemKey::CloseWindow,
        PredefinedMenuItem::close_window(None),
    );
    fill(
        &file_menu,
        &[
            Slot::of(&open),
            Slot::Separator,
            Slot::of(&print),
            Slot::Separator,
            Slot::of(&close_window),
        ],
    );

    // Edit menu. Only Copy — Cut/Paste/Select All make no sense in a viewer, and
    // showing them disabled would look broken.
    let edit_menu = Submenu::new("Edit", true);
    let copy = build.item(
        MenuItemKey::Copy,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyC)),
    );
    fill(&edit_menu, &[Slot::of(&copy)]);

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
    fill(
        &sort_by_submenu,
        &[
            Slot::of(&sort_by_name),
            Slot::of(&sort_by_date),
            Slot::of(&sort_by_file_type),
        ],
    );

    // The Sort by submenu is a menu, not an item, so it has no registry key of its own; it
    // rides along when at least one of the three sort items survived the filter.
    let sort_by_entry = (!sort_by_submenu.items().is_empty()).then_some(sort_by_submenu.clone());
    fill(
        &view_menu,
        &[
            Slot::of(&zoom_in),
            Slot::of(&zoom_out),
            Slot::Separator,
            Slot::of(&actual_size),
            Slot::of(&fit_to_window),
            Slot::of(&auto_fit_window),
            Slot::of(&enlarge_small_images),
            Slot::Separator,
            Slot::of(&icc_color_management),
            Slot::of(&color_match_display),
            Slot::of(&relative_colorimetric),
            Slot::Separator,
            Slot::of(&histogram),
            Slot::of(&exif_info),
            Slot::of(&sort_by_entry),
            Slot::Separator,
            Slot::of(&fullscreen),
            Slot::Separator,
            Slot::of(&refresh),
        ],
    );

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
    fill(
        &nav_menu,
        &[
            Slot::of(&browse_toggle),
            Slot::Separator,
            Slot::of(&previous),
            Slot::of(&next),
            Slot::Separator,
            Slot::of(&go_to_first),
            Slot::of(&go_to_last),
            Slot::Separator,
            Slot::of(&loop_navigation),
        ],
    );

    // Slideshow menu
    let slideshow_menu = Submenu::new("Slideshow", true);
    let slideshow_toggle = build.item(
        MenuItemKey::SlideshowToggle,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyS)),
    );
    // `]` / `[` are shown cosmetically, for the same reason the bare letters above are.
    let slideshow_increase_speed = build.item(MenuItemKey::SlideshowIncreaseSpeed, None);
    let slideshow_decrease_speed = build.item(MenuItemKey::SlideshowDecreaseSpeed, None);
    fill(
        &slideshow_menu,
        &[
            Slot::of(&slideshow_toggle),
            Slot::Separator,
            Slot::of(&slideshow_increase_speed),
            Slot::of(&slideshow_decrease_speed),
        ],
    );

    // Help menu. Left empty on purpose: macOS auto-adds its Spotlight-style "Search" field
    // to any menu titled "Help", which is all we want here.
    let help_menu = Submenu::new("Help", true);

    // A menu the filter emptied is dropped rather than shown blank. Help is the exception: it
    // is built empty on purpose, because AppKit fills it with its own search field.
    for submenu in [
        &app_menu,
        &file_menu,
        &edit_menu,
        &view_menu,
        &nav_menu,
        &slideshow_menu,
    ] {
        if !submenu.items().is_empty() {
            menu.append(submenu).expect("Failed to build menu bar");
        }
    }
    menu.append(&help_menu).expect("Failed to build menu bar");

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
    for item in [context_copy, context_print].iter().flatten() {
        context_menu
            .append(item)
            .expect("Failed to build context menu");
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS ships every menu item, so the filter has to be a no-op there. If this fails, the
    /// Mac build just lost a menu item.
    #[test]
    fn macos_offers_every_item() {
        for key in MenuItemKey::ALL {
            assert!(
                MenuBuilder::offers(*key, Platform::MacOs),
                "{} vanished from the macOS menu bar",
                key.name()
            );
        }
    }

    /// What the two platforms without chrome drop, and why. The image browser is the live case
    /// (M1 step 3): it's suppressed because `CommandKey::BrowseMode` says `Missing`, so M5
    /// brings the item back by building the feature rather than by touching this file.
    #[test]
    fn platforms_without_the_feature_dont_offer_the_item() {
        for platform in [Platform::Windows, Platform::Linux] {
            let dropped: Vec<&str> = MenuItemKey::ALL
                .iter()
                .filter(|key| !MenuBuilder::offers(**key, platform))
                .map(|key| key.name())
                .collect();
            assert_eq!(
                dropped,
                vec![
                    "About",
                    "Settings",
                    "Hide",
                    "HideOthers",
                    "ShowAll",
                    "Print",
                    "Copy",
                    "BrowseToggle",
                    "ContextCopy",
                    "ContextPrint",
                ],
                "on {}",
                platform.name()
            );
        }
    }

    /// The items a person can still reach off macOS, once the bar attaches there (M4). Open is
    /// the one this milestone added, and it's the reason an empty window isn't a dead end.
    #[test]
    fn open_survives_on_every_platform() {
        for platform in Platform::ALL {
            assert!(MenuBuilder::offers(MenuItemKey::Open, *platform));
        }
    }
}
