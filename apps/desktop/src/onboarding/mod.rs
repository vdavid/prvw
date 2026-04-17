//! # Onboarding window
//!
//! Shown on first launch when no file is passed via CLI (Finder double-click or Dock
//! launch with no image).
//!
//! **Non-modal** because Finder's Apple Event delivering the file must still reach the
//! event loop while the onboarding is visible. An `NSTimer` polls file-association state
//! every second and re-renders via `OnboardingUI::render()`.
//!
//! The window shows a four-step checklist:
//!
//! 1. **Install Prvw.app** — always checked; running the binary means it's installed.
//! 2. **Set Prvw as your default image viewer** — checked iff every UTI in
//!    `SUPPORTED_UTIS` resolves to Prvw. Holds the "Set as default" button, disabled
//!    when already done, plus a natural-language summary of current defaults.
//! 3. **Move Prvw.app to /Applications** — checked iff the binary path starts with
//!    `/Applications/`. Computed once on open (the path doesn't change at runtime).
//! 4. **How to open images** — no checkmark; copy depends on step 2's state.
//!
//! `OnboardingState` is pure data — it's a snapshot of what Prvw sees right now.
//! `OnboardingUI` holds raw pointers to the widgets and knows how to write state into
//! them. This split keeps the render path trivial to reason about.
//!
//! Timing: `main()` delays 500ms after `EventLoop::new()` before showing the window. If
//! an Apple Event arrives in that window, onboarding is skipped entirely.
//!
//! **Close = quit.** While the onboarding is up, no winit window exists — so a raw
//! AppKit close wouldn't propagate to winit's event loop, and the process would hang.
//! `OnboardingDelegate::windowWillClose:` is set as the window's delegate and sends
//! `AppCommand::Exit` when the user dismisses it. `close_window()` (called when a file
//! arrives via Apple Event) detaches the delegate before closing so the file-arrived
//! transition isn't misread as a user dismiss.

mod checkmark;
pub mod defaults_sentence;

use crate::platform::macos::ui_common::{
    add_vibrancy_background, as_view, center_window, is_window_already_open, load_app_icon,
    make_bold_label, make_close_button, make_escape_button, make_label,
};
use checkmark::CheckState;
use defaults_sentence::{FormatGroup, FormatHandler, describe_defaults};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSFont, NSImageScaling,
    NSImageView, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView,
    NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

const ONBOARDING_TITLE: &str = "Welcome to Prvw";
const CHECKMARK_SIZE_PT: f64 = 18.0;
/// Horizontal distance from the left edge of a step row to where the step's title starts.
/// Sub-content under a step (description, defaults sentence, button, etc.) is indented
/// by this amount so it aligns with the step title, not the checkmark.
const STEP_TEXT_INDENT: f64 = 30.0;

/// Pure data snapshot of the onboarding window's dynamic state.
/// No UI references — computed from system queries, rendered by `OnboardingUI`.
struct OnboardingState {
    /// Checked only when every supported UTI resolves to Prvw. The "Set as default"
    /// button disables in this state and step 4's copy switches to the happy path.
    is_default_for_all: bool,
    /// True when the binary runs out of a `.app` bundle inside `/Applications/`. The
    /// onboarding doesn't re-check this at runtime — the path doesn't change without
    /// a relaunch — but we expose it here so `OnboardingUI::render` stays the single
    /// place that writes to widgets.
    is_in_applications: bool,
    /// Natural-language summary of what currently opens each format.
    defaults_sentence: String,
}

impl OnboardingState {
    fn current() -> Self {
        let is_default_for_all = is_prvw_default_for_all();
        let is_in_applications = crate::file_associations::is_in_applications();
        let handlers = collect_format_handlers();
        let defaults_sentence = describe_defaults(&handlers);
        Self {
            is_default_for_all,
            is_in_applications,
            defaults_sentence,
        }
    }

    fn step4_text(&self) -> &'static str {
        if self.is_default_for_all {
            "Close this window, then double-click any image to open it in Prvw."
        } else {
            "Close this window, then right-click any image file and choose Open with \u{2192} Prvw."
        }
    }
}

/// Collect per-UTI handler rows for `describe_defaults`. Standard formats first, then
/// RAW — matches the grouping in `defaults_sentence` which branches on `FormatGroup`.
fn collect_format_handlers() -> Vec<FormatHandler> {
    let mut out = Vec::with_capacity(
        crate::file_associations::SUPPORTED_STANDARD_UTIS.len()
            + crate::file_associations::SUPPORTED_RAW_UTIS.len(),
    );
    for entry in crate::file_associations::SUPPORTED_STANDARD_UTIS {
        out.push(handler_for(entry, FormatGroup::Standard));
    }
    for entry in crate::file_associations::SUPPORTED_RAW_UTIS {
        out.push(handler_for(entry, FormatGroup::Raw));
    }
    out
}

fn handler_for(entry: &crate::file_associations::UtiEntry, group: FormatGroup) -> FormatHandler {
    let bundle_id = crate::file_associations::get_handler_bundle_id(entry.uti);
    let is_prvw = bundle_id
        .as_deref()
        .map(|id| id.contains("prvw") || id.contains("Prvw"))
        .unwrap_or(false);
    let display = bundle_id
        .as_deref()
        .map(crate::file_associations::bundle_id_to_app_name);
    FormatHandler {
        format_label: entry.label,
        group,
        current_handler: display,
        is_prvw,
    }
}

/// Holds widget pointers for the onboarding window's dynamic elements.
/// The single `render()` method is the ONLY place these widgets get updated.
struct OnboardingUI {
    step2_check: *const NSImageView,
    step3_check: *const NSImageView,
    set_default_button: *const NSButton,
    defaults_label: *const NSTextField,
    step4_label: *const NSTextField,
    /// Pre-rendered checkmark images. Retained for the window's lifetime and
    /// shared between steps 2 and 3 when flipping states.
    check_green: Retained<objc2_app_kit::NSImage>,
    check_dim: Retained<objc2_app_kit::NSImage>,
}

// SAFETY: These raw pointers are only used on the main thread within the modal session,
// and the pointed-to objects are kept alive by retained_views. The Retained<NSImage>
// handles are refcounted; accessing them from the main thread only is sound.
unsafe impl Send for OnboardingUI {}
unsafe impl Sync for OnboardingUI {}

impl OnboardingUI {
    /// Apply state to all widgets.
    fn render(&self, state: &OnboardingState) {
        unsafe {
            if !self.step2_check.is_null() {
                let img = if state.is_default_for_all {
                    &*self.check_green
                } else {
                    &*self.check_dim
                };
                (*self.step2_check).setImage(Some(img));
            }
            if !self.step3_check.is_null() {
                let img = if state.is_in_applications {
                    &*self.check_green
                } else {
                    &*self.check_dim
                };
                (*self.step3_check).setImage(Some(img));
            }
            if !self.set_default_button.is_null() {
                let _: () =
                    msg_send![self.set_default_button, setEnabled: !state.is_default_for_all];
            }
            if !self.defaults_label.is_null() {
                (*self.defaults_label)
                    .setStringValue(&NSString::from_str(&state.defaults_sentence));
            }
            if !self.step4_label.is_null() {
                (*self.step4_label).setStringValue(&NSString::from_str(state.step4_text()));
            }
        }
    }
}

struct OnboardingDelegateIvars {
    ui: OnboardingUI,
}

// SAFETY: OnboardingUI already upholds Send+Sync; no other ivars live here.
unsafe impl Send for OnboardingDelegateIvars {}
unsafe impl Sync for OnboardingDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwOnboardingDelegate"]
    #[ivars = OnboardingDelegateIvars]
    struct OnboardingDelegate;

    unsafe impl NSObjectProtocol for OnboardingDelegate {}

    impl OnboardingDelegate {
        /// Called when the "Set as default viewer" button is pressed. Claims all
        /// supported UTIs, then re-renders so the checkmark flips and the button
        /// disables without waiting for the next poll tick.
        #[unsafe(method(setAsDefault:))]
        fn set_as_default(&self, _sender: &AnyObject) {
            log::info!("Setting Prvw as default viewer");
            crate::file_associations::set_as_default_viewer();
            let state = OnboardingState::current();
            self.ivars().ui.render(&state);
        }

        /// Called by NSTimer every second to poll file association state.
        #[unsafe(method(pollStatus:))]
        fn poll_status(&self, _timer: &AnyObject) {
            let state = OnboardingState::current();
            self.ivars().ui.render(&state);
        }

        /// NSWindowDelegate callback. Fires when the user dismisses the onboarding
        /// window (Close button, red traffic light, ESC). The file-arrived path detaches
        /// this delegate before calling `close`, so this only runs for user-initiated
        /// closes — which means "quit," since there's nothing else to show.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &AnyObject) {
            log::info!("Onboarding dismissed by user, exiting");
            crate::commands::send_command(crate::commands::AppCommand::Exit);
        }
    }
);

impl OnboardingDelegate {
    fn new(mtm: MainThreadMarker, ui: OnboardingUI) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(OnboardingDelegateIvars { ui });
        unsafe { msg_send![super(this), init] }
    }
}

/// Check if Prvw is the default handler for every UTI we claim.
fn is_prvw_default_for_all() -> bool {
    crate::file_associations::SUPPORTED_UTIS
        .iter()
        .all(|e| crate::file_associations::is_prvw_default(e.uti))
}

/// Build a small checkmark `NSImageView` with a fixed square frame.
fn make_check_image_view(
    initial: &objc2_app_kit::NSImage,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> Retained<NSImageView> {
    let view = NSImageView::imageViewWithImage(initial, mtm);
    unsafe {
        let _: () = msg_send![
            &*view,
            setImageScaling: NSImageScaling::ScaleProportionallyUpOrDown
        ];
    }
    let w = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &view, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, CHECKMARK_SIZE_PT,
        )
    };
    let h = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &view, NSLayoutAttribute::Height, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, CHECKMARK_SIZE_PT,
        )
    };
    w.setActive(true);
    h.setActive(true);
    retained_views.push(unsafe { Retained::cast_unchecked(w) });
    retained_views.push(unsafe { Retained::cast_unchecked(h) });
    view
}

/// Label factory matching the rest of the onboarding body: left-aligned, single line
/// or wrapping per the caller's choice. `wrapping=true` enables `[NSTextField
/// wrappingLabelWithString:]` which lets multi-line text flow inside the stack.
fn make_left_label(
    text: &str,
    font_size: f64,
    wrapping: bool,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
    let label = if wrapping {
        let ns = NSString::from_str(text);
        let l: Retained<NSTextField> = unsafe {
            msg_send![
                objc2::class!(NSTextField),
                wrappingLabelWithString: &*ns
            ]
        };
        l.setFont(Some(&NSFont::systemFontOfSize(font_size)));
        l
    } else {
        make_label(text, font_size, mtm)
    };
    label.setAlignment(NSTextAlignment(0)); // NSTextAlignmentLeft
    label.setEditable(false);
    label.setSelectable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label
}

/// Build a horizontal row: checkmark on the left, title label on the right. Returned
/// tuple is `(row, check_view)` — the caller decides whether the check view is a real
/// state indicator (steps 2 and 3) or fixed (step 1, always green).
fn build_step_header(
    check_image: &objc2_app_kit::NSImage,
    title: &str,
    retained_views: &mut Vec<Retained<AnyObject>>,
    mtm: MainThreadMarker,
) -> (Retained<NSStackView>, Retained<NSImageView>) {
    let check = make_check_image_view(check_image, retained_views, mtm);
    let title_label = make_left_label(title, 14.0, false, mtm);
    title_label.setFont(Some(&NSFont::systemFontOfSize(14.0)));

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.setSpacing(12.0);
    row.addArrangedSubview(unsafe { as_view::<NSImageView>(&check) });
    row.addArrangedSubview(unsafe { as_view::<NSTextField>(&title_label) });

    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    (row, check)
}

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

    // Wider and taller than the old single-call-to-action window: the 4-step
    // layout needs room for a wrapping defaults sentence plus a long primary
    // button without crowding.
    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 640.0));

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

    // ── Checkmark images ──────────────────────────────────────────────
    let check_green = checkmark::make_image(CHECKMARK_SIZE_PT, CheckState::Green);
    let check_dim = checkmark::make_image(CHECKMARK_SIZE_PT, CheckState::Dim);

    // ── App icon ──────────────────────────────────────────────────────
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
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 72.0,
            )
        };
        let h = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Height, NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 72.0,
            )
        };
        w.setActive(true);
        h.setActive(true);
        retained_views.push(unsafe { Retained::cast_unchecked(icon_image) });
        retained_views.push(unsafe { Retained::cast_unchecked(w) });
        retained_views.push(unsafe { Retained::cast_unchecked(h) });
        icon_view
    };

    // ── Title + tagline ────────────────────────────────────────────────
    let title_label = make_bold_label(&format!("Prvw {version}"), 22.0, mtm);
    let tagline_label = make_label("The absolute fastest image viewer for macOS.", 13.0, mtm);
    let secondary_color = NSColor::secondaryLabelColor();
    let tertiary_color = NSColor::tertiaryLabelColor();
    tagline_label.setTextColor(Some(&secondary_color));

    // Gather the initial system state so every widget starts in the right shape.
    let initial_state = OnboardingState::current();
    let step2_initial_check = if initial_state.is_default_for_all {
        &*check_green
    } else {
        &*check_dim
    };
    let step3_initial_check = if initial_state.is_in_applications {
        &*check_green
    } else {
        &*check_dim
    };

    // ── Section heading ────────────────────────────────────────────────
    let steps_heading = make_left_label("Steps to start:", 13.0, false, mtm);
    steps_heading.setTextColor(Some(&secondary_color));

    // ── Step 1 ─────────────────────────────────────────────────────────
    let (step1_row, _step1_check) = build_step_header(
        &check_green,
        "1. Install Prvw.app",
        &mut retained_views,
        mtm,
    );

    // ── Step 2 ─────────────────────────────────────────────────────────
    let (step2_row, step2_check) = build_step_header(
        step2_initial_check,
        "2. Set Prvw as your default image viewer",
        &mut retained_views,
        mtm,
    );
    let step2_description = make_left_label(
        "Prvw can handle JPEG, PNG, GIF (no animations yet), WebP, TIFF, BMP, and 10 raw formats.",
        12.0,
        true,
        mtm,
    );
    step2_description.setTextColor(Some(&secondary_color));

    let defaults_label = make_left_label(&initial_state.defaults_sentence, 12.0, true, mtm);
    defaults_label.setTextColor(Some(&secondary_color));

    let set_default_button = unsafe {
        let button = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Set Prvw as the default viewer for all of these files"),
            None,
            None,
            mtm,
        );
        button.setBezelStyle(NSBezelStyle::Push);
        let _: () = msg_send![&*button, setEnabled: !initial_state.is_default_for_all];
        button
    };

    let step2_footer = make_left_label(
        "Reverse this any time, or tweak file types in Settings.",
        12.0,
        true,
        mtm,
    );
    step2_footer.setTextColor(Some(&tertiary_color));

    // ── Step 3 ─────────────────────────────────────────────────────────
    let (step3_row, step3_check) = build_step_header(
        step3_initial_check,
        "3. Move Prvw.app to /Applications",
        &mut retained_views,
        mtm,
    );
    // Include a small hint only when the step isn't checked. Hidden by toggling the
    // label's visibility on each render would require yet another pointer; instead we
    // build it conditionally — the `.app` location doesn't change at runtime.
    let step3_hint = if !initial_state.is_in_applications {
        let hint = make_left_label(
            "Drag Prvw.app from where it is now into your Applications folder.",
            12.0,
            true,
            mtm,
        );
        hint.setTextColor(Some(&tertiary_color));
        Some(hint)
    } else {
        None
    };

    // ── Step 4 (no checkmark) ──────────────────────────────────────────
    let step4_prefix = make_left_label("4.", 14.0, false, mtm);
    step4_prefix.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    let step4_label = make_left_label(initial_state.step4_text(), 13.0, true, mtm);

    let step4_row = {
        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setAlignment(NSLayoutAttribute::Top);
        row.setSpacing(8.0);
        // Spacer to align the "4." with the other step titles (past the checkmark
        // column). Width = checkmark width + row spacing used in build_step_header.
        let spacer = crate::platform::macos::ui_common::FlippedView::new_as_nsview(mtm);
        unsafe {
            let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
            let w = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &spacer, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, CHECKMARK_SIZE_PT,
            );
            w.setActive(true);
            retained_views.push(Retained::cast_unchecked(w));
        }
        row.addArrangedSubview(&spacer);
        row.addArrangedSubview(unsafe { as_view::<NSTextField>(&step4_prefix) });
        row.addArrangedSubview(unsafe { as_view::<NSTextField>(&step4_label) });
        retained_views.push(unsafe { Retained::cast_unchecked(spacer) });
        row
    };

    // ── Indented sub-blocks for step 2 and step 3 ─────────────────────
    // Sub-content aligns with the step's title, not its checkmark, so we left-pad
    // every line by STEP_TEXT_INDENT.
    let step2_subblock = make_indented_block(
        &[
            unsafe { as_view::<NSTextField>(&step2_description) },
            unsafe { as_view::<NSTextField>(&defaults_label) },
            unsafe { as_view::<NSButton>(&set_default_button) },
            unsafe { as_view::<NSTextField>(&step2_footer) },
        ],
        mtm,
    );

    let step3_subblock = step3_hint
        .as_ref()
        .map(|hint| make_indented_block(&[unsafe { as_view::<NSTextField>(hint) }], mtm));

    // ── Close button ──────────────────────────────────────────────────
    let close_button = make_close_button("Close", &window, mtm);
    let esc_button = make_escape_button(&window, mtm);

    // ── Header stack (icon + title + tagline, centered) ───────────────
    let header_stack = NSStackView::new(mtm);
    header_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    header_stack.setAlignment(NSLayoutAttribute::CenterX);
    header_stack.setSpacing(6.0);
    header_stack.addArrangedSubview(unsafe { as_view::<NSImageView>(&icon_view) });
    header_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&title_label) });
    header_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&tagline_label) });
    header_stack.setCustomSpacing_afterView(10.0, unsafe { as_view::<NSImageView>(&icon_view) });

    // ── Body stack (steps heading + 4 steps, left-aligned) ────────────
    let body_stack = NSStackView::new(mtm);
    body_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    body_stack.setAlignment(NSLayoutAttribute::Leading);
    body_stack.setSpacing(8.0);
    body_stack.addArrangedSubview(unsafe { as_view::<NSTextField>(&steps_heading) });
    body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(&step1_row) });
    body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(&step2_row) });
    body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(&step2_subblock) });
    body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(&step3_row) });
    if let Some(sub) = &step3_subblock {
        body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(sub) });
    }
    body_stack.addArrangedSubview(unsafe { as_view::<NSStackView>(&step4_row) });

    // Larger gaps between logical groups: after the heading (so it reads as a
    // heading), after each step's content block.
    body_stack.setCustomSpacing_afterView(12.0, unsafe { as_view::<NSTextField>(&steps_heading) });
    body_stack.setCustomSpacing_afterView(14.0, unsafe { as_view::<NSStackView>(&step1_row) });
    body_stack.setCustomSpacing_afterView(6.0, unsafe { as_view::<NSStackView>(&step2_row) });
    body_stack.setCustomSpacing_afterView(18.0, unsafe { as_view::<NSStackView>(&step2_subblock) });
    if let Some(sub) = &step3_subblock {
        body_stack.setCustomSpacing_afterView(6.0, unsafe { as_view::<NSStackView>(&step3_row) });
        body_stack.setCustomSpacing_afterView(18.0, unsafe { as_view::<NSStackView>(sub) });
    } else {
        body_stack.setCustomSpacing_afterView(18.0, unsafe { as_view::<NSStackView>(&step3_row) });
    }

    // ── Wire delegate + timer ─────────────────────────────────────────
    let ui = OnboardingUI {
        step2_check: &*step2_check as *const NSImageView,
        step3_check: &*step3_check as *const NSImageView,
        set_default_button: &*set_default_button as *const NSButton,
        defaults_label: &*defaults_label as *const NSTextField,
        step4_label: &*step4_label as *const NSTextField,
        check_green: check_green.clone(),
        check_dim: check_dim.clone(),
    };

    let onboarding_delegate = OnboardingDelegate::new(mtm, ui);
    unsafe {
        set_default_button.setTarget(Some(&onboarding_delegate as &AnyObject));
        set_default_button.setAction(Some(sel!(setAsDefault:)));
        // Route the close (Close button / red traffic light / ESC) through our
        // windowWillClose: handler so a user dismiss turns into AppCommand::Exit.
        let _: () = msg_send![&*window, setDelegate: &*onboarding_delegate];
    };

    // ── Layout: header at top, body below, Close button bottom-right ──
    unsafe {
        let _: () = msg_send![&*header_stack, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*body_stack, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*close_button, setTranslatesAutoresizingMaskIntoConstraints: false];

        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();
        content_view_ref.addSubview(&header_stack);
        content_view_ref.addSubview(&body_stack);
        content_view_ref.addSubview(as_view::<NSButton>(&close_button));
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // Header: top-aligned, centered horizontally.
        let c1 = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &header_stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 32.0,
        );
        let c2 = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &header_stack, NSLayoutAttribute::CenterX, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::CenterX, 1.0, 0.0,
        );
        c1.setActive(true);
        c2.setActive(true);
        retained_views.push(Retained::cast_unchecked(c1));
        retained_views.push(Retained::cast_unchecked(c2));

        // Body: below header, pinned left + right with margins.
        let body_top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &body_stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(&*header_stack as &AnyObject), NSLayoutAttribute::Bottom, 1.0, 24.0,
        );
        let body_leading = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &body_stack, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Leading, 1.0, 40.0,
        );
        let body_trailing = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &body_stack, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -40.0,
        );
        body_top.setActive(true);
        body_leading.setActive(true);
        body_trailing.setActive(true);
        retained_views.push(Retained::cast_unchecked(body_top));
        retained_views.push(Retained::cast_unchecked(body_leading));
        retained_views.push(Retained::cast_unchecked(body_trailing));

        // Close button: bottom-right corner.
        let close_trailing = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -20.0,
        );
        let close_bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, -20.0,
        );
        close_trailing.setActive(true);
        close_bottom.setActive(true);
        retained_views.push(Retained::cast_unchecked(close_trailing));
        retained_views.push(Retained::cast_unchecked(close_bottom));

        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    let delegate_ptr: *const AnyObject =
        &*onboarding_delegate as *const OnboardingDelegate as *const AnyObject;

    // ── Retain everything for the window's lifetime ────────────────────
    retained_views.push(unsafe { Retained::cast_unchecked(onboarding_delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(icon_view) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(tagline_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(steps_heading) });
    retained_views.push(unsafe { Retained::cast_unchecked(step1_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(step2_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(step2_check) });
    retained_views.push(unsafe { Retained::cast_unchecked(step2_description) });
    retained_views.push(unsafe { Retained::cast_unchecked(defaults_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(set_default_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(step2_footer) });
    retained_views.push(unsafe { Retained::cast_unchecked(step2_subblock) });
    retained_views.push(unsafe { Retained::cast_unchecked(step3_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(step3_check) });
    if let Some(hint) = step3_hint {
        retained_views.push(unsafe { Retained::cast_unchecked(hint) });
    }
    if let Some(sub) = step3_subblock {
        retained_views.push(unsafe { Retained::cast_unchecked(sub) });
    }
    retained_views.push(unsafe { Retained::cast_unchecked(step4_prefix) });
    retained_views.push(unsafe { Retained::cast_unchecked(step4_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(step4_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(close_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(header_stack) });
    retained_views.push(unsafe { Retained::cast_unchecked(body_stack) });
    retained_views.push(unsafe { Retained::cast_unchecked(check_green) });
    retained_views.push(unsafe { Retained::cast_unchecked(check_dim) });

    center_window(&window, None);
    window.makeKeyAndOrderFront(None);
    unsafe {
        let _: () = msg_send![&*window, orderFrontRegardless];
    }

    // 1-second poll refreshes step 2's check + button + defaults sentence + step 4
    // copy. Step 3's state is computed once from the binary path, which doesn't
    // change without a relaunch.
    let poll_timer: Retained<AnyObject> = unsafe {
        msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: delegate_ptr,
            selector: sel!(pollStatus:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ]
    };
    retained_views.push(unsafe { Retained::cast_unchecked(poll_timer) });

    // Non-modal: forget views (they live until the window closes)
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("Onboarding window shown");
}

/// Wrap a row of subviews in a horizontal stack that begins with a spacer equal to
/// the step's title indent. Used for content under a step's title (sub-description,
/// current-defaults sentence, primary button, hint text). The inner stack stays
/// vertical so callers can pass multiple children.
fn make_indented_block(children: &[&NSView], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let inner = NSStackView::new(mtm);
    inner.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    inner.setAlignment(NSLayoutAttribute::Leading);
    inner.setSpacing(6.0);
    for child in children {
        inner.addArrangedSubview(child);
    }

    let spacer = crate::platform::macos::ui_common::FlippedView::new_as_nsview(mtm);
    unsafe {
        let _: () = msg_send![&*spacer, setTranslatesAutoresizingMaskIntoConstraints: false];
        let w = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &spacer, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, STEP_TEXT_INDENT,
        );
        w.setActive(true);
        // Leak the constraint — the stack carries enough constraint baggage already,
        // and the spacer's frame width is also baked into its autoresize metadata.
        std::mem::forget(w);
    }

    let outer = NSStackView::new(mtm);
    outer.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    outer.setAlignment(NSLayoutAttribute::Top);
    outer.setSpacing(0.0);
    outer.addArrangedSubview(&spacer);
    outer.addArrangedSubview(unsafe { as_view::<NSStackView>(&inner) });

    // Keep the spacer + inner alive via the outer stack; dropping the Rust handles
    // here is safe because `addArrangedSubview` retains them.
    std::mem::forget(spacer);
    std::mem::forget(inner);

    outer
}

/// Close the onboarding window if it's open. Detaches the window delegate first so
/// the windowWillClose: handler doesn't mistake this for a user dismiss (which would
/// send AppCommand::Exit — we're transitioning to the viewer, not quitting).
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
                    let null_delegate: *const AnyObject = std::ptr::null();
                    let _: () = msg_send![win, setDelegate: null_delegate];
                    let _: () = msg_send![win, close];
                    log::debug!("Closed onboarding window");
                    return;
                }
            }
        }
    }
}
