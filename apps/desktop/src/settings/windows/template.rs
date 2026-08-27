//! The in-memory `DLGTEMPLATE` the settings dialog and its pages are created from.
//!
//! `CreateDialogIndirectParamW` wants a dialog template, and the alternative to building one
//! here is an `.rc` file. We don't use one: a template's control entries hardcode their label
//! strings, and that would sever the link between a settings row and the
//! [`SettingKey`](crate::parity::setting_keys::SettingKey) it satisfies, which is the whole
//! point of the parity harness (`docs/specs/windows-ui-design.md`). So every template here
//! carries **zero controls**, and the controls are created afterwards with `CreateWindowExW`
//! from [`super::layout`]'s rects.
//!
//! Zero controls also sidesteps dialog units. A template's `x`, `y`, `cx`, and `cy` are in
//! dialog units rather than pixels, which would mean converting our whole pixel layout through
//! the dialog base units of a font we set ourselves. Instead the size is zero here and the real
//! size arrives through `SetWindowPos` in device pixels.
//!
//! The format is documented as `DLGTEMPLATE` followed by three variable-length fields, in this
//! order: the menu, the window class, and the title. We want no menu and the standard dialog
//! class, so the first two are a single zero word each.

/// A dialog template, kept as `u32`s so the buffer is DWORD-aligned.
///
/// Alignment is not cosmetic here: `CreateDialogIndirectParamW` reads the header as a
/// `DLGTEMPLATE`, and a `Vec<u8>` is only byte-aligned. Holding `u32`s makes the alignment a
/// property of the type rather than something to remember.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    words: Vec<u32>,
}

impl Template {
    /// The bytes, for a caller that wants to read them back. The Win32 side casts
    /// [`Template::as_ptr`] instead, so this is how the tests check the layout.
    #[cfg(test)]
    pub fn bytes(&self) -> Vec<u8> {
        self.words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    /// A DWORD-aligned pointer to the template, to hand to `CreateDialogIndirectParamW`.
    pub fn as_ptr(&self) -> *const u32 {
        self.words.as_ptr()
    }
}

/// Build a template for a dialog with no controls of its own.
///
/// `title` is what the caption bar shows, and is ignored for a child dialog (a `WS_CHILD`
/// window has no caption).
pub fn dialog(style: u32, extended_style: u32, title: &str) -> Template {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend(style.to_le_bytes());
    bytes.extend(extended_style.to_le_bytes());
    bytes.extend(0u16.to_le_bytes()); // cdit: no control entries follow.
    for value in [0i16, 0, 0, 0] {
        bytes.extend(value.to_le_bytes()); // x, y, cx, cy: set with `SetWindowPos` instead.
    }
    bytes.extend(0u16.to_le_bytes()); // No menu.
    bytes.extend(0u16.to_le_bytes()); // The standard dialog class.
    for unit in title.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend(unit.to_le_bytes());
    }
    // No `DS_SETFONT`, so no font block: every control gets `WM_SETFONT` with the DPI-aware
    // `lfMessageFont` instead, which is what gets the size right on a scaled monitor.

    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let words = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Template { words }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `DLGTEMPLATE` header is 18 bytes in this exact order, and Windows reads it as a
    /// struct. Getting a field's offset wrong gives a dialog of a nonsense size or style, with
    /// no error to go on, so the bytes are worth pinning.
    #[test]
    fn the_header_is_laid_out_the_way_windows_reads_it() {
        let template = dialog(0x8000_0000, 0x0001_0000, "");
        let bytes = template.bytes();
        assert_eq!(&bytes[0..4], &0x8000_0000u32.to_le_bytes(), "style");
        assert_eq!(
            &bytes[4..8],
            &0x0001_0000u32.to_le_bytes(),
            "dwExtendedStyle"
        );
        assert_eq!(&bytes[8..10], &0u16.to_le_bytes(), "cdit is zero");
        assert_eq!(&bytes[10..18], &[0; 8], "x, y, cx, cy");
        assert_eq!(&bytes[18..20], &0u16.to_le_bytes(), "no menu");
        assert_eq!(&bytes[20..22], &0u16.to_le_bytes(), "standard class");
    }

    /// The title is UTF-16 and null-terminated, right after the class word.
    #[test]
    fn the_title_is_wide_and_terminated() {
        let bytes = dialog(0, 0, "Settings").bytes();
        let title: Vec<u16> = bytes[22..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        assert_eq!(String::from_utf16(&title).unwrap(), "Settings");
    }

    /// Windows reads the header as a `DLGTEMPLATE`, so the buffer has to be DWORD-aligned.
    /// A `Vec<u8>` would only promise byte alignment, which is why this holds `u32`s.
    #[test]
    fn the_buffer_is_dword_aligned() {
        for title in ["", "a", "ab", "abc", "Settings"] {
            let template = dialog(0, 0, title);
            assert_eq!(template.bytes().len() % 4, 0, "{title:?} isn't padded");
            assert_eq!(template.as_ptr() as usize % 4, 0, "{title:?} isn't aligned");
        }
    }
}
