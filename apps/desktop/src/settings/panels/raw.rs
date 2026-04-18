//! "RAW" panel: per-stage toggles for the RAW decode pipeline + a custom
//! DCP directory picker (Phase 3.7 + 4.0) + continuous-valued sliders
//! co-located with each parametric toggle (Phase 6.0 + 6.1.1 + 6.2).
//!
//! Each flag row mirrors the pattern in the other panels: title label on the
//! left, NSSwitch on the right, secondary description directly underneath.
//! Section headers group the toggles into "Sensor corrections (DNG only)",
//! "Color", "Tone", "Detail", "Denoise" (Phase 6.1 — chroma noise reduction
//! via a mild Gaussian blur on Cb / Cr), "Geometry" (Phase 4.0 — lens
//! correction via `lensfun-rs`), and "Output". Sliders sit directly under
//! their matching toggles (baseline exposure offset under baseline exposure,
//! saturation amount under saturation boost, midtone anchor under default
//! tone curve, sharpening amount under capture sharpening, Phase 6.2's
//! clarity radius + amount under the new clarity toggle). Sliders are
//! non-continuous, so moving a knob fires exactly one `SetRawPipelineFlags`
//! per mouse release, avoiding decode-spam during drag. Two label formats
//! are supported via a small `LabelFormat` enum: `TwoDecimal` for the
//! 0.0 – 1.0-ish knobs and `IntegerPx` for the clarity-radius slider's
//! 2 – 50 px range. A final "Custom DCP directory" row + "Reset to
//! defaults" button live at the bottom.
//!
//! All widgets write back through a single `AppCommand::SetRawPipelineFlags`
//! so the executor flushes the image cache and re-decodes once per change.
//! The custom DCP directory goes through `AppCommand::SetCustomDcpDir` and
//! hits the same flush path via `App::apply_custom_dcp_dir_change`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSOpenPanel, NSSlider, NSStackView,
    NSSwitch, NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};

use crate::commands::{self, AppCommand};
use crate::decoding::{
    BASELINE_EXPOSURE_OFFSET_RANGE, CLARITY_AMOUNT_RANGE, CLARITY_RADIUS_RANGE, HDR_GAIN_RANGE,
    MIDTONE_ANCHOR_RANGE, RawPipelineFlags, SATURATION_BOOST_RANGE, SHARPEN_AMOUNT_RANGE,
};
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
const TAG_DEFAULT_TONE_CURVE: isize = 121;
const TAG_DCP_TONE_CURVE: isize = 122;
const TAG_CLARITY: isize = 130;
const TAG_CAPTURE_SHARPENING: isize = 131;
const TAG_LENS_CORRECTION: isize = 140;
const TAG_HDR_OUTPUT: isize = 150;
/// "Denoise" section toggle. The spec nominally pinned this at 123 inside
/// the tone-row range, but keeping each section in its own 1xx decade makes
/// accidental collisions impossible as sections grow — so we park it at
/// 160, matching the pattern the other sections follow.
const TAG_CHROMA_DENOISE: isize = 160;
// Sliders. Kept in a separate 200-range so a stray tag collision with
// the toggles above is impossible. Since 6.1.1, sliders are co-located
// with their toggles (e.g., saturation boost slider sits under the
// saturation boost switch) — the tag range is still separate, but the
// layout order no longer implies a standalone "Tuning" section.
const TAG_BASELINE_EXPOSURE_OFFSET: isize = 200;
const TAG_SHARPEN_AMOUNT: isize = 201;
const TAG_SATURATION_AMOUNT: isize = 202;
const TAG_MIDTONE_ANCHOR: isize = 203;
const TAG_CLARITY_RADIUS: isize = 204;
const TAG_CLARITY_AMOUNT: isize = 205;
const TAG_HDR_GAIN: isize = 206;

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
    default_tone_curve: *const NSSwitch,
    dcp_tone_curve: *const NSSwitch,
    clarity: *const NSSwitch,
    capture_sharpening: *const NSSwitch,
    chroma_denoise: *const NSSwitch,
    lens_correction: *const NSSwitch,
    hdr_output: *const NSSwitch,
    // Phase 6.0 Tuning sliders + their right-hand value labels.
    baseline_exposure_offset_slider: *const NSSlider,
    baseline_exposure_offset_label: *const NSTextField,
    sharpen_amount_slider: *const NSSlider,
    sharpen_amount_label: *const NSTextField,
    saturation_amount_slider: *const NSSlider,
    saturation_amount_label: *const NSTextField,
    midtone_anchor_slider: *const NSSlider,
    midtone_anchor_label: *const NSTextField,
    clarity_radius_slider: *const NSSlider,
    clarity_radius_label: *const NSTextField,
    clarity_amount_slider: *const NSSlider,
    clarity_amount_label: *const NSTextField,
    hdr_gain_slider: *const NSSlider,
    hdr_gain_label: *const NSTextField,
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

        /// Any Tuning-section slider release (continuous = false, so this
        /// fires exactly once per drag). Reads all current widget values,
        /// refreshes the value labels, and emits a single
        /// `SetRawPipelineFlags` — same funnel the toggles use.
        #[unsafe(method(tuningSliderChanged:))]
        fn tuning_slider_changed(&self, _sender: &NSSlider) {
            let flags = self.read_current_flags();
            self.refresh_slider_labels(flags);
            log::debug!(
                "RAW panel: tuning slider -> sharpen={:.2}, sat={:.2}, midtone={:.2}, clarity r={:.0}px a={:.2}",
                flags.sharpen_amount,
                flags.saturation_boost_amount,
                flags.midtone_anchor,
                flags.clarity_radius,
                flags.clarity_amount,
            );
            commands::send_command(AppCommand::SetRawPipelineFlags(flags));
        }

        /// "Reset to defaults" button click. Sends a default flags command
        /// and refreshes each widget (switches, sliders, value labels) to
        /// match.
        #[unsafe(method(resetToDefaults:))]
        fn reset_to_defaults(&self, _sender: &AnyObject) {
            let defaults = RawPipelineFlags::default();
            self.write_flags_to_all_widgets(defaults);
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

    /// Snapshot every widget (switches + sliders) into a fresh
    /// `RawPipelineFlags`. Slider values are already clamped to their
    /// `min..=max` by AppKit, so no extra clamp is needed here — the
    /// authoritative `clamp_knobs()` still runs inside the decoder.
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
            default_tone_curve: switch_is_on(ivars.default_tone_curve),
            dcp_tone_curve: switch_is_on(ivars.dcp_tone_curve),
            clarity: switch_is_on(ivars.clarity),
            capture_sharpening: switch_is_on(ivars.capture_sharpening),
            chroma_denoise: switch_is_on(ivars.chroma_denoise),
            lens_correction: switch_is_on(ivars.lens_correction),
            hdr_output: switch_is_on(ivars.hdr_output),
            baseline_exposure_offset: slider_value(ivars.baseline_exposure_offset_slider),
            sharpen_amount: slider_value(ivars.sharpen_amount_slider),
            saturation_boost_amount: slider_value(ivars.saturation_amount_slider),
            midtone_anchor: slider_value(ivars.midtone_anchor_slider),
            clarity_radius: slider_value(ivars.clarity_radius_slider),
            clarity_amount: slider_value(ivars.clarity_amount_slider),
            hdr_gain: slider_value(ivars.hdr_gain_slider),
        }
    }

    /// Push `flags` into every widget we own: the fourteen switches, the
    /// three Tuning sliders, and their value labels. Used by the "Reset to
    /// defaults" path so a click snaps the UI back to the production
    /// baseline in one atomic step.
    fn write_flags_to_all_widgets(&self, flags: RawPipelineFlags) {
        let ivars = self.ivars();
        set_switch(ivars.dng_opcode_1, flags.dng_opcode_list_1);
        set_switch(ivars.dng_opcode_2, flags.dng_opcode_list_2);
        set_switch(ivars.dng_opcode_3, flags.dng_opcode_list_3);
        set_switch(ivars.baseline_exposure, flags.baseline_exposure);
        set_switch(ivars.dcp_hue_sat_map, flags.dcp_hue_sat_map);
        set_switch(ivars.dcp_look_table, flags.dcp_look_table);
        set_switch(ivars.saturation_boost, flags.saturation_boost);
        set_switch(ivars.highlight_recovery, flags.highlight_recovery);
        set_switch(ivars.default_tone_curve, flags.default_tone_curve);
        set_switch(ivars.dcp_tone_curve, flags.dcp_tone_curve);
        set_switch(ivars.clarity, flags.clarity);
        set_switch(ivars.capture_sharpening, flags.capture_sharpening);
        set_switch(ivars.chroma_denoise, flags.chroma_denoise);
        set_switch(ivars.lens_correction, flags.lens_correction);
        set_switch(ivars.hdr_output, flags.hdr_output);
        set_slider(
            ivars.baseline_exposure_offset_slider,
            flags.baseline_exposure_offset,
        );
        set_slider(ivars.sharpen_amount_slider, flags.sharpen_amount);
        set_slider(
            ivars.saturation_amount_slider,
            flags.saturation_boost_amount,
        );
        set_slider(ivars.midtone_anchor_slider, flags.midtone_anchor);
        set_slider(ivars.clarity_radius_slider, flags.clarity_radius);
        set_slider(ivars.clarity_amount_slider, flags.clarity_amount);
        set_slider(ivars.hdr_gain_slider, flags.hdr_gain);
        self.refresh_slider_labels(flags);
    }

    /// Rewrite the three numeric labels to match the current knob values.
    /// Called on slider release and on reset.
    fn refresh_slider_labels(&self, flags: RawPipelineFlags) {
        let ivars = self.ivars();
        set_label_value(
            ivars.baseline_exposure_offset_label,
            flags.baseline_exposure_offset,
        );
        set_label_value(ivars.sharpen_amount_label, flags.sharpen_amount);
        set_label_value(ivars.saturation_amount_label, flags.saturation_boost_amount);
        set_label_value(ivars.midtone_anchor_label, flags.midtone_anchor);
        set_label_px(ivars.clarity_radius_label, flags.clarity_radius);
        set_label_value(ivars.clarity_amount_label, flags.clarity_amount);
        set_label_value(ivars.hdr_gain_label, flags.hdr_gain);
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

fn slider_value(ptr: *const NSSlider) -> f32 {
    if ptr.is_null() {
        return 0.0;
    }
    // NSSlider exposes `doubleValue` — we narrow to f32 because that's
    // what `RawPipelineFlags` stores.
    unsafe { (*ptr).doubleValue() as f32 }
}

fn set_slider(ptr: *const NSSlider, value: f32) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        (*ptr).setDoubleValue(value as f64);
    }
}

fn set_label_value(ptr: *const NSTextField, value: f32) {
    if ptr.is_null() {
        return;
    }
    // Two-decimal formatting is fine for the three ranges we expose:
    // 0.00 – 1.00, 0.00 – 0.30, and 0.20 – 0.50. Wider resolution would
    // just add noise without conveying more signal to the user.
    let text = format!("{value:.2}");
    unsafe {
        (*ptr).setStringValue(&NSString::from_str(&text));
    }
}

/// Like [`set_label_value`] but renders an integer pixel count, as in the
/// Clarity radius slider where the 2–50 range is too coarse to benefit
/// from sub-pixel resolution.
fn set_label_px(ptr: *const NSTextField, value: f32) {
    if ptr.is_null() {
        return;
    }
    let text = format!("{:.0} px", value.round());
    unsafe {
        (*ptr).setStringValue(&NSString::from_str(&text));
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

/// One Tuning-section slider row: bold title + caption on the left, an
/// `NSSlider` in the middle, and a 2-decimal value label on the right.
/// The slider is non-continuous (`setContinuous(false)`), so AppKit fires
/// the action exactly once, on mouse release. That trades live-during-drag
/// feedback for zero decode-spam, which matters because each decode on a
/// 20 MP RAW costs tens of milliseconds.
struct SliderRow {
    row: Retained<NSStackView>,
    slider: Retained<NSSlider>,
    value_label: Retained<NSTextField>,
    extras: Vec<Retained<AnyObject>>,
}

/// How to render the slider's value-label text. Most knobs are 0.0–1.0-ish
/// floats (two decimals read right); the Clarity radius slider is 2–50 px
/// and reads better as an integer with a unit suffix.
#[derive(Clone, Copy)]
enum LabelFormat {
    TwoDecimal,
    IntegerPx,
}

impl LabelFormat {
    fn render(self, v: f32) -> String {
        match self {
            LabelFormat::TwoDecimal => format!("{v:.2}"),
            LabelFormat::IntegerPx => format!("{:.0} px", v.round()),
        }
    }
}

#[allow(clippy::too_many_arguments)] // Straight-through factory; struct-ifying obscures the AppKit wiring.
fn build_slider_row(
    title: &str,
    description: &str,
    value: f32,
    min: f32,
    max: f32,
    tag: isize,
    label_format: LabelFormat,
    mtm: MainThreadMarker,
) -> SliderRow {
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

    let slider = NSSlider::new(mtm);
    slider.setMinValue(min as f64);
    slider.setMaxValue(max as f64);
    slider.setDoubleValue(value.clamp(min, max) as f64);
    // Non-continuous: fire action on mouse release only.
    slider.setContinuous(false);
    unsafe {
        let _: () = msg_send![&*slider, setTag: tag];
        let _: () = msg_send![&*slider, setControlSize: 1i64];
        // Pin a reasonable minimum width so very long titles don't shrink
        // the slider track into uselessness.
        let _: () = msg_send![&*slider, setTranslatesAutoresizingMaskIntoConstraints: false];
    }
    let slider_min_width = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &slider, NSLayoutAttribute::Width,
            NSLayoutRelation::GreaterThanOrEqual,
            None, NSLayoutAttribute::NotAnAttribute,
            1.0, 160.0,
        )
    };
    slider_min_width.setActive(true);

    let value_label = make_label(&label_format.render(value), 12.0, mtm);
    value_label.setAlignment(NSTextAlignment(1)); // NSTextAlignmentRight
    value_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    unsafe {
        let _: () = msg_send![&*value_label, setTranslatesAutoresizingMaskIntoConstraints: false];
    }
    // Fixed width keeps the column tidy as the digits change.
    let label_width = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &value_label, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            None, NSLayoutAttribute::NotAnAttribute,
            1.0, 42.0,
        )
    };
    label_width.setActive(true);

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(10.0);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.addArrangedSubview(unsafe { as_view::<NSStackView>(&label_stack) });
    row.addArrangedSubview(unsafe { as_view::<NSSlider>(&slider) });
    row.addArrangedSubview(unsafe { as_view::<NSTextField>(&value_label) });

    let extras: Vec<Retained<AnyObject>> = unsafe {
        vec![
            Retained::cast_unchecked(title_label),
            Retained::cast_unchecked(desc_label),
            Retained::cast_unchecked(label_stack),
            Retained::cast_unchecked(slider_min_width),
            Retained::cast_unchecked(label_width),
        ]
    };

    SliderRow {
        row,
        slider,
        value_label,
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
            "Apply the camera's intended baseline exposure (or a neutral default) plus the user offset below.",
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
            "Mild global chroma lift in linear Rec.2020.",
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
            "Default tone curve",
            "Prvw's filmic S-curve: shadow lift and highlight shoulder.",
            flags.default_tone_curve,
            TAG_DEFAULT_TONE_CURVE,
            mtm,
        ),
        build_flag_row(
            "DCP tone curve",
            "Per-camera curve from a matched DCP profile. Auto-skipped for fuzzy-family matches.",
            flags.dcp_tone_curve,
            TAG_DCP_TONE_CURVE,
            mtm,
        ),
        build_flag_row(
            "Clarity (local contrast)",
            "Larger-radius unsharp mask on luminance. Lifts midtone features so the image reads crisper.",
            flags.clarity,
            TAG_CLARITY,
            mtm,
        ),
        build_flag_row(
            "Capture sharpening",
            "Mild unsharp mask on luminance in display space.",
            flags.capture_sharpening,
            TAG_CAPTURE_SHARPENING,
            mtm,
        ),
        build_flag_row(
            "Chroma noise reduction",
            "Mild Gaussian blur on color channels; keeps luminance sharp.",
            flags.chroma_denoise,
            TAG_CHROMA_DENOISE,
            mtm,
        ),
        build_flag_row(
            "Lens correction",
            "Distortion, TCA, and vignetting from the LensFun database.",
            flags.lens_correction,
            TAG_LENS_CORRECTION,
            mtm,
        ),
        build_flag_row(
            "HDR / EDR output",
            "Keep highlights above display-white alive when the screen supports it.",
            flags.hdr_output,
            TAG_HDR_OUTPUT,
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
    let denoise_header = make_section_header("Denoise", mtm);
    let geometry_header = make_section_header("Geometry", mtm);
    let output_header = make_section_header("Output", mtm);
    // Sliders live directly under their matching toggle instead of in a
    // standalone "Tuning" section (since 6.1.1). That makes the
    // tune-by-eye UX obvious: the slider for each parametric stage sits
    // inside the section header that names it, right below the on/off
    // switch. The sliders still use their own TAG range (see above) and
    // share a single `AppCommand::SetRawPipelineFlags` path with the
    // toggles.
    let baseline_exposure_offset_row = build_slider_row(
        "Baseline exposure offset",
        "User offset in EV stops on top of the camera / default baseline.",
        flags.baseline_exposure_offset,
        BASELINE_EXPOSURE_OFFSET_RANGE.0,
        BASELINE_EXPOSURE_OFFSET_RANGE.1,
        TAG_BASELINE_EXPOSURE_OFFSET,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let sharpen_row = build_slider_row(
        "Sharpening amount",
        "Unsharp-mask strength on the luminance-only capture sharpen pass.",
        flags.sharpen_amount,
        SHARPEN_AMOUNT_RANGE.0,
        SHARPEN_AMOUNT_RANGE.1,
        TAG_SHARPEN_AMOUNT,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let saturation_row = build_slider_row(
        "Saturation amount",
        "Chroma lift strength in linear Rec.2020 (post-tone, pre-ICC).",
        flags.saturation_boost_amount,
        SATURATION_BOOST_RANGE.0,
        SATURATION_BOOST_RANGE.1,
        TAG_SATURATION_AMOUNT,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let midtone_row = build_slider_row(
        "Tone midtone anchor",
        "Where the filmic S-curve's midtone line passes through (x, x).",
        flags.midtone_anchor,
        MIDTONE_ANCHOR_RANGE.0,
        MIDTONE_ANCHOR_RANGE.1,
        TAG_MIDTONE_ANCHOR,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let clarity_radius_row = build_slider_row(
        "Clarity radius",
        "Gaussian σ in pixels for the local-contrast pass. Larger = bigger features.",
        flags.clarity_radius,
        CLARITY_RADIUS_RANGE.0,
        CLARITY_RADIUS_RANGE.1,
        TAG_CLARITY_RADIUS,
        LabelFormat::IntegerPx,
        mtm,
    );
    let clarity_amount_row = build_slider_row(
        "Clarity amount",
        "Strength of the local-contrast unsharp mask (0 = off, 1 = aggressive).",
        flags.clarity_amount,
        CLARITY_AMOUNT_RANGE.0,
        CLARITY_AMOUNT_RANGE.1,
        TAG_CLARITY_AMOUNT,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let hdr_gain_row = build_slider_row(
        "HDR brightness gain",
        "Multiplier pushing scene white into EDR headroom. 1.0 = off, 2.0 = double brightness.",
        flags.hdr_gain,
        HDR_GAIN_RANGE.0,
        HDR_GAIN_RANGE.1,
        TAG_HDR_GAIN,
        LabelFormat::TwoDecimal,
        mtm,
    );
    let slider_ptrs = [
        &*baseline_exposure_offset_row.slider as *const NSSlider,
        &*sharpen_row.slider as *const NSSlider,
        &*saturation_row.slider as *const NSSlider,
        &*midtone_row.slider as *const NSSlider,
        &*clarity_radius_row.slider as *const NSSlider,
        &*clarity_amount_row.slider as *const NSSlider,
        &*hdr_gain_row.slider as *const NSSlider,
    ];
    let slider_label_ptrs = [
        &*baseline_exposure_offset_row.value_label as *const NSTextField,
        &*sharpen_row.value_label as *const NSTextField,
        &*saturation_row.value_label as *const NSTextField,
        &*midtone_row.value_label as *const NSTextField,
        &*clarity_radius_row.value_label as *const NSTextField,
        &*clarity_amount_row.value_label as *const NSTextField,
        &*hdr_gain_row.value_label as *const NSTextField,
    ];
    let sliders: Vec<Retained<NSSlider>> = vec![
        baseline_exposure_offset_row.slider.clone(),
        sharpen_row.slider.clone(),
        saturation_row.slider.clone(),
        midtone_row.slider.clone(),
        clarity_radius_row.slider.clone(),
        clarity_amount_row.slider.clone(),
        hdr_gain_row.slider.clone(),
    ];

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
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[3].row) }); // baseline exposure toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&baseline_exposure_offset_row.row) }); // baseline exposure offset slider
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[4].row) }); // dcp hue sat
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[5].row) }); // dcp look
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[6].row) }); // saturation toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&saturation_row.row) }); // saturation amount slider
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&tone_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[7].row) }); // highlight
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[8].row) }); // default tone curve toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&midtone_row.row) }); // midtone anchor slider
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[9].row) }); // DCP tone curve
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&detail_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[10].row) }); // clarity toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&clarity_radius_row.row) }); // clarity radius slider
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&clarity_amount_row.row) }); // clarity amount slider
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[11].row) }); // sharpening toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&sharpen_row.row) }); // sharpening amount slider
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&denoise_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[12].row) }); // chroma denoise
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&geometry_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[13].row) }); // lens correction
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&output_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&rows[14].row) }); // HDR output toggle
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&hdr_gain_row.row) }); // HDR brightness gain slider
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&dcp_header) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&custom_dcp_outer) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&reset_row) });

    // Add section-header breathing room (so the grouping reads visually).
    // The "after X group" comments refer to the LAST element in each group
    // — which is the slider when one exists, otherwise the last toggle.
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[2].row) }); // after sensor group
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&saturation_row.row) }); // after color group
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[9].row) }); // after tone group
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&sharpen_row.row) }); // after detail group
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[12].row) }); // after denoise (chroma)
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&rows[13].row) }); // after geometry (lens)
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&hdr_gain_row.row) }); // after output (HDR gain)
    panel.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&custom_dcp_outer) });

    // Pin every row to the panel width so the switches align flush right.
    for row in &rows {
        pin_width(&row.row, &panel, retained_views);
    }
    // Same for the slider rows — without this, the slider track
    // collapses to its intrinsic size.
    pin_width(&baseline_exposure_offset_row.row, &panel, retained_views);
    pin_width(&sharpen_row.row, &panel, retained_views);
    pin_width(&saturation_row.row, &panel, retained_views);
    pin_width(&midtone_row.row, &panel, retained_views);
    pin_width(&clarity_radius_row.row, &panel, retained_views);
    pin_width(&clarity_amount_row.row, &panel, retained_views);
    pin_width(&hdr_gain_row.row, &panel, retained_views);
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
        default_tone_curve: toggle_ptrs[8],
        dcp_tone_curve: toggle_ptrs[9],
        clarity: toggle_ptrs[10],
        capture_sharpening: toggle_ptrs[11],
        chroma_denoise: toggle_ptrs[12],
        lens_correction: toggle_ptrs[13],
        hdr_output: toggle_ptrs[14],
        baseline_exposure_offset_slider: slider_ptrs[0],
        baseline_exposure_offset_label: slider_label_ptrs[0],
        sharpen_amount_slider: slider_ptrs[1],
        sharpen_amount_label: slider_label_ptrs[1],
        saturation_amount_slider: slider_ptrs[2],
        saturation_amount_label: slider_label_ptrs[2],
        midtone_anchor_slider: slider_ptrs[3],
        midtone_anchor_label: slider_label_ptrs[3],
        clarity_radius_slider: slider_ptrs[4],
        clarity_radius_label: slider_label_ptrs[4],
        clarity_amount_slider: slider_ptrs[5],
        clarity_amount_label: slider_label_ptrs[5],
        hdr_gain_slider: slider_ptrs[6],
        hdr_gain_label: slider_label_ptrs[6],
        custom_dcp_field: &*custom_dcp_field as *const NSTextField,
    };
    let delegate = RawDelegate::new(mtm, ivars);

    unsafe {
        for toggle in &toggles {
            toggle.setTarget(Some(&delegate as &AnyObject));
            toggle.setAction(Some(sel!(togglePipelineFlag:)));
        }
        for slider in &sliders {
            slider.setTarget(Some(&delegate as &AnyObject));
            slider.setAction(Some(sel!(tuningSliderChanged:)));
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
    retained_views.push(unsafe { Retained::cast_unchecked(denoise_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(geometry_header) });
    retained_views.push(unsafe { Retained::cast_unchecked(output_header) });
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
    // Phase 6.0: retain the Tuning section widgets for the window's
    // lifetime. Sliders are kept alive via `sliders`, and each row's
    // `extras` + `value_label` need retaining too.
    for slider in sliders {
        retained_views.push(unsafe { Retained::cast_unchecked(slider) });
    }
    for slider_row in [
        baseline_exposure_offset_row,
        sharpen_row,
        saturation_row,
        midtone_row,
        clarity_radius_row,
        clarity_amount_row,
        hdr_gain_row,
    ] {
        for extra in slider_row.extras {
            retained_views.push(extra);
        }
        retained_views.push(unsafe { Retained::cast_unchecked(slider_row.value_label) });
        retained_views.push(unsafe { Retained::cast_unchecked(slider_row.row) });
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
