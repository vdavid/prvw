//! Every action the app can carry out, and what each platform actually does with it.
//!
//! `commands::AppCommand` is the vocabulary the whole app speaks; [`CommandKey`] is the subset
//! a person can invoke, which is the only part parity has an opinion about. `AppCommand`'s
//! `parity_key` maps one to the other through an exhaustive match, so a new command has to
//! declare itself an action or plumbing before it compiles.
//!
//! Coverage here answers "does running it do something on this platform?", not "can you reach
//! it?". Reachability is the menu registry's question ([`super::menu_items`]) and the
//! keyboard's (`input::key_to_command`). Both matter, and they fail differently: an
//! unimplemented action is a stub, an unreachable one is a missing menu.

use super::{Coverage, Platform};

/// The part of the app an action belongs to. Matches how the menus are organized, so the
/// parity table reads in the same order as the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Navigation,
    View,
    Browse,
    Slideshow,
    Raw,
    App,
}

impl Area {
    pub const fn name(self) -> &'static str {
        match self {
            Area::Navigation => "Navigation",
            Area::View => "View",
            Area::Browse => "Browse mode",
            Area::Slideshow => "Slideshow",
            Area::Raw => "RAW",
            Area::App => "App",
        }
    }
}

/// Declares the registry from one table, the same way [`super::setting_keys`] does.
macro_rules! command_keys {
    ($(
        $(#[$doc:meta])*
        $variant:ident { label: $label:literal, area: $area:ident, }
    )*) => {
        /// One variant per user-invocable action.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum CommandKey {
            $( $(#[$doc])* $variant, )*
        }

        impl CommandKey {
            /// Every action, in the order the parity table lists them.
            pub const ALL: &'static [CommandKey] = &[ $( CommandKey::$variant, )* ];

            /// Stable identifier: the variant's own name.
            pub const fn name(self) -> &'static str {
                match self { $( CommandKey::$variant => stringify!($variant), )* }
            }

            /// What the action is called, in the app's own words.
            pub const fn label(self) -> &'static str {
                match self { $( CommandKey::$variant => $label, )* }
            }

            /// Which part of the app it belongs to.
            pub const fn area(self) -> Area {
                match self { $( CommandKey::$variant => Area::$area, )* }
            }
        }
    };
}

command_keys! {
    // ── Navigation ───────────────────────────────────────────────────
    NextPreviousImage { label: "Next / previous image", area: Navigation, }
    GoToFirst { label: "Go to first", area: Navigation, }
    GoToLast { label: "Go to last", area: Navigation, }
    OpenFile { label: "Open a file", area: Navigation, }
    DropToOpen { label: "Drop files onto the window", area: Navigation, }
    LoopNavigation { label: "Loop navigation", area: Navigation, }
    SortBy { label: "Sort by", area: Navigation, }
    Refresh { label: "Refresh", area: Navigation, }

    // ── View ─────────────────────────────────────────────────────────
    ZoomIn { label: "Zoom in", area: View, }
    ZoomOut { label: "Zoom out", area: View, }
    SetZoom { label: "Set zoom level", area: View, }
    FitToWindow { label: "Fit to window", area: View, }
    ActualSize { label: "Actual size", area: View, }
    ToggleFit { label: "Toggle fit and actual size", area: View, }
    Fullscreen { label: "Fullscreen", area: View, }
    AutoFitWindow { label: "Auto-fit window", area: View, }
    EnlargeSmallImages { label: "Enlarge small images", area: View, }
    IccColorManagement { label: "ICC color management", area: View, }
    ColorMatchDisplay { label: "Color match display", area: View, }
    RelativeColorimetric { label: "Relative colorimetric", area: View, }
    ScrollToZoom { label: "Scroll to zoom", area: View, }
    PreloadNeighbors { label: "Preload next/prev images", area: View, }
    TitleBar { label: "Title bar", area: View, }
    Histogram { label: "Histogram", area: View, }
    ExifInfo { label: "Exif info", area: View, }

    // ── Browse mode ──────────────────────────────────────────────────
    BrowseMode { label: "Image browser and image view", area: Browse, }
    BrowseFocus { label: "Move focus between tree and grid", area: Browse, }
    BrowseOpenSelected { label: "Open the selected image", area: Browse, }

    // ── Slideshow ────────────────────────────────────────────────────
    Slideshow { label: "Start / stop slideshow", area: Slideshow, }
    SlideshowSeconds { label: "Time per image", area: Slideshow, }
    SlideshowCrossfade { label: "Crossfade", area: Slideshow, }
    SlideshowLoop { label: "Loop the slideshow", area: Slideshow, }
    SlideshowSpeed { label: "Increase / decrease speed", area: Slideshow, }

    // ── RAW ──────────────────────────────────────────────────────────
    RawPipelineFlags { label: "RAW pipeline stages", area: Raw, }
    CustomDcpDir { label: "Custom DCP directory", area: Raw, }

    // ── App ──────────────────────────────────────────────────────────
    CopyImage { label: "Copy image", area: App, }
    Print { label: "Print", area: App, }
    About { label: "About Prvw", area: App, }
    Settings { label: "Settings window", area: App, }
    Exit { label: "Exit", area: App, }
}

/// What a `commands::AppCommand` variant means to parity.
///
/// [`CommandParity::Internal`] is for the wiring a person never invokes: worker-thread
/// wakeups, folder-watch results, and the QA driving hooks the integration tests use because
/// they can't synthesize a native click. It is not the escape hatch for an action nobody got
/// around to registering. If a person can trigger it and would notice it missing, it's an
/// [`CommandParity::Action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandParity {
    Action(CommandKey),
    Internal,
}

impl CommandKey {
    /// What `platform` does when this action runs.
    pub const fn coverage(self, platform: Platform) -> Coverage {
        match platform {
            Platform::MacOs => self.macos_coverage(),
            Platform::Windows => self.windows_coverage(),
            Platform::Linux => self.linux_coverage(),
        }
    }

    /// macOS implements every action; `app/executor.rs` is written against it.
    const fn macos_coverage(self) -> Coverage {
        match self {
            CommandKey::NextPreviousImage
            | CommandKey::GoToFirst
            | CommandKey::GoToLast
            | CommandKey::OpenFile
            | CommandKey::DropToOpen
            | CommandKey::LoopNavigation
            | CommandKey::SortBy
            | CommandKey::Refresh
            | CommandKey::ZoomIn
            | CommandKey::ZoomOut
            | CommandKey::SetZoom
            | CommandKey::FitToWindow
            | CommandKey::ActualSize
            | CommandKey::ToggleFit
            | CommandKey::Fullscreen
            | CommandKey::AutoFitWindow
            | CommandKey::EnlargeSmallImages
            | CommandKey::IccColorManagement
            | CommandKey::ColorMatchDisplay
            | CommandKey::RelativeColorimetric
            | CommandKey::ScrollToZoom
            | CommandKey::PreloadNeighbors
            | CommandKey::TitleBar
            | CommandKey::Histogram
            | CommandKey::ExifInfo
            | CommandKey::BrowseMode
            | CommandKey::BrowseFocus
            | CommandKey::BrowseOpenSelected
            | CommandKey::Slideshow
            | CommandKey::SlideshowSeconds
            | CommandKey::SlideshowCrossfade
            | CommandKey::SlideshowLoop
            | CommandKey::SlideshowSpeed
            | CommandKey::RawPipelineFlags
            | CommandKey::CustomDcpDir
            | CommandKey::CopyImage
            | CommandKey::Print
            | CommandKey::About
            | CommandKey::Settings
            | CommandKey::Exit => Coverage::Present,
        }
    }

    /// Windows runs the whole platform-neutral half of `execute_command` already. What's
    /// missing is what the arms gate behind `#[cfg(target_os = "macos")]`: browse mode, and
    /// nothing else. The clipboard, the print dialog, the About box, and the settings dialog are
    /// all Windows' own now (`platform::windows::clipboard`, `platform::windows::print`,
    /// `about::windows`, `settings::windows`).
    const fn windows_coverage(self) -> Coverage {
        match self {
            CommandKey::TitleBar => Coverage::NotApplicable {
                reason: "The title bar never covers the image on Windows, so there's no strip \
                         to reserve and nothing for the command to switch.",
            },
            CommandKey::NextPreviousImage
            | CommandKey::GoToFirst
            | CommandKey::GoToLast
            | CommandKey::OpenFile
            | CommandKey::DropToOpen
            | CommandKey::LoopNavigation
            | CommandKey::SortBy
            | CommandKey::Refresh
            | CommandKey::ZoomIn
            | CommandKey::ZoomOut
            | CommandKey::SetZoom
            | CommandKey::FitToWindow
            | CommandKey::ActualSize
            | CommandKey::ToggleFit
            | CommandKey::Fullscreen
            | CommandKey::AutoFitWindow
            | CommandKey::EnlargeSmallImages
            | CommandKey::IccColorManagement
            | CommandKey::ColorMatchDisplay
            | CommandKey::RelativeColorimetric
            | CommandKey::ScrollToZoom
            | CommandKey::PreloadNeighbors
            | CommandKey::Histogram
            | CommandKey::ExifInfo
            | CommandKey::Slideshow
            | CommandKey::SlideshowSeconds
            | CommandKey::SlideshowCrossfade
            | CommandKey::SlideshowLoop
            | CommandKey::SlideshowSpeed
            | CommandKey::RawPipelineFlags
            | CommandKey::CustomDcpDir
            | CommandKey::CopyImage
            | CommandKey::About
            | CommandKey::Print
            | CommandKey::Settings
            | CommandKey::Exit => Coverage::Present,
            CommandKey::BrowseMode | CommandKey::BrowseFocus | CommandKey::BrowseOpenSelected => {
                Coverage::Missing
            }
        }
    }

    /// Linux keeps the platform-neutral arms and has nothing else: the AppKit gates are absent,
    /// and so is the clipboard, which is a decision rather than an oversight. Copy can't be
    /// invoked there at all (no menu bar, and `input::key_to_command` binds no copy key), and
    /// owning an X11 or Wayland selection is a Linux spec's problem to solve rather than a
    /// `#[cfg]` arm's — see `crate::clipboard`. Most of the rest is unreachable there anyway
    /// for want of a menu bar.
    const fn linux_coverage(self) -> Coverage {
        match self {
            CommandKey::TitleBar => Coverage::NotApplicable {
                reason: "Linux decorations sit outside the surface Prvw draws into, so the \
                         command has no strip to reserve or release.",
            },
            CommandKey::NextPreviousImage
            | CommandKey::GoToFirst
            | CommandKey::GoToLast
            | CommandKey::OpenFile
            | CommandKey::DropToOpen
            | CommandKey::LoopNavigation
            | CommandKey::SortBy
            | CommandKey::Refresh
            | CommandKey::ZoomIn
            | CommandKey::ZoomOut
            | CommandKey::SetZoom
            | CommandKey::FitToWindow
            | CommandKey::ActualSize
            | CommandKey::ToggleFit
            | CommandKey::Fullscreen
            | CommandKey::AutoFitWindow
            | CommandKey::EnlargeSmallImages
            | CommandKey::IccColorManagement
            | CommandKey::ColorMatchDisplay
            | CommandKey::RelativeColorimetric
            | CommandKey::ScrollToZoom
            | CommandKey::PreloadNeighbors
            | CommandKey::Histogram
            | CommandKey::ExifInfo
            | CommandKey::Slideshow
            | CommandKey::SlideshowSeconds
            | CommandKey::SlideshowCrossfade
            | CommandKey::SlideshowLoop
            | CommandKey::SlideshowSpeed
            | CommandKey::RawPipelineFlags
            | CommandKey::CustomDcpDir
            | CommandKey::Exit => Coverage::Present,
            CommandKey::BrowseMode
            | CommandKey::BrowseFocus
            | CommandKey::BrowseOpenSelected
            | CommandKey::CopyImage
            | CommandKey::Print
            | CommandKey::About
            | CommandKey::Settings => Coverage::Missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique() {
        for (index, key) in CommandKey::ALL.iter().enumerate() {
            let duplicate = CommandKey::ALL[index + 1..]
                .iter()
                .find(|other| other.name() == key.name());
            assert!(duplicate.is_none(), "{} is declared twice", key.name());
        }
    }

    /// Browse mode is the whole Windows gap in `execute_command` now. If that list shrinks,
    /// this test is where it gets noticed.
    #[test]
    fn windows_gap_is_browse_mode() {
        let missing: Vec<&str> = CommandKey::ALL
            .iter()
            .filter(|key| key.coverage(Platform::Windows) == Coverage::Missing)
            .map(|key| key.name())
            .collect();
        assert_eq!(
            missing,
            vec!["BrowseMode", "BrowseFocus", "BrowseOpenSelected"]
        );
    }
}
