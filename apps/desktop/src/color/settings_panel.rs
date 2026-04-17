//! "Color" panel: ICC color management, display matching, rendering intent.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView, NSSwitch, NSTextField,
};

use crate::platform::macos::ui_common::{as_view, make_vertical_stack};
use crate::settings::Settings;
use crate::settings::widgets::make_setting_row;

pub(crate) struct ColorPanel {
    pub panel: Retained<NSStackView>,
    pub icc_toggle: Retained<NSSwitch>,
    pub cm_toggle: Retained<NSSwitch>,
    pub rc_toggle: Retained<NSSwitch>,
}

pub(crate) fn build(
    settings: &Settings,
    content_max_width: f64,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> ColorPanel {
    let (icc_row, icc_toggle, icc_desc) = make_setting_row(
        "ICC color management",
        "Corrects colors in images that have an embedded color profile, like photos from professional cameras. Without this, some images \u{2014} especially those shot in Adobe RGB or ProPhoto \u{2014} can look washed out or have wrong colors.",
        settings.icc_color_management,
        true,
        content_max_width,
        mtm,
    );

    let (cm_row, cm_toggle, cm_desc) = make_setting_row(
        "Color match display",
        "Adapts colors to your specific display instead of assuming a standard sRGB screen. Different monitors reproduce colors differently, and this ensures you see the most accurate colors on yours. Makes the most difference on wide-gamut (P3) screens like MacBooks and Studio Displays.",
        settings.color_match_display,
        true,
        content_max_width,
        mtm,
    );
    cm_toggle.setEnabled(settings.icc_color_management);

    let (rc_row, rc_toggle, rc_desc) = make_setting_row(
        "Relative colorimetric",
        "Changes how colors outside your display\u{2019}s range are handled. By default, Prvw smoothly adjusts all colors to fit (perceptual). With this on, colors that your display can show stay pixel-perfect, but out-of-range colors get clipped. The difference is subtle \u{2014} photographers comparing specific color values may prefer this.",
        settings.use_relative_colorimetric,
        true,
        content_max_width,
        mtm,
    );
    rc_toggle.setEnabled(settings.icc_color_management);

    let icc_desc_ref = unsafe { as_view::<NSTextField>(&icc_desc) };
    let cm_desc_ref = unsafe { as_view::<NSTextField>(&cm_desc) };
    let rc_desc_ref = unsafe { as_view::<NSTextField>(&rc_desc) };

    let panel = make_vertical_stack(
        &[
            unsafe { as_view::<NSStackView>(&icc_row) },
            icc_desc_ref,
            unsafe { as_view::<NSStackView>(&cm_row) },
            cm_desc_ref,
            unsafe { as_view::<NSStackView>(&rc_row) },
            rc_desc_ref,
        ],
        8.0,
        mtm,
    );
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setCustomSpacing_afterView(16.0, icc_desc_ref);
    panel.setCustomSpacing_afterView(16.0, cm_desc_ref);

    for row in [&icc_row, &cm_row, &rc_row] {
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

    retained_views.push(unsafe { Retained::cast_unchecked(icc_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(icc_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(cm_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(cm_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(rc_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(rc_desc) });

    ColorPanel {
        panel,
        icc_toggle,
        cm_toggle,
        rc_toggle,
    }
}
