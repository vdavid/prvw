//! Where every control on a settings page goes, in logical pixels.
//!
//! Windows scales a `CreateDialog` template's controls for us under Per-Monitor v2, which is
//! the one real argument for building this dialog from an `.rc` file. We don't, because a
//! template hardcodes its label strings and that would cut the link between a settings row and
//! the [`SettingKey`](crate::parity::setting_keys::SettingKey) it satisfies, which is the
//! whole point of the parity harness. The price is this module: a vertical stacker that says
//! where each control sits, and [`scale`] to turn its answer into device pixels.
//!
//! Everything here is pure, so a Mac can prove that no two controls overlap and that a knob
//! really is indented under its toggle, at every scale factor a Windows user might be on. Two
//! platform-shaped inputs come in as parameters rather than being reached for: the monitor's
//! DPI, which scales every spacing constant through [`scale`], and text measurement, which
//! decides how tall a wrapped description is. The Win32 side passes a GDI-backed measurer;
//! tests pass a fixed-width fake.

use super::model::{Page, Row, RowKind};

/// A control's box, top-left origin, in whatever unit the caller is working in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    /// True when the two boxes share any pixel. What the tests prove a page with: nothing is
    /// stacked on top of anything else, at any scale factor.
    #[cfg(test)]
    pub const fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Logical pixels to device pixels, the way `MulDiv` does it: multiply, divide, round to
/// nearest. Every rect this module produces goes through it before it reaches `CreateWindowEx`.
pub fn scale(value: i32, dpi: u32) -> i32 {
    let dpi = i64::from(dpi.max(1));
    let scaled = i64::from(value) * dpi;
    let rounded = if scaled >= 0 {
        (scaled + 48) / 96
    } else {
        (scaled - 48) / 96
    };
    rounded as i32
}

/// The dialog's own size and furniture, at 96 DPI.
pub mod dialog {
    /// Client width. Wide enough for a two-line description at a comfortable measure, narrow
    /// enough to sit beside the image window rather than over it.
    pub const WIDTH: i32 = 560;
    /// Client height.
    pub const HEIGHT: i32 = 480;
    /// Margin around the tab control and the button row.
    pub const MARGIN: i32 = 10;
    pub const BUTTON_WIDTH: i32 = 80;
    pub const BUTTON_HEIGHT: i32 = 23;
    /// Gap between the tab control's bottom and the Close button's top.
    pub const BUTTON_GAP: i32 = 10;

    /// Where the tab control sits in the dialog's client area.
    pub const fn tab_rect() -> super::Rect {
        super::Rect {
            x: MARGIN,
            y: MARGIN,
            width: WIDTH - MARGIN * 2,
            height: HEIGHT - MARGIN * 2 - BUTTON_HEIGHT - BUTTON_GAP,
        }
    }

    /// Where the Close button sits: bottom-right, the Windows convention.
    pub const fn close_button_rect() -> super::Rect {
        super::Rect {
            x: WIDTH - MARGIN - BUTTON_WIDTH,
            y: HEIGHT - MARGIN - BUTTON_HEIGHT,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        }
    }
}

/// The spacing rules a page is stacked by, in logical pixels at 96 DPI. [`place`] scales
/// every one of them for the monitor it's placing on.
mod metrics {
    /// Margin between the page's edge and its content.
    pub const PAGE_MARGIN: i32 = 12;
    /// How far a group box insets its rows.
    pub const GROUP_INSET: i32 = 10;
    /// Space under a group box's caption before its first row.
    pub const GROUP_TOP: i32 = 18;
    /// Space under a group's last row, inside the frame.
    pub const GROUP_BOTTOM: i32 = 10;
    /// Space between two group boxes.
    pub const GROUP_GAP: i32 = 10;
    /// Space between two rows.
    pub const ROW_GAP: i32 = 12;
    /// Space between a control and the grey line describing it.
    pub const DESCRIPTION_GAP: i32 = 2;
    /// How far a knob is indented under the toggle it belongs to.
    pub const INDENT: i32 = 18;
    /// A checkbox's box is this wide, so its description lines up with the label rather than
    /// the box. This is the Windows 11 Settings card subtitle idea without the card.
    pub const CHECKBOX_TEXT_INDENT: i32 = 17;
    pub const CHECKBOX_HEIGHT: i32 = 17;
    /// A `msctls_trackbar32` with `TBS_AUTOTICKS` needs room for its tick marks.
    pub const TRACKBAR_HEIGHT: i32 = 26;
    /// The read-only static showing a trackbar's value.
    pub const VALUE_WIDTH: i32 = 52;
    pub const VALUE_GAP: i32 = 8;
    pub const LABEL_HEIGHT: i32 = 16;
    pub const EDIT_HEIGHT: i32 = 22;
    pub const BUTTON_WIDTH: i32 = 76;
    pub const BUTTON_HEIGHT: i32 = 23;
    pub const BUTTON_GAP: i32 = 6;
    /// The extension list on the File associations page.
    pub const LIST_HEIGHT: i32 = 96;
}

/// How tall a run of text is once it wraps to `width`, in device pixels.
///
/// The Win32 side measures with `DrawTextW(DT_CALCRECT | DT_WORDBREAK)` in the dialog's own
/// font, which is already the right size for the monitor's DPI; tests use a fixed-width
/// stand-in. Either way [`place`] never guesses.
pub type Measure<'a> = &'a dyn Fn(&str, i32) -> i32;

/// One row's controls, placed.
#[derive(Clone, Debug)]
pub struct PlacedRow {
    pub row: &'static Row,
    /// The static carrying the row's title, for a control that doesn't draw its own text. A
    /// checkbox does, so it has none.
    pub title: Option<Rect>,
    /// The checkbox, trackbar, or read-only edit.
    pub control: Rect,
    /// The static showing a trackbar's current value.
    pub value: Option<Rect>,
    /// Browse… and Clear, for the folder row.
    pub buttons: Vec<Rect>,
    /// The grey line under it all.
    pub description: Rect,
}

impl PlacedRow {
    /// Every box this row occupies, for the overlap checks.
    pub fn rects(&self) -> Vec<Rect> {
        let mut all = vec![self.control, self.description];
        all.extend(self.title);
        all.extend(self.value);
        all.extend(self.buttons.iter().copied());
        all
    }

    /// The bottom of the lowest thing in the row.
    pub fn bottom(&self) -> i32 {
        self.rects()
            .iter()
            .map(Rect::bottom)
            .max()
            .unwrap_or(self.control.bottom())
    }
}

/// A group box and the rows inside it. `frame` is `None` for an untitled group, whose rows sit
/// loose on the page.
#[derive(Clone, Debug)]
pub struct PlacedGroup {
    pub title: Option<&'static str>,
    pub frame: Option<Rect>,
    pub rows: Vec<PlacedRow>,
}

/// The File associations page, which is a paragraph, a list, and two buttons rather than rows.
#[derive(Clone, Debug)]
pub struct PlacedFileTypes {
    pub explanation: Rect,
    pub list: Rect,
    pub register_button: Rect,
    pub windows_settings_button: Rect,
}

/// Everything on one tab, placed.
#[derive(Clone, Debug)]
pub struct PageLayout {
    pub groups: Vec<PlacedGroup>,
    /// The RAW page's "Reset to defaults" button.
    pub reset_button: Option<Rect>,
    pub file_types: Option<PlacedFileTypes>,
    /// How tall the content is. Taller than the page means it scrolls.
    pub height: i32,
}

impl PageLayout {
    /// Every placed rect on the page, in the order they were stacked. The tests' way in.
    #[cfg(test)]
    pub fn rects(&self) -> Vec<Rect> {
        let mut all = Vec::new();
        for group in &self.groups {
            all.extend(group.frame);
            for row in &group.rows {
                all.extend(row.rects());
            }
        }
        all.extend(self.reset_button);
        if let Some(file_types) = &self.file_types {
            all.extend([
                file_types.explanation,
                file_types.list,
                file_types.register_button,
                file_types.windows_settings_button,
            ]);
        }
        all
    }
}

/// Stack `page`'s controls into a client area `width` device pixels wide, at `dpi`.
///
/// Every spacing constant goes through [`scale`] on the way in, so the answer is in device
/// pixels and ready for `CreateWindowExW`. The height that comes back is the content's, not
/// the window's: the RAW page is taller than the dialog on purpose and scrolls (see
/// [`ScrollState`]).
pub fn place(page: Page, width: i32, dpi: u32, measure: Measure<'_>) -> PageLayout {
    use metrics as m;
    let at = |value: i32| scale(value, dpi);

    let mut y = at(m::PAGE_MARGIN);
    let mut groups = Vec::new();

    for group in page.groups {
        let framed = group.title.is_some();
        let frame_top = y;
        let inset = if framed { at(m::GROUP_INSET) } else { 0 };
        let left = at(m::PAGE_MARGIN) + inset;
        let content_width = width - at(m::PAGE_MARGIN) * 2 - inset * 2;
        if framed {
            y += at(m::GROUP_TOP);
        }

        let mut rows = Vec::new();
        for (index, row) in group.rows.iter().enumerate() {
            if index > 0 {
                y += at(m::ROW_GAP);
            }
            let placed = place_row(row, left, y, content_width, dpi, measure);
            y = placed.bottom();
            rows.push(placed);
        }

        let frame = framed.then(|| {
            y += at(m::GROUP_BOTTOM);
            Rect {
                x: at(m::PAGE_MARGIN),
                y: frame_top,
                width: width - at(m::PAGE_MARGIN) * 2,
                height: y - frame_top,
            }
        });
        groups.push(PlacedGroup {
            title: group.title,
            frame,
            rows,
        });
        y += at(m::GROUP_GAP);
    }

    let file_types = page.file_types.then(|| {
        let left = at(m::PAGE_MARGIN);
        let content_width = width - at(m::PAGE_MARGIN) * 2;
        // The page's one row carries the copy explaining why Windows, not Prvw, owns the
        // choice. Everything below it is this page's own furniture.
        let explanation = Rect {
            x: left,
            y: at(m::PAGE_MARGIN),
            width: content_width,
            height: page
                .rows()
                .map(|row| measure(row.description, content_width))
                .max()
                .unwrap_or_else(|| at(m::LABEL_HEIGHT)),
        };
        let list = Rect {
            x: left,
            y: explanation.bottom() + at(m::ROW_GAP),
            width: content_width,
            height: at(m::LIST_HEIGHT),
        };
        let register_button = Rect {
            x: left,
            y: list.bottom() + at(m::ROW_GAP),
            width: at(m::BUTTON_WIDTH * 2),
            height: at(m::BUTTON_HEIGHT),
        };
        let windows_settings_button = Rect {
            x: register_button.right() + at(m::BUTTON_GAP),
            y: register_button.y,
            width: at(m::BUTTON_WIDTH * 3),
            height: at(m::BUTTON_HEIGHT),
        };
        PlacedFileTypes {
            explanation,
            list,
            register_button,
            windows_settings_button,
        }
    });
    if let Some(placed) = &file_types {
        y = placed.register_button.bottom();
    }

    let reset_button = page.reset_button.then(|| Rect {
        x: width - at(m::PAGE_MARGIN) - at(m::BUTTON_WIDTH * 2),
        y,
        width: at(m::BUTTON_WIDTH * 2),
        height: at(m::BUTTON_HEIGHT),
    });
    if let Some(reset) = reset_button {
        y = reset.bottom();
    }

    PageLayout {
        groups,
        reset_button,
        file_types,
        height: y + at(m::PAGE_MARGIN),
    }
}

fn place_row(
    row: &'static Row,
    left: i32,
    top: i32,
    width: i32,
    dpi: u32,
    measure: Measure<'_>,
) -> PlacedRow {
    use metrics as m;
    let at = |value: i32| scale(value, dpi);

    let indent = if row.indented { at(m::INDENT) } else { 0 };
    let x = left + indent;
    let width = width - indent;

    let (title, control, value, buttons, description_x, description_top) = match row.kind {
        RowKind::Checkbox => {
            let control = Rect {
                x,
                y: top,
                width,
                height: at(m::CHECKBOX_HEIGHT),
            };
            (
                None,
                control,
                None,
                Vec::new(),
                x + at(m::CHECKBOX_TEXT_INDENT),
                control.bottom() + at(m::DESCRIPTION_GAP),
            )
        }
        RowKind::Trackbar(_) => {
            let title = Rect {
                x,
                y: top,
                width,
                height: at(m::LABEL_HEIGHT),
            };
            let control = Rect {
                x,
                y: title.bottom(),
                width: width - at(m::VALUE_WIDTH) - at(m::VALUE_GAP),
                height: at(m::TRACKBAR_HEIGHT),
            };
            // Centred against the track, so the number reads as part of the bar.
            let value = Rect {
                x: control.right() + at(m::VALUE_GAP),
                y: control.y + (control.height - at(m::LABEL_HEIGHT)) / 2,
                width: at(m::VALUE_WIDTH),
                height: at(m::LABEL_HEIGHT),
            };
            (
                Some(title),
                control,
                Some(value),
                Vec::new(),
                x,
                control.bottom() + at(m::DESCRIPTION_GAP),
            )
        }
        RowKind::Folder => {
            let title = Rect {
                x,
                y: top,
                width,
                height: at(m::LABEL_HEIGHT),
            };
            let buttons_width = at(m::BUTTON_WIDTH) * 2 + at(m::BUTTON_GAP);
            let control = Rect {
                x,
                y: title.bottom() + at(m::DESCRIPTION_GAP),
                width: width - buttons_width - at(m::BUTTON_GAP),
                height: at(m::EDIT_HEIGHT),
            };
            let browse = Rect {
                x: control.right() + at(m::BUTTON_GAP),
                y: control.y,
                width: at(m::BUTTON_WIDTH),
                height: at(m::BUTTON_HEIGHT),
            };
            let clear = Rect {
                x: browse.right() + at(m::BUTTON_GAP),
                y: browse.y,
                width: at(m::BUTTON_WIDTH),
                height: at(m::BUTTON_HEIGHT),
            };
            let bottom = clear.bottom().max(control.bottom());
            (
                Some(title),
                control,
                None,
                vec![browse, clear],
                x,
                bottom + at(m::DESCRIPTION_GAP),
            )
        }
        // The File associations row's own controls are placed by `place`, because they aren't
        // a row at all. Its copy goes there too, so the row itself takes no space.
        RowKind::FileTypes => {
            let control = Rect {
                x,
                y: top,
                width,
                height: 0,
            };
            (None, control, None, Vec::new(), x, top)
        }
    };

    let description_width = width - (description_x - x);
    let description = Rect {
        x: description_x,
        y: description_top,
        width: description_width,
        height: if matches!(row.kind, RowKind::FileTypes) {
            0
        } else {
            measure(row.description, description_width)
        },
    };

    PlacedRow {
        row,
        title,
        control,
        value,
        buttons,
        description,
    }
}

/// Where a scrolling page is scrolled to, and what its scroll bar should say.
///
/// The RAW page is the only one that needs it: 23 settings in eight group boxes is far taller
/// than 480 pixels, and splitting RAW across two tabs would put one feature in two places for
/// a reason that's purely about pixel height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollState {
    /// Content height, in device pixels.
    pub content: i32,
    /// Visible height, in device pixels.
    pub visible: i32,
    /// How far down we are, from 0 to [`ScrollState::max`].
    pub position: i32,
}

impl ScrollState {
    pub const fn new(content: i32, visible: i32) -> Self {
        Self {
            content,
            visible,
            position: 0,
        }
    }

    /// The furthest down the content can go, which is zero when it all fits.
    pub const fn max(&self) -> i32 {
        let overflow = self.content - self.visible;
        if overflow > 0 { overflow } else { 0 }
    }

    /// True when there's anything to scroll, and so whether the bar is worth showing.
    pub const fn scrollable(&self) -> bool {
        self.max() > 0
    }

    /// Move to `position`, clamped. Returns how far the content actually moved, which is what
    /// `ScrollWindowEx` wants and is zero at either end.
    pub fn scroll_to(&mut self, position: i32) -> i32 {
        let target = position.clamp(0, self.max());
        let delta = self.position - target;
        self.position = target;
        delta
    }

    /// One `SB_LINEUP` / `SB_LINEDOWN` notch. A wheel notch is three of these, which is what
    /// `SPI_GETWHEELSCROLLLINES` defaults to.
    pub fn line(&mut self, down: bool) -> i32 {
        let step = if down { LINE } else { -LINE };
        self.scroll_to(self.position + step)
    }

    /// One `SB_PAGEUP` / `SB_PAGEDOWN`: a screenful less a line of overlap, so the reader
    /// keeps their place.
    pub fn page(&mut self, down: bool) -> i32 {
        let step = (self.visible - LINE).max(LINE);
        self.scroll_to(self.position + if down { step } else { -step })
    }
}

/// How far one scroll-bar line moves the content, in device pixels. A comfortable notch at
/// 100% scaling; at 200% the caller has already doubled the content, so this stays honest.
const LINE: i32 = 24;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parity::setting_keys::SettingKey;
    use crate::settings::windows::model::{self, Tab};

    /// The width of a page inside the tab control, in logical pixels. Real enough for the
    /// geometry tests, and the number the Win32 side will land near.
    const PAGE_WIDTH: i32 = 520;

    /// Every scale factor Windows offers between 100% and 200%, so a layout that only works at
    /// one of them can't pass.
    const SCALES: &[u32] = &[96, 120, 144, 168, 192];

    /// A stand-in for GDI: eight pixels a character at 96 DPI, wrapping at the given width,
    /// 15 pixels a line. The point isn't the numbers, it's that the same text always measures
    /// the same, so a layout test can assert geometry.
    fn measure_at(dpi: u32) -> impl Fn(&str, i32) -> i32 {
        move |text: &str, width: i32| {
            let char_width = scale(8, dpi).max(1);
            let line_height = scale(15, dpi);
            let per_line = (width / char_width).max(1);
            let lines = ((text.len() as i32 + per_line - 1) / per_line).max(1);
            lines * line_height
        }
    }

    fn layout_at(tab: Tab, dpi: u32) -> PageLayout {
        let measure = measure_at(dpi);
        place(model::page(tab), scale(PAGE_WIDTH, dpi), dpi, &measure)
    }

    fn layout(tab: Tab) -> PageLayout {
        layout_at(tab, 96)
    }

    /// Nothing on a page sits on top of anything else. This is the one thing a layout can get
    /// wrong that no Windows machine is needed to catch.
    #[test]
    fn no_two_controls_overlap() {
        for dpi in SCALES {
            for tab in Tab::ALL {
                let placed = layout_at(*tab, *dpi);
                let rects = placed.rects();
                for (index, rect) in rects.iter().enumerate() {
                    for other in &rects[index + 1..] {
                        // A group box's frame is meant to contain its rows, so it's excluded from
                        // the pairwise check and gets its own test below.
                        let is_frame = placed
                            .groups
                            .iter()
                            .any(|group| group.frame == Some(*rect) || group.frame == Some(*other));
                        assert!(
                            is_frame || !rect.overlaps(other),
                            "{} at {dpi} DPI overlaps: {rect:?} and {other:?}",
                            tab.title()
                        );
                    }
                }
            }
        }
    }

    /// A group box's frame really does enclose the rows inside it, and the rows are inset from
    /// its edges rather than touching them.
    #[test]
    fn group_frames_enclose_their_rows() {
        for group in layout(Tab::Raw).groups {
            let Some(frame) = group.frame else {
                panic!("every RAW group is titled");
            };
            for row in &group.rows {
                for rect in row.rects() {
                    assert!(rect.x >= frame.x, "{rect:?} escapes {frame:?} on the left");
                    assert!(
                        rect.right() <= frame.right(),
                        "{rect:?} escapes {frame:?} on the right"
                    );
                    assert!(rect.y >= frame.y, "{rect:?} escapes {frame:?} at the top");
                    assert!(
                        rect.bottom() <= frame.bottom(),
                        "{rect:?} escapes {frame:?} at the bottom"
                    );
                }
            }
        }
    }

    /// A description belongs to the control above it: below it, and lined up with its text
    /// rather than its box.
    #[test]
    fn descriptions_sit_under_their_control() {
        for tab in Tab::ALL {
            for group in layout(*tab).groups {
                for row in group.rows {
                    if matches!(row.row.kind, RowKind::FileTypes) {
                        continue;
                    }
                    assert!(
                        row.description.y >= row.control.bottom(),
                        "{}'s description is above its control",
                        row.row.key.name()
                    );
                    if matches!(row.row.kind, RowKind::Checkbox) {
                        assert!(
                            row.description.x > row.control.x,
                            "{}'s description lines up with the box, not the label",
                            row.row.key.name()
                        );
                    }
                }
            }
        }
    }

    /// A knob is indented under the toggle it belongs to, which is how the RAW page shows that
    /// "Clarity radius" means nothing without "Clarity".
    #[test]
    fn knobs_are_indented_under_their_toggle() {
        let detail = layout(Tab::Raw)
            .groups
            .into_iter()
            .find(|group| group.title == Some("Detail"))
            .expect("the Detail group");
        let clarity = detail
            .rows
            .iter()
            .find(|row| row.row.key == SettingKey::RawClarity)
            .expect("the clarity toggle");
        let radius = detail
            .rows
            .iter()
            .find(|row| row.row.key == SettingKey::RawClarityRadius)
            .expect("the clarity radius bar");
        assert!(radius.control.x > clarity.control.x);
        assert!(radius.control.y > clarity.control.y);
    }

    /// Rows stack downward, and each starts below the one before it.
    #[test]
    fn rows_stack_in_order() {
        for tab in Tab::ALL {
            let mut previous_bottom = 0;
            for group in layout(*tab).groups {
                for row in group.rows {
                    let top = row.title.unwrap_or(row.control).y;
                    assert!(
                        top >= previous_bottom,
                        "{} stacks backwards at {}",
                        tab.title(),
                        row.row.key.name()
                    );
                    previous_bottom = row.bottom();
                }
            }
        }
    }

    /// A trackbar's value static sits to its right and inside the page, never off the edge.
    #[test]
    fn value_labels_sit_beside_their_bar() {
        for tab in Tab::ALL {
            for group in layout(*tab).groups {
                for row in group.rows {
                    let RowKind::Trackbar(_) = row.row.kind else {
                        assert!(row.value.is_none(), "{}", row.row.key.name());
                        continue;
                    };
                    let value = row.value.expect("a bar has a value static");
                    assert!(value.x >= row.control.right(), "{}", row.row.key.name());
                    assert!(
                        value.right() <= PAGE_WIDTH - metrics::PAGE_MARGIN,
                        "{} runs off the page",
                        row.row.key.name()
                    );
                }
            }
        }
    }

    /// The RAW page is taller than the dialog, and the others aren't. That's what decides
    /// which page gets a scroll bar, so it's worth pinning rather than assuming.
    /// The RAW page is taller than the dialog and the others aren't, at every scale factor.
    /// That's what decides which page gets a scroll bar, and it has to hold at 200% too, where
    /// everything grew.
    #[test]
    fn only_the_raw_page_outgrows_the_dialog() {
        for dpi in SCALES {
            let visible = scale(dialog::tab_rect().height, *dpi);
            for tab in Tab::ALL {
                let height = layout_at(*tab, *dpi).height;
                assert_eq!(
                    height > visible,
                    *tab == Tab::Raw,
                    "{} is {height} tall against {visible} at {dpi} DPI",
                    tab.title()
                );
            }
        }
    }

    /// The Close button is inside the dialog, below the tab control, at the right edge.
    #[test]
    fn the_close_button_sits_bottom_right() {
        let tab = dialog::tab_rect();
        let close = dialog::close_button_rect();
        assert!(close.y >= tab.bottom());
        assert!(close.bottom() <= dialog::HEIGHT);
        assert_eq!(close.right(), tab.right());
    }

    #[test]
    fn scaling_rounds_the_way_muldiv_does() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
        assert_eq!(scale(100, 192), 200);
        assert_eq!(scale(17, 144), 26); // 25.5 rounds up
        assert_eq!(scale(-17, 144), -26);
        assert_eq!(scale(0, 240), 0);
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut state = ScrollState::new(1000, 400);
        assert!(state.scrollable());
        assert_eq!(state.max(), 600);

        state.line(true);
        assert_eq!(state.position, LINE);
        state.scroll_to(9999);
        assert_eq!(state.position, 600);
        assert_eq!(state.line(true), 0, "already at the bottom");
        state.scroll_to(-50);
        assert_eq!(state.position, 0);
        assert_eq!(state.line(false), 0, "already at the top");
    }

    /// The delta a scroll reports is what the content moves by, so `ScrollWindowEx` gets it
    /// right: positive moves the content down the screen, which is scrolling up.
    #[test]
    fn a_scroll_reports_how_far_the_content_moved() {
        let mut state = ScrollState::new(1000, 400);
        assert_eq!(state.scroll_to(100), -100);
        assert_eq!(state.scroll_to(40), 60);
    }

    #[test]
    fn a_page_that_fits_doesnt_scroll() {
        let mut state = ScrollState::new(300, 400);
        assert!(!state.scrollable());
        assert_eq!(state.max(), 0);
        assert_eq!(state.line(true), 0);
        assert_eq!(state.position, 0);
    }

    /// The File associations page puts its two buttons side by side, below the list, without
    /// running off the page.
    #[test]
    fn the_file_types_page_has_room_for_both_buttons() {
        let placed = layout(Tab::FileAssociations);
        let file_types = placed.file_types.expect("the file types surface");
        assert!(file_types.list.y >= file_types.explanation.bottom());
        assert!(file_types.register_button.y >= file_types.list.bottom());
        assert_eq!(
            file_types.windows_settings_button.y,
            file_types.register_button.y
        );
        assert!(file_types.windows_settings_button.right() <= PAGE_WIDTH - metrics::PAGE_MARGIN);
    }

    /// The RAW page's Reset button lands below everything else, at the right edge.
    #[test]
    fn reset_lands_under_the_last_group() {
        let placed = layout(Tab::Raw);
        let reset = placed.reset_button.expect("the RAW page resets");
        let last = placed
            .groups
            .last()
            .and_then(|group| group.rows.last())
            .expect("a last row");
        assert!(reset.y >= last.bottom());
        assert!(reset.right() <= PAGE_WIDTH - metrics::PAGE_MARGIN);
        assert!(placed.height >= reset.bottom());
    }
}
