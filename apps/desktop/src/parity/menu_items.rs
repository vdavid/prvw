//! Every menu item, and whether each platform's menu bar has it.
//!
//! `menu/native.rs` builds its items from these keys: the item wears
//! [`MenuItemKey::title`], and a click dispatches [`MenuItemKey::command`]. So an item can't
//! reach a menu bar without a key, and a key with no item is caught by the audit in
//! `create_menu_bar`.
//!
//! Coverage here answers "can a person reach it from a menu on this platform?". Whether the
//! action behind it does anything is [`super::command_keys`]'s question.

use super::command_keys::CommandKey;
use super::{Coverage, Platform};

/// Which menu an item hangs under. macOS puts the first menu under the app name; other
/// platforms move those items where their own conventions want them, which is a coverage-arm
/// detail rather than a change to the product's structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    App,
    File,
    Edit,
    View,
    Navigate,
    Slideshow,
    /// The right-click menu over the image, which is a separate menu from the bar.
    Context,
}

impl Menu {
    pub const fn name(self) -> &'static str {
        match self {
            Menu::App => "Prvw",
            Menu::File => "File",
            Menu::Edit => "Edit",
            Menu::View => "View",
            Menu::Navigate => "Navigate",
            Menu::Slideshow => "Slideshow",
            Menu::Context => "Context menu",
        }
    }
}

/// Declares the registry from one table, the same way [`super::setting_keys`] does.
///
/// `hint` is the bare-key shortcut a few items advertise in their own title, padding included.
/// Those keys can't be real accelerators (a bare-letter menu equivalent is app-global and would
/// hijack typing into a text field), so the title carries them cosmetically and `input` handles
/// the keys. Items with a real accelerator leave it empty.
macro_rules! menu_items {
    ($(
        $(#[$doc:meta])*
        $variant:ident {
            label: $label:literal,
            hint: $hint:literal,
            menu: $menu:ident,
            command: $command:expr,
        }
    )*) => {
        /// One variant per menu item.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum MenuItemKey {
            $( $(#[$doc])* $variant, )*
        }

        impl MenuItemKey {
            /// Every item, in the order the menus list them.
            pub const ALL: &'static [MenuItemKey] = &[ $( MenuItemKey::$variant, )* ];

            /// Stable identifier: the variant's own name.
            pub const fn name(self) -> &'static str {
                match self { $( MenuItemKey::$variant => stringify!($variant), )* }
            }

            /// What the item is called, without the cosmetic shortcut hint. For the items the
            /// toolkit builds (Hide, Quit, Close window) this is the name we call them: muda
            /// and AppKit supply their own localized titles.
            pub const fn label(self) -> &'static str {
                match self { $( MenuItemKey::$variant => $label, )* }
            }

            /// The cosmetic shortcut hint, padding included, or `""` for an item with none.
            /// `menu::macos` pads it onto the title; `menu::windows` has its own column.
            pub const fn hint(self) -> &'static str {
                match self { $( MenuItemKey::$variant => $hint, )* }
            }

            /// The exact string the menu item wears on macOS, hint included.
            pub const fn title(self) -> &'static str {
                match self { $( MenuItemKey::$variant => concat!($label, $hint), )* }
            }

            /// Which menu it hangs under.
            pub const fn menu(self) -> Menu {
                match self { $( MenuItemKey::$variant => Menu::$menu, )* }
            }

            /// The action a click runs. `None` for the items the toolkit handles itself
            /// (Hide, Quit, Close window), which the app never dispatches.
            pub const fn command(self) -> Option<CommandKey> {
                match self { $( MenuItemKey::$variant => $command, )* }
            }
        }
    };
}

menu_items! {
    // ── Prvw menu ────────────────────────────────────────────────────
    About { label: "About Prvw", hint: "", menu: App, command: Some(CommandKey::About), }
    Settings { label: "Settings\u{2026}", hint: "", menu: App, command: Some(CommandKey::Settings), }
    Hide { label: "Hide Prvw", hint: "", menu: App, command: None, }
    HideOthers { label: "Hide others", hint: "", menu: App, command: None, }
    ShowAll { label: "Show all", hint: "", menu: App, command: None, }
    Quit { label: "Quit Prvw", hint: "", menu: App, command: None, }

    // ── File menu ────────────────────────────────────────────────────
    Open { label: "Open\u{2026}", hint: "", menu: File, command: Some(CommandKey::OpenFile), }
    Print { label: "Print\u{2026}", hint: "", menu: File, command: Some(CommandKey::Print), }
    CloseWindow { label: "Close window", hint: "", menu: File, command: None, }

    // ── Edit menu ────────────────────────────────────────────────────
    Copy { label: "Copy image", hint: "", menu: Edit, command: Some(CommandKey::CopyImage), }

    // ── View menu ────────────────────────────────────────────────────
    ZoomIn { label: "Zoom in", hint: "", menu: View, command: Some(CommandKey::ZoomIn), }
    ZoomOut { label: "Zoom out", hint: "", menu: View, command: Some(CommandKey::ZoomOut), }
    ActualSize { label: "Actual size", hint: "", menu: View, command: Some(CommandKey::ActualSize), }
    FitToWindow { label: "Fit to window", hint: "", menu: View, command: Some(CommandKey::FitToWindow), }
    AutoFitWindow { label: "Auto-fit window", hint: "", menu: View, command: Some(CommandKey::AutoFitWindow), }
    EnlargeSmallImages { label: "Enlarge small images", hint: "", menu: View, command: Some(CommandKey::EnlargeSmallImages), }
    IccColorManagement { label: "ICC color management", hint: "", menu: View, command: Some(CommandKey::IccColorManagement), }
    ColorMatchDisplay { label: "Color match display", hint: "", menu: View, command: Some(CommandKey::ColorMatchDisplay), }
    RelativeColorimetric { label: "Relative colorimetric", hint: "", menu: View, command: Some(CommandKey::RelativeColorimetric), }
    Histogram { label: "Histogram", hint: "        H", menu: View, command: Some(CommandKey::Histogram), }
    ExifInfo { label: "Exif info", hint: "        E", menu: View, command: Some(CommandKey::ExifInfo), }
    SortByName { label: "Name", hint: "", menu: View, command: Some(CommandKey::SortBy), }
    SortByDate { label: "Date", hint: "", menu: View, command: Some(CommandKey::SortBy), }
    SortByFileType { label: "File type", hint: "", menu: View, command: Some(CommandKey::SortBy), }
    Fullscreen { label: "Fullscreen", hint: "        F", menu: View, command: Some(CommandKey::Fullscreen), }
    Refresh { label: "Refresh", hint: "", menu: View, command: Some(CommandKey::Refresh), }

    // ── Navigate menu ────────────────────────────────────────────────
    /// Its title flips with the mode (`set_browse_mode`), so `title` is only the image-mode
    /// half. `menu::native::browse_toggle_label` owns both.
    BrowseToggle { label: "Image browser", hint: "        \u{23ce}", menu: Navigate, command: Some(CommandKey::BrowseMode), }
    Previous { label: "Previous", hint: "      \u{2190}", menu: Navigate, command: Some(CommandKey::NextPreviousImage), }
    Next { label: "Next", hint: "            \u{2192}", menu: Navigate, command: Some(CommandKey::NextPreviousImage), }
    GoToFirst { label: "Go to first", hint: "", menu: Navigate, command: Some(CommandKey::GoToFirst), }
    GoToLast { label: "Go to last", hint: "", menu: Navigate, command: Some(CommandKey::GoToLast), }
    LoopNavigation { label: "Loop navigation", hint: "", menu: Navigate, command: Some(CommandKey::LoopNavigation), }

    // ── Slideshow menu ───────────────────────────────────────────────
    /// Its title flips between "Start slideshow" and "Stop slideshow"
    /// (`menu::native::slideshow_toggle_label`).
    SlideshowToggle { label: "Start slideshow", hint: "     S", menu: Slideshow, command: Some(CommandKey::Slideshow), }
    SlideshowIncreaseSpeed { label: "Increase speed", hint: "      ]", menu: Slideshow, command: Some(CommandKey::SlideshowSpeed), }
    SlideshowDecreaseSpeed { label: "Decrease speed", hint: "     [", menu: Slideshow, command: Some(CommandKey::SlideshowSpeed), }

    // ── Right-click menu over the image ──────────────────────────────
    ContextCopy { label: "Copy image", hint: "", menu: Context, command: Some(CommandKey::CopyImage), }
    ContextPrint { label: "Print\u{2026}", hint: "", menu: Context, command: Some(CommandKey::Print), }
}

impl MenuItemKey {
    /// Whether `platform`'s menus offer this item.
    pub const fn coverage(self, platform: Platform) -> Coverage {
        match platform {
            Platform::MacOs => self.macos_coverage(),
            Platform::Windows => self.windows_coverage(),
            Platform::Linux => self.linux_coverage(),
        }
    }

    /// macOS has the full menu bar and the context menu.
    const fn macos_coverage(self) -> Coverage {
        match self {
            MenuItemKey::About
            | MenuItemKey::Settings
            | MenuItemKey::Hide
            | MenuItemKey::HideOthers
            | MenuItemKey::ShowAll
            | MenuItemKey::Quit
            | MenuItemKey::Open
            | MenuItemKey::Print
            | MenuItemKey::CloseWindow
            | MenuItemKey::Copy
            | MenuItemKey::ZoomIn
            | MenuItemKey::ZoomOut
            | MenuItemKey::ActualSize
            | MenuItemKey::FitToWindow
            | MenuItemKey::AutoFitWindow
            | MenuItemKey::EnlargeSmallImages
            | MenuItemKey::IccColorManagement
            | MenuItemKey::ColorMatchDisplay
            | MenuItemKey::RelativeColorimetric
            | MenuItemKey::Histogram
            | MenuItemKey::ExifInfo
            | MenuItemKey::SortByName
            | MenuItemKey::SortByDate
            | MenuItemKey::SortByFileType
            | MenuItemKey::Fullscreen
            | MenuItemKey::Refresh
            | MenuItemKey::BrowseToggle
            | MenuItemKey::Previous
            | MenuItemKey::Next
            | MenuItemKey::GoToFirst
            | MenuItemKey::GoToLast
            | MenuItemKey::LoopNavigation
            | MenuItemKey::SlideshowToggle
            | MenuItemKey::SlideshowIncreaseSpeed
            | MenuItemKey::SlideshowDecreaseSpeed
            | MenuItemKey::ContextCopy
            | MenuItemKey::ContextPrint => Coverage::Present,
        }
    }

    /// Windows has a real menu bar: `menu::windows::attach` puts it on the winit window and
    /// `platform::windows::msg_hook` translates its accelerators. What's `Missing` here is
    /// nothing to do with the bar; it's the items whose action `command_keys` says Windows
    /// doesn't implement yet, which `MenuBuilder::offers` keeps out rather than showing dead.
    ///
    /// Where an item sits differs, because there's no app menu: About is Help's only item,
    /// Settings is Tools', and Quit is File → Exit. `menu::windows::decoration` owns that
    /// placement; `Menu` below stays the product's shared grouping.
    const fn windows_coverage(self) -> Coverage {
        match self {
            MenuItemKey::Hide | MenuItemKey::HideOthers | MenuItemKey::ShowAll => {
                Coverage::NotApplicable {
                    reason: "Hiding an app while leaving it running is a macOS app-menu \
                             convention. Windows minimizes windows instead, from the window \
                             itself rather than a menu.",
                }
            }
            MenuItemKey::CloseWindow => Coverage::NotApplicable {
                reason: "Prvw has one window on Windows, and a Windows app with no windows is \
                         an invisible process rather than a running app. Closing that window is \
                         exiting, which File → Exit already does.",
            },
            MenuItemKey::Quit
            | MenuItemKey::Open
            | MenuItemKey::ZoomIn
            | MenuItemKey::ZoomOut
            | MenuItemKey::ActualSize
            | MenuItemKey::FitToWindow
            | MenuItemKey::AutoFitWindow
            | MenuItemKey::EnlargeSmallImages
            | MenuItemKey::IccColorManagement
            | MenuItemKey::ColorMatchDisplay
            | MenuItemKey::RelativeColorimetric
            | MenuItemKey::Histogram
            | MenuItemKey::ExifInfo
            | MenuItemKey::SortByName
            | MenuItemKey::SortByDate
            | MenuItemKey::SortByFileType
            | MenuItemKey::Fullscreen
            | MenuItemKey::Refresh
            | MenuItemKey::Previous
            | MenuItemKey::Next
            | MenuItemKey::GoToFirst
            | MenuItemKey::GoToLast
            | MenuItemKey::LoopNavigation
            | MenuItemKey::SlideshowToggle
            | MenuItemKey::SlideshowIncreaseSpeed
            | MenuItemKey::SlideshowDecreaseSpeed
            | MenuItemKey::Copy
            | MenuItemKey::About
            | MenuItemKey::Settings
            | MenuItemKey::ContextCopy
            | MenuItemKey::Print
            | MenuItemKey::ContextPrint => Coverage::Present,
            // Browse mode (M5) is the only one left waiting on its own action. Copy's, Print's,
            // About's, and Settings' items are all built (M1 step 12, M3, M6, and M4), and a
            // right-click over the image pops the same context menu macOS shows.
            MenuItemKey::BrowseToggle => Coverage::Missing,
        }
    }

    /// Linux has no menu bar Prvw can attach: muda only offers `init_for_gtk_window` there and
    /// winit can't hand it a `gtk::Window`, which is why muda isn't even a Linux dependency
    /// (`menu/absent.rs`). Restoring these needs an in-app menu of some kind, which is a Linux
    /// spec's job. `input::key_to_command` is the only route there today.
    const fn linux_coverage(self) -> Coverage {
        match self {
            MenuItemKey::Hide | MenuItemKey::HideOthers | MenuItemKey::ShowAll => {
                Coverage::NotApplicable {
                    reason: "Hiding an app while leaving it running is a macOS app-menu \
                             convention, and no Linux desktop offers the equivalent from an \
                             app's own menu.",
                }
            }
            MenuItemKey::About
            | MenuItemKey::Settings
            | MenuItemKey::Quit
            | MenuItemKey::Open
            | MenuItemKey::Print
            | MenuItemKey::CloseWindow
            | MenuItemKey::Copy
            | MenuItemKey::ZoomIn
            | MenuItemKey::ZoomOut
            | MenuItemKey::ActualSize
            | MenuItemKey::FitToWindow
            | MenuItemKey::AutoFitWindow
            | MenuItemKey::EnlargeSmallImages
            | MenuItemKey::IccColorManagement
            | MenuItemKey::ColorMatchDisplay
            | MenuItemKey::RelativeColorimetric
            | MenuItemKey::Histogram
            | MenuItemKey::ExifInfo
            | MenuItemKey::SortByName
            | MenuItemKey::SortByDate
            | MenuItemKey::SortByFileType
            | MenuItemKey::Fullscreen
            | MenuItemKey::Refresh
            | MenuItemKey::BrowseToggle
            | MenuItemKey::Previous
            | MenuItemKey::Next
            | MenuItemKey::GoToFirst
            | MenuItemKey::GoToLast
            | MenuItemKey::LoopNavigation
            | MenuItemKey::SlideshowToggle
            | MenuItemKey::SlideshowIncreaseSpeed
            | MenuItemKey::SlideshowDecreaseSpeed
            | MenuItemKey::ContextCopy
            | MenuItemKey::ContextPrint => Coverage::Missing,
        }
    }

    /// What a platform's menu bar owes, for [`super::Audit::mismatches`].
    pub fn declared(platform: Platform) -> impl Iterator<Item = (MenuItemKey, Coverage)> {
        MenuItemKey::ALL
            .iter()
            .map(move |key| (*key, key.coverage(platform)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique() {
        for (index, key) in MenuItemKey::ALL.iter().enumerate() {
            let duplicate = MenuItemKey::ALL[index + 1..]
                .iter()
                .find(|other| other.name() == key.name());
            assert!(duplicate.is_none(), "{} is declared twice", key.name());
        }
    }

    /// The cosmetic hints are part of the item's title, and the padding in them is what lines
    /// the shortcuts up in the menu. Pin the composition so a refactor can't drop it.
    #[test]
    fn titles_carry_their_shortcut_hints() {
        assert_eq!(MenuItemKey::Previous.title(), "Previous      \u{2190}");
        assert_eq!(MenuItemKey::Fullscreen.title(), "Fullscreen        F");
        // Padded to the same column as its two neighbours in the Slideshow menu, which sit at
        // "Increase speed" plus six and "Decrease speed" plus five.
        assert_eq!(
            MenuItemKey::SlideshowToggle.title(),
            "Start slideshow     S"
        );
        assert_eq!(MenuItemKey::Refresh.title(), "Refresh");
    }

    /// Every item a click has to dispatch names an action. The four the toolkit handles
    /// itself are the only ones without one.
    #[test]
    fn only_toolkit_items_lack_a_command() {
        let without: Vec<&str> = MenuItemKey::ALL
            .iter()
            .filter(|key| key.command().is_none())
            .map(|key| key.name())
            .collect();
        assert_eq!(
            without,
            vec!["Hide", "HideOthers", "ShowAll", "Quit", "CloseWindow"]
        );
    }
}
