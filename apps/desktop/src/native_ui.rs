//! AppKit-based secondary windows (About, Onboarding, Settings).
//!
//! Uses objc2 bindings to build native NSWindow UIs with system controls.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSCursor, NSFont, NSImage, NSImageScaling, NSImageView,
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceLayoutOrientation, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSBundle, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL,
};

// ─── Helper functions ───────────────────────────────────────────────────────

/// Upcast any AppKit control to NSView for use with NSStackView.
/// SAFETY: All AppKit controls (NSTextField, NSButton, etc.) inherit from NSView
/// and have #[repr(C)] layout, making this pointer cast sound.
unsafe fn as_view<T>(obj: &T) -> &NSView {
    unsafe { &*(obj as *const T as *const NSView) }
}

/// Check if a window with the given title is already visible. Prevents opening duplicate
/// About/Settings windows when the user clicks the menu multiple times.
fn is_window_already_open(title: &str) -> bool {
    unsafe {
        // NSApplication::sharedApplication() is safe here because we're on the main thread
        // (called from winit event handlers which run on the main thread).
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str(title);
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    // Bring the existing window to front instead of creating a new one
                    let _: () = msg_send![win, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
                    return true;
                }
            }
        }
    }
    false
}

/// Create a non-editable, non-selectable NSTextField configured as a label.
fn make_label(text: &str, font_size: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(font_size)));
    label.setEditable(false);
    label.setSelectable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setAlignment(NSTextAlignment(2)); // NSTextAlignmentCenter = 2
    label
}

/// Create a bold label using the bold system font.
fn make_bold_label(text: &str, font_size: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(font_size)));
    label.setEditable(false);
    label.setSelectable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setAlignment(NSTextAlignment(2));
    label
}

/// Create a clickable link label that opens a URL in the default browser.
///
/// Uses NSTextField with an attributed string containing an NSLink attribute.
/// A tracking area is added so the pointing hand cursor appears on hover.
fn make_link(
    title: &str,
    url: &str,
    mtm: MainThreadMarker,
    retained_views: &mut Vec<Retained<AnyObject>>,
) -> Retained<NSTextField> {
    let ns_url = NSURL::URLWithString(&NSString::from_str(url)).unwrap();

    // Build a mutable attributed string with link + font attributes via msg_send.
    // We use raw msg_send because objc2-foundation doesn't expose NSMutableAttributedString
    // with the `addAttribute:value:range:` method through typed bindings.
    let range = NSRange::new(0, title.len());
    let ns_title = NSString::from_str(title);

    unsafe {
        let attr_string: *mut AnyObject =
            msg_send![objc2::class!(NSMutableAttributedString), alloc];
        let attr_string: *mut AnyObject = msg_send![attr_string, initWithString: &*ns_title];

        let link_attr_name = NSString::from_str("NSLink");
        let _: () = msg_send![
            attr_string, addAttribute: &*link_attr_name,
            value: &*ns_url, range: range
        ];

        let font = NSFont::systemFontOfSize(13.0);
        let font_attr_name = NSString::from_str("NSFont");
        let _: () = msg_send![
            attr_string, addAttribute: &*font_attr_name,
            value: &*font, range: range
        ];

        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        let _: () = msg_send![&*label, setAttributedStringValue: attr_string];
        label.setEditable(false);
        label.setSelectable(true); // Must be selectable for links to work
        label.setBordered(false);
        label.setDrawsBackground(false);
        label.setAlignment(NSTextAlignment(2));
        let _: () = msg_send![&*label, setAllowsEditingTextAttributes: true];

        // Add a tracking area so the pointing hand cursor shows on hover.
        // SAFETY: NSTrackingArea options are bitmask flags. We want cursor updates and
        // active-always tracking within the label's bounds.
        let tracking_options = NSTrackingAreaOptions::CursorUpdate
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let tracking_area = NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            tracking_options,
            Some(&label),
            None,
        );
        label.addTrackingArea(&tracking_area);
        retained_views.push(Retained::cast_unchecked(tracking_area));

        // Set the cursor to pointing hand via resetCursorRects proxy: add a cursor rect
        // covering the entire label.
        let cursor = NSCursor::pointingHandCursor();
        let bounds: NSRect = msg_send![&*label, bounds];
        label.addCursorRect_cursor(bounds, &cursor);

        // Release the attributed string (we created it with alloc+init, so we own it)
        let _: () = msg_send![attr_string, release];

        label
    }
}

/// Create an NSButton with the given title that closes its window on click.
fn make_close_button(title: &str, window: &NSWindow, mtm: MainThreadMarker) -> Retained<NSButton> {
    unsafe {
        let button = NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(window as &AnyObject),
            Some(objc2::sel!(performClose:)),
            mtm,
        );
        button.setBezelStyle(NSBezelStyle::Push);
        button
    }
}

/// Create a hidden button with Escape as key equivalent that closes the window.
/// Standard macOS pattern for "ESC to close".
fn make_escape_button(window: &NSWindow, mtm: MainThreadMarker) -> Retained<NSButton> {
    unsafe {
        let button = NSButton::new(mtm);
        let _: () = msg_send![&*button, setKeyEquivalent: &*NSString::from_str("\x1b")];
        button.setTarget(Some(window as &AnyObject));
        button.setAction(Some(sel!(performClose:)));
        let _: () = msg_send![&*button, setHidden: true];
        button
    }
}

/// Create a vertical NSStackView with centered alignment and the given spacing.
fn make_vertical_stack(
    views: &[&NSView],
    spacing: f64,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::CenterX);
    stack.setSpacing(spacing);

    for view in views {
        stack.addArrangedSubview(view);
    }

    stack
}

/// Add an NSVisualEffectView as background for frosted glass appearance.
/// Must be called after window creation but before adding other content.
fn add_vibrancy_background(
    window: &NSWindow,
    mtm: MainThreadMarker,
    retained_views: &mut Vec<Retained<AnyObject>>,
) {
    unsafe {
        let content_view: *mut NSView = msg_send![window, contentView];
        if content_view.is_null() {
            return;
        }
        let content_view_ref = &*content_view;

        let bounds: NSRect = msg_send![content_view_ref, bounds];
        let effect_view = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
        effect_view.setMaterial(NSVisualEffectMaterial::HUDWindow);
        effect_view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect_view.setState(NSVisualEffectState::Active);

        // Pin to fill using autoresizing mask (simpler than Auto Layout for a background)
        let _: () = msg_send![
            &*effect_view,
            setTranslatesAutoresizingMaskIntoConstraints: true
        ];
        // NSViewWidthSizable | NSViewHeightSizable = 2 | 16 = 18
        let _: () = msg_send![&*effect_view, setAutoresizingMask: 18u64];

        // Add as subview positioned below (behind) all existing subviews
        let effect_ref = as_view::<NSVisualEffectView>(&effect_view);
        // NSWindowBelow = -1: places effect_view behind all siblings
        let _: () = msg_send![content_view_ref, addSubview: effect_ref,
            positioned: -1i64, relativeTo: std::ptr::null::<NSView>()];

        retained_views.push(Retained::cast_unchecked(effect_view));
    }
}

/// Center a window on a parent window, or center on screen if no parent is given.
fn center_window(window: &NSWindow, parent: Option<&NSWindow>) {
    match parent {
        Some(parent) => unsafe {
            let parent_frame: NSRect = msg_send![parent, frame];
            let window_frame: NSRect = msg_send![window, frame];

            let x =
                parent_frame.origin.x + (parent_frame.size.width - window_frame.size.width) / 2.0;
            let y =
                parent_frame.origin.y + (parent_frame.size.height - window_frame.size.height) / 2.0;

            let origin = NSPoint::new(x, y);
            let _: () = msg_send![window, setFrameOrigin: origin];
        },
        None => unsafe {
            // Manually center using the screen's visible frame to avoid issues
            // with retina scaling or premature centering before the window is visible.
            let screen: *const AnyObject = msg_send![window, screen];
            if !screen.is_null() {
                let screen_frame: NSRect = msg_send![screen, visibleFrame];
                let window_frame: NSRect = msg_send![window, frame];
                let x = (screen_frame.size.width - window_frame.size.width) / 2.0
                    + screen_frame.origin.x;
                let y = (screen_frame.size.height - window_frame.size.height) / 2.0
                    + screen_frame.origin.y;
                let _: () = msg_send![window, setFrameOrigin: NSPoint::new(x, y)];
            } else {
                // Fallback if screen isn't available yet
                window.center();
            }
        },
    }
}

// ─── About window ───────────────────────────────────────────────────────────

/// Show the About window as a non-modal NSWindow.
///
/// Takes an optional parent NSWindow pointer to center on. The window is shown
/// immediately and returns without blocking.
///
/// # Safety
/// `parent_ns_window` must be a valid NSWindow pointer or null.
pub fn show_about_window(parent_ns_window: *const NSWindow) {
    if is_window_already_open("About Prvw") {
        return;
    }

    // SAFETY: we're on the main thread (called from winit event handler)
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let version = env!("CARGO_PKG_VERSION");

    // ── Create the window ──────────────────────────────────────────────

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));

    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );

        window.setTitle(&NSString::from_str("About Prvw"));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];

        // Prevent release on close so we don't double-free with Retained
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];

        window
    };

    // ── Build content views ────────────────────────────────────────────

    // All Retained<> objects must be kept alive for the window's lifetime.
    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    // Add frosted glass background
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

        // Constrain icon to 64x64
        let w = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
            )
        };
        let h = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Height,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
            )
        };
        w.setActive(true);
        h.setActive(true);

        retained_views.push(unsafe { Retained::cast_unchecked(icon_image) });
        retained_views.push(unsafe { Retained::cast_unchecked(w) });
        retained_views.push(unsafe { Retained::cast_unchecked(h) });

        icon_view
    };

    let title_label = make_bold_label("Prvw", 20.0, mtm);
    let version_label = make_label(&format!("Version {version}"), 13.0, mtm);
    let subtitle_label = make_label("A fast image viewer for macOS.", 13.0, mtm);
    let author_label = make_label("By David Veszelovszki", 13.0, mtm);

    // Dim secondary text
    let secondary_color = NSColor::secondaryLabelColor();
    version_label.setTextColor(Some(&secondary_color));
    subtitle_label.setTextColor(Some(&secondary_color));
    author_label.setTextColor(Some(&secondary_color));

    let link_website = make_link(
        "veszelovszki.com",
        "https://veszelovszki.com",
        mtm,
        &mut retained_views,
    );
    let link_product = make_link(
        "getprvw.com",
        "https://getprvw.com",
        mtm,
        &mut retained_views,
    );

    let ok_button = make_close_button("Close", &window, mtm);

    // Hidden ESC button to close with Escape key
    let esc_button = make_escape_button(&window, mtm);

    // ── Layout with NSStackView ────────────────────────────────────────

    let icon_ref = unsafe { as_view::<NSImageView>(&icon_view) };
    let title_ref = unsafe { as_view::<NSTextField>(&title_label) };
    let version_ref = unsafe { as_view::<NSTextField>(&version_label) };
    let subtitle_ref = unsafe { as_view::<NSTextField>(&subtitle_label) };
    let author_ref = unsafe { as_view::<NSTextField>(&author_label) };
    let link_web_ref = unsafe { as_view::<NSTextField>(&link_website) };
    let link_prod_ref = unsafe { as_view::<NSTextField>(&link_product) };
    let button_ref = unsafe { as_view::<NSButton>(&ok_button) };

    let views: Vec<&NSView> = vec![
        icon_ref,
        title_ref,
        version_ref,
        subtitle_ref,
        author_ref,
        link_web_ref,
        link_prod_ref,
        button_ref,
    ];

    let stack = make_vertical_stack(&views, 6.0, mtm);

    // Extra spacing after the icon and before the OK button
    stack.setCustomSpacing_afterView(12.0, icon_ref);
    stack.setCustomSpacing_afterView(16.0, link_prod_ref);

    // Set the stack as the window's content view with padding
    unsafe {
        let _: () = msg_send![
            &*stack,
            setTranslatesAutoresizingMaskIntoConstraints: false
        ];

        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        content_view_ref.addSubview(&stack);

        // Add the hidden ESC button to the content view (not the stack)
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // Pin stack edges to content view with padding.
        // Top is larger to clear the transparent titlebar.
        let top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Top,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top,
            1.0, 36.0,
        );
        let bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Bottom,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom,
            1.0, -20.0,
        );
        let cx = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::CenterX,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::CenterX,
            1.0, 0.0,
        );

        top.setActive(true);
        bottom.setActive(true);
        cx.setActive(true);

        retained_views.push(Retained::cast_unchecked(top));
        retained_views.push(Retained::cast_unchecked(bottom));
        retained_views.push(Retained::cast_unchecked(cx));
        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    // Store all Retained objects so they live as long as the window
    retained_views.push(unsafe { Retained::cast_unchecked(icon_view) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(version_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(subtitle_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(author_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(link_website) });
    retained_views.push(unsafe { Retained::cast_unchecked(link_product) });
    retained_views.push(unsafe { Retained::cast_unchecked(ok_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(stack) });

    // ── Position and show ──────────────────────────────────────────────

    let parent = if parent_ns_window.is_null() {
        None
    } else {
        // SAFETY: caller guarantees this is a valid NSWindow pointer
        Some(unsafe { &*parent_ns_window })
    };
    center_window(&window, parent);

    window.makeKeyAndOrderFront(None);

    // FIXME: leaks ~a few KB per window open. Each call to `show_about_window` leaks the
    // Retained<> wrappers and the window itself. The deduplication guard above prevents
    // stacking, but if the user closes and re-opens, it leaks again. Proper fix: use an
    // NSWindowDelegate with `windowWillClose:` to clean up, or store in a static Option.
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("About window shown");
}

// ─── Onboarding window ────────────────────────────────────────────────────

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
        let handler_status = crate::onboarding::query_handler_status();
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
            crate::onboarding::set_as_default_viewer();
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
    let status = crate::onboarding::query_handler_status();
    // query_handler_status returns lines like "  JPEG: Prvw.app (you)\n  PNG: Preview.app\n"
    // Count lines with content and check they all contain "(you)"
    let content_lines: Vec<&str> = status.lines().filter(|l| !l.trim().is_empty()).collect();
    !content_lines.is_empty() && content_lines.iter().all(|l| l.contains("(you)"))
}

/// Show the onboarding window as a modal NSWindow. Runs its own event loop
/// via `NSApplication::runModalForWindow`, so this MUST be called BEFORE
/// `EventLoop::new()` to avoid nested run loop segfaults.
///
/// Returns after the user closes the window (either via "Close" or "Set as default viewer").
pub fn show_onboarding_window() {
    // SAFETY: this runs before winit's event loop, on the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // Ensure NSApplication is initialized (needed for `cargo run` dev builds
    // where there's no .app bundle and NSApp isn't fully activated yet).
    let ns_app = NSApplication::sharedApplication(mtm);
    unsafe {
        // NSApplicationActivationPolicyRegular = 0. Required for the window to appear
        // in the foreground and receive events in dev builds.
        let _: bool = msg_send![&*ns_app, setActivationPolicy: 0i64];
        // Bring the app to the foreground. Needed in dev builds (`cargo run`) where
        // the app doesn't go through the normal app launch sequence.
        let _: () = msg_send![&*ns_app, activateIgnoringOtherApps: true];
    }

    let version = env!("CARGO_PKG_VERSION");

    // ── Create the window ──────────────────────────────────────────────

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

        window.setTitle(&NSString::from_str("Welcome to Prvw"));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];

        window
    };

    // ── Build content views ────────────────────────────────────────────

    // All Retained<> objects must be kept alive for the modal's duration.
    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    // Add frosted glass background
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
                &icon_view, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
            )
        };
        let h = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Height,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
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

    let is_dev_build = !crate::onboarding::is_app_bundle();
    let state = OnboardingState::current(is_dev_build);

    let instruction_label = make_label(state.instruction_text(), 13.0, mtm);
    instruction_label.setTextColor(Some(&secondary_color));

    // Green success label (shown when Prvw is the default for all types)
    let success_label = make_label("Prvw is your default image viewer.", 13.0, mtm);
    unsafe {
        let green = NSColor::systemGreenColor();
        success_label.setTextColor(Some(&green));
        let _: () = msg_send![&*success_label, setHidden: !state.is_default];
    }

    // Current file association status
    let status_label = make_label(&state.status_text(), 12.0, mtm);
    let tertiary_color = NSColor::tertiaryLabelColor();
    status_label.setTextColor(Some(&tertiary_color));

    // Tip if not in /Applications
    let tip_label = if !crate::onboarding::is_in_applications() {
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

    // ── Buttons ────────────────────────────────────────────────────────

    // Create the button first (delegate needs its pointer), then wire the delegate as target.
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

    // Build the OnboardingUI with raw pointers to the dynamic widgets.
    // SAFETY: the Retained<> in retained_views keeps the objects alive for the modal's duration.
    let ui = OnboardingUI {
        status_label: &*status_label as *const NSTextField,
        success_label: &*success_label as *const NSTextField,
        instruction_label: &*instruction_label as *const NSTextField,
        set_default_button: &*set_default_button as *const NSButton,
    };

    // Create the onboarding delegate that handles "Set as default" without stopping the modal.
    let onboarding_delegate = OnboardingDelegate::new(mtm, ui, is_dev_build);

    // Wire the delegate as the button's target now that it exists.
    unsafe {
        set_default_button.setTarget(Some(&onboarding_delegate as &AnyObject));
        set_default_button.setAction(Some(sel!(setAsDefault:)));
    };

    // Close code for the close button
    const CLOSE_CODE: isize = 1001;

    let close_button = unsafe {
        let button = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Close"),
            Some(&ns_app as &AnyObject),
            Some(objc2::sel!(stopModalWithCode:)),
            mtm,
        );
        button.setBezelStyle(NSBezelStyle::Push);
        let _: () = msg_send![&*button, setTag: CLOSE_CODE];
        button
    };

    // Button row (horizontal stack)
    let button_row = {
        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(12.0);
        row.addArrangedSubview(unsafe { as_view::<NSButton>(&set_default_button) });
        row.addArrangedSubview(unsafe { as_view::<NSButton>(&close_button) });
        row
    };

    // ── Layout with NSStackView ────────────────────────────────────────

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

    // Track the last content view before the button row for custom spacing
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

    // Extra spacing after icon and before button row
    stack.setCustomSpacing_afterView(14.0, icon_ref);
    stack.setCustomSpacing_afterView(20.0, last_before_buttons);

    // Set the stack as the window's content view with padding
    unsafe {
        let _: () = msg_send![
            &*stack,
            setTranslatesAutoresizingMaskIntoConstraints: false
        ];

        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        content_view_ref.addSubview(&stack);

        let top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Top,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top,
            1.0, 36.0,
        );
        let bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Bottom,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom,
            1.0, -20.0,
        );
        let cx = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::CenterX,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::CenterX,
            1.0, 0.0,
        );

        top.setActive(true);
        bottom.setActive(true);
        cx.setActive(true);

        retained_views.push(Retained::cast_unchecked(top));
        retained_views.push(Retained::cast_unchecked(bottom));
        retained_views.push(Retained::cast_unchecked(cx));
        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    // Save a raw pointer to the delegate for the timer (created after retained_views takes ownership).
    // SAFETY: the Retained<> in retained_views keeps the delegate alive for the modal's duration.
    let delegate_ptr: *const AnyObject =
        &*onboarding_delegate as *const OnboardingDelegate as *const AnyObject;

    // Store all Retained objects so they live through the modal session
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
    retained_views.push(unsafe { Retained::cast_unchecked(button_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(stack) });

    // ── Run modal ─────────────────────────────────────────────────────

    center_window(&window, None);
    window.makeKeyAndOrderFront(None);

    // Also bring to front forcefully for dev builds where activation may not work
    unsafe {
        let _: () = msg_send![&*window, orderFrontRegardless];
    }

    // Poll file association status every second. The NSTimer fires within the
    // modal run loop, so `pollStatus:` runs while the modal is active.
    // SAFETY: delegate_ptr is kept alive by retained_views for the modal's duration.
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

    // Only the close button calls `stopModalWithCode:`. The "Set as default" button
    // uses the delegate which stays within the modal.
    let _response: isize = unsafe { msg_send![&*ns_app, runModalForWindow: &*window] };

    // Stop the timer when the modal exits
    unsafe {
        let _: () = msg_send![&*poll_timer, invalidate];
    }

    window.orderOut(None);
    log::debug!("Onboarding window closed");

    drop(retained_views);
    drop(window);
}

// ─── Settings window ──────────────────────────────────────────────────────

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwSettingsDelegate"]
    struct SettingsDelegate;

    unsafe impl NSObjectProtocol for SettingsDelegate {}

    impl SettingsDelegate {
        /// Called when the auto-update NSSwitch is flipped.
        #[unsafe(method(toggleAutoUpdate:))]
        fn toggle_auto_update(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-update toggled: {on}");
            let mut settings = crate::settings::Settings::load();
            settings.auto_update = on;
            settings.save();
        }

        /// Called when the auto-fit window NSSwitch is flipped.
        /// Sends a command through the event loop so the App updates its state, the menu
        /// checkmark, and persists the setting — all in one place.
        #[unsafe(method(toggleAutoFitWindow:))]
        fn toggle_auto_fit_window(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-fit window toggled via settings: {on}");
            crate::qa_server::send_command(crate::qa_server::AppCommand::SetAutoFitWindow(on));
        }
    }
);

impl SettingsDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
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
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 260.0));

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

        window
    };

    // ── Build content views ────────────────────────────────────────────

    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    // Add frosted glass background
    add_vibrancy_background(&window, mtm, &mut retained_views);

    let settings = crate::settings::Settings::load();

    // Action delegate for the toggle
    let delegate = SettingsDelegate::new(mtm);

    // Auto-update label
    let toggle_label = make_label("Auto-update", 14.0, mtm);
    toggle_label.setAlignment(NSTextAlignment(0)); // NSTextAlignmentLeft

    // NSSwitch toggle
    let toggle = NSSwitch::new(mtm);
    let initial_state = if settings.auto_update {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    };
    toggle.setState(initial_state);

    // Wire the action: when toggled, call SettingsDelegate::toggleAutoUpdate:
    unsafe {
        toggle.setTarget(Some(&delegate as &AnyObject));
        toggle.setAction(Some(sel!(toggleAutoUpdate:)));
    }

    // Horizontal row: label + toggle
    let label_ref = unsafe { as_view::<NSTextField>(&toggle_label) };
    let toggle_ref = unsafe { as_view::<NSSwitch>(&toggle) };

    let toggle_row = NSStackView::new(mtm);
    toggle_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    toggle_row.setSpacing(12.0);
    toggle_row.addArrangedSubview(label_ref);
    toggle_row.addArrangedSubview(toggle_ref);

    // Auto-update description label
    let desc_label = make_label("Check for updates when Prvw starts.", 12.0, mtm);
    desc_label.setAlignment(NSTextAlignment(0)); // NSTextAlignmentLeft
    let secondary_color = NSColor::secondaryLabelColor();
    desc_label.setTextColor(Some(&secondary_color));

    // ── Auto-fit window toggle ───────────────────────────────────────

    let auto_fit_label = make_label("Auto-fit window", 14.0, mtm);
    auto_fit_label.setAlignment(NSTextAlignment(0));

    let auto_fit_toggle = NSSwitch::new(mtm);
    let auto_fit_state = if settings.auto_fit_window {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    };
    auto_fit_toggle.setState(auto_fit_state);

    unsafe {
        auto_fit_toggle.setTarget(Some(&delegate as &AnyObject));
        auto_fit_toggle.setAction(Some(sel!(toggleAutoFitWindow:)));
    }

    let auto_fit_label_ref = unsafe { as_view::<NSTextField>(&auto_fit_label) };
    let auto_fit_toggle_ref = unsafe { as_view::<NSSwitch>(&auto_fit_toggle) };

    let auto_fit_row = NSStackView::new(mtm);
    auto_fit_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    auto_fit_row.setSpacing(12.0);
    auto_fit_row.addArrangedSubview(auto_fit_label_ref);
    auto_fit_row.addArrangedSubview(auto_fit_toggle_ref);

    let auto_fit_desc_label = make_label("Resize the window to match each image.", 12.0, mtm);
    auto_fit_desc_label.setAlignment(NSTextAlignment(0));
    auto_fit_desc_label.setTextColor(Some(&secondary_color));

    // OK button
    let ok_button = make_close_button("Close", &window, mtm);

    // Hidden ESC button to close with Escape key
    let esc_button = make_escape_button(&window, mtm);

    // ── Layout with NSStackView ────────────────────────────────────────

    let toggle_row_ref = unsafe { as_view::<NSStackView>(&toggle_row) };
    let desc_ref = unsafe { as_view::<NSTextField>(&desc_label) };
    let auto_fit_row_ref = unsafe { as_view::<NSStackView>(&auto_fit_row) };
    let auto_fit_desc_ref = unsafe { as_view::<NSTextField>(&auto_fit_desc_label) };
    let button_ref = unsafe { as_view::<NSButton>(&ok_button) };

    let views: Vec<&NSView> = vec![
        toggle_row_ref,
        desc_ref,
        auto_fit_row_ref,
        auto_fit_desc_ref,
        button_ref,
    ];

    let stack = make_vertical_stack(&views, 8.0, mtm);
    stack.setAlignment(NSLayoutAttribute::Leading);

    // Visual grouping: extra spacing between setting groups and before OK
    stack.setCustomSpacing_afterView(16.0, desc_ref);
    stack.setCustomSpacing_afterView(24.0, auto_fit_desc_ref);

    // Set the stack as the window's content view with padding
    unsafe {
        let _: () = msg_send![
            &*stack,
            setTranslatesAutoresizingMaskIntoConstraints: false
        ];

        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        content_view_ref.addSubview(&stack);

        // Add the hidden ESC button to the content view (not the stack)
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // Pin stack edges to content view with padding
        let top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Top,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top,
            1.0, 36.0,
        );
        let leading = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Leading,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Leading,
            1.0, 24.0,
        );
        let trailing = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Trailing,
            NSLayoutRelation::LessThanOrEqual,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing,
            1.0, -24.0,
        );
        let bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Bottom,
            NSLayoutRelation::LessThanOrEqual,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom,
            1.0, -20.0,
        );

        top.setActive(true);
        leading.setActive(true);
        trailing.setActive(true);
        bottom.setActive(true);

        retained_views.push(Retained::cast_unchecked(top));
        retained_views.push(Retained::cast_unchecked(leading));
        retained_views.push(Retained::cast_unchecked(trailing));
        retained_views.push(Retained::cast_unchecked(bottom));
        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    // Store all Retained objects so they live as long as the window
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(toggle_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(toggle_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(desc_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_row) });
    retained_views.push(unsafe { Retained::cast_unchecked(auto_fit_desc_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(ok_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(stack) });

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

/// Load the app icon from the bundle or fall back to the resources dir.
fn load_app_icon() -> Retained<NSImage> {
    unsafe {
        // Try loading from bundle first (works in .app builds)
        let bundle = NSBundle::mainBundle();
        let icon_name = NSString::from_str("AppIcon");
        let image: *const NSImage = msg_send![&*bundle, imageForResource: &*icon_name];

        if !image.is_null() {
            return Retained::retain(image as *mut NSImage).unwrap();
        }

        // Fall back to loading from the resources directory (dev builds)
        let resource_path = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join("../../../apps/desktop/resources/AppIcon.icns"))
            })
            .and_then(|p| p.canonicalize().ok());

        if let Some(path) = resource_path {
            let ns_path = NSString::from_str(&path.to_string_lossy());
            let image = NSImage::initByReferencingFile(NSImage::alloc(), &ns_path);
            if let Some(image) = image {
                return image;
            }
        }

        // Last resort: use the generic application icon
        let app_icon_name = NSString::from_str("NSApplicationIcon");
        NSImage::imageNamed(&app_icon_name).expect("Couldn't load any app icon")
    }
}
