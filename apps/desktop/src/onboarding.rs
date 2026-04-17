//! # Onboarding window
//!
//! Shown on first launch when no file is passed via CLI (Finder double-click or Dock
//! launch with no image).
//!
//! **Non-modal** because Finder's Apple Event delivering the file must still reach the
//! event loop while the onboarding is visible. An `NSTimer` polls file-association state
//! every second and re-renders via `OnboardingUI::render()`.
//!
//! `OnboardingState` is pure data — it's a snapshot of what Prvw sees right now
//! (`is_default`, `is_dev_build`, `handler_status`). `OnboardingUI` holds raw pointers
//! to the widgets and knows how to write state into them. This split keeps the render
//! path trivial to reason about.
//!
//! Timing: `main()` delays 500ms after `EventLoop::new()` before showing the window. If
//! an Apple Event arrives in that window, onboarding is skipped entirely.

use crate::platform::macos::ui_common::{
    add_vibrancy_background, as_view, center_window, is_window_already_open, load_app_icon,
    make_bold_label, make_close_button, make_escape_button, make_label, make_vertical_stack,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSImageScaling,
    NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

/// Pure data snapshot of the onboarding window's dynamic state.
/// No UI references — computed from system queries, rendered by `OnboardingUI`.
struct OnboardingState {
    is_default: bool,
    is_dev_build: bool,
    handler_status: String,
}

impl OnboardingState {
    /// Query current file association state.
    fn current(is_dev_build: bool) -> Self {
        let handler_status = crate::file_associations::query_handler_status();
        let is_default = is_prvw_default_for_all();
        Self {
            is_default,
            is_dev_build,
            handler_status,
        }
    }

    fn instruction_text(&self) -> &str {
        if self.is_default {
            "You're all set. Double-click any image to open it in Prvw."
        } else if self.is_dev_build {
            "[DEV] Right-click any image and choose \"Open With\" > \"Prvw\".\nInstall Prvw.app to set it as your default viewer."
        } else {
            "To open images in Prvw, set it as your default viewer below,\nor right-click any image on your computer and choose \"Open With\" > \"Prvw\"."
        }
    }

    fn button_enabled(&self) -> bool {
        !self.is_default && !self.is_dev_build
    }

    fn button_title(&self) -> &str {
        if self.is_default {
            "Already set as default"
        } else {
            "Set as default viewer"
        }
    }

    fn status_text(&self) -> String {
        format!("Current defaults:\n{}", self.handler_status)
    }
}

/// Holds widget pointers for the onboarding window's dynamic elements.
/// The single `render()` method is the ONLY place these widgets get updated.
struct OnboardingUI {
    status_label: *const NSTextField,
    success_label: *const NSTextField,
    instruction_label: *const NSTextField,
    set_default_button: *const NSButton,
}

// SAFETY: These raw pointers are only used on the main thread within the modal session,
// and the pointed-to objects are kept alive by retained_views.
unsafe impl Send for OnboardingUI {}
unsafe impl Sync for OnboardingUI {}

impl OnboardingUI {
    /// Apply state to all widgets.
    fn render(&self, state: &OnboardingState) {
        unsafe {
            if !self.status_label.is_null() {
                (*self.status_label).setStringValue(&NSString::from_str(&state.status_text()));
            }
            if !self.success_label.is_null() {
                let _: () = msg_send![self.success_label, setHidden: !state.is_default];
            }
            if !self.instruction_label.is_null() {
                (*self.instruction_label)
                    .setStringValue(&NSString::from_str(state.instruction_text()));
            }
            if !self.set_default_button.is_null() {
                let _: () = msg_send![self.set_default_button, setEnabled: state.button_enabled()];
                let _: () = msg_send![self.set_default_button, setTitle: &*NSString::from_str(state.button_title())];
            }
        }
    }
}

struct OnboardingDelegateIvars {
    ui: OnboardingUI,
    is_dev_build: bool,
}

// SAFETY: OnboardingUI already has Send+Sync, and is_dev_build is a plain bool.
unsafe impl Send for OnboardingDelegateIvars {}
unsafe impl Sync for OnboardingDelegateIvars {}

// Delegate for the "Set as default viewer" button. Updates the status label
// without stopping the modal, so the window stays open.
define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwOnboardingDelegate"]
    #[ivars = OnboardingDelegateIvars]
    struct OnboardingDelegate;

    unsafe impl NSObjectProtocol for OnboardingDelegate {}

    impl OnboardingDelegate {
        /// Called when "Set as default viewer" is pressed. Sets defaults and refreshes
        /// the status label without stopping the modal.
        #[unsafe(method(setAsDefault:))]
        fn set_as_default(&self, _sender: &AnyObject) {
            log::info!("Setting Prvw as default viewer");
            crate::file_associations::set_as_default_viewer();
            let state = OnboardingState::current(self.ivars().is_dev_build);
            self.ivars().ui.render(&state);
        }

        /// Called by NSTimer every second to poll file association state.
        #[unsafe(method(pollStatus:))]
        fn poll_status(&self, _timer: &AnyObject) {
            let state = OnboardingState::current(self.ivars().is_dev_build);
            self.ivars().ui.render(&state);
        }
    }
);

impl OnboardingDelegate {
    fn new(mtm: MainThreadMarker, ui: OnboardingUI, is_dev_build: bool) -> Retained<Self> {
        let this = mtm
            .alloc()
            .set_ivars(OnboardingDelegateIvars { ui, is_dev_build });
        unsafe { msg_send![super(this), init] }
    }
}

/// Check if Prvw is the default handler for all queried types (JPEG and PNG).
fn is_prvw_default_for_all() -> bool {
    crate::file_associations::SUPPORTED_UTIS
        .iter()
        .all(|e| crate::file_associations::is_prvw_default(e.uti))
}
const ONBOARDING_TITLE: &str = "Welcome to Prvw";

/// Show the onboarding window as a non-modal NSWindow. Used when the app is launched
/// via Finder double-click (Apple Event) or Dock, where we need the event loop running
/// to receive the file-open event. The window closes when a file arrives or the user
/// clicks Close.
pub fn show_window() {
    if is_window_already_open(ONBOARDING_TITLE) {
        return;
    }

    // SAFETY: called from the main thread (winit event handler)
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // Ensure NSApplication is initialized (needed for `cargo run` dev builds)
    let ns_app = NSApplication::sharedApplication(mtm);
    unsafe {
        let _: bool = msg_send![&*ns_app, setActivationPolicy: 0i64];
        let _: () = msg_send![&*ns_app, activateIgnoringOtherApps: true];
    }

    let version = env!("CARGO_PKG_VERSION");

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 400.0));

    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setTitle(&NSString::from_str(ONBOARDING_TITLE));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];
        window
    };

    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();
    add_vibrancy_background(&window, mtm, &mut retained_views);

    // App icon
    let icon_view = {
        let icon_image = load_app_icon();
        let icon_view = NSImageView::imageViewWithImage(&icon_image, mtm);
        unsafe {
            let _: () = msg_send![
                &*icon_view,
                setImageScaling: NSImageScaling::ScaleProportionallyUpOrDown
            ];
        }
        let w = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 64.0,
            )
        };
        let h = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Height, NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 64.0,
            )
        };
        w.setActive(true);
        h.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(icon_image) });
        retained_views.push(unsafe { Retained::cast_unchecked(w) });
        retained_views.push(unsafe { Retained::cast_unchecked(h) });
        icon_view
    };

    let title_label = make_bold_label(&format!("Prvw v{version}"), 20.0, mtm);
    let subtitle_label = make_label("A fast image viewer for macOS.", 14.0, mtm);
    let secondary_color = NSColor::secondaryLabelColor();
    subtitle_label.setTextColor(Some(&secondary_color));

    let is_dev_build = !crate::file_associations::is_app_bundle();
    let state = OnboardingState::current(is_dev_build);

    let instruction_label = make_label(state.instruction_text(), 13.0, mtm);
    instruction_label.setTextColor(Some(&secondary_color));

    let success_label = make_label("Prvw is your default image viewer.", 13.0, mtm);
    unsafe {
        let green = NSColor::systemGreenColor();
        success_label.setTextColor(Some(&green));
        let _: () = msg_send![&*success_label, setHidden: !state.is_default];
    }

    let status_label = make_label(&state.status_text(), 12.0, mtm);
    let tertiary_color = NSColor::tertiaryLabelColor();
    status_label.setTextColor(Some(&tertiary_color));

    let tip_label = if !crate::file_associations::is_in_applications() {
        let label = make_label(
            "Tip: move Prvw.app to /Applications for the best experience.",
            12.0,
            mtm,
        );
        label.setTextColor(Some(&tertiary_color));
        Some(label)
    } else {
        None
    };

    let set_default_button = unsafe {
        let button = NSButton::buttonWithTitle_target_action(
            &NSString::from_str(state.button_title()),
            None,
            None,
            mtm,
        );
        button.setBezelStyle(NSBezelStyle::Push);
        let _: () = msg_send![&*button, setEnabled: state.button_enabled()];
        button
    };

    let ui = OnboardingUI {
        status_label: &*status_label as *const NSTextField,
        success_label: &*success_label as *const NSTextField,
        instruction_label: &*instruction_label as *const NSTextField,
        set_default_button: &*set_default_button as *const NSButton,
    };

    let onboarding_delegate = OnboardingDelegate::new(mtm, ui, is_dev_build);
    unsafe {
        set_default_button.setTarget(Some(&onboarding_delegate as &AnyObject));
        set_default_button.setAction(Some(sel!(setAsDefault:)));
    };

    // Non-modal: Close button uses performClose: (not stopModalWithCode:)
    let close_button = make_close_button("Close", &window, mtm);
    let esc_button = make_escape_button(&window, mtm);

    let button_row = {
        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(12.0);
        row.addArrangedSubview(unsafe { as_view::<NSButton>(&set_default_button) });
        row.addArrangedSubview(unsafe { as_view::<NSButton>(&close_button) });
        row
    };

    let icon_ref = unsafe { as_view::<NSImageView>(&icon_view) };
    let title_ref = unsafe { as_view::<NSTextField>(&title_label) };
    let subtitle_ref = unsafe { as_view::<NSTextField>(&subtitle_label) };
    let instruction_ref = unsafe { as_view::<NSTextField>(&instruction_label) };
    let success_ref = unsafe { as_view::<NSTextField>(&success_label) };
    let status_ref = unsafe { as_view::<NSTextField>(&status_label) };
    let button_row_ref = unsafe { as_view::<NSStackView>(&button_row) };

    let mut views: Vec<&NSView> = vec![
        icon_ref,
        title_ref,
        subtitle_ref,
        instruction_ref,
        success_ref,
        status_ref,
    ];

    let last_before_buttons: &NSView;
    if let Some(ref tip) = tip_label {
        let tip_ref = unsafe { as_view::<NSTextField>(tip) };
        views.push(tip_ref);
        last_before_buttons = tip_ref;
    } else {
        last_before_buttons = status_ref;
    }
    views.push(button_row_ref);

    let stack = make_vertical_stack(&views, 8.0, mtm);
    stack.setCustomSpacing_afterView(14.0, icon_ref);
    stack.setCustomSpacing_afterView(20.0, last_before_buttons);

    unsafe {
        let _: () = msg_send![&*stack, setTranslatesAutoresizingMaskIntoConstraints: false];
        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();
        content_view_ref.addSubview(&stack);
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        let top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        let bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, -20.0,
        );
        let cx = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::CenterX, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::CenterX, 1.0, 0.0,
        );
        top.setActive(true);
        bottom.setActive(true);
        cx.setActive(true);
        retained_views.push(Retained::cast_unchecked(top));
        retained_views.push(Retained::cast_unchecked(bottom));
        retained_views.push(Retained::cast_unchecked(cx));
        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    let delegate_ptr: *const AnyObject =
        &*onboarding_delegate as *const OnboardingDelegate as *const AnyObject;

    retained_views.push(unsafe { Retained::cast_unchecked(onboarding_delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(icon_view) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(subtitle_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(instruction_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(success_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(status_label) });
    if let Some(tip) = tip_label {
        retained_views.push(unsafe { Retained::cast_unchecked(tip) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(set_default_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(close_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(button_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(stack) });

    center_window(&window, None);
    window.makeKeyAndOrderFront(None);
    unsafe {
        let _: () = msg_send![&*window, orderFrontRegardless];
    }

    // Poll timer for file association updates
    let _poll_timer: Retained<AnyObject> = unsafe {
        msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: delegate_ptr,
            selector: sel!(pollStatus:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ]
    };
    retained_views.push(unsafe { Retained::cast_unchecked(_poll_timer) });

    // Non-modal: forget views (they live until the window closes)
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("Non-modal onboarding window shown");
}

/// Close the onboarding window if it's open.
pub fn close_window() {
    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str(ONBOARDING_TITLE);
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    let _: () = msg_send![win, close];
                    log::debug!("Closed onboarding window");
                    return;
                }
            }
        }
    }
}
