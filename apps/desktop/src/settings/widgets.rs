//! Widget factories specific to the Settings window.
//!
//! `make_setting_row` builds a "title + switch + description" row. The row keeps the
//! label and spacer alive via `mem::forget` — they're owned by the NSStackView at that
//! point, and releasing the Rust `Retained` would cause a double-free.
//!
//! `make_wrapping_label` creates an NSTextField via `wrappingLabelWithString:` with a
//! preferred max layout width — used for multi-line descriptions under toggles that
//! have longer explanations.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::NSString;

use crate::platform::macos::ui_common::{FlippedView, as_view, make_label};

/// Create a wrapping description label using `[NSTextField wrappingLabelWithString:]`.
pub(crate) fn make_wrapping_label(text: &str, max_width: f64) -> Retained<NSTextField> {
    unsafe {
        let ns_str = NSString::from_str(text);
        let label: Retained<NSTextField> =
            msg_send![objc2::class!(NSTextField), wrappingLabelWithString: &*ns_str];
        label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let _: () = msg_send![&*label, setPreferredMaxLayoutWidth: max_width];
        label
    }
}

/// Create a toggle row (label + NSSwitch) and a description label underneath.
/// Returns (row_stack, toggle, desc_label).
pub(crate) fn make_setting_row(
    title: &str,
    description: &str,
    is_on: bool,
    wrapping: bool,
    max_width: f64,
    mtm: MainThreadMarker,
) -> (
    Retained<NSStackView>,
    Retained<NSSwitch>,
    Retained<NSTextField>,
) {
    let label = make_label(title, 14.0, mtm);
    label.setAlignment(NSTextAlignment(0));

    let toggle = NSSwitch::new(mtm);
    toggle.setState(if is_on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });

    // Spacer pushes the toggle to the trailing edge
    let spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        // Hugging priority 1 = spacer happily expands to fill available space
        let _: () = msg_send![&*spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64]; // Horizontal
    }

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(12.0);
    row.addArrangedSubview(unsafe { as_view::<NSTextField>(&label) });
    row.addArrangedSubview(unsafe { as_view::<NSView>(&spacer) });
    row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&toggle) });

    let desc = if wrapping {
        make_wrapping_label(description, max_width)
    } else {
        let d = make_label(description, 12.0, mtm);
        d.setAlignment(NSTextAlignment(0));
        d.setTextColor(Some(&NSColor::secondaryLabelColor()));
        d
    };

    // Keep the label and spacer alive (they're added to the row via addArrangedSubview)
    std::mem::forget(label);
    std::mem::forget(spacer);

    (row, toggle, desc)
}
