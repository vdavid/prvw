//! "General" panel: auto-update, scroll-to-zoom, title bar.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView, NSSwitch, NSTextField,
};

use super::super::widgets::make_setting_row;
use crate::platform::macos::ui_common::as_view;
use crate::settings::Settings;

/// Output of `build`: the panel itself plus toggles and the dynamic description label
/// that `SettingsDelegate` mutates on scroll-to-zoom toggles.
pub(crate) struct GeneralPanel {
    pub panel: Retained<NSStackView>,
    pub auto_update_toggle: Retained<NSSwitch>,
    pub scroll_to_zoom_toggle: Retained<NSSwitch>,
    pub title_bar_toggle: Retained<NSSwitch>,
    pub scroll_to_zoom_desc: Retained<NSTextField>,
}

pub(crate) fn build(
    settings: &Settings,
    content_max_width: f64,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> GeneralPanel {
    let (auto_update_row, auto_update_toggle, auto_update_desc) = make_setting_row(
        "Auto-update",
        "Check for updates when Prvw starts.",
        settings.auto_update,
        false,
        content_max_width,
        mtm,
    );

    let scroll_to_zoom_desc_text = if settings.scroll_to_zoom {
        "Use scroll to zoom instead of switching images."
    } else {
        "You can still zoom with trackpad pinch and \u{2318}+/\u{2318}\u{2212}."
    };
    let (scroll_to_zoom_row, scroll_to_zoom_toggle, scroll_to_zoom_desc) = make_setting_row(
        "Scroll to zoom",
        scroll_to_zoom_desc_text,
        settings.scroll_to_zoom,
        false,
        content_max_width,
        mtm,
    );

    let (title_bar_row, title_bar_toggle, title_bar_desc) = make_setting_row(
        "Title bar",
        "Reserve space at the top so the title bar doesn\u{2019}t cover the image.",
        settings.title_bar,
        false,
        content_max_width,
        mtm,
    );

    let auto_update_desc_ref = unsafe { as_view::<NSTextField>(&auto_update_desc) };
    let scroll_to_zoom_desc_ref = unsafe { as_view::<NSTextField>(&scroll_to_zoom_desc) };
    let title_bar_desc_ref = unsafe { as_view::<NSTextField>(&title_bar_desc) };

    let panel = crate::platform::macos::ui_common::make_vertical_stack(
        &[
            unsafe { as_view::<NSStackView>(&auto_update_row) },
            auto_update_desc_ref,
            unsafe { as_view::<NSStackView>(&scroll_to_zoom_row) },
            scroll_to_zoom_desc_ref,
            unsafe { as_view::<NSStackView>(&title_bar_row) },
            title_bar_desc_ref,
        ],
        8.0,
        mtm,
    );
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setCustomSpacing_afterView(16.0, auto_update_desc_ref);
    panel.setCustomSpacing_afterView(16.0, scroll_to_zoom_desc_ref);

    for row in [&auto_update_row, &scroll_to_zoom_row, &title_bar_row] {
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

    // Rows and description labels are owned by `panel` (addArrangedSubview retains them);
    // keep the Rust Retained alive for the window's lifetime by moving to retained_views.
    retained_views.push(unsafe { Retained::cast_unchecked(auto_update_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_update_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(scroll_to_zoom_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_bar_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_bar_desc) });

    GeneralPanel {
        panel,
        auto_update_toggle,
        scroll_to_zoom_toggle,
        title_bar_toggle,
        scroll_to_zoom_desc,
    }
}
