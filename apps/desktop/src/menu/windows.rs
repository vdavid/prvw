//! How a menu item is dressed on Windows, and how the bar gets onto the window.
//!
//! The shared half of a menu item is `parity::menu_items`: one [`MenuItemKey`] per item, one
//! `label` per key, the same string on every platform. Everything here is decoration on top of
//! that label, which is why Windows never renames anything: `&` marks the mnemonic Alt
//! underlines, a tab right-aligns the shortcut column, and Ctrl stands where macOS uses Command.
//! [`super::macos`] is the counterpart. Both compile on both platforms, so a Mac runs this
//! table's tests and a Windows build type-checks the other one (`menu/CLAUDE.md`).
//!
//! The one exception is File → Exit, which is Windows' name for Quit. muda supplies the title
//! for the items the toolkit owns, so the registry's label there "is the name we call them" and
//! [`decoration`] carries the real one.

use muda::accelerator::{Accelerator, Code, Modifiers};

use crate::parity::menu_items::MenuItemKey;

/// Which menu on the Windows bar an item hangs under.
///
/// `parity::menu_items::Menu` is the product's shared grouping and has no Tools or Help, because
/// macOS keeps About, Settings, and Quit in the app menu. Windows has no app menu, so those three
/// scatter: About to Help, Settings to Tools, Quit to File as Exit
/// (`docs/specs/windows-ui-design.md`). Placement is per-platform data rather than a comment
/// because [`tests::mnemonics_are_unique_within_a_menu`] needs to know which items compete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsMenu {
    File,
    Edit,
    View,
    /// The Sort by submenu, whose three items compete with each other and nothing else.
    SortBy,
    Navigate,
    Slideshow,
    Tools,
    Help,
    /// The right-click menu over the image.
    Context,
}

/// Everything Windows adds on top of an item's shared label.
pub struct Decoration {
    /// Which menu it hangs under here. Read by the mnemonic-uniqueness test rather than by the
    /// builder, which places items itself: nothing else can say which items compete for Alt+F.
    #[cfg_attr(not(test), allow(dead_code))]
    menu: WindowsMenu,
    /// The letter Alt underlines. Inserted before its first occurrence in the label, so the two
    /// items whose label flips with a mode keep their mnemonic through the flip.
    mnemonic: char,
    /// Windows' own name for the item, where it has one. Only File → Exit does.
    label: Option<&'static str>,
    /// The real accelerator, which muda turns into an `ACCEL` and renders into the item's
    /// shortcut column itself. `hint` stays empty when this is set.
    accelerator: Option<Accelerator>,
    /// Shortcut text for a key `input::key_to_command` handles rather than the accelerator
    /// table. Bare letters can't be accelerators (they would fire while a text field has focus),
    /// so the column advertises them and the key itself arrives as an ordinary keystroke.
    hint: &'static str,
}

impl Decoration {
    const fn new(menu: WindowsMenu, mnemonic: char) -> Self {
        Self {
            menu,
            mnemonic,
            label: None,
            accelerator: None,
            hint: "",
        }
    }

    const fn named(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    fn accel(mut self, modifiers: Option<Modifiers>, code: Code) -> Self {
        self.accelerator = Some(Accelerator::new(modifiers, code));
        self
    }

    const fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }
}

/// What Windows adds to `key`, or `None` where Windows never shows the item at all.
///
/// Exhaustive with no `_` arm, like every other table built from the registry: a new menu item
/// can't be added without deciding what it looks like here. `None` is only for the items the
/// registry already calls `NotApplicable` on Windows, so it never hides a gap.
pub fn decoration(key: MenuItemKey) -> Option<Decoration> {
    use WindowsMenu::{Context, Edit, File, Help, Navigate, Slideshow, SortBy, Tools, View};

    const CTRL: Modifiers = Modifiers::CONTROL;

    let decoration = match key {
        // ── The three that move, because Windows has no app menu ──────────
        MenuItemKey::About => Decoration::new(Help, 'a'),
        MenuItemKey::Settings => Decoration::new(Tools, 's').accel(Some(CTRL), Code::Comma),
        // "E&xit" is the classic Windows spelling; muda's own default is "&Exit". muda also
        // gives its predefined Quit a Ctrl+Q accelerator with no way to take it off, so the
        // item shows one where `docs/specs/windows-ui-design.md` asked for none. Ctrl+Q is a
        // real quit convention on Windows (Qt binds it by default), so it stays rather than
        // costing us the toolkit's own Quit item.
        MenuItemKey::Quit => Decoration::new(File, 'x').named("Exit"),

        // ── File ──────────────────────────────────────────────────────────
        MenuItemKey::Open => Decoration::new(File, 'o').accel(Some(CTRL), Code::KeyO),
        MenuItemKey::Print => Decoration::new(File, 'p').accel(Some(CTRL), Code::KeyP),

        // ── Edit ──────────────────────────────────────────────────────────
        MenuItemKey::Copy => Decoration::new(Edit, 'c').accel(Some(CTRL), Code::KeyC),

        // ── View ──────────────────────────────────────────────────────────
        MenuItemKey::ZoomIn => Decoration::new(View, 'z').accel(Some(CTRL), Code::Equal),
        MenuItemKey::ZoomOut => Decoration::new(View, 'o').accel(Some(CTRL), Code::Minus),
        MenuItemKey::ActualSize => Decoration::new(View, 'a').accel(Some(CTRL), Code::Digit0),
        MenuItemKey::FitToWindow => Decoration::new(View, 't'),
        MenuItemKey::AutoFitWindow => Decoration::new(View, 'u'),
        MenuItemKey::EnlargeSmallImages => Decoration::new(View, 'e'),
        MenuItemKey::IccColorManagement => {
            Decoration::new(View, 'i').accel(Some(CTRL | Modifiers::SHIFT), Code::KeyI)
        }
        MenuItemKey::ColorMatchDisplay => {
            Decoration::new(View, 'c').accel(Some(CTRL | Modifiers::SHIFT), Code::KeyC)
        }
        MenuItemKey::RelativeColorimetric => {
            Decoration::new(View, 'l').accel(Some(CTRL | Modifiers::SHIFT), Code::KeyR)
        }
        MenuItemKey::Histogram => Decoration::new(View, 'h').hint("H"),
        MenuItemKey::ExifInfo => Decoration::new(View, 'x').hint("E"),
        MenuItemKey::SortByName => Decoration::new(SortBy, 'n'),
        MenuItemKey::SortByDate => Decoration::new(SortBy, 'd'),
        MenuItemKey::SortByFileType => Decoration::new(SortBy, 'f'),
        // F11 and F5 are real accelerators, unlike macOS: they aren't typing keys, so they
        // can't hijack a text field, and F5 means refresh in Explorer and every browser.
        MenuItemKey::Fullscreen => Decoration::new(View, 'f').accel(None, Code::F11),
        MenuItemKey::Refresh => Decoration::new(View, 'r').accel(None, Code::F5),

        // ── Navigate ──────────────────────────────────────────────────────
        // The mnemonic has to occur in both of a flipping item's labels: 'i' is in "Image
        // browser" and "Image view" alike, 's' in "Start slideshow" and "Stop slideshow".
        MenuItemKey::BrowseToggle => Decoration::new(Navigate, 'i').hint("Enter"),
        MenuItemKey::Previous => Decoration::new(Navigate, 'p').hint("Left"),
        MenuItemKey::Next => Decoration::new(Navigate, 'n').hint("Right"),
        MenuItemKey::GoToFirst => Decoration::new(Navigate, 'f').accel(None, Code::Home),
        MenuItemKey::GoToLast => Decoration::new(Navigate, 'l').accel(None, Code::End),
        MenuItemKey::LoopNavigation => Decoration::new(Navigate, 'o').hint("L"),

        // ── Slideshow ─────────────────────────────────────────────────────
        // Bare S, and Ctrl+S stays bound to nothing: every other viewer-state toggle is a bare
        // letter, and Ctrl+S is where a Windows user's Save reflex lands. Doing nothing is the
        // right answer in an app that can't save.
        MenuItemKey::SlideshowToggle => Decoration::new(Slideshow, 's').hint("S"),
        MenuItemKey::SlideshowIncreaseSpeed => Decoration::new(Slideshow, 'i').hint("]"),
        MenuItemKey::SlideshowDecreaseSpeed => Decoration::new(Slideshow, 'd').hint("["),

        // ── Right-click menu over the image ───────────────────────────────
        MenuItemKey::ContextCopy => Decoration::new(Context, 'c'),
        MenuItemKey::ContextPrint => Decoration::new(Context, 'p'),

        // The registry calls these `NotApplicable` on Windows, so there's nothing to dress.
        MenuItemKey::Hide
        | MenuItemKey::HideOthers
        | MenuItemKey::ShowAll
        | MenuItemKey::CloseWindow => return None,
    };
    Some(decoration)
}

/// The exact string an item wears, mnemonic marker and shortcut column included.
///
/// `label` is what the item is called right now, which is [`MenuItemKey::label`] for every item
/// except the two that flip with a mode. `menu::native` composes those two, because whether a
/// mode advertises the item's key is a product question rather than a Windows one.
pub fn title(key: MenuItemKey, label: &str) -> String {
    let Some(decoration) = decoration(key) else {
        return label.to_string();
    };
    let mut text = with_mnemonic(decoration.label.unwrap_or(label), decoration.mnemonic);
    if !decoration.hint.is_empty() {
        // A tab is what makes Windows right-align the shortcut column. muda writes this
        // separator itself for an item that has a real accelerator, so only hints land here.
        text.push('\t');
        text.push_str(decoration.hint);
    }
    text
}

/// A top-level menu's own title, with its mnemonic. Alt+F then O is how a lot of Windows users
/// drive a menu bar, and a bar without mnemonics is one of the clearest tells that an app was
/// ported rather than written for Windows.
pub fn menu_title(title: &'static str) -> String {
    let mnemonic = match title {
        "File" => 'f',
        "Edit" => 'e',
        "View" => 'v',
        "Sort by" => 's',
        "Navigate" => 'n',
        "Slideshow" => 's',
        "Tools" => 't',
        "Help" => 'h',
        // The app menu is macOS-only: it is built on every platform and dropped here, empty,
        // before the bar goes up.
        "Prvw" => return title.to_string(),
        other => {
            debug_assert!(false, "no Windows mnemonic for the \"{other}\" menu");
            return title.to_string();
        }
    };
    with_mnemonic(title, mnemonic)
}

/// What File → Quit is called here. muda writes this title verbatim.
pub fn quit_text() -> Option<String> {
    Some(title(MenuItemKey::Quit, MenuItemKey::Quit.label()))
}

/// The accelerator muda registers in the menu's `ACCEL` table for this item.
pub fn accelerator(key: MenuItemKey) -> Option<Accelerator> {
    decoration(key).and_then(|decoration| decoration.accelerator)
}

/// Mark `mnemonic` in `label` by putting an `&` before its first occurrence.
///
/// Case-insensitive, so the table can spell every mnemonic in lower case whatever the label
/// does. A label with an `&` of its own would need it doubled to survive Win32's parsing; none
/// has one, and the assertion below is what keeps that true.
fn with_mnemonic(label: &str, mnemonic: char) -> String {
    debug_assert!(
        !label.contains('&'),
        "\"{label}\" carries an ampersand, which Win32 would eat as a mnemonic marker"
    );
    let Some((index, _)) = label
        .char_indices()
        .find(|(_, letter)| letter.eq_ignore_ascii_case(&mnemonic))
    else {
        debug_assert!(false, "'{mnemonic}' doesn't occur in \"{label}\"");
        return label.to_string();
    };
    let mut text = String::with_capacity(label.len() + 1);
    text.push_str(&label[..index]);
    text.push('&');
    text.push_str(&label[index..]);
    text
}

// ─────────────────────────────────────────────────────────────────────────────
// Attaching the bar. Windows-only from here down.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod bar {
    use std::cell::RefCell;

    use muda::{Menu, MenuTheme};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::HACCEL;
    use winit::window::Window;

    use crate::platform::windows::msg_hook::{self, AcceleratorTarget};

    /// The bar, once it's on a window. Kept so the accelerator table stays reachable from the
    /// message hook and so fullscreen can take the bar away and put it back.
    struct AttachedBar {
        hwnd: HWND,
        menu: Menu,
    }

    thread_local! {
        /// One window, one bar, one thread: everything here runs on the event loop's thread,
        /// which is also the only thread allowed to touch either handle.
        static BAR: RefCell<Option<AttachedBar>> = const { RefCell::new(None) };
    }

    /// Put the menu bar on the window and point the message hook at its accelerator table.
    ///
    /// muda installs its own `SetWindowSubclass` proc to catch `WM_COMMAND`, so the bar composes
    /// with winit's window procedure and `poll_command` keeps working unchanged. Accelerators do
    /// not compose by themselves: muda's docs say the message loop has to call
    /// `TranslateAcceleratorW`, and winit's doesn't, which is what
    /// `platform::windows::msg_hook` is for.
    pub fn attach(menu: &Menu, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(RawWindowHandle::Win32(handle)) = window.window_handle().map(|h| h.as_raw()) else {
            log::error!("No Win32 window handle; the menu bar can't attach");
            return;
        };
        let hwnd = handle.hwnd.get();

        // `MenuTheme::Auto` is muda's own default and the decided v1 behavior: it darkens the
        // bar when the system is dark, and leaves the dropdowns light because muda can only
        // theme the bar. Owner-drawing the dropdowns to match is deliberately out of scope
        // (`docs/specs/windows-ui-design.md`).
        // SAFETY: `hwnd` is the live window winit just handed us, and the app outlives the menu.
        if let Err(error) = unsafe { menu.init_for_hwnd_with_theme(hwnd, MenuTheme::Auto) } {
            log::error!("Couldn't attach the menu bar: {error}");
            return;
        }

        BAR.replace(Some(AttachedBar {
            hwnd: HWND(hwnd as *mut std::ffi::c_void),
            menu: menu.clone(),
        }));
        msg_hook::set_accelerator_source(accelerator_target);
        log::debug!("Menu bar attached to the window");
    }

    /// Where an accelerator keystroke goes, and which table translates it.
    ///
    /// The `HACCEL` is read fresh on every message rather than cached: muda destroys and
    /// recreates the table whenever an item joins or leaves a menu, so a stored handle can
    /// outlive the table it names.
    fn accelerator_target() -> Option<AcceleratorTarget> {
        BAR.with_borrow(|bar| {
            let bar = bar.as_ref()?;
            Some(AcceleratorTarget {
                hwnd: bar.hwnd,
                haccel: HACCEL(bar.menu.haccel() as *mut std::ffi::c_void),
            })
        })
    }

    /// Show or hide the bar. Fullscreen hides it: fullscreen is where the image really is the
    /// whole app, and no Windows app shows a menu bar there. There's no auto-hide and no
    /// setting, so F11 is the whole story.
    ///
    /// Hiding keeps muda's subclass on the window, so accelerators (F11 among them) keep working
    /// while the bar is gone.
    pub fn set_visible(visible: bool) {
        BAR.with_borrow(|bar| {
            let Some(bar) = bar else {
                return;
            };
            let hwnd = bar.hwnd.0 as isize;
            // SAFETY: the window this bar was attached to is alive for as long as the bar is.
            let result = unsafe {
                if visible {
                    bar.menu.show_for_hwnd(hwnd)
                } else {
                    bar.menu.hide_for_hwnd(hwnd)
                }
            };
            if let Err(error) = result {
                log::warn!("Couldn't {} the menu bar: {error}", {
                    if visible { "show" } else { "hide" }
                });
            }
        });
    }
}

#[cfg(target_os = "windows")]
pub use bar::{attach, set_visible};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every item the Windows menu bar can carry is dressed, and only the ones the registry
    /// calls `NotApplicable` there are not. A `Missing` item with no decoration would build a
    /// blank menu entry the day someone implements its command.
    #[test]
    fn every_item_windows_can_show_is_dressed() {
        use crate::parity::{Coverage, Platform};

        for key in MenuItemKey::ALL {
            let not_applicable = matches!(
                key.coverage(Platform::Windows),
                Coverage::NotApplicable { .. }
            );
            assert_eq!(
                decoration(*key).is_none(),
                not_applicable,
                "{} is dressed for Windows but the registry disagrees",
                key.name()
            );
        }
    }

    /// Alt+F then O has to be unambiguous, and Windows resolves a duplicate mnemonic by
    /// cycling rather than acting, which reads as a broken menu.
    #[test]
    fn mnemonics_are_unique_within_a_menu() {
        let mut taken: Vec<(WindowsMenu, char, MenuItemKey)> = Vec::new();
        for key in MenuItemKey::ALL {
            let Some(decoration) = decoration(*key) else {
                continue;
            };
            let mnemonic = decoration.mnemonic.to_ascii_lowercase();
            if let Some((_, _, other)) = taken
                .iter()
                .find(|(menu, letter, _)| *menu == decoration.menu && *letter == mnemonic)
            {
                panic!(
                    "{} and {} both claim Alt+{mnemonic} in the same menu",
                    key.name(),
                    other.name()
                );
            }
            taken.push((decoration.menu, mnemonic, *key));
        }
    }

    /// A mnemonic that doesn't occur in the label underlines nothing, and Windows silently
    /// shows the item with no mnemonic at all.
    #[test]
    fn every_mnemonic_occurs_in_its_label() {
        for key in MenuItemKey::ALL {
            let Some(decoration) = decoration(*key) else {
                continue;
            };
            let label = decoration.label.unwrap_or(key.label());
            assert!(
                label.contains(decoration.mnemonic.to_ascii_lowercase())
                    || label.contains(decoration.mnemonic.to_ascii_uppercase()),
                "{}: '{}' doesn't occur in \"{label}\"",
                key.name(),
                decoration.mnemonic
            );
        }
    }

    /// The two items whose label flips with a mode keep their mnemonic and their shortcut
    /// column through the flip, so Alt+S doesn't stop working the moment a slideshow starts.
    #[test]
    fn a_flipped_label_keeps_its_mnemonic() {
        assert_eq!(
            title(MenuItemKey::SlideshowToggle, "Stop slideshow"),
            "&Stop slideshow\tS"
        );
        assert_eq!(
            title(MenuItemKey::BrowseToggle, "Image view"),
            "&Image view\tEnter"
        );
    }

    /// Two items firing on the same keystroke means one of them never runs, and the accelerator
    /// table picks the winner by insertion order rather than by anything we decided.
    #[test]
    fn no_two_items_share_a_shortcut() {
        let mut taken: Vec<(Accelerator, MenuItemKey)> = Vec::new();
        for key in MenuItemKey::ALL {
            let Some(accelerator) = accelerator(*key) else {
                continue;
            };
            if let Some((_, other)) = taken.iter().find(|(a, _)| *a == accelerator) {
                panic!("{} and {} share a shortcut", key.name(), other.name());
            }
            taken.push((accelerator, *key));
        }
    }

    /// An item with a real accelerator leaves the shortcut column to muda, which writes the
    /// tab and the accelerator's own name. Both would give the item two shortcut strings.
    #[test]
    fn a_hint_and_an_accelerator_are_never_both_set() {
        for key in MenuItemKey::ALL {
            let Some(decoration) = decoration(*key) else {
                continue;
            };
            assert!(
                decoration.hint.is_empty() || decoration.accelerator.is_none(),
                "{} carries both a hint and an accelerator",
                key.name()
            );
        }
    }

    /// The shared label is the one string both platforms use. Windows decorates it; it never
    /// renames it, Exit excepted.
    #[test]
    fn windows_renames_only_quit() {
        let renamed: Vec<&str> = MenuItemKey::ALL
            .iter()
            .filter(|key| decoration(**key).is_some_and(|d| d.label.is_some()))
            .map(|key| key.name())
            .collect();
        assert_eq!(renamed, vec!["Quit"]);
        assert_eq!(quit_text().as_deref(), Some("E&xit"));
    }

    #[test]
    fn titles_carry_a_mnemonic_and_a_tab_separated_column() {
        assert_eq!(
            title(MenuItemKey::Open, MenuItemKey::Open.label()),
            "&Open\u{2026}"
        );
        assert_eq!(
            title(MenuItemKey::Histogram, MenuItemKey::Histogram.label()),
            "&Histogram\tH"
        );
        assert_eq!(menu_title("File"), "&File");
        assert_eq!(menu_title("Sort by"), "&Sort by");
    }
}
