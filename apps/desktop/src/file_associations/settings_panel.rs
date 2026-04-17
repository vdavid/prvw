//! "File associations" panel: two sections (standard formats, camera RAW) each with a
//! master toggle plus per-UTI toggles. Master toggles show a tri-state indicator when
//! some — but not all — formats in the section are handled by Prvw.
//!
//! Each per-UTI row shows a live handler-transparency caption beneath the format name:
//! "Currently opens with Preview.app." or "Before Prvw, these opened with Preview.app."
//! The captions refresh on every toggle and on the 1-second polling tick, so the user
//! always sees the truth even when another app steals the association via Get Info.
//!
//! The panel is self-contained: `build` creates the widgets, wires `FileAssocDelegate`
//! for toggle and 1-second polling callbacks, and returns just the outer `NSStackView`
//! for the Settings window to slot in. All `Retained` handles go into `retained_views`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSLayoutAttribute,
    NSLayoutConstraint, NSLayoutRelation, NSStackView, NSSwitch, NSTextAlignment, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::file_associations::{self, GroupState, SUPPORTED_RAW_UTIS, SUPPORTED_STANDARD_UTIS};
use crate::platform::macos::ui_common::{FlippedView, as_view, make_bold_label, make_label};

const STANDARD_COUNT: usize = SUPPORTED_STANDARD_UTIS.len();
const RAW_COUNT: usize = SUPPORTED_RAW_UTIS.len();
const TOTAL_COUNT: usize = STANDARD_COUNT + RAW_COUNT;

/// Tag offset: per-UTI toggles get tags `[0..TOTAL_COUNT)`, the two section masters
/// get tags `TAG_STANDARD_MASTER` and `TAG_RAW_MASTER` to route through `tag` dispatch.
const TAG_STANDARD_MASTER: isize = 1000;
const TAG_RAW_MASTER: isize = 1001;

/// AppKit's `NSSwitch` only renders on/off — it has no built-in "mixed" visual. When a
/// section is partially enabled, we signal this by dimming the switch and showing a
/// "Mixed" pill next to it. Click on a mixed switch follows macOS Finder's convention:
/// promote to all-on.
const SWITCH_ALPHA_MIXED: f64 = 0.55;
const SWITCH_ALPHA_FULL: f64 = 1.0;

/// Which group a master row covers, for status-copy selection.
#[derive(Copy, Clone)]
enum Section {
    Standard,
    Raw,
}

struct FileAssocDelegateIvars {
    /// Per-UTI toggles, combined order: standard formats then RAW.
    uti_toggles: [*const NSSwitch; TOTAL_COUNT],
    /// Per-UTI secondary captions showing live handler state.
    uti_secondary: [*const NSTextField; TOTAL_COUNT],
    /// "Set all standard formats" row widgets.
    standard_master: MasterRowPtrs,
    /// "Set all RAW formats" row widgets.
    raw_master: MasterRowPtrs,
}

#[derive(Copy, Clone)]
struct MasterRowPtrs {
    toggle: *const NSSwitch,
    mixed_pill: *const NSTextField,
    secondary: *const NSTextField,
    section: Section,
}

// SAFETY: Raw pointers are only used on the main thread within the window's lifetime.
unsafe impl Send for FileAssocDelegateIvars {}
unsafe impl Sync for FileAssocDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwFileAssocDelegate"]
    #[ivars = FileAssocDelegateIvars]
    struct FileAssocDelegate;

    unsafe impl NSObjectProtocol for FileAssocDelegate {}

    impl FileAssocDelegate {
        /// Per-UTI toggle click. Tag in `[0..TOTAL_COUNT)` selects which UTI.
        #[unsafe(method(toggleFileAssoc:))]
        fn toggle_file_assoc(&self, sender: &NSSwitch) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let idx = tag as usize;
            if idx >= TOTAL_COUNT {
                return;
            }
            let uti = combined_entry(idx).uti;
            if sender.state() == NSControlStateValueOn {
                file_associations::set_prvw_as_handler(uti);
            } else {
                file_associations::restore_handler(uti);
            }
            self.refresh_all();
        }

        /// Master toggle click. Tag selects which section.
        #[unsafe(method(toggleSetAll:))]
        fn toggle_set_all(&self, sender: &NSSwitch) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let (group, state) = match tag {
                TAG_STANDARD_MASTER => (SUPPORTED_STANDARD_UTIS, section_state(SUPPORTED_STANDARD_UTIS)),
                TAG_RAW_MASTER => (SUPPORTED_RAW_UTIS, section_state(SUPPORTED_RAW_UTIS)),
                _ => return,
            };
            // macOS Finder convention: mixed click promotes to all-on, not all-off.
            // So Off/Mixed → all on; All → all off.
            let turn_on = !matches!(state, GroupState::All);
            for entry in group {
                if turn_on {
                    file_associations::set_prvw_as_handler(entry.uti);
                } else {
                    file_associations::restore_handler(entry.uti);
                }
            }
            self.refresh_all();
        }

        /// Called by NSTimer every 1 second to poll file association state.
        #[unsafe(method(pollFileAssoc:))]
        fn poll_file_assoc(&self, _timer: &AnyObject) {
            self.refresh_all();
        }
    }
);

impl FileAssocDelegate {
    fn new(mtm: MainThreadMarker, ivars: FileAssocDelegateIvars) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Re-query handler state and update every toggle, every per-row caption, and both
    /// section masters.
    fn refresh_all(&self) {
        let ivars = self.ivars();

        for (idx, entry) in SUPPORTED_STANDARD_UTIS
            .iter()
            .chain(SUPPORTED_RAW_UTIS.iter())
            .enumerate()
        {
            let is_prvw = file_associations::is_prvw_default(entry.uti);
            let toggle = ivars.uti_toggles[idx];
            if !toggle.is_null() {
                let state = if is_prvw {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                unsafe {
                    let _: () = msg_send![toggle, setState: state];
                }
            }
            let secondary = ivars.uti_secondary[idx];
            if !secondary.is_null() {
                let caption = row_caption(entry.uti, is_prvw);
                unsafe {
                    (*secondary).setStringValue(&NSString::from_str(&caption));
                }
            }
        }

        render_master_row(
            &ivars.standard_master,
            section_state(SUPPORTED_STANDARD_UTIS),
        );
        render_master_row(&ivars.raw_master, section_state(SUPPORTED_RAW_UTIS));
    }
}

/// The combined per-UTI list, in tag order.
fn combined_entry(idx: usize) -> &'static file_associations::UtiEntry {
    if idx < STANDARD_COUNT {
        &SUPPORTED_STANDARD_UTIS[idx]
    } else {
        &SUPPORTED_RAW_UTIS[idx - STANDARD_COUNT]
    }
}

/// Compute the tri-state summary of a section from live handler queries.
fn section_state(group: &[file_associations::UtiEntry]) -> GroupState {
    let flags: Vec<bool> = group
        .iter()
        .map(|e| file_associations::is_prvw_default(e.uti))
        .collect();
    GroupState::from_flags(&flags)
}

/// Per-row handler-state caption. Uses the pure [`format_row_caption`] helper so the
/// wording has unit test coverage; this thin wrapper does the live lookups.
fn row_caption(uti: &str, is_prvw: bool) -> String {
    if is_prvw {
        format_row_caption(is_prvw, file_associations::previous_handler_name(uti))
    } else {
        format_row_caption(
            is_prvw,
            file_associations::get_handler_bundle_id(uti)
                .map(|id| file_associations::bundle_id_to_app_name(&id)),
        )
    }
}

/// Pure formatter for the per-row caption. `other` is the app name to mention —
/// the previous handler when Prvw is current, or the current handler when Prvw isn't.
fn format_row_caption(is_prvw: bool, other: Option<String>) -> String {
    if is_prvw {
        match other {
            Some(name) => format!("Before Prvw, these opened with {name}."),
            None => "Before Prvw, these opened with another app.".to_string(),
        }
    } else {
        match other {
            Some(name) => format!("Currently opens with {name}."),
            None => "Currently opens with another app.".to_string(),
        }
    }
}

/// Copy for a master-row secondary caption. Pure, so it's unit-testable.
fn master_caption(section: Section, state: GroupState) -> &'static str {
    match (section, state) {
        (Section::Standard, GroupState::All) => "Prvw handles every standard format.",
        (Section::Standard, GroupState::None) => "Other apps handle the standard formats.",
        (Section::Standard, GroupState::Mixed) => "Some standard formats open with other apps.",
        (Section::Raw, GroupState::All) => "Prvw handles every camera RAW format.",
        (Section::Raw, GroupState::None) => "Other apps handle the camera RAW formats.",
        (Section::Raw, GroupState::Mixed) => "Some camera RAW formats open with other apps.",
    }
}

/// Apply a tri-state to a master row's switch + mixed pill + secondary text.
fn render_master_row(ptrs: &MasterRowPtrs, state: GroupState) {
    unsafe {
        if !ptrs.toggle.is_null() {
            // NSSwitch has no native mixed render, so Mixed maps to Off visually plus a
            // dimmed alpha and a "Mixed" pill beside it.
            let value = if matches!(state, GroupState::All) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            };
            let alpha = if matches!(state, GroupState::Mixed) {
                SWITCH_ALPHA_MIXED
            } else {
                SWITCH_ALPHA_FULL
            };
            let _: () = msg_send![ptrs.toggle, setState: value];
            let _: () = msg_send![ptrs.toggle, setAlphaValue: alpha];
        }
        if !ptrs.mixed_pill.is_null() {
            let hidden = !matches!(state, GroupState::Mixed);
            let _: () = msg_send![ptrs.mixed_pill, setHidden: hidden];
        }
        if !ptrs.secondary.is_null() {
            let text = master_caption(ptrs.section, state);
            (*ptrs.secondary).setStringValue(&NSString::from_str(text));
        }
    }
}

/// Handles to the widgets we expose to the delegate for a section master.
struct MasterRow {
    row: Retained<NSStackView>,
    toggle: Retained<NSSwitch>,
    mixed_pill: Retained<NSTextField>,
    secondary: Retained<NSTextField>,
    /// Everything owned by this row that we need to retain for the window's lifetime
    /// (labels, spacers, label stacks). Added to the panel's `retained_views` at the
    /// end of `build`.
    extras: Vec<Retained<AnyObject>>,
}

/// Build a section master row: bold title + secondary description on the left, optional
/// "Mixed" pill + NSSwitch on the right.
fn build_master_row(title: &str, tag: isize, mtm: MainThreadMarker) -> MasterRow {
    let title_label = make_bold_label(title, 14.0, mtm);
    title_label.setAlignment(NSTextAlignment(0));

    let secondary = make_label("", 12.0, mtm);
    secondary.setAlignment(NSTextAlignment(0));
    secondary.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let label_stack = NSStackView::new(mtm);
    label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    label_stack.setAlignment(NSLayoutAttribute::Leading);
    label_stack.setSpacing(2.0);
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&title_label) });
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&secondary) });

    let spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let mixed_pill = make_mixed_pill(mtm);
    unsafe {
        let _: () = msg_send![&*mixed_pill, setHidden: true];
    }

    let toggle = NSSwitch::new(mtm);
    unsafe {
        let _: () = msg_send![&*toggle, setTag: tag];
    }

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(10.0);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.addArrangedSubview(unsafe { as_view::<NSStackView>(&label_stack) });
    row.addArrangedSubview(unsafe { as_view::<NSView>(&spacer) });
    row.addArrangedSubview(unsafe { as_view::<NSTextField>(&mixed_pill) });
    row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&toggle) });

    let extras: Vec<Retained<AnyObject>> = unsafe {
        vec![
            Retained::cast_unchecked(title_label),
            Retained::cast_unchecked(spacer),
            Retained::cast_unchecked(label_stack),
        ]
    };

    MasterRow {
        row,
        toggle,
        mixed_pill,
        secondary,
        extras,
    }
}

/// "Mixed" pill label — small caps, secondary color, rounded background.
fn make_mixed_pill(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let pill = NSTextField::labelWithString(&NSString::from_str("Mixed"), mtm);
    pill.setFont(Some(&NSFont::systemFontOfSize(10.0)));
    pill.setEditable(false);
    pill.setSelectable(false);
    pill.setBordered(false);
    pill.setTextColor(Some(&NSColor::secondaryLabelColor()));
    pill.setAlignment(NSTextAlignment(2));
    unsafe {
        let _: () = msg_send![&*pill, setDrawsBackground: true];
        // A lightly tinted background so the pill reads as an indicator, not body text.
        let bg = NSColor::quaternaryLabelColor();
        let _: () = msg_send![&*pill, setBackgroundColor: &*bg];
    }
    pill
}

/// Section header label above both the master row and the per-UTI rows.
fn make_section_header(title: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
    label.setEditable(false);
    label.setSelectable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setAlignment(NSTextAlignment(0));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label
}

/// Format the primary per-row label, inlining the detail (vendor or extension list)
/// in parens. Example: `"JPEG (*.jpg, *.jpeg)"`, `"DNG (Universal)"`, `"CR2 (Canon)"`.
fn primary_label_text(entry: &file_associations::UtiEntry) -> String {
    format!("{} ({})", entry.label, entry.detail)
}

/// Build a per-UTI row: primary label + live handler caption stacked on the left, small
/// switch on the right. Returns the row view, the switch, and the caption label — the
/// delegate needs the caption label to refresh text on every tick.
fn build_uti_row(
    entry: &file_associations::UtiEntry,
    tag: isize,
    mtm: MainThreadMarker,
    extras: &mut Vec<Retained<AnyObject>>,
) -> (
    Retained<NSStackView>,
    Retained<NSSwitch>,
    Retained<NSTextField>,
) {
    let primary = make_label(&primary_label_text(entry), 13.0, mtm);
    primary.setAlignment(NSTextAlignment(0));

    // Start with the caption the initial refresh will overwrite — having a non-empty
    // string here keeps the row's vertical size stable before the first refresh tick.
    let caption = make_label(" ", 12.0, mtm);
    caption.setAlignment(NSTextAlignment(0));
    caption.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let toggle = NSSwitch::new(mtm);
    unsafe {
        let _: () = msg_send![&*toggle, setTag: tag];
        // NSControlSizeSmall = 1 — distinguishes per-format from section masters.
        let _: () = msg_send![&*toggle, setControlSize: 1i64];
    }

    let spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let label_stack = NSStackView::new(mtm);
    label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    label_stack.setAlignment(NSLayoutAttribute::Leading);
    label_stack.setSpacing(2.0);
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&primary) });
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&caption) });

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(10.0);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.addArrangedSubview(unsafe { as_view::<NSStackView>(&label_stack) });
    row.addArrangedSubview(unsafe { as_view::<NSView>(&spacer) });
    row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&toggle) });

    unsafe {
        extras.push(Retained::cast_unchecked(primary));
        extras.push(Retained::cast_unchecked(spacer));
        extras.push(Retained::cast_unchecked(label_stack));
    }

    (row, toggle, caption)
}

/// Build the File associations panel. Wires toggles, starts the 1-second polling timer,
/// and returns the outer view for the Settings window to slot into its layout.
pub(crate) fn build(
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    // ── Master rows + per-UTI rows ────────────────────────────────────
    let mut standard_master = build_master_row(
        "Set all standard formats as default",
        TAG_STANDARD_MASTER,
        mtm,
    );
    let mut raw_master = build_master_row("Set all RAW formats as default", TAG_RAW_MASTER, mtm);

    let standard_header = make_section_header("Standard image formats", mtm);
    let raw_header = make_section_header("Camera RAW formats", mtm);

    let mut uti_toggle_ptrs: [*const NSSwitch; TOTAL_COUNT] = [std::ptr::null(); TOTAL_COUNT];
    let mut uti_secondary_ptrs: [*const NSTextField; TOTAL_COUNT] = [std::ptr::null(); TOTAL_COUNT];
    let mut uti_toggle_retained: Vec<Retained<NSSwitch>> = Vec::with_capacity(TOTAL_COUNT);
    let mut uti_caption_retained: Vec<Retained<NSTextField>> = Vec::with_capacity(TOTAL_COUNT);
    let mut uti_rows: Vec<Retained<NSStackView>> = Vec::with_capacity(TOTAL_COUNT);
    let mut uti_row_extras: Vec<Retained<AnyObject>> = Vec::new();

    for (offset, entry) in SUPPORTED_STANDARD_UTIS.iter().enumerate() {
        let tag = offset as isize;
        let (row, toggle, caption) = build_uti_row(entry, tag, mtm, &mut uti_row_extras);
        uti_toggle_ptrs[offset] = &*toggle as *const NSSwitch;
        uti_secondary_ptrs[offset] = &*caption as *const NSTextField;
        uti_toggle_retained.push(toggle);
        uti_caption_retained.push(caption);
        uti_rows.push(row);
    }
    for (offset, entry) in SUPPORTED_RAW_UTIS.iter().enumerate() {
        let tag = (STANDARD_COUNT + offset) as isize;
        let (row, toggle, caption) = build_uti_row(entry, tag, mtm, &mut uti_row_extras);
        let idx = STANDARD_COUNT + offset;
        uti_toggle_ptrs[idx] = &*toggle as *const NSSwitch;
        uti_secondary_ptrs[idx] = &*caption as *const NSTextField;
        uti_toggle_retained.push(toggle);
        uti_caption_retained.push(caption);
        uti_rows.push(row);
    }

    // ── Assemble the panel stack ──────────────────────────────────────
    let panel = NSStackView::new(mtm);
    panel.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setSpacing(6.0);

    let add = |panel: &NSStackView, view: &NSView| panel.addArrangedSubview(view);

    add(&panel, unsafe { as_view::<NSTextField>(&standard_header) });
    add(&panel, unsafe {
        as_view::<NSStackView>(&standard_master.row)
    });
    for row in &uti_rows[..STANDARD_COUNT] {
        add(&panel, unsafe { as_view::<NSStackView>(row) });
    }
    add(&panel, unsafe { as_view::<NSTextField>(&raw_header) });
    add(&panel, unsafe { as_view::<NSStackView>(&raw_master.row) });
    for row in &uti_rows[STANDARD_COUNT..] {
        add(&panel, unsafe { as_view::<NSStackView>(row) });
    }

    // Larger gaps between the logical groupings.
    panel.setCustomSpacing_afterView(14.0, unsafe {
        as_view::<NSStackView>(&standard_master.row)
    });
    panel.setCustomSpacing_afterView(20.0, unsafe {
        as_view::<NSStackView>(&uti_rows[STANDARD_COUNT - 1])
    });
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&raw_master.row) });

    // Pin every row to full panel width so switches align on the right edge.
    pin_width(&standard_master.row, &panel, retained_views);
    pin_width(&raw_master.row, &panel, retained_views);
    for row in &uti_rows {
        pin_width(row, &panel, retained_views);
    }

    unsafe {
        let _: () = msg_send![&*panel, setHidden: true];
    }

    // ── Delegate + target/action + polling timer ──────────────────────
    let ivars = FileAssocDelegateIvars {
        uti_toggles: uti_toggle_ptrs,
        uti_secondary: uti_secondary_ptrs,
        standard_master: MasterRowPtrs {
            toggle: &*standard_master.toggle as *const NSSwitch,
            mixed_pill: &*standard_master.mixed_pill as *const NSTextField,
            secondary: &*standard_master.secondary as *const NSTextField,
            section: Section::Standard,
        },
        raw_master: MasterRowPtrs {
            toggle: &*raw_master.toggle as *const NSSwitch,
            mixed_pill: &*raw_master.mixed_pill as *const NSTextField,
            secondary: &*raw_master.secondary as *const NSTextField,
            section: Section::Raw,
        },
    };
    let delegate = FileAssocDelegate::new(mtm, ivars);

    unsafe {
        standard_master
            .toggle
            .setTarget(Some(&delegate as &AnyObject));
        standard_master.toggle.setAction(Some(sel!(toggleSetAll:)));

        raw_master.toggle.setTarget(Some(&delegate as &AnyObject));
        raw_master.toggle.setAction(Some(sel!(toggleSetAll:)));

        for toggle in &uti_toggle_retained {
            toggle.setTarget(Some(&delegate as &AnyObject));
            toggle.setAction(Some(sel!(toggleFileAssoc:)));
        }
    }

    // Sync the visuals to current system state before the first timer tick.
    delegate.refresh_all();

    let delegate_ptr: *const AnyObject = &*delegate as *const FileAssocDelegate as *const AnyObject;
    let poll_timer: Retained<AnyObject> = unsafe {
        msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: delegate_ptr,
            selector: sel!(pollFileAssoc:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ]
    };

    // ── Retain everything for the window's lifetime ───────────────────
    retained_views.push(unsafe { Retained::cast_unchecked(standard_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw_header) });

    for extra in standard_master.extras.drain(..) {
        retained_views.push(extra);
    }
    for extra in raw_master.extras.drain(..) {
        retained_views.push(extra);
    }
    retained_views.push(unsafe { Retained::cast_unchecked(standard_master.row) });
    retained_views.push(unsafe { Retained::cast_unchecked(standard_master.toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(standard_master.mixed_pill) });
    retained_views.push(unsafe { Retained::cast_unchecked(standard_master.secondary) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw_master.row) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw_master.toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw_master.mixed_pill) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw_master.secondary) });

    for extra in uti_row_extras {
        retained_views.push(extra);
    }
    for row in uti_rows {
        retained_views.push(unsafe { Retained::cast_unchecked(row) });
    }
    for toggle in uti_toggle_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(toggle) });
    }
    for caption in uti_caption_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(caption) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });
    retained_views.push(poll_timer);

    panel
}

fn pin_width(
    row: &NSStackView,
    panel: &NSStackView,
    retained_views: &mut Vec<Retained<AnyObject>>,
) {
    let c = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            row, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            Some(panel as &AnyObject), NSLayoutAttribute::Width,
            1.0, 0.0,
        )
    };
    c.setActive(true);
    retained_views.push(unsafe { Retained::cast_unchecked(c) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_caption_when_prvw_with_known_previous() {
        assert_eq!(
            format_row_caption(true, Some("Preview.app".to_string())),
            "Before Prvw, these opened with Preview.app."
        );
    }

    #[test]
    fn row_caption_when_prvw_with_unknown_previous() {
        assert_eq!(
            format_row_caption(true, None),
            "Before Prvw, these opened with another app."
        );
    }

    #[test]
    fn row_caption_when_other_app_is_known() {
        assert_eq!(
            format_row_caption(false, Some("Photos.app".to_string())),
            "Currently opens with Photos.app."
        );
    }

    #[test]
    fn row_caption_when_other_app_is_unknown() {
        assert_eq!(
            format_row_caption(false, None),
            "Currently opens with another app."
        );
    }

    #[test]
    fn master_caption_reads_per_section_and_state() {
        assert_eq!(
            master_caption(Section::Standard, GroupState::All),
            "Prvw handles every standard format."
        );
        assert_eq!(
            master_caption(Section::Standard, GroupState::None),
            "Other apps handle the standard formats."
        );
        assert_eq!(
            master_caption(Section::Standard, GroupState::Mixed),
            "Some standard formats open with other apps."
        );
        assert_eq!(
            master_caption(Section::Raw, GroupState::All),
            "Prvw handles every camera RAW format."
        );
        assert_eq!(
            master_caption(Section::Raw, GroupState::None),
            "Other apps handle the camera RAW formats."
        );
        assert_eq!(
            master_caption(Section::Raw, GroupState::Mixed),
            "Some camera RAW formats open with other apps."
        );
    }

    #[test]
    fn primary_label_inlines_detail() {
        let jpeg = &SUPPORTED_STANDARD_UTIS[0];
        assert_eq!(primary_label_text(jpeg), "JPEG (*.jpg, *.jpeg)");
        let dng = &SUPPORTED_RAW_UTIS[0];
        assert_eq!(primary_label_text(dng), "DNG (Universal)");
    }
}
