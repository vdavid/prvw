//! The muda-backed menu bar, for the platforms that have a native one.
//!
//! See `menu/mod.rs` for the seam this sits behind and for the one-way flow between settings
//! and menu state.
//!
//! ## What is shared and what forks
//!
//! Every item comes from a `MenuItemKey`, so both platforms show the same set of things called
//! the same names. Two things fork, and only these two:
//!
//! - **How an item is dressed**: the shortcut it carries and the exact string it wears. That is
//!   [`chrome`], which is [`super::macos`] or [`super::windows`] depending on the host.
//! - **Where the app menu's items land**. macOS has an app menu; Windows has none, so About goes
//!   to Help, Settings to Tools, and Quit becomes File → Exit. That is the two `#[cfg]` blocks
//!   in [`create_menu_bar`], and it is the whole structural difference between the two bars.
//!
//! Everything else (which menus exist, what is in them, the order, the separators) is one
//! definition, and [`MenuBuilder::offers`] is what thins it per platform.

use muda::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use winit::window::Window;

use crate::commands::AppCommand;
use crate::navigation::SortBy;
use crate::parity::command_keys::CommandParity;
use crate::parity::menu_items::MenuItemKey;
use crate::parity::{Audit, Coverage, Mismatch, Platform};
use crate::settings::Settings;

/// How this platform dresses a menu item. See the module docs.
#[cfg(target_os = "macos")]
use super::macos as chrome;
#[cfg(target_os = "windows")]
use super::windows as chrome;

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
    ///
    /// The title and the shortcut both come from [`chrome`], so no call site here spells either:
    /// that is what keeps the Windows menu's mnemonics and Ctrl bindings out of the shared
    /// definition below.
    fn item(&mut self, key: MenuItemKey) -> Option<MenuItem> {
        if !Self::offers(key, Platform::HOST) {
            return None;
        }
        let item = MenuItem::new(
            chrome::title(key, key.label()),
            true,
            chrome::accelerator(key),
        );
        self.ids.push((item.id().clone(), key));
        self.audit.record(key);
        Some(item)
    }

    /// A checkable item. Built unchecked and enabled; `sync_from_settings` gives it its real
    /// state, so settings-to-menu stays one mapping.
    fn check_item(&mut self, key: MenuItemKey) -> Option<CheckMenuItem> {
        if !Self::offers(key, Platform::HOST) {
            return None;
        }
        let item = CheckMenuItem::new(
            chrome::title(key, key.label()),
            true,
            false,
            chrome::accelerator(key),
        );
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
    ///
    /// Runs on both platforms that have a bar. Nothing here is `#[cfg]`-gated by platform: the
    /// host's own declaration is what it is checked against, so a Windows arm that says
    /// `Present` for an item the Windows bar never builds is caught the first time the app
    /// starts there.
    fn finish(self) -> Vec<(MenuId, MenuItemKey)> {
        let host = Platform::HOST;
        let mismatches = self.audit.mismatches(MenuItemKey::declared(host));
        for mismatch in &mismatches {
            match mismatch {
                Mismatch::Declared(key) => log::error!(
                    "Menu parity: {} declares {} present, but no item was built for it",
                    host.name(),
                    key.name()
                ),
                Mismatch::Undeclared(key, coverage) => log::error!(
                    "Menu parity: an item was built for {}, which {} declares {}",
                    key.name(),
                    host.name(),
                    coverage.status()
                ),
            }
        }
        debug_assert!(
            mismatches.is_empty(),
            "the menu bar and parity::menu_items disagree: {mismatches:?}"
        );
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
/// mode it takes you back. `Enter` is advertised as a shortcut hint rather than a real
/// accelerator: a bare-key equivalent is app-global and would hijack typing into a text field.
///
/// Only image mode advertises it. Enter takes you into the browser, and once you are there the
/// focused pane owns the key (`browser::browse_keydown_command`), so nothing brings you back
/// with it.
fn browse_toggle_title(browsing: bool) -> String {
    let key = MenuItemKey::BrowseToggle;
    if browsing {
        "Image view".to_string()
    } else {
        chrome::title(key, key.label())
    }
}

/// The Slideshow menu's first item, which starts or stops the slideshow. Both names carry the
/// same shortcut, because the one key starts and stops alike.
fn slideshow_toggle_title(running: bool) -> String {
    let key = MenuItemKey::SlideshowToggle;
    let label = if running {
        "Stop slideshow"
    } else {
        key.label()
    };
    chrome::title(key, label)
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
            item.set_text(slideshow_toggle_title(running));
        }
    }

    /// Flip the Navigate menu's first item between "Image browser" and "Image view". A no-op
    /// where the registry says there's no image browser to switch to.
    pub fn set_browse_mode(&self, browsing: bool) {
        if let Some(item) = &self.browse_toggle_item {
            item.set_text(browse_toggle_title(browsing));
        }
    }

    /// Take the bar away for fullscreen, and put it back on the way out.
    ///
    /// Fullscreen is where the image really is the whole app, and no Windows app shows a menu
    /// bar there. There's no auto-hide and no setting for it: F11 is the whole story. macOS
    /// hides its own bar, so this is a no-op there.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        chrome::set_visible(!fullscreen);
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

/// Create a top-level menu and put it on the bar straight away, before anything fills it.
///
/// Joining the bar first is load-bearing on Windows, and silently so. muda registers an item's
/// accelerator into the **root** menu's `HACCEL` table as the item is appended (`AccelAction::add`
/// walks `root_menu_haccel_stores`), and a submenu that hasn't joined a root yet has no table to
/// register into. Appending the submenu afterwards doesn't go back for its children. Fill first
/// and every accelerator in the bar quietly does nothing, while the items still *show* their
/// shortcut, because the text is composed on a different path.
///
/// The menus the filter leaves empty come back off at the end of [`create_menu_bar`].
fn top_level(menu: &Menu, title: &'static str) -> Submenu {
    let submenu = Submenu::new(chrome::menu_title(title), true);
    menu.append(&submenu).expect("Failed to build menu bar");
    submenu
}

/// Build the native menu bar and put it up. The caller MUST keep the returned `AppMenu` alive.
///
/// Checkable items are built unchecked and enabled; the `sync_from_settings` at the end is what
/// gives them their real state, so there's one mapping from settings to menu, not two.
///
/// `window` is what a Windows bar attaches to. macOS hangs its bar off the application rather
/// than a window, so it ignores the argument.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn create_menu_bar(window: &Window) -> Option<AppMenu> {
    #[cfg(target_os = "macos")]
    suppress_auto_edit_menu_items();

    let mut build = MenuBuilder::new();
    let menu = Menu::new();

    // The whole bar, in order, before anything fills it. That order is what makes Windows
    // accelerators work at all; `top_level` has the why, and the menus the filter leaves empty
    // come back off at the end.
    //
    // macOS keeps About, Settings, and Quit under the app's own name. Windows has no app menu,
    // so About goes to Help, Settings to Tools, and Quit becomes File → Exit, which leaves the
    // app menu empty there and Tools and Help empty on macOS.
    let app_menu = top_level(&menu, "Prvw");
    let file_menu = top_level(&menu, "File");
    // Only Copy — Cut/Paste/Select all make no sense in a viewer, and showing them disabled
    // would look broken.
    let edit_menu = top_level(&menu, "Edit");
    let view_menu = top_level(&menu, "View");
    let nav_menu = top_level(&menu, "Navigate");
    let slideshow_menu = top_level(&menu, "Slideshow");
    let tools_menu = top_level(&menu, "Tools");
    // Help is left empty on macOS on purpose: AppKit auto-adds its Spotlight-style "Search"
    // field to any menu titled "Help", which is all we want there.
    let help_menu = top_level(&menu, "Help");

    let about = build.item(MenuItemKey::About);
    let settings_item = build.item(MenuItemKey::Settings);
    let open = build.item(MenuItemKey::Open);
    let print = build.item(MenuItemKey::Print);
    let quit = build.predefined(
        MenuItemKey::Quit,
        PredefinedMenuItem::quit(chrome::quit_text().as_deref()),
    );

    #[cfg(target_os = "macos")]
    {
        let hide = build.predefined(MenuItemKey::Hide, PredefinedMenuItem::hide(None));
        let hide_others = build.predefined(
            MenuItemKey::HideOthers,
            PredefinedMenuItem::hide_others(None),
        );
        let show_all = build.predefined(MenuItemKey::ShowAll, PredefinedMenuItem::show_all(None));
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
        // Close window belongs to a platform where an app outlives its last window.
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
    }
    #[cfg(target_os = "windows")]
    {
        fill(
            &file_menu,
            &[
                Slot::of(&open),
                Slot::Separator,
                Slot::of(&print),
                Slot::Separator,
                Slot::of(&quit),
            ],
        );
        fill(&tools_menu, &[Slot::of(&settings_item)]);
        // About is Help's only item and therefore its last, which is the Windows convention.
        fill(&help_menu, &[Slot::of(&about)]);
    }

    // Edit menu
    let copy = build.item(MenuItemKey::Copy);
    fill(&edit_menu, &[Slot::of(&copy)]);

    // View menu
    let zoom_in = build.item(MenuItemKey::ZoomIn);
    let zoom_out = build.item(MenuItemKey::ZoomOut);
    let actual_size = build.item(MenuItemKey::ActualSize);
    let fit_to_window = build.item(MenuItemKey::FitToWindow);
    // "Auto-fit window" and "Enlarge small images" stay enabled unconditionally: even with
    // auto-fit on, enlarging governs fullscreen, where auto-fit is inert.
    let auto_fit_window = build.check_item(MenuItemKey::AutoFitWindow);
    let enlarge_small_images = build.check_item(MenuItemKey::EnlargeSmallImages);
    let icc_color_management = build.check_item(MenuItemKey::IccColorManagement);
    let color_match_display = build.check_item(MenuItemKey::ColorMatchDisplay);
    let relative_colorimetric = build.check_item(MenuItemKey::RelativeColorimetric);
    let fullscreen = build.item(MenuItemKey::Fullscreen);
    let refresh = build.item(MenuItemKey::Refresh);
    let histogram = build.check_item(MenuItemKey::Histogram);
    let exif_info = build.check_item(MenuItemKey::ExifInfo);

    // Sort by is filled before it joins View, which would cost its items any accelerator they
    // carried (see `top_level`). None of the three has one, and
    // `the_sort_by_submenu_carries_no_accelerators` is what keeps that true.
    let sort_by_submenu = Submenu::new(chrome::menu_title("Sort by"), true);
    let sort_by_name = build.check_item(MenuItemKey::SortByName);
    let sort_by_date = build.check_item(MenuItemKey::SortByDate);
    let sort_by_file_type = build.check_item(MenuItemKey::SortByFileType);
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

    // Navigate menu. Top item swaps the main screen between the image viewer and the browse screen. Its label
    // flips by mode (`set_browse_mode`), like the slideshow Start/Stop item. `Enter` in image
    // mode is handled in `input`.
    let browse_toggle = build.item(MenuItemKey::BrowseToggle);
    let previous = build.item(MenuItemKey::Previous);
    let next = build.item(MenuItemKey::Next);
    let go_to_first = build.item(MenuItemKey::GoToFirst);
    let go_to_last = build.item(MenuItemKey::GoToLast);
    let loop_navigation = build.check_item(MenuItemKey::LoopNavigation);
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
    let slideshow_toggle = build.item(MenuItemKey::SlideshowToggle);
    let slideshow_increase_speed = build.item(MenuItemKey::SlideshowIncreaseSpeed);
    let slideshow_decrease_speed = build.item(MenuItemKey::SlideshowDecreaseSpeed);
    fill(
        &slideshow_menu,
        &[
            Slot::of(&slideshow_toggle),
            Slot::Separator,
            Slot::of(&slideshow_increase_speed),
            Slot::of(&slideshow_decrease_speed),
        ],
    );

    // A menu the filter emptied comes back off rather than showing blank, so a platform with no
    // Settings item shows no Tools menu at all. Help is the exception, and only on macOS: an
    // empty one is exactly what we want there, because AppKit fills it with its own search field.
    let keep_empty_help = cfg!(target_os = "macos");
    for submenu in [
        &app_menu,
        &file_menu,
        &edit_menu,
        &view_menu,
        &nav_menu,
        &slideshow_menu,
        &tools_menu,
    ]
    .into_iter()
    .chain((!keep_empty_help).then_some(&help_menu))
    {
        if submenu.items().is_empty() {
            menu.remove(submenu).expect("Failed to drop an empty menu");
        }
    }

    #[cfg(target_os = "macos")]
    let menu_pruners;
    #[cfg(target_os = "macos")]
    {
        menu.init_for_nsapp();
        // Strip AppKit's auto-injected items (Edit: Writing Tools/AutoFill/etc.; View:
        // Enter Full Screen) before each open. Must run after `init_for_nsapp`.
        menu_pruners = crate::platform::macos::menu_cleanup::install();
    }
    // Windows hangs the bar off the window, and points the message hook at its accelerator
    // table. Last, so the window gets a finished bar in one `DrawMenuBar` rather than watching
    // one grow. The table itself is already right whenever this runs: `top_level` is what
    // decides that, and the hook reads the `HACCEL` fresh on every message.
    #[cfg(target_os = "windows")]
    chrome::attach(&menu, window);

    // Right-click context menu. A separate menu (not part of the menu bar) with its own
    // Copy and Print items; both routes funnel to the same commands via `command_for`.
    let context_menu = Menu::new();
    let context_copy = build.item(MenuItemKey::ContextCopy);
    let context_print = build.item(MenuItemKey::ContextPrint);
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

    /// What each platform drops, and why. Windows' list is what M1 leaves behind: the
    /// clipboard, the print sheet, the settings window, the about box, and browse mode, each
    /// suppressed because `parity::command_keys` says `Missing`, so building the feature is
    /// what brings the item back rather than an edit to this file.
    #[test]
    fn platforms_without_the_feature_dont_offer_the_item() {
        assert_eq!(
            dropped_by(Platform::Windows),
            vec![
                "About",
                "Settings",
                "Hide",
                "HideOthers",
                "ShowAll",
                "Print",
                "CloseWindow",
                "Copy",
                "BrowseToggle",
                "ContextCopy",
                "ContextPrint",
            ]
        );
        // Linux has no bar at all, so nothing here reaches anyone there. Close window is the
        // one difference from Windows: it stays `Missing` rather than `NotApplicable`, because
        // no Linux window model has been decided (M8).
        assert_eq!(
            dropped_by(Platform::Linux),
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
            ]
        );
    }

    /// The static half of the audit `finish` runs, and the only half a Mac can check for
    /// Windows: what the filter lets through has to be exactly what the platform declares
    /// `Present`. A mismatch is a menu that logs a parity error the first time it opens.
    ///
    /// The two platforms that build a bar are the two the audit runs on, so they're the two
    /// checked here.
    #[test]
    fn what_a_platform_offers_is_what_it_declares() {
        for platform in [Platform::MacOs, Platform::Windows] {
            let offered: Vec<&str> = MenuItemKey::ALL
                .iter()
                .filter(|key| MenuBuilder::offers(**key, platform))
                .map(|key| key.name())
                .collect();
            let declared: Vec<&str> = MenuItemKey::ALL
                .iter()
                .filter(|key| key.coverage(platform) == Coverage::Present)
                .map(|key| key.name())
                .collect();
            assert_eq!(offered, declared, "on {}", platform.name());
        }
    }

    fn dropped_by(platform: Platform) -> Vec<&'static str> {
        MenuItemKey::ALL
            .iter()
            .filter(|key| !MenuBuilder::offers(**key, platform))
            .map(|key| key.name())
            .collect()
    }

    /// Sort by is filled before it joins the bar, and muda registers an accelerator into the
    /// root menu's table as the item is appended, so an accelerator on one of its three items
    /// would show in the menu and never fire. See `top_level`.
    #[test]
    fn the_sort_by_submenu_carries_no_accelerators() {
        for key in [
            MenuItemKey::SortByName,
            MenuItemKey::SortByDate,
            MenuItemKey::SortByFileType,
        ] {
            assert!(
                chrome::accelerator(key).is_none(),
                "{} grew an accelerator that would never fire",
                key.name()
            );
        }
    }

    /// The items a person can still reach off macOS. Open is the one M1 added, and it's the
    /// reason an empty window isn't a dead end.
    #[test]
    fn open_survives_on_every_platform() {
        for platform in Platform::ALL {
            assert!(MenuBuilder::offers(MenuItemKey::Open, *platform));
        }
    }
}
