//! What colour every Win32 window of ours paints, surface by surface.
//!
//! Windows asks one control at a time, through the six `WM_CTLCOLOR*` messages, and each answer
//! is three things: a text colour and a background colour set on the device context, and a brush
//! handed back for Windows to fill the control's background with. Which three a given control
//! gets is a pure mapping, so it lives here where a Mac can assert it.
//! `platform::windows::dark_mode` is the other half: it calls `GetSysColor`, keeps the brushes,
//! and answers the messages.
//!
//! ## Two surfaces, and why the message can't name them
//!
//! A Windows dialog has exactly two backgrounds: the window itself, and the inside of a field.
//! Every theme Windows has ever shipped draws those differently, because that contrast is how a
//! person tells "you can type here" from "you can't".
//!
//! The message looks like it says which is which, and doesn't. **A read-only or disabled edit
//! control sends `WM_CTLCOLORSTATIC`**, not `WM_CTLCOLOREDIT`, so it arrives indistinguishable
//! from a label. Answering the message alone paints the field with the window's colour, which is
//! the bug this module exists to make impossible. [`surface_for_class`] asks the control what it
//! is instead, and the message stops mattering: every one of them gets the same reply.
//!
//! ## Light asks the system, dark names its colours
//!
//! Every colour the light theme paints is a [`Color::System`], and that's the whole of how
//! high contrast works. [`theme_for`] answers [`Theme::Light`] under a high-contrast scheme on
//! purpose, so a person's own accessibility colours arrive through `GetSysColor` without us
//! knowing anything about them. A hard-coded light colour anywhere in here would paint straight
//! over that, which is why `the_light_theme_never_names_a_colour` sweeps the whole table.
//!
//! Dark is the mirror image: Windows has no system colour for it, so every dark value is a
//! literal, and `dark_text_is_legible_on_its_own_background` is what keeps the pairs honest.

/// Which way a window paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

/// The first build where `uxtheme` ordinal 135 is `SetPreferredAppMode(PreferredAppMode)` rather
/// than 17763's `AllowDarkModeForApp(BOOL)`. Below it, we don't try.
pub const FIRST_DARK_MODE_BUILD: u32 = 18362;

/// Which theme a window should paint in, given the three things the system can tell us.
///
/// `apps_use_light_theme` is `None` when the value isn't there at all, which is how a fresh
/// profile looks and means light.
pub fn theme_for(build: u32, apps_use_light_theme: Option<u32>, high_contrast: bool) -> Theme {
    if high_contrast || build < FIRST_DARK_MODE_BUILD {
        return Theme::Light;
    }
    match apps_use_light_theme {
        Some(0) => Theme::Dark,
        _ => Theme::Light,
    }
}

/// One of the two backgrounds a window of ours has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The window, and everything drawn straight onto it: labels, checkbox and radio text, group
    /// boxes, trackbars, push buttons.
    Dialog,
    /// The inside of a field: an edit control or a list box.
    Field,
}

/// How loud a piece of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// A label, a checkbox's text, a field's contents.
    Body,
    /// The grey line under a setting, and the number beside a trackbar.
    Secondary,
}

/// A system colour, named for the job it does rather than for its `COLOR_*` index, so this
/// module carries no Win32 constants. `platform::windows::dark_mode` maps it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemColor {
    ButtonFace,
    ButtonText,
    Window,
    WindowText,
    GrayText,
}

/// Where a colour comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// Ask Windows, through `GetSysColor`. This is what carries a high-contrast scheme through.
    System(SystemColor),
    /// A literal `0x00BBGGRR`. Dark mode only: Windows has no system colours for it.
    Fixed(u32),
}

impl Theme {
    /// What a surface's background is filled with.
    ///
    /// The dark pair is Windows 11's own dialog grey and a field a shade lighter, rather than
    /// pure black: comctl32's dark assets are drawn against that grey, and a black window behind
    /// them reads as two dark themes touching.
    pub fn background(self, surface: Surface) -> Color {
        match (self, surface) {
            (Theme::Light, Surface::Dialog) => Color::System(SystemColor::ButtonFace),
            (Theme::Light, Surface::Field) => Color::System(SystemColor::Window),
            (Theme::Dark, Surface::Dialog) => Color::Fixed(0x0020_2020),
            (Theme::Dark, Surface::Field) => Color::Fixed(0x002B_2B2B),
        }
    }

    /// What text drawn on that surface is coloured.
    ///
    /// Body text differs by surface even in light, because a high-contrast scheme sets window
    /// text and button text separately and a field's contents follow the field.
    pub fn text(self, surface: Surface, ink: Ink) -> Color {
        match (self, ink, surface) {
            (Theme::Light, Ink::Body, Surface::Dialog) => Color::System(SystemColor::ButtonText),
            (Theme::Light, Ink::Body, Surface::Field) => Color::System(SystemColor::WindowText),
            (Theme::Light, Ink::Secondary, _) => Color::System(SystemColor::GrayText),
            (Theme::Dark, Ink::Body, _) => Color::Fixed(0x00F0_F0F0),
            (Theme::Dark, Ink::Secondary, _) => Color::Fixed(0x00A0_A0A0),
        }
    }
}

/// Which surface a control of this window class sits on.
///
/// Class names are what Windows itself matches on, and it matches them without regard to case,
/// so this does too. Anything unrecognised sits on the window, which is the right answer for
/// every control Prvw creates that isn't a field and the safe one for anything it grows later:
/// a control painted the window's colour looks plain, where a label painted the field's colour
/// looks broken.
pub fn surface_for_class(class: &str) -> Surface {
    const FIELDS: [&str; 3] = [
        "Edit",
        "ListBox",
        // A combo box's drop-down list, which is a list box under another name.
        "ComboLBox",
    ];
    if FIELDS.iter().any(|field| class.eq_ignore_ascii_case(field)) {
        Surface::Field
    } else {
        Surface::Dialog
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEMES: [Theme; 2] = [Theme::Light, Theme::Dark];
    const SURFACES: [Surface; 2] = [Surface::Dialog, Surface::Field];
    const INKS: [Ink; 2] = [Ink::Body, Ink::Secondary];

    /// High contrast is an accessibility setting and it beats everything, including an explicit
    /// dark preference on a build that could honour it.
    #[test]
    fn high_contrast_stays_light() {
        assert_eq!(theme_for(22631, Some(0), true), Theme::Light);
        assert_eq!(theme_for(19045, Some(0), true), Theme::Light);
    }

    /// 19045 is Prvw's floor and it gets dark mode. This is the case `win32-darkmode`'s
    /// exact-match build list gets wrong, and the reason we don't copy it.
    #[test]
    fn the_support_floor_gets_dark_mode() {
        assert_eq!(theme_for(19045, Some(0), false), Theme::Dark);
        assert_eq!(theme_for(19045, Some(1), false), Theme::Light);
    }

    /// Below 18362 the ordinal means something else, so we don't touch it.
    #[test]
    fn older_builds_stay_light_whatever_the_preference() {
        assert_eq!(theme_for(17763, Some(0), false), Theme::Light);
        assert_eq!(theme_for(0, Some(0), false), Theme::Light);
    }

    /// A profile that never set the value is a light profile.
    #[test]
    fn a_missing_preference_means_light() {
        assert_eq!(theme_for(22631, None, false), Theme::Light);
    }

    /// The whole reason this decision hangs off the class rather than off the message: a
    /// read-only edit sends `WM_CTLCOLORSTATIC`, so answering the message paints the file-type
    /// list and the DCP folder field with the window's own colour instead of a field's.
    #[test]
    fn a_read_only_edit_is_still_a_field() {
        assert_eq!(surface_for_class("Edit"), Surface::Field);
        assert_eq!(surface_for_class("ListBox"), Surface::Field);
    }

    /// Window classes are case-insensitive, and `GetClassNameW` gives back whatever spelling the
    /// class was registered with.
    #[test]
    fn a_class_matches_however_its_spelled() {
        assert_eq!(surface_for_class("EDIT"), Surface::Field);
        assert_eq!(surface_for_class("edit"), Surface::Field);
    }

    /// Every other control the settings dialog and the About box create, plus the dialog itself,
    /// which is what `WM_CTLCOLORDLG` hands over.
    #[test]
    fn everything_else_sits_on_the_window() {
        for class in [
            "Static",
            "Button",
            "msctls_trackbar32",
            "SysTabControl32",
            "SysLink",
            "#32770",
            "",
        ] {
            assert_eq!(surface_for_class(class), Surface::Dialog, "{class}");
        }
    }

    /// The high-contrast guarantee, as an invariant rather than as a promise in a comment: a
    /// literal anywhere in the light theme would paint over a person's accessibility scheme.
    #[test]
    fn the_light_theme_never_names_a_colour() {
        for surface in SURFACES {
            assert!(matches!(Theme::Light.background(surface), Color::System(_)));
            for ink in INKS {
                assert!(matches!(Theme::Light.text(surface, ink), Color::System(_)));
            }
        }
    }

    /// The mirror image. A system colour in the dark theme would come back light, because
    /// `GetSysColor` answers with the light scheme whatever the app is painting.
    #[test]
    fn the_dark_theme_never_asks_the_system() {
        for surface in SURFACES {
            assert!(matches!(Theme::Dark.background(surface), Color::Fixed(_)));
            for ink in INKS {
                assert!(matches!(Theme::Dark.text(surface, ink), Color::Fixed(_)));
            }
        }
    }

    /// A field has to read as a field. Light gets this from the system; dark is ours to keep.
    #[test]
    fn a_dark_field_is_not_the_dark_window() {
        assert_ne!(
            Theme::Dark.background(Surface::Dialog),
            Theme::Dark.background(Surface::Field)
        );
    }

    /// Every dark pair clears WCAG AA for body text and AA-large for the secondary line. Nothing
    /// on a Mac can look at the dialog, so the contrast is asserted rather than eyeballed.
    #[test]
    fn dark_text_is_legible_on_its_own_background() {
        for surface in SURFACES {
            let background = fixed(Theme::Dark.background(surface));
            for ink in INKS {
                let text = fixed(Theme::Dark.text(surface, ink));
                let floor = match ink {
                    Ink::Body => 7.0,
                    Ink::Secondary => 4.5,
                };
                let ratio = contrast(text, background);
                assert!(ratio >= floor, "{surface:?}/{ink:?} is only {ratio:.1}:1");
            }
        }
    }

    /// Both themes answer for every surface and ink. A `match` arm that got missed would be a
    /// compile error, so this is really a guard on the sweeps above covering the whole table.
    #[test]
    fn every_surface_and_ink_has_an_answer_in_both_themes() {
        let mut answers = Vec::new();
        for theme in THEMES {
            for surface in SURFACES {
                answers.push(theme.background(surface));
                for ink in INKS {
                    answers.push(theme.text(surface, ink));
                }
            }
        }
        assert_eq!(answers.len(), 2 * 2 * 3);
    }

    fn fixed(color: Color) -> u32 {
        match color {
            Color::Fixed(value) => value,
            Color::System(system) => panic!("{system:?} isn't a literal"),
        }
    }

    /// WCAG 2.1 relative luminance, over a `0x00BBGGRR` `COLORREF`.
    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((color >> shift) & 0xFF) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16)
    }

    fn contrast(one: u32, other: u32) -> f64 {
        let (one, other) = (luminance(one), luminance(other));
        (one.max(other) + 0.05) / (one.min(other) + 0.05)
    }
}
