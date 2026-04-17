//! "File associations" panel: "Set all" toggle + per-UTI toggles.
//!
//! This panel is self-contained — `build` creates the widgets, wires
//! `FileAssocDelegate` for toggle and timer callbacks, and returns just the
//! `NSStackView` for the caller to slot into its layout. All Retained handles are
//! pushed into `retained_views`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSLayoutAttribute, NSLayoutConstraint,
    NSLayoutRelation, NSStackView, NSSwitch, NSTextAlignment, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::file_associations;
use crate::platform::macos::ui_common::{FlippedView, as_view, make_label, make_vertical_stack};

/// Number of supported UTIs (must match `file_associations::SUPPORTED_UTIS.len()`).
const UTI_COUNT: usize = 6;

struct FileAssocDelegateIvars {
    /// Per-UTI toggles (index matches SUPPORTED_UTIS).
    uti_toggles: [*const NSSwitch; UTI_COUNT],
    /// Per-UTI secondary labels showing handler info.
    uti_labels: [*const NSTextField; UTI_COUNT],
    /// "Set all" toggle.
    set_all_toggle: *const NSSwitch,
    /// "Set all" secondary label.
    set_all_label: *const NSTextField,
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
        /// Called when a per-UTI toggle is switched. Tag identifies which UTI.
        #[unsafe(method(toggleFileAssoc:))]
        fn toggle_file_assoc(&self, sender: &NSSwitch) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let idx = tag as usize;
            let utis = file_associations::SUPPORTED_UTIS;
            if idx >= utis.len() {
                return;
            }
            let on = sender.state() == NSControlStateValueOn;
            if on {
                file_associations::set_prvw_as_handler(utis[idx].uti);
            } else {
                file_associations::restore_handler(utis[idx].uti);
            }
            // Refresh all states after a short delay (the OS may take a moment)
            self.refresh_all();
        }

        /// Called when the "Set all" toggle is switched.
        #[unsafe(method(toggleSetAll:))]
        fn toggle_set_all(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            for entry in file_associations::SUPPORTED_UTIS {
                if on {
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

    /// Re-query handler state for every UTI and update toggles + labels.
    fn refresh_all(&self) {
        let utis = file_associations::SUPPORTED_UTIS;
        let ivars = self.ivars();
        let mut all_prvw = true;
        for (i, entry) in utis.iter().enumerate() {
            let is_prvw = file_associations::is_prvw_default(entry.uti);
            if !is_prvw {
                all_prvw = false;
            }
            let state = if is_prvw {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            };
            unsafe {
                let toggle = ivars.uti_toggles[i];
                if !toggle.is_null() {
                    let _: () = msg_send![toggle, setState: state];
                }
                let label = ivars.uti_labels[i];
                if !label.is_null() {
                    let text = secondary_text(entry.uti, is_prvw);
                    (*label).setStringValue(&NSString::from_str(&text));
                }
            }
        }
        // Update "Set all" toggle and label
        unsafe {
            if !ivars.set_all_toggle.is_null() {
                let state = if all_prvw {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                let _: () = msg_send![ivars.set_all_toggle, setState: state];
            }
            if !ivars.set_all_label.is_null() {
                let text = if all_prvw {
                    "All image types are handled by Prvw."
                } else {
                    "Some image types are handled by other apps."
                };
                (*ivars.set_all_label).setStringValue(&NSString::from_str(text));
            }
        }
    }
}

/// Build secondary label text for a per-UTI row.
fn secondary_text(uti: &str, is_prvw: bool) -> String {
    if is_prvw {
        let prev = file_associations::previous_handler_name(uti);
        format!("Before Prvw, these opened with {prev}.")
    } else {
        let current = file_associations::get_handler_bundle_id(uti)
            .map(|id| file_associations::bundle_id_to_app_name(&id))
            .unwrap_or_else(|| "unknown".to_string());
        format!("Currently opens with {current}.")
    }
}

/// Build the File Associations panel. Creates the delegate internally, wires toggles
/// and the 1-second polling timer, and returns the panel view for the caller to slot
/// into its layout.
pub(crate) fn build(
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let utis = file_associations::SUPPORTED_UTIS;
    let all_prvw = utis
        .iter()
        .all(|e| file_associations::is_prvw_default(e.uti));

    // "Set all" row
    let set_all_label = make_label("Open all supported images with Prvw", 14.0, mtm);
    set_all_label.setAlignment(NSTextAlignment(0));

    let set_all_secondary_text = if all_prvw {
        "All image types are handled by Prvw."
    } else {
        "Some image types are handled by other apps."
    };
    let set_all_secondary = make_label(set_all_secondary_text, 12.0, mtm);
    set_all_secondary.setAlignment(NSTextAlignment(0));
    set_all_secondary.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let set_all_toggle = NSSwitch::new(mtm);
    set_all_toggle.setState(if all_prvw {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });

    let set_all_spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () =
            msg_send![&*set_all_spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () =
            msg_send![&*set_all_spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let set_all_label_stack = NSStackView::new(mtm);
    set_all_label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    set_all_label_stack.setAlignment(NSLayoutAttribute::Leading);
    set_all_label_stack.setSpacing(2.0);
    set_all_label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&set_all_label) });
    set_all_label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&set_all_secondary) });

    let set_all_row = NSStackView::new(mtm);
    set_all_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    set_all_row.setSpacing(12.0);
    set_all_row.addArrangedSubview(unsafe { as_view::<NSStackView>(&set_all_label_stack) });
    set_all_row.addArrangedSubview(unsafe { as_view::<NSView>(&set_all_spacer) });
    set_all_row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&set_all_toggle) });

    // Per-UTI rows
    let mut uti_toggle_ptrs: [*const NSSwitch; UTI_COUNT] = [std::ptr::null(); UTI_COUNT];
    let mut uti_label_ptrs: [*const NSTextField; UTI_COUNT] = [std::ptr::null(); UTI_COUNT];
    let mut uti_rows: Vec<Retained<NSStackView>> = Vec::new();
    let mut uti_toggles_retained: Vec<Retained<NSSwitch>> = Vec::new();

    for (i, entry) in utis.iter().enumerate() {
        let is_prvw = file_associations::is_prvw_default(entry.uti);

        let primary_text = format!("{} ({})", entry.label, entry.extensions);
        let primary = make_label(&primary_text, 14.0, mtm);
        primary.setAlignment(NSTextAlignment(0));

        let sec = secondary_text(entry.uti, is_prvw);
        let secondary = make_label(&sec, 12.0, mtm);
        secondary.setAlignment(NSTextAlignment(0));
        secondary.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let toggle = NSSwitch::new(mtm);
        toggle.setState(if is_prvw {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        unsafe {
            let _: () = msg_send![&*toggle, setTag: i as isize];
            // Use small control size for per-UTI toggles
            let _: () = msg_send![&*toggle, setControlSize: 1i64]; // NSControlSizeSmall = 1
        }

        let spacer = FlippedView::new_as_nsview(mtm);
        unsafe {
            let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
            let _: () =
                msg_send![&*spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
        }

        let label_stack = NSStackView::new(mtm);
        label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        label_stack.setAlignment(NSLayoutAttribute::Leading);
        label_stack.setSpacing(2.0);
        label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&primary) });
        label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&secondary) });

        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(12.0);
        row.addArrangedSubview(unsafe { as_view::<NSStackView>(&label_stack) });
        row.addArrangedSubview(unsafe { as_view::<NSView>(&spacer) });
        row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&toggle) });

        uti_toggle_ptrs[i] = &*toggle as *const NSSwitch;
        uti_label_ptrs[i] = &*secondary as *const NSTextField;

        uti_rows.push(row);
        uti_toggles_retained.push(toggle);
        retained_views.push(unsafe { Retained::cast_unchecked(primary) });
        retained_views.push(unsafe { Retained::cast_unchecked(secondary) });
        retained_views.push(unsafe { Retained::cast_unchecked(spacer) });
        retained_views.push(unsafe { Retained::cast_unchecked(label_stack) });
    }

    let set_all_row_ref = unsafe { as_view::<NSStackView>(&set_all_row) };

    let mut views: Vec<&NSView> = vec![set_all_row_ref];
    let row_refs: Vec<&NSView> = uti_rows
        .iter()
        .map(|r| unsafe { as_view::<NSStackView>(r) })
        .collect();
    for r in &row_refs {
        views.push(r);
    }

    let panel = make_vertical_stack(&views, 8.0, mtm);
    panel.setAlignment(NSLayoutAttribute::Leading);
    panel.setCustomSpacing_afterView(16.0, set_all_row_ref);

    // Pin all toggle rows to full panel width
    let c = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &set_all_row, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            Some(&panel as &AnyObject), NSLayoutAttribute::Width,
            1.0, 0.0,
        )
    };
    c.setActive(true);
    retained_views.push(unsafe { Retained::cast_unchecked(c) });

    for row in &uti_rows {
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

    // Create the delegate, wire toggle actions, start the polling timer.
    let ivars = FileAssocDelegateIvars {
        uti_toggles: uti_toggle_ptrs,
        uti_labels: uti_label_ptrs,
        set_all_toggle: &*set_all_toggle as *const NSSwitch,
        set_all_label: &*set_all_secondary as *const NSTextField,
    };
    let delegate = FileAssocDelegate::new(mtm, ivars);

    unsafe {
        set_all_toggle.setTarget(Some(&delegate as &AnyObject));
        set_all_toggle.setAction(Some(sel!(toggleSetAll:)));

        for toggle in &uti_toggles_retained {
            toggle.setTarget(Some(&delegate as &AnyObject));
            toggle.setAction(Some(sel!(toggleFileAssoc:)));
        }
    }

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

    // Retain everything so it lives as long as the window.
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_secondary) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_spacer) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_label_stack) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_all_row) });
    for row in uti_rows {
        retained_views.push(unsafe { Retained::cast_unchecked(row) });
    }
    for toggle in uti_toggles_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(toggle) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });
    retained_views.push(poll_timer);

    panel
}
