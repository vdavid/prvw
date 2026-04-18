//! "RAW" panel: per-stage toggles for the RAW decode pipeline + a custom
//! DCP directory picker (Phase 3.7).
//!
//! Each row mirrors the pattern in the other panels: title label on the left,
//! NSSwitch on the right, secondary description directly underneath. Three
//! section headers (sensor corrections, color, tone, detail) and a final
//! "Custom DCP directory" row + "Reset to defaults" button live at the bottom.
//!
//! All toggles write back through a single `AppCommand::SetRawPipelineFlags`
//! so the executor flushes the image cache and re-decodes once per change.
//! The custom DCP directory goes through `AppCommand::SetCustomDcpDir` and
//! hits the same flush path via `App::apply_custom_dcp_dir_change`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSOpenPanel, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};

use crate::commands::{self, AppCommand};
use crate::decoding::RawPipelineFlags;
use crate::platform::macos::ui_common::{FlippedView, as_view, make_bold_label, make_label};
use crate::settings::Settings;

/// Tag assignments for each toggle. The `raw_toggle_tag_to_flag` mutator
/// decodes these back into a pipeline-flag mutation. Order matches the UI.
const TAG_OPCODE_1: isize = 100;
const TAG_OPCODE_2: isize = 101;
const TAG_OPCODE_3: isize = 102;
const TAG_BASELINE_EXPOSURE: isize = 110;
const TAG_DCP_HUE_SAT_MAP: isize = 111;
const TAG_DCP_LOOK_TABLE: isize = 112;
const TAG_SATURATION_BOOST: isize = 113;
const TAG_HIGHLIGHT_RECOVERY: isize = 120;
const TAG_TONE_CURVE: isize = 121;
const TAG_CAPTURE_SHARPENING: isize = 130;

/// Ivars are raw pointers so the delegate can read current toggle states
/// without holding Rust borrows through AppKit message dispatch. They all
/// point into `retained_views`, which outlives the window.
struct RawDelegateIvars {
    // Pipeline toggles, one pointer per flag.
    dng_opcode_1: *const NSSwitch,
    dng_opcode_2: *const NSSwitch,
    dng_opcode_3: *const NSSwitch,
    baseline_exposure: *const NSSwitch,
    dcp_hue_sat_map: *const NSSwitch,
    dcp_look_table: *const NSSwitch,
    saturation_boost: *const NSSwitch,
    highlight_recovery: *const NSSwitch,
    tone_curve: *const NSSwitch,
    capture_sharpening: *const NSSwitch,
    // Custom DCP dir row.
    custom_dcp_field: *const NSTextField,
}

// SAFETY: Raw pointers are only used on the main thread within the window's lifetime.
unsafe impl Send for RawDelegateIvars {}
unsafe impl Sync for RawDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwRawSettingsDelegate"]
    #[ivars = RawDelegateIvars]
    struct RawDelegate;

    unsafe impl NSObjectProtocol for RawDelegate {}

    impl RawDelegate {
        /// Any per-stage toggle click. We read the current state of every
        /// toggle and send a single `SetRawPipelineFlags` command, so the
        /// executor flushes and re-decodes exactly once per click.
        #[unsafe(method(togglePipelineFlag:))]
        fn toggle_pipeline_flag(&self, _sender: &NSSwitch) {
            let flags = self.read_current_flags();
            log::debug!(
                "RAW panel: flag toggle -> {} disabled",
                flags.disabled_step_labels().len()
            );
            commands::send_command(AppCommand::SetRawPipelineFlags(flags));
        }

        /// "Reset to defaults" button click. Sends an all-true flags command
        /// and refreshes each switch to match.
        #[unsafe(method(resetToDefaults:))]
        fn reset_to_defaults(&self, _sender: &AnyObject) {
            let defaults = RawPipelineFlags::default();
            self.write_flags_to_switches(defaults);
            self.clear_custom_dcp_field();
            commands::send_command(AppCommand::SetRawPipelineFlags(defaults));
            commands::send_command(AppCommand::SetCustomDcpDir(None));
        }

        /// "Browse…" button for the custom DCP directory. Opens an
        /// NSOpenPanel restricted to directories, writes the result into
        /// the text field, and broadcasts `SetCustomDcpDir`.
        #[unsafe(method(browseDcpDir:))]
        fn browse_dcp_dir(&self, _sender: &AnyObject) {
            // SAFETY: we're on the main thread (AppKit action).
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            panel.setAllowsMultipleSelection(false);
            panel.setPrompt(Some(&NSString::from_str("Choose")));

            let response: i64 = unsafe { msg_send![&*panel, runModal] };
            // NSModalResponseOK = 1. Anything else (cancel, esc) is a no-op.
            if response != 1 {
                return;
            }
            let urls: Retained<NSArray<NSURL>> = panel.URLs();
            let count: usize = unsafe { msg_send![&*urls, count] };
            if count == 0 {
                return;
            }
            let url: *const NSURL = unsafe { msg_send![&*urls, objectAtIndex: 0usize] };
            if url.is_null() {
                return;
            }
            let path: Retained<NSString> = unsafe { msg_send![url, path] };
            let path_string = path.to_string();
            self.set_custom_dcp_field_text(&path_string);
            commands::send_command(AppCommand::SetCustomDcpDir(Some(path_string)));
        }

        /// "Clear" button next to the custom DCP field. Blanks the field
        /// and sends `None` so discovery falls back to the bundled path.
        #[unsafe(method(clearDcpDir:))]
        fn clear_dcp_dir(&self, _sender: &AnyObject) {
            self.clear_custom_dcp_field();
            commands::send_command(AppCommand::SetCustomDcpDir(None));
        }
    }
);

impl RawDelegate {
    fn new(mtm: MainThreadMarker, ivars: RawDelegateIvars) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Snapshot every switch into a fresh `RawPipelineFlags`.
    fn read_current_flags(&self) -> RawPipelineFlags {
        let ivars = self.ivars();
        RawPipelineFlags {
            dng_opcode_list_1: switch_is_on(ivars.dng_opcode_1),
            dng_opcode_list_2: switch_is_on(ivars.dng_opcode_2),
            dng_opcode_list_3: switch_is_on(ivars.dng_opcode_3),
            baseline_exposure: switch_is_on(ivars.baseline_exposure),
            dcp_hue_sat_map: switch_is_on(ivars.dcp_hue_sat_map),
            dcp_look_table: switch_is_on(ivars.dcp_look_table),
            saturation_boost: switch_is_on(ivars.saturation_boost),
            highlight_recovery: switch_is_on(ivars.highlight_recovery),
            tone_curve: switch_is_on(ivars.tone_curve),
            capture_sharpening: switch_is_on(ivars.capture_sharpening),
        }
    }

    fn write_flags_to_switches(&self, flags: RawPipelineFlags) {
        let ivars = self.ivars();
        set_switch(ivars.dng_opcode_1, flags.dng_opcode_list_1);
        set_switch(ivars.dng_opcode_2, flags.dng_opcode_list_2);
        set_switch(ivars.dng_opcode_3, flags.dng_opcode_list_3);
        set_switch(ivars.baseline_exposure, flags.baseline_exposure);
        set_switch(ivars.dcp_hue_sat_map, flags.dcp_hue_sat_map);
        set_switch(ivars.dcp_look_table, flags.dcp_look_table);
        set_switch(ivars.saturation_boost, flags.saturation_boost);
        set_switch(ivars.highlight_recovery, flags.highlight_recovery);
        set_switch(ivars.tone_curve, flags.tone_curve);
        set_switch(ivars.capture_sharpening, flags.capture_sharpening);
    }

    fn clear_custom_dcp_field(&self) {
        self.set_custom_dcp_field_text("");
    }

    fn set_custom_dcp_field_text(&self, text: &str) {
        let field = self.ivars().custom_dcp_field;
        if !field.is_null() {
            unsafe {
                (*field).setStringValue(&NSString::from_str(text));
            }
        }
    }
}

fn switch_is_on(ptr: *const NSSwitch) -> bool {
    if ptr.is_null() {
        return true;
    }
    unsafe { (*ptr).state() == NSControlStateValueOn }
}

fn set_switch(ptr: *const NSSwitch, on: bool) {
    if ptr.is_null() {
        return;
    }
    let state = if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    };
    unsafe {
        let _: () = msg_send![ptr, setState: state];
    }
}

/// Output of `build`: the outer stack view. We don't hand individual switch
/// handles back — the delegate owns all of them and funnels changes through
/// its own method impls.
pub(crate) struct RawPanel {
    pub panel: Retained<NSStackView>,
}

/// One per-flag row: bold title, 12pt description beneath, switch on the right.
struct FlagRow {
    row: Retained<NSStackView>,
    toggle: Retained<NSSwitch>,
    extras: Vec<Retained<AnyObject>>,
}

fn build_flag_row(
    title: &str,
    description: &str,
    is_on: bool,
    tag: isize,
    mtm: MainThreadMarker,
) -> FlagRow {
    let title_label = make_label(title, 13.0, mtm);
    title_label.setAlignment(NSTextAlignment(0));

    let desc_label = make_label(description, 11.0, mtm);
    desc_label.setAlignment(NSTextAlignment(0));
    desc_label.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let label_stack = NSStackView::new(mtm);
    label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    label_stack.setAlignment(NSLayoutAttribute::Leading);
    label_stack.setSpacing(2.0);
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&title_label) });
    label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&desc_label) });

    let spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let toggle = NSSwitch::new(mtm);
    toggle.setState(if is_on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    unsafe {
        let _: () = msg_send![&*toggle, setTag: tag];
        // Small control size matches the per-UTI rows in File associations.
        let _: () = msg_send![&*toggle, setControlSize: 1i64];
    }

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(10.0);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.addArrangedSubview(unsafe { as_view::<NSStackView>(&label_stack) });
    row.addArrangedSubview(unsafe { as_view::<NSView>(&spacer) });
    row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&toggle) });

    let extras: Vec<Retained<AnyObject>> = unsafe {
        vec![
            Retained::cast_unchecked(title_label),
            Retained::cast_unchecked(desc_label),
            Retained::cast_unchecked(spacer),
            Retained::cast_unchecked(label_stack),
        ]
    };

    FlagRow {
        row,
        toggle,
        extras,
    }
}

fn make_section_header(title: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = make_bold_label(title, 11.0, mtm);
    label.setAlignment(NSTextAlignment(0));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label
}

/// Build the custom DCP directory row: bold title, caption, path field,
/// Browse + Clear buttons. Returns the outer stack view plus the path
/// field (the delegate rewrites its text on browse / clear actions).
fn build_custom_dcp_row(
    current_path: Option<&str>,
    mtm: MainThreadMarker,
    retained_views: &mut Vec<Retained<AnyObject>>,
) -> (
    Retained<NSStackView>,
    Retained<NSTextField>,
    Retained<NSButton>,
    Retained<NSButton>,
) {
    let title = make_bold_label("Custom DCP directory", 13.0, mtm);
    title.setAlignment(NSTextAlignment(0));

    let caption = make_label(
        "User-provided DCPs in this directory override the bundled set.",
        11.0,
        mtm,
    );
    caption.setAlignment(NSTextAlignment(0));
    caption.setTextColor(Some(&NSColor::secondaryLabelColor()));

    // Plain NSTextField (not a label) so the user can paste a path too.
    let field = unsafe {
        let f = NSTextField::new(mtm);
        f.setEditable(true);
        f.setSelectable(true);
        f.setBordered(true);
        f.setDrawsBackground(true);
        f.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        f.setStringValue(&NSString::from_str(current_path.unwrap_or("")));
        let _: () = msg_send![&*f, setPlaceholderString: &*NSString::from_str("/path/to/dcps")];
        let _: () = msg_send![&*f, setTranslatesAutoresizingMaskIntoConstraints: false];
        f
    };

    let browse = unsafe {
        let b = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Browse\u{2026}"),
            None,
            None,
            mtm,
        );
        b.setBezelStyle(NSBezelStyle::Push);
        let _: () = msg_send![&*b, setControlSize: 1i64];
        b
    };

    let clear = unsafe {
        let b =
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Clear"), None, None, mtm);
        b.setBezelStyle(NSBezelStyle::Push);
        let _: () = msg_send![&*b, setControlSize: 1i64];
        b
    };

    let buttons_row = NSStackView::new(mtm);
    buttons_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    buttons_row.setSpacing(6.0);
    buttons_row.setAlignment(NSLayoutAttribute::CenterY);
    buttons_row.addArrangedSubview(unsafe { as_view::<NSTextField>(&field) });
    buttons_row.addArrangedSubview(unsafe { as_view::<NSButton>(&browse) });
    buttons_row.addArrangedSubview(unsafe { as_view::<NSButton>(&clear) });

    // Let the text field expand to fill the row.
    unsafe {
        let _: () = msg_send![&*field, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let outer = NSStackView::new(mtm);
    outer.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    outer.setAlignment(NSLayoutAttribute::Leading);
    outer.setSpacing(4.0);
    outer.addArrangedSubview(unsafe { as_view::<NSTextField>(&title) });
    outer.addArrangedSubview(unsafe { as_view::<NSStackView>(&buttons_row) });
    outer.addArrangedSubview(unsafe { as_view::<NSTextField>(&caption) });

    // Pin the buttons row to the panel width.
    unsafe {
        let _: () = msg_send![&*buttons_row, setTranslatesAutoresizingMaskIntoConstraints: false];
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &buttons_row, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            Some(&outer as &AnyObject), NSLayoutAttribute::Width,
            1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));
    }

    retained_views.push(unsafe { Retained::cast_unchecked(title) });
    retained_views.push(unsafe { Retained::cast_unchecked(caption) });
    retained_views.push(unsafe { Retained::cast_unchecked(buttons_row) });

    (outer, field, browse, clear)
}

pub(crate) fn build(
    settings: &Settings,
    _content_max_width: f64,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> RawPanel {
    let flags = settings.raw;

    // ── All ten per-stage rows ────────────────────────────────────────
    let rows: Vec<FlagRow> = vec![
        build_flag_row(
            "DNG OpcodeList 1",
            "Pre-linearization gain maps and bad-pixel fixes (DNG only).",
            flags.dng_opcode_list_1,
            TAG_OPCODE_1,
            mtm,
        ),
        build_flag_row(
            "DNG OpcodeList 2",
            "CFA-level gain maps and bad-pixel fixes (DNG only, iPhone ProRAW).",
            flags.dng_opcode_list_2,
            TAG_OPCODE_2,
            mtm,
        ),
        build_flag_row(
            "DNG OpcodeList 3",
            "Post-color lens distortion correction (DNG only).",
            flags.dng_opcode_list_3,
            TAG_OPCODE_3,
            mtm,
        ),
        build_flag_row(
            "Baseline exposure",
            "Lift exposure by +0.5 EV or the camera-specified value.",
            flags.baseline_exposure,
            TAG_BASELINE_EXPOSURE,
            mtm,
        ),
        build_flag_row(
            "DCP HueSatMap",
            "Per-camera color calibration table from the profile.",
            flags.dcp_hue_sat_map,
            TAG_DCP_HUE_SAT_MAP,
            mtm,
        ),
        build_flag_row(
            "DCP LookTable",
            "Adobe \u{201C}Look\u{201D} refinement applied after HueSatMap.",
            flags.dcp_look_table,
            TAG_DCP_LOOK_TABLE,
            mtm,
        ),
        build_flag_row(
            "Saturation boost",
            "Mild global chroma lift (+8 %) in linear Rec.2020.",
            flags.saturation_boost,
            TAG_SATURATION_BOOST,
            mtm,
        ),
        build_flag_row(
            "Highlight recovery",
            "Desaturate near-clip pixels toward their own luminance.",
            flags.highlight_recovery,
            TAG_HIGHLIGHT_RECOVERY,
            mtm,
        ),
        build_flag_row(
            "Tone curve",
            "S-shaped lift and shoulder (camera DCP curve when present).",
            flags.tone_curve,
            TAG_TONE_CURVE,
            mtm,
        ),
        build_flag_row(
            "Capture sharpening",
            "Mild unsharp mask on luminance in display space.",
            flags.capture_sharpening,
            TAG_CAPTURE_SHARPENING,
            mtm,
        ),
    ];

    // Pull out the toggle pointers before handing rows to the panel.
    let toggle_ptrs: Vec<*const NSSwitch> =
        rows.iter().map(|r| &*r.toggle as *const NSSwitch).collect();
    let toggles: Vec<Retained<NSSwitch>> = rows.iter().map(|r| r.toggle.clone()).collect();

    // ── Section headers ───────────────────────────────────────────────
    let sensor_header = make_section_header("Sensor corrections (DNG only)", mtm);
    let color_header = make_section_header("Color", mtm);
    let tone_header = make_section_header("Tone", mtm);
    let detail_header = make_section_header("Detail", mtm);

    // ── Custom DCP dir row + Reset button ─────────────────────────────
    let dcp_header = make_section_header("DCP profile", mtm);
    let (custom_dcp_outer, custom_dcp_field, browse_btn, clear_btn) =
        build_custom_dcp_row(settings.custom_dcp_dir.as_deref(), mtm, retained_views);

    let reset_btn = unsafe {
        let b = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Reset to defaults"),
            None,
            None,
            mtm,
        );
        b.setBezelStyle(NSBezelStyle::Push);
        b
    };
    let reset_row = NSStackView::new(mtm);
    reset_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    reset_row.setSpacing(8.0);
    reset_row.setAlignment(NSLayoutAttribute::CenterY);
    let reset_spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*reset_spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () =
            msg_send![&*reset_spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }
    reset_row.addArrangedSubview(unsafe { as_view::<NSView>(&reset_spacer) });
    reset_row.addArrangedSubview(unsafe { as_view::<NSButton>(&reset_btn) });

    // ── Assemble the panel ────────────────────────────────────────────
    let panel = NSStackView::new(mtm);
    panel.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setSpacing(6.0);

    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&sensor_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[0].row) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[1].row) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[2].row) });
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&color_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[3].row) }); // baseline exposure
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[4].row) }); // dcp hue sat
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[5].row) }); // dcp look
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[6].row) }); // saturation
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&tone_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[7].row) }); // highlight
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[8].row) }); // tone curve
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&detail_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[9].row) }); // sharpening
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&dcp_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&custom_dcp_outer) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&reset_row) });

    // Add section-header breathing room (so the grouping reads visually).
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[2].row) });
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[6].row) });
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[8].row) });
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[9].row) });
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&custom_dcp_outer) });

    // Pin every row to the panel width so the switches align flush right.
    for row in &rows {
        pin_width(&row.row, &panel, retained_views);
    }
    pin_width_any(&custom_dcp_outer, &panel, retained_views);
    pin_width_any(&reset_row, &panel, retained_views);

    unsafe {
        let _: () = msg_send![&*panel, setHidden: true];
    }

    // ── Build delegate + wire actions ─────────────────────────────────
    let ivars = RawDelegateIvars {
        dng_opcode_1: toggle_ptrs[0],
        dng_opcode_2: toggle_ptrs[1],
        dng_opcode_3: toggle_ptrs[2],
        baseline_exposure: toggle_ptrs[3],
        dcp_hue_sat_map: toggle_ptrs[4],
        dcp_look_table: toggle_ptrs[5],
        saturation_boost: toggle_ptrs[6],
        highlight_recovery: toggle_ptrs[7],
        tone_curve: toggle_ptrs[8],
        capture_sharpening: toggle_ptrs[9],
        custom_dcp_field: &*custom_dcp_field as *const NSTextField,
    };
    let delegate = RawDelegate::new(mtm, ivars);

    unsafe {
        for toggle in &toggles {
            toggle.setTarget(Some(&delegate as &AnyObject));
            toggle.setAction(Some(sel!(togglePipelineFlag:)));
        }
        reset_btn.setTarget(Some(&delegate as &AnyObject));
        reset_btn.setAction(Some(sel!(resetToDefaults:)));
        browse_btn.setTarget(Some(&delegate as &AnyObject));
        browse_btn.setAction(Some(sel!(browseDcpDir:)));
        clear_btn.setTarget(Some(&delegate as &AnyObject));
        clear_btn.setAction(Some(sel!(clearDcpDir:)));
    }

    // ── Retain every object for the window's lifetime ────────────────
    retained_views.push(unsafe { Retained::cast_unchecked(sensor_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(color_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(tone_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(detail_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(dcp_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(reset_spacer) });
    retained_views.push(unsafe { Retained::cast_unchecked(reset_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(reset_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(browse_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(clear_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(custom_dcp_field) });
    retained_views.push(unsafe { Retained::cast_unchecked(custom_dcp_outer) });

    for toggle in toggles {
        retained_views.push(unsafe { Retained::cast_unchecked(toggle) });
    }
    for row in rows {
        for extra in row.extras {
            retained_views.push(extra);
        }
        retained_views.push(unsafe { Retained::cast_unchecked(row.row) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });

    RawPanel { panel }
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

/// Width-pinning helper for stack views stored behind Retained handles.
fn pin_width_any(
    view: &NSStackView,
    panel: &NSStackView,
    retained_views: &mut Vec<Retained<AnyObject>>,
) {
    pin_width(view, panel, retained_views);
}
