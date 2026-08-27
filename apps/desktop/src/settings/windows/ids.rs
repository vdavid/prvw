//! Control ids, and how a `WM_COMMAND` finds its way back to a settings row.
//!
//! A Win32 notification carries an integer, so every control the dialog creates needs one that
//! says both which row it belongs to and which part of that row it is. This is that mapping,
//! kept apart from the Win32 code because an id collision is a bug that shows up as a click
//! doing the wrong thing, and a Mac can rule it out.

/// The tab control across the top.
pub const TAB: i32 = 100;
/// "Reset to defaults", on the RAW page.
pub const RESET: i32 = 101;
/// "Register Prvw's file types", on the File associations page.
pub const REGISTER_FILE_TYPES: i32 = 102;
/// "Open Windows default apps settings", beside it.
pub const OPEN_DEFAULT_APPS: i32 = 103;
/// The read-only list of extensions on that page.
pub const FILE_TYPE_LIST: i32 = 104;
/// The paragraph above it, explaining that Windows owns the default-handler choice.
pub const FILE_TYPE_EXPLANATION: i32 = 106;
/// A group box, which is never notified and never asked about, but still needs an id no other
/// control has.
pub const GROUP_BOX: i32 = 105;

/// Where row ids start. Clear of the dialog manager's own (`IDOK` is 1, `IDCANCEL` is 2) and of
/// everything above.
const ROW_BASE: i32 = 1000;

/// How many controls one row can own. A power of two so the arithmetic is a shift, and one
/// more than [`Slot`] has variants so a new one doesn't renumber every id.
const SLOTS: i32 = 8;

/// Which part of a row a control is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The checkbox, trackbar, or read-only edit: the thing the setting reads from.
    Control = 0,
    /// The static showing a trackbar's value.
    Value = 1,
    /// "Browse…", on the folder row.
    Browse = 2,
    /// "Clear", beside it.
    Clear = 3,
    /// The grey line under the control.
    Description = 4,
    /// The static carrying the row's title, where the control doesn't draw its own.
    Title = 5,
}

impl Slot {
    /// Every part a row can have. The tests sweep it; the dialog names the one it's creating.
    #[cfg(test)]
    const ALL: &'static [Slot] = &[
        Slot::Control,
        Slot::Value,
        Slot::Browse,
        Slot::Clear,
        Slot::Description,
        Slot::Title,
    ];

    const fn from_index(index: i32) -> Option<Slot> {
        match index {
            0 => Some(Slot::Control),
            1 => Some(Slot::Value),
            2 => Some(Slot::Browse),
            3 => Some(Slot::Clear),
            4 => Some(Slot::Description),
            5 => Some(Slot::Title),
            _ => None,
        }
    }
}

/// The id for one control of row `index`, where `index` counts every row on every page.
pub const fn control(index: usize, slot: Slot) -> i32 {
    ROW_BASE + index as i32 * SLOTS + slot as i32
}

/// Which row and part an id names, or `None` for one of the dialog's own controls.
pub const fn row(id: i32) -> Option<(usize, Slot)> {
    if id < ROW_BASE {
        return None;
    }
    let offset = id - ROW_BASE;
    match Slot::from_index(offset % SLOTS) {
        Some(slot) => Some(((offset / SLOTS) as usize, slot)),
        None => None,
    }
}

/// Whether the control with this id draws its text in the dimmer secondary ink.
///
/// A row's description and the number beside a trackbar, plus the File associations paragraph,
/// which is a row description the page draws as its own furniture rather than as part of a row.
/// Everything else is body text.
pub const fn is_secondary(id: i32) -> bool {
    if id == FILE_TYPE_EXPLANATION {
        return true;
    }
    matches!(row(id), Some((_, Slot::Description | Slot::Value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id a row hands out comes back as the same row and the same part. A slip here is a
    /// click on one setting changing another.
    #[test]
    fn row_ids_round_trip() {
        for index in 0..200usize {
            for slot in Slot::ALL {
                assert_eq!(row(control(index, *slot)), Some((index, *slot)));
            }
        }
    }

    /// No row id collides with the dialog's own controls, or with the dialog manager's `IDOK`
    /// and `IDCANCEL`, which the Close button uses so Esc and the title bar's X both land there.
    #[test]
    fn row_ids_dont_collide_with_the_dialogs_own() {
        let reserved = [
            1, // IDOK
            2, // IDCANCEL
            TAB,
            RESET,
            REGISTER_FILE_TYPES,
            OPEN_DEFAULT_APPS,
            FILE_TYPE_LIST,
            FILE_TYPE_EXPLANATION,
            GROUP_BOX,
        ];
        for id in reserved {
            assert_eq!(row(id), None, "{id} is one of the dialog's own");
        }
        for index in 0..200usize {
            for slot in Slot::ALL {
                assert!(!reserved.contains(&control(index, *slot)));
            }
        }
    }

    /// The dialog's own ids are distinct from each other, which nothing else would notice.
    #[test]
    fn the_dialogs_own_ids_are_distinct() {
        let ids = [
            TAB,
            RESET,
            REGISTER_FILE_TYPES,
            OPEN_DEFAULT_APPS,
            FILE_TYPE_LIST,
            FILE_TYPE_EXPLANATION,
            GROUP_BOX,
        ];
        for (index, id) in ids.iter().enumerate() {
            assert!(!ids[index + 1..].contains(id), "{id} is used twice");
        }
    }

    /// Which text is grey. The File associations paragraph is the one that isn't a row's, and
    /// it reads as one, so it takes the same ink.
    #[test]
    fn the_grey_text_is_the_descriptions_and_the_trackbar_values() {
        assert!(is_secondary(control(3, Slot::Description)));
        assert!(is_secondary(control(3, Slot::Value)));
        assert!(is_secondary(FILE_TYPE_EXPLANATION));
        for slot in [Slot::Control, Slot::Browse, Slot::Clear, Slot::Title] {
            assert!(!is_secondary(control(3, slot)), "{slot:?}");
        }
        for id in [
            TAB,
            RESET,
            REGISTER_FILE_TYPES,
            OPEN_DEFAULT_APPS,
            FILE_TYPE_LIST,
            GROUP_BOX,
        ] {
            assert!(!is_secondary(id), "{id}");
        }
    }
}
