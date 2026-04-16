//! Settings window — sidebar + content panel layout, modeled on macOS System Settings.
//!
//! Four sections: General, Zoom, Color, File associations. All panels are built once
//! and section-switching toggles their visibility. Dynamic text (like file-association
//! descriptions) is updated in place via stored NSTextField pointers in `SettingsDelegateIvars`.

use super::{
    FlippedView, add_vibrancy_background, as_view, center_window, is_window_already_open,
    make_close_button, make_escape_button, make_label, make_vertical_stack,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSFont, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation,
    NSStackView, NSSwitch, NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

/// Number of supported UTIs (must match `crate::platform::macos::file_associations::SUPPORTED_UTIS.len()`).
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
            let utis = crate::platform::macos::file_associations::SUPPORTED_UTIS;
            if idx >= utis.len() {
                return;
            }
            let on = sender.state() == NSControlStateValueOn;
            if on {
                crate::platform::macos::file_associations::set_prvw_as_handler(utis[idx].uti);
            } else {
                crate::platform::macos::file_associations::restore_handler(utis[idx].uti);
            }
            // Refresh all states after a short delay (the OS may take a moment)
            self.refresh_all();
        }

        /// Called when the "Set all" toggle is switched.
        #[unsafe(method(toggleSetAll:))]
        fn toggle_set_all(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            for entry in crate::platform::macos::file_associations::SUPPORTED_UTIS {
                if on {
                    crate::platform::macos::file_associations::set_prvw_as_handler(entry.uti);
                } else {
                    crate::platform::macos::file_associations::restore_handler(entry.uti);
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
        let utis = crate::platform::macos::file_associations::SUPPORTED_UTIS;
        let ivars = self.ivars();
        let mut all_prvw = true;
        for (i, entry) in utis.iter().enumerate() {
            let is_prvw = crate::platform::macos::file_associations::is_prvw_default(entry.uti);
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
                    let text = file_assoc_secondary_text(entry.uti, is_prvw);
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
fn file_assoc_secondary_text(uti: &str, is_prvw: bool) -> String {
    if is_prvw {
        let prev = crate::platform::macos::file_associations::previous_handler_name(uti);
        format!("Before Prvw, these opened with {prev}.")
    } else {
        let current = crate::platform::macos::file_associations::get_handler_bundle_id(uti)
            .map(|id| crate::platform::macos::file_associations::bundle_id_to_app_name(&id))
            .unwrap_or_else(|| "unknown".to_string());
        format!("Currently opens with {current}.")
    }
}

// ─── Settings window ──────────────────────────────────────────────────────

struct SettingsDelegateIvars {
    enlarge_toggle: *const NSSwitch,
    color_match_toggle: *const NSSwitch,
    relative_col_toggle: *const NSSwitch,
    scroll_to_zoom_desc: *const NSTextField,
    general_panel: *const NSStackView,
    zoom_panel: *const NSStackView,
    color_panel: *const NSStackView,
    file_assoc_panel: *const NSStackView,
    sidebar_general_btn: *const NSButton,
    sidebar_zoom_btn: *const NSButton,
    sidebar_color_btn: *const NSButton,
    sidebar_file_assoc_btn: *const NSButton,
}

// SAFETY: Raw pointers are only used on the main thread within the window's lifetime,
// and the pointed-to objects are kept alive by retained_views.
unsafe impl Send for SettingsDelegateIvars {}
unsafe impl Sync for SettingsDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwSettingsDelegate"]
    #[ivars = SettingsDelegateIvars]
    struct SettingsDelegate;

    unsafe impl NSObjectProtocol for SettingsDelegate {}

    impl SettingsDelegate {
        #[unsafe(method(toggleAutoUpdate:))]
        fn toggle_auto_update(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-update toggled: {on}");
            let mut settings = crate::settings::Settings::load();
            settings.auto_update = on;
            settings.save();
        }

        #[unsafe(method(toggleAutoFitWindow:))]
        fn toggle_auto_fit_window(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-fit window toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetAutoFitWindow(on));
            unsafe {
                let enlarge = self.ivars().enlarge_toggle;
                if !enlarge.is_null() {
                    let _: () = msg_send![enlarge, setEnabled: !on];
                }
            }
        }

        #[unsafe(method(toggleEnlargeSmallImages:))]
        fn toggle_enlarge_small_images(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Enlarge small images toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetEnlargeSmallImages(on),
            );
        }

        #[unsafe(method(toggleIccColorManagement:))]
        fn toggle_icc_color_management(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("ICC color management toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetIccColorManagement(on),
            );
            unsafe {
                let cm = self.ivars().color_match_toggle;
                if !cm.is_null() {
                    let _: () = msg_send![cm, setEnabled: on];
                }
                let rc = self.ivars().relative_col_toggle;
                if !rc.is_null() {
                    let _: () = msg_send![rc, setEnabled: on];
                }
            }
        }

        #[unsafe(method(toggleColorMatchDisplay:))]
        fn toggle_color_match_display(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Color match display toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetColorMatchDisplay(on),
            );
        }

        #[unsafe(method(toggleRelativeColorimetric:))]
        fn toggle_relative_colorimetric(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Relative colorimetric toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetRelativeColorimetric(on),
            );
        }

        #[unsafe(method(toggleScrollToZoom:))]
        fn toggle_scroll_to_zoom(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Scroll to zoom toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetScrollToZoom(on));
            unsafe {
                let desc = self.ivars().scroll_to_zoom_desc;
                if !desc.is_null() {
                    let text = if on {
                        "Use scroll to zoom instead of switching images."
                    } else {
                        "You can still zoom with trackpad pinch and \u{2318}+/\u{2318}\u{2212}."
                    };
                    let _: () = msg_send![desc, setStringValue: &*NSString::from_str(text)];
                }
            }
        }

        #[unsafe(method(toggleTitleBar:))]
        fn toggle_title_bar(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Title bar toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetTitleBar(on));
        }

        #[unsafe(method(selectGeneral:))]
        fn select_general(&self, _sender: &AnyObject) {
            self.select_panel(0);
        }

        #[unsafe(method(selectZoom:))]
        fn select_zoom(&self, _sender: &AnyObject) {
            self.select_panel(1);
        }

        #[unsafe(method(selectColor:))]
        fn select_color(&self, _sender: &AnyObject) {
            self.select_panel(2);
        }

        #[unsafe(method(selectFileAssoc:))]
        fn select_file_assoc(&self, _sender: &AnyObject) {
            self.select_panel(3);
        }
    }
);

impl SettingsDelegate {
    fn new(mtm: MainThreadMarker, ivars: SettingsDelegateIvars) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn select_panel(&self, index: usize) {
        let ivars = self.ivars();
        let panels = [
            ivars.general_panel,
            ivars.zoom_panel,
            ivars.color_panel,
            ivars.file_assoc_panel,
        ];
        let buttons = [
            ivars.sidebar_general_btn,
            ivars.sidebar_zoom_btn,
            ivars.sidebar_color_btn,
            ivars.sidebar_file_assoc_btn,
        ];
        for (i, &panel) in panels.iter().enumerate() {
            if !panel.is_null() {
                let hidden = i != index;
                unsafe {
                    let _: () = msg_send![panel, setHidden: hidden];
                }
            }
        }
        for (i, &btn) in buttons.iter().enumerate() {
            if !btn.is_null() {
                let state = if i == index {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                unsafe {
                    let _: () = msg_send![btn, setState: state];
                }
            }
        }
    }
}

/// Create a wrapping description label using `[NSTextField wrappingLabelWithString:]`.
fn make_wrapping_label(text: &str, max_width: f64) -> Retained<NSTextField> {
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
fn make_setting_row(
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

/// Show the Settings window as a non-modal NSWindow.
///
/// Takes an optional parent NSWindow pointer to center on. The window is shown
/// immediately and returns without blocking.
///
/// # Safety
/// `parent_ns_window` must be a valid NSWindow pointer or null.
pub fn show_settings_window(parent_ns_window: *const NSWindow) {
    if is_window_already_open("Settings") {
        return;
    }

    // SAFETY: we're on the main thread (called from winit event handler)
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // ── Create the window ──────────────────────────────────────────────

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 520.0));

    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );

        window.setTitle(&NSString::from_str("Settings"));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];

        window.setMinSize(NSSize::new(500.0, 400.0));
        window.setMaxSize(NSSize::new(900.0, 800.0));

        window
    };

    // ── Build content views ────────────────────────────────────────────

    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    add_vibrancy_background(&window, mtm, &mut retained_views);

    let settings = crate::settings::Settings::load();
    let content_max_width = 400.0;

    // ── Sidebar buttons ───────────────────────────────────────────────

    let mut make_sidebar_button = |title: &str| -> Retained<NSButton> {
        unsafe {
            let btn = NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                None,
                None,
                mtm,
            );
            btn.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            let _: () = msg_send![&*btn, setButtonType: 1i64]; // PushOnPushOff
            let _: () = msg_send![&*btn, setAlignment: 0i64]; // NSTextAlignmentLeft

            // Fixed width
            let w = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &btn, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 140.0,
            );
            w.setActive(true);
            retained_views.push(Retained::cast_unchecked(w));

            btn
        }
    };

    let sidebar_general_btn = make_sidebar_button("General");
    let sidebar_zoom_btn = make_sidebar_button("Zoom");
    let sidebar_color_btn = make_sidebar_button("Color");
    let sidebar_file_assoc_btn = make_sidebar_button("File associations");

    // General starts selected
    sidebar_general_btn.setState(NSControlStateValueOn);

    let sidebar_stack = NSStackView::new(mtm);
    sidebar_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    sidebar_stack.setAlignment(NSLayoutAttribute::Leading);
    sidebar_stack.setSpacing(12.0);
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_general_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_zoom_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_color_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_file_assoc_btn) });

    // ── General panel ─────────────────────────────────────────────────

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

    let auto_update_desc_ref = unsafe { as_view::<NSTextField>(&auto_update_desc) };
    let auto_fit_desc_ref = unsafe { as_view::<NSTextField>(&auto_fit_desc) };
    let enlarge_desc_ref = unsafe { as_view::<NSTextField>(&enlarge_desc) };
    let icc_desc_ref = unsafe { as_view::<NSTextField>(&icc_desc) };
    let cm_desc_ref = unsafe { as_view::<NSTextField>(&cm_desc) };

    let scroll_to_zoom_desc_ref = unsafe { as_view::<NSTextField>(&scroll_to_zoom_desc) };
    let title_bar_desc_ref = unsafe { as_view::<NSTextField>(&title_bar_desc) };

    // General panel: Auto-update + Scroll to zoom + Title bar
    let general_panel = make_vertical_stack(
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
    general_panel.setAlignment(NSLayoutAttribute::Leading);
    general_panel.setCustomSpacing_afterView(16.0, auto_update_desc_ref);
    general_panel.setCustomSpacing_afterView(16.0, scroll_to_zoom_desc_ref);

    // Pin toggle rows to full panel width
    for row in [&auto_update_row, &scroll_to_zoom_row, &title_bar_row] {
        let c = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                row, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                Some(&general_panel as &AnyObject), NSLayoutAttribute::Width,
                1.0, 0.0,
            )
        };
        c.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(c) });
    }

    // Zoom panel: Auto-fit window + Enlarge small images
    let zoom_panel = make_vertical_stack(
        &[
            unsafe { as_view::<NSStackView>(&auto_fit_row) },
            auto_fit_desc_ref,
            unsafe { as_view::<NSStackView>(&enlarge_row) },
            enlarge_desc_ref,
        ],
        8.0,
        mtm,
    );
    zoom_panel.setAlignment(NSLayoutAttribute::Leading);
    zoom_panel.setCustomSpacing_afterView(16.0, auto_fit_desc_ref);

    for row in [&auto_fit_row, &enlarge_row] {
        let c = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                row, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                Some(&zoom_panel as &AnyObject), NSLayoutAttribute::Width,
                1.0, 0.0,
            )
        };
        c.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(c) });
    }

    unsafe {
        let _: () = msg_send![&*zoom_panel, setHidden: true];
    }

    // Color panel: ICC color management + Color match display + Relative colorimetric
    let color_panel = make_vertical_stack(
        &[
            unsafe { as_view::<NSStackView>(&icc_row) },
            icc_desc_ref,
            unsafe { as_view::<NSStackView>(&cm_row) },
            cm_desc_ref,
            unsafe { as_view::<NSStackView>(&rc_row) },
            unsafe { as_view::<NSTextField>(&rc_desc) },
        ],
        8.0,
        mtm,
    );
    color_panel.setAlignment(NSLayoutAttribute::Leading);
    color_panel.setCustomSpacing_afterView(16.0, icc_desc_ref);
    color_panel.setCustomSpacing_afterView(16.0, cm_desc_ref);

    for row in [&icc_row, &cm_row, &rc_row] {
        let c = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                row, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                Some(&color_panel as &AnyObject), NSLayoutAttribute::Width,
                1.0, 0.0,
            )
        };
        c.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(c) });
    }

    unsafe {
        let _: () = msg_send![&*color_panel, setHidden: true];
    }

    // ── File associations panel ──────────────────────────────────────

    let utis = crate::platform::macos::file_associations::SUPPORTED_UTIS;
    let all_prvw = utis
        .iter()
        .all(|e| crate::platform::macos::file_associations::is_prvw_default(e.uti));

    // "Set all" row
    let fa_set_all_label = make_label("Open all supported images with Prvw", 14.0, mtm);
    fa_set_all_label.setAlignment(NSTextAlignment(0));

    let fa_set_all_secondary_text = if all_prvw {
        "All image types are handled by Prvw."
    } else {
        "Some image types are handled by other apps."
    };
    let fa_set_all_secondary = make_label(fa_set_all_secondary_text, 12.0, mtm);
    fa_set_all_secondary.setAlignment(NSTextAlignment(0));
    fa_set_all_secondary.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let fa_set_all_toggle = NSSwitch::new(mtm);
    fa_set_all_toggle.setState(if all_prvw {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });

    let fa_set_all_spacer = FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () =
            msg_send![&*fa_set_all_spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () =
            msg_send![&*fa_set_all_spacer, setContentHuggingPriority: 1.0f32, forOrientation: 0i64];
    }

    let fa_set_all_label_stack = NSStackView::new(mtm);
    fa_set_all_label_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    fa_set_all_label_stack.setAlignment(NSLayoutAttribute::Leading);
    fa_set_all_label_stack.setSpacing(2.0);
    fa_set_all_label_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&fa_set_all_label) });
    fa_set_all_label_stack
        .addArrangedSubview(unsafe { as_view::<NSTextField>(&fa_set_all_secondary) });

    let fa_set_all_row = NSStackView::new(mtm);
    fa_set_all_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    fa_set_all_row.setSpacing(12.0);
    fa_set_all_row.addArrangedSubview(unsafe { as_view::<NSStackView>(&fa_set_all_label_stack) });
    fa_set_all_row.addArrangedSubview(unsafe { as_view::<NSView>(&fa_set_all_spacer) });
    fa_set_all_row.addArrangedSubview(unsafe { as_view::<NSSwitch>(&fa_set_all_toggle) });

    // Per-UTI rows
    let mut fa_uti_toggles: [*const NSSwitch; UTI_COUNT] = [std::ptr::null(); UTI_COUNT];
    let mut fa_uti_labels: [*const NSTextField; UTI_COUNT] = [std::ptr::null(); UTI_COUNT];
    let mut fa_uti_rows: Vec<Retained<NSStackView>> = Vec::new();
    let mut fa_uti_toggle_retained: Vec<Retained<NSSwitch>> = Vec::new();
    let mut fa_uti_label_retained: Vec<Retained<NSTextField>> = Vec::new();
    let mut fa_uti_primary_retained: Vec<Retained<NSTextField>> = Vec::new();
    let mut fa_uti_label_stack_retained: Vec<Retained<NSStackView>> = Vec::new();
    let mut fa_uti_spacer_retained: Vec<Retained<NSView>> = Vec::new();

    for (i, entry) in utis.iter().enumerate() {
        let is_prvw = crate::platform::macos::file_associations::is_prvw_default(entry.uti);

        let primary_text = format!("{} ({})", entry.label, entry.extensions);
        let primary = make_label(&primary_text, 14.0, mtm);
        primary.setAlignment(NSTextAlignment(0));

        let secondary_text = file_assoc_secondary_text(entry.uti, is_prvw);
        let secondary = make_label(&secondary_text, 12.0, mtm);
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

        fa_uti_toggles[i] = &*toggle as *const NSSwitch;
        fa_uti_labels[i] = &*secondary as *const NSTextField;

        fa_uti_rows.push(row);
        fa_uti_toggle_retained.push(toggle);
        fa_uti_label_retained.push(secondary);
        fa_uti_primary_retained.push(primary);
        fa_uti_label_stack_retained.push(label_stack);
        fa_uti_spacer_retained.push(spacer);
    }

    let fa_set_all_row_ref = unsafe { as_view::<NSStackView>(&fa_set_all_row) };

    let mut file_assoc_views: Vec<&NSView> = vec![fa_set_all_row_ref];
    let fa_row_refs: Vec<&NSView> = fa_uti_rows
        .iter()
        .map(|r| unsafe { as_view::<NSStackView>(r) })
        .collect();
    for r in &fa_row_refs {
        file_assoc_views.push(r);
    }

    let file_assoc_panel = make_vertical_stack(&file_assoc_views, 8.0, mtm);
    file_assoc_panel.setAlignment(NSLayoutAttribute::Leading);
    // Extra spacing after the "Set all" row
    file_assoc_panel.setCustomSpacing_afterView(16.0, fa_set_all_row_ref);

    // Pin all toggle rows to full panel width
    let c = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &fa_set_all_row, NSLayoutAttribute::Width,
            NSLayoutRelation::Equal,
            Some(&file_assoc_panel as &AnyObject), NSLayoutAttribute::Width,
            1.0, 0.0,
        )
    };
    c.setActive(true);
    retained_views.push(unsafe { Retained::cast_unchecked(c) });

    for row in &fa_uti_rows {
        let c = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                row, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                Some(&file_assoc_panel as &AnyObject), NSLayoutAttribute::Width,
                1.0, 0.0,
            )
        };
        c.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(c) });
    }

    // File associations panel starts hidden
    unsafe {
        let _: () = msg_send![&*file_assoc_panel, setHidden: true];
    }

    // ── Create delegate with ivars ────────────────────────────────────

    let ivars = SettingsDelegateIvars {
        enlarge_toggle: &*enlarge_toggle as *const NSSwitch,
        color_match_toggle: &*cm_toggle as *const NSSwitch,
        relative_col_toggle: &*rc_toggle as *const NSSwitch,
        scroll_to_zoom_desc: &*scroll_to_zoom_desc as *const NSTextField,
        general_panel: &*general_panel as *const NSStackView,
        zoom_panel: &*zoom_panel as *const NSStackView,
        color_panel: &*color_panel as *const NSStackView,
        file_assoc_panel: &*file_assoc_panel as *const NSStackView,
        sidebar_general_btn: &*sidebar_general_btn as *const NSButton,
        sidebar_zoom_btn: &*sidebar_zoom_btn as *const NSButton,
        sidebar_color_btn: &*sidebar_color_btn as *const NSButton,
        sidebar_file_assoc_btn: &*sidebar_file_assoc_btn as *const NSButton,
    };
    let delegate = SettingsDelegate::new(mtm, ivars);

    // ── Wire target/action on all controls ────────────────────────────

    unsafe {
        auto_update_toggle.setTarget(Some(&delegate as &AnyObject));
        auto_update_toggle.setAction(Some(sel!(toggleAutoUpdate:)));

        scroll_to_zoom_toggle.setTarget(Some(&delegate as &AnyObject));
        scroll_to_zoom_toggle.setAction(Some(sel!(toggleScrollToZoom:)));

        title_bar_toggle.setTarget(Some(&delegate as &AnyObject));
        title_bar_toggle.setAction(Some(sel!(toggleTitleBar:)));

        auto_fit_toggle.setTarget(Some(&delegate as &AnyObject));
        auto_fit_toggle.setAction(Some(sel!(toggleAutoFitWindow:)));

        enlarge_toggle.setTarget(Some(&delegate as &AnyObject));
        enlarge_toggle.setAction(Some(sel!(toggleEnlargeSmallImages:)));

        icc_toggle.setTarget(Some(&delegate as &AnyObject));
        icc_toggle.setAction(Some(sel!(toggleIccColorManagement:)));

        cm_toggle.setTarget(Some(&delegate as &AnyObject));
        cm_toggle.setAction(Some(sel!(toggleColorMatchDisplay:)));

        rc_toggle.setTarget(Some(&delegate as &AnyObject));
        rc_toggle.setAction(Some(sel!(toggleRelativeColorimetric:)));

        sidebar_general_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_general_btn.setAction(Some(sel!(selectGeneral:)));

        sidebar_zoom_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_zoom_btn.setAction(Some(sel!(selectZoom:)));

        sidebar_color_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_color_btn.setAction(Some(sel!(selectColor:)));

        sidebar_file_assoc_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_file_assoc_btn.setAction(Some(sel!(selectFileAssoc:)));
    }

    // ── Create file association delegate ──────────────────────────────

    let fa_ivars = FileAssocDelegateIvars {
        uti_toggles: fa_uti_toggles,
        uti_labels: fa_uti_labels,
        set_all_toggle: &*fa_set_all_toggle as *const NSSwitch,
        set_all_label: &*fa_set_all_secondary as *const NSTextField,
    };
    let fa_delegate = FileAssocDelegate::new(mtm, fa_ivars);

    // Wire file association toggles
    unsafe {
        fa_set_all_toggle.setTarget(Some(&fa_delegate as &AnyObject));
        fa_set_all_toggle.setAction(Some(sel!(toggleSetAll:)));

        for toggle in &fa_uti_toggle_retained {
            toggle.setTarget(Some(&fa_delegate as &AnyObject));
            toggle.setAction(Some(sel!(toggleFileAssoc:)));
        }
    }

    // 1-second polling timer for file association state
    let fa_delegate_ptr: *const AnyObject =
        &*fa_delegate as *const FileAssocDelegate as *const AnyObject;
    let _fa_poll_timer: Retained<AnyObject> = unsafe {
        msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: fa_delegate_ptr,
            selector: sel!(pollFileAssoc:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ]
    };

    // ── Close + ESC buttons ───────────────────────────────────────────

    let close_button = make_close_button("Close", &window, mtm);
    let esc_button = make_escape_button(&window, mtm);

    // ── Layout with Auto Layout ───────────────────────────────────────

    unsafe {
        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        // Sidebar
        let _: () = msg_send![&*sidebar_stack, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_view_ref.addSubview(as_view::<NSStackView>(&sidebar_stack));

        // Separator line
        let separator = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*separator, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*separator, setWantsLayer: true];
        let sep_layer: *const AnyObject = msg_send![&*separator, layer];
        if !sep_layer.is_null() {
            // Use raw objc_msgSend for CGColor — it returns a CGColorRef (opaque C pointer)
            // which msg_send! mis-encodes as '@' (ObjC object) instead of '^{CGColor=}'.
            let sep_color = NSColor::separatorColor();
            let cg_color_sel = sel!(CGColor);
            let bg_color_sel = sel!(setBackgroundColor:);
            let get_cg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
            ) -> *const std::ffi::c_void =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            let cg_color = get_cg(&*sep_color as *const _ as *const AnyObject, cg_color_sel);
            let set_bg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
                *const std::ffi::c_void,
            ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            set_bg(sep_layer, bg_color_sel, cg_color);
        }
        content_view_ref.addSubview(&separator);

        // Content container (holds both panels, pinned to same edges)
        let content_container = FlippedView::new_as_nsview(mtm);
        let _: () =
            msg_send![&*content_container, setTranslatesAutoresizingMaskIntoConstraints: false];

        // Wrap content in a scroll view so tall sections are scrollable
        let scroll_view: Retained<AnyObject> = {
            let sv: Retained<AnyObject> = msg_send![objc2::class!(NSScrollView), new];
            let _: () = msg_send![&*sv, setTranslatesAutoresizingMaskIntoConstraints: false];
            let _: () = msg_send![&*sv, setDocumentView: &*content_container];
            let _: () = msg_send![&*sv, setHasVerticalScroller: true];
            let _: () = msg_send![&*sv, setHasHorizontalScroller: false];
            let _: () = msg_send![&*sv, setAutohidesScrollers: true];
            let _: () = msg_send![&*sv, setDrawsBackground: false];
            let _: () = msg_send![&*sv, setBorderType: 0i64]; // NSNoBorder
            sv
        };
        content_view_ref.addSubview(&*(&*scroll_view as *const AnyObject as *const NSView));

        let clip_view: *const AnyObject = msg_send![&*scroll_view, contentView];

        // Pin content_container width to the clip view so it doesn't scroll horizontally.
        // Top-anchoring is handled by FlippedView (Y=0 at top).
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &content_container, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            Some(&*clip_view as &AnyObject), NSLayoutAttribute::Width, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Add panels to content container
        let _: () = msg_send![&*general_panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*zoom_panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*color_panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () =
            msg_send![&*file_assoc_panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_container.addSubview(as_view::<NSStackView>(&general_panel));
        content_container.addSubview(as_view::<NSStackView>(&zoom_panel));
        content_container.addSubview(as_view::<NSStackView>(&color_panel));
        content_container.addSubview(as_view::<NSStackView>(&file_assoc_panel));

        // Horizontal separator above Close button
        let close_separator = FlippedView::new_as_nsview(mtm);
        let _: () =
            msg_send![&*close_separator, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*close_separator, setWantsLayer: true];
        let close_sep_layer: *const AnyObject = msg_send![&*close_separator, layer];
        if !close_sep_layer.is_null() {
            let sep_color = NSColor::separatorColor();
            let cg_color_sel = sel!(CGColor);
            let bg_color_sel = sel!(setBackgroundColor:);
            let get_cg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
            ) -> *const std::ffi::c_void =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            let cg_color = get_cg(&*sep_color as *const _ as *const AnyObject, cg_color_sel);
            let set_bg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
                *const std::ffi::c_void,
            ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            set_bg(close_sep_layer, bg_color_sel, cg_color);
        }
        content_view_ref.addSubview(&close_separator);

        // Close button below panels
        let _: () = msg_send![&*close_button, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_view_ref.addSubview(as_view::<NSButton>(&close_button));

        // ESC button (hidden)
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // ── Constraints ───────────────────────────────────────────────

        // Sidebar: top, leading, bottom, fixed width
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Leading, 1.0, 8.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 150.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Separator: 1px wide, after sidebar
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(as_view::<NSStackView>(&sidebar_stack) as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 1.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Scroll view: after vertical separator, top, trailing, above horizontal separator
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(&separator as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(&close_separator as &AnyObject), NSLayoutAttribute::Top, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Horizontal separator: from vertical separator to right edge, 1px tall, above close button
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(&separator as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Height, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 1.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(as_view::<NSButton>(&close_button) as &AnyObject), NSLayoutAttribute::Top, 1.0, -12.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Pin each panel to content container edges
        for panel in [&general_panel, &zoom_panel, &color_panel, &file_assoc_panel] {
            let panel_view = as_view::<NSStackView>(panel);

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Top, 1.0, 0.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Leading, 1.0, 20.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -20.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            // Bottom: content_container must be at least as tall as the panel (for scroll view sizing)
            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &content_container, NSLayoutAttribute::Bottom, NSLayoutRelation::GreaterThanOrEqual,
                Some(panel_view as &AnyObject), NSLayoutAttribute::Bottom, 1.0, 16.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));
        }

        // Close button: bottom-right
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -20.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, -16.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        retained_views.push(Retained::cast_unchecked(content_view_retained));
        retained_views.push(Retained::cast_unchecked(separator));
        retained_views.push(Retained::cast_unchecked(close_separator));
        retained_views.push(Retained::cast_unchecked(content_container));
        retained_views.push(scroll_view);
    }

    // Store all Retained objects so they live as long as the window
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_general_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_zoom_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_color_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_file_assoc_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_stack) });
    // General panel views
    retained_views.push(unsafe { Retained::cast_unchecked(auto_update_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_update_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_update_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(scroll_to_zoom_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(scroll_to_zoom_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(scroll_to_zoom_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_bar_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_bar_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_bar_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(enlarge_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(enlarge_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(enlarge_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(icc_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(icc_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(icc_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(cm_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(cm_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(cm_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(rc_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(rc_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(rc_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(general_panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(zoom_panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(color_panel) });
    // File association panel views
    retained_views.push(unsafe { Retained::cast_unchecked(fa_delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_secondary) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_spacer) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_label_stack) });
    retained_views.push(unsafe { Retained::cast_unchecked(fa_set_all_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(file_assoc_panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(_fa_poll_timer) });
    for row in fa_uti_rows {
        retained_views.push(unsafe { Retained::cast_unchecked(row) });
    }
    for toggle in fa_uti_toggle_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(toggle) });
    }
    for label in fa_uti_label_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(label) });
    }
    for primary in fa_uti_primary_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(primary) });
    }
    for label_stack in fa_uti_label_stack_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(label_stack) });
    }
    for spacer in fa_uti_spacer_retained {
        retained_views.push(unsafe { Retained::cast_unchecked(spacer) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(close_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });

    // ── Position and show ──────────────────────────────────────────────

    let parent = if parent_ns_window.is_null() {
        None
    } else {
        // SAFETY: caller guarantees this is a valid NSWindow pointer
        Some(unsafe { &*parent_ns_window })
    };
    center_window(&window, parent);

    window.makeKeyAndOrderFront(None);

    // FIXME: same leak as `show_about_window` — see the FIXME there for details.
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("Settings window shown");
}

/// Switch the Settings window to the given section by name.
/// Called by the QA server's `ShowSettingsSection` command.
pub fn switch_settings_section(section: &str) {
    let index = match section.to_lowercase().as_str() {
        "general" => 0,
        "zoom" => 1,
        "color" => 2,
        "file associations" | "file_associations" | "fileassociations" => 3,
        _ => {
            log::warn!("Unknown settings section: {section}");
            return;
        }
    };

    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str("Settings");
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    // Find the delegate and call select_panel
                    let delegate: *const AnyObject = msg_send![win, delegate];
                    if !delegate.is_null() {
                        let sel = match index {
                            0 => sel!(selectGeneral:),
                            1 => sel!(selectZoom:),
                            2 => sel!(selectColor:),
                            3 => sel!(selectFileAssoc:),
                            _ => return,
                        };
                        let _: () = msg_send![delegate, performSelector: sel, withObject: std::ptr::null::<AnyObject>()];
                    }
                    return;
                }
            }
        }
    }
    log::debug!("Settings window not open, cannot switch section");
}

/// Close the Settings window if it's open.
pub fn close_settings_window() {
    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str("Settings");
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    let _: () = msg_send![win, close];
                    log::debug!("Closed settings window");
                    return;
                }
            }
        }
    }
}
