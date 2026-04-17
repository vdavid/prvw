//! "Zoom" panel: auto-fit window, enlarge small images.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView, NSSwitch, NSTextField,
};

use crate::platform::macos::ui_common::{as_view, make_vertical_stack};
use crate::settings::Settings;
use crate::settings::widgets::make_setting_row;

pub(crate) struct ZoomPanel {
    pub panel: Retained<NSStackView>,
    pub auto_fit_toggle: Retained<NSSwitch>,
    pub enlarge_toggle: Retained<NSSwitch>,
}

pub(crate) fn build(
    settings: &Settings,
    content_max_width: f64,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> ZoomPanel {
    let (auto_fit_row, auto_fit_toggle, auto_fit_desc) = make_setting_row(
        "Auto-fit window",
        "Resize the window to match each image.",
        settings.auto_fit_window,
        false,
        content_max_width,
        mtm,
    );

    let (enlarge_row, enlarge_toggle, enlarge_desc) = make_setting_row(
        "Enlarge small images",
        "Scale up images smaller than the window. Off by default to avoid pixelation.",
        settings.enlarge_small_images,
        false,
        content_max_width,
        mtm,
    );
    enlarge_toggle.setEnabled(!settings.auto_fit_window);

    let auto_fit_desc_ref = unsafe { as_view::<NSTextField>(&auto_fit_desc) };
    let enlarge_desc_ref = unsafe { as_view::<NSTextField>(&enlarge_desc) };

    let panel = make_vertical_stack(
        &[
            unsafe { as_view::<NSStackView>(&auto_fit_row) },
            auto_fit_desc_ref,
            unsafe { as_view::<NSStackView>(&enlarge_row) },
            enlarge_desc_ref,
        ],
        8.0,
        mtm,
    );
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setCustomSpacing_afterView(16.0, auto_fit_desc_ref);

    for row in [&auto_fit_row, &enlarge_row] {
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

    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(enlarge_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(enlarge_desc) });

    ZoomPanel {
        panel,
        auto_fit_toggle,
        enlarge_toggle,
    }
}
