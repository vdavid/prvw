//! Settings → Slideshow panel: time-per-image slider + crossfade and loop
//! toggles.
//!
//! Like the RAW panel, this owns its own delegate (`SlideshowDelegate`) and
//! funnels every change through `AppCommand`s on the global event-loop proxy.
//! The slider is continuous so the value label tracks the drag; the executor
//! skips the disk write when the rounded second-count is unchanged, so a drag
//! costs at most one save per integer step.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControlStateValueOn, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation,
    NSSlider, NSStackView, NSSwitch, NSTextAlignment, NSTextField,
    NSUserInterfaceLayoutOrientation,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::commands::{self, AppCommand};
use crate::parity::Audit;
use crate::parity::setting_keys::SettingKey;
use crate::platform::macos::ui_common::{as_view, make_label};
use crate::settings::Settings;
use crate::slideshow::{MAX_SECONDS, MIN_SECONDS, clamp_seconds};

/// Format the value-label text for a given second count.
fn seconds_label(seconds: u32) -> String {
    format!("{seconds}s")
}

struct SlideshowDelegateIvars {
    seconds_label: *const NSTextField,
}

// SAFETY: the raw pointer is only touched on the main thread within the
// window's lifetime; its target lives in `retained_views`.
unsafe impl Send for SlideshowDelegateIvars {}
unsafe impl Sync for SlideshowDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwSlideshowDelegate"]
    #[ivars = SlideshowDelegateIvars]
    struct SlideshowDelegate;

    unsafe impl NSObjectProtocol for SlideshowDelegate {}

    impl SlideshowDelegate {
        /// Time-per-image slider moved. Continuous, so this fires repeatedly
        /// during a drag; we round to whole seconds, refresh the label, and
        /// broadcast `SetSlideshowSeconds` (the executor skips the save when
        /// the value is unchanged).
        #[unsafe(method(timePerImageChanged:))]
        fn time_per_image_changed(&self, sender: &NSSlider) {
            let seconds = clamp_seconds(sender.doubleValue().round() as u32);
            unsafe {
                let label = self.ivars().seconds_label;
                if !label.is_null() {
                    let text = NSString::from_str(&seconds_label(seconds));
                    let _: () = msg_send![label, setStringValue: &*text];
                }
            }
            commands::send_command(AppCommand::SetSlideshowSeconds(seconds));
        }

        #[unsafe(method(toggleCrossfade:))]
        fn toggle_crossfade(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            commands::send_command(AppCommand::SetSlideshowCrossfade(on));
        }

        #[unsafe(method(toggleLoop:))]
        fn toggle_loop(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            commands::send_command(AppCommand::SetSlideshowLoop(on));
        }
    }
);

impl SlideshowDelegate {
    fn new(mtm: MainThreadMarker, ivars: SlideshowDelegateIvars) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

/// Output of `build`: just the panel (the delegate owns the controls).
pub(crate) struct SlideshowPanel {
    pub panel: Retained<NSStackView>,
}

pub(crate) fn build(
    audit: &mut Audit<SettingKey>,
    settings: &Settings,
    content_max_width: f64,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> SlideshowPanel {
    let seconds = clamp_seconds(settings.slideshow_seconds);

    // ── Time-per-image slider row ─────────────────────────────────────
    // Built by hand rather than through `make_setting_row` (it's a slider, not a switch), so
    // it records itself with the parity audit here.
    audit.record(SettingKey::SlideshowSeconds);
    let title_label = make_label(SettingKey::SlideshowSeconds.label(), 14.0, mtm);
    title_label.setAlignment(NSTextAlignment(0));

    let slider = NSSlider::new(mtm);
    slider.setMinValue(MIN_SECONDS as f64);
    slider.setMaxValue(MAX_SECONDS as f64);
    slider.setDoubleValue(seconds as f64);
    // Continuous so the label tracks the drag live.
    slider.setContinuous(true);
    unsafe {
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

    let value_label = make_label(&seconds_label(seconds), 12.0, mtm);
    value_label.setAlignment(NSTextAlignment(1)); // NSTextAlignmentRight
    value_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    unsafe {
        let _: () = msg_send![&*value_label, setTranslatesAutoresizingMaskIntoConstraints: false];
    }
    let label_width = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &value_label, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            None, NSLayoutAttribute::NotAnAttribute,
            1.0, 42.0,
        )
    };
    label_width.setActive(true);

    let slider_row = NSStackView::new(mtm);
    slider_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    slider_row.setSpacing(12.0);
    slider_row.setAlignment(NSLayoutAttribute::CenterY);
    slider_row.addArrangedSubview(unsafe { as_view::<NSTextField>(&title_label) });
    slider_row.addArrangedSubview(unsafe { as_view::<NSSlider>(&slider) });
    slider_row.addArrangedSubview(unsafe { as_view::<NSTextField>(&value_label) });

    let slider_desc = crate::settings::widgets::make_wrapping_label(
        "How long each photo stays on screen during a slideshow (1\u{2013}30 seconds). The [ and ] keys change this too.",
        content_max_width,
    );

    // ── Crossfade + loop toggles ──────────────────────────────────────
    let (crossfade_row, crossfade_toggle, crossfade_desc) =
        crate::settings::widgets::make_setting_row(
            audit,
            SettingKey::SlideshowCrossfade,
            "Fade between images instead of cutting.",
            settings.slideshow_crossfade,
            false,
            content_max_width,
            mtm,
        );
    let (loop_row, loop_toggle, loop_desc) = crate::settings::widgets::make_setting_row(
        audit,
        SettingKey::SlideshowLoop,
        "Start over from the first image after the last one.",
        settings.slideshow_loop,
        false,
        content_max_width,
        mtm,
    );

    // ── Assemble the panel ────────────────────────────────────────────
    let panel = NSStackView::new(mtm);
    panel.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setSpacing(8.0);
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&slider_row) });
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&slider_desc) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&crossfade_row) });
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&crossfade_desc) });
    panel.addArrangedSubview(unsafe { as_view::<NSStackView>(&loop_row) });
    panel.addArrangedSubview(unsafe { as_view::<NSTextField>(&loop_desc) });

    panel.setCustomSpacing_afterView(16.0, unsafe { as_view::<NSTextField>(&slider_desc) });
    panel.setCustomSpacing_afterView(16.0, unsafe { as_view::<NSTextField>(&crossfade_desc) });

    // Pin each row to the panel width so the toggles align flush right and the
    // slider track fills the available space.
    for row in [&slider_row, &crossfade_row, &loop_row] {
        let c = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                row, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                Some(&panel as &AnyObject), NSLayoutAttribute::Width,
                1.0, 0.0,
            )
        };
        c.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(c) });
    }

    unsafe {
        let _: () = msg_send![&*panel, setHidden: true];
    }

    // ── Build delegate + wire actions ─────────────────────────────────
    let ivars = SlideshowDelegateIvars {
        seconds_label: &*value_label as *const NSTextField,
    };
    let delegate = SlideshowDelegate::new(mtm, ivars);
    unsafe {
        slider.setTarget(Some(&delegate as &AnyObject));
        slider.setAction(Some(sel!(timePerImageChanged:)));
        crossfade_toggle.setTarget(Some(&delegate as &AnyObject));
        crossfade_toggle.setAction(Some(sel!(toggleCrossfade:)));
        loop_toggle.setTarget(Some(&delegate as &AnyObject));
        loop_toggle.setAction(Some(sel!(toggleLoop:)));
    }

    // ── Retain everything for the window's lifetime ───────────────────
    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(slider_min_width) });
    retained_views.push(unsafe { Retained::cast_unchecked(label_width) });
    retained_views.push(unsafe { Retained::cast_unchecked(slider) });
    retained_views.push(unsafe { Retained::cast_unchecked(value_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(slider_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(slider_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(crossfade_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(crossfade_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(crossfade_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(loop_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(loop_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(loop_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });

    SlideshowPanel { panel }
}
