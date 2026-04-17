//! Shared AppKit helpers for About / Onboarding / Settings windows.
//!
//! `FlippedView`, label/button/link factories, vibrancy background, window centering,
//! app-icon loader. Siblings inside `native_ui::{about, onboarding, settings}` import
//! these via `use crate::platform::macos::ui_common::*`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBezelStyle, NSButton, NSCursor, NSFont, NSImage, NSLayoutAttribute,
    NSStackView, NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceLayoutOrientation, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow,
};
use objc2_foundation::{
    NSBundle, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL,
};

// ─── FlippedView ─────────────────────────────────────────────────────────────

define_class!(
    /// NSView subclass with a flipped coordinate system (Y=0 at top, like iOS/CSS/SwiftUI).
    /// Use this instead of `NSView::new()` for all custom container views to avoid
    /// layout surprises (especially with NSScrollView which bottom-anchors non-flipped views).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwFlippedView"]
    pub(crate) struct FlippedView;

    unsafe impl NSObjectProtocol for FlippedView {}

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }

    /// Create a FlippedView and return it as an NSView (for APIs that expect NSView).
    pub(crate) fn new_as_nsview(mtm: MainThreadMarker) -> Retained<NSView> {
        let view = Self::new(mtm);
        // SAFETY: FlippedView inherits from NSView, this cast is sound.
        unsafe { Retained::cast_unchecked(view) }
    }
}

// ─── Helper functions ───────────────────────────────────────────────────────

/// Upcast any AppKit control to NSView for use with NSStackView.
/// SAFETY: All AppKit controls (NSTextField, NSButton, etc.) inherit from NSView
/// and have #[repr(C)] layout, making this pointer cast sound.
pub(crate) unsafe fn as_view<T>(obj: &T) -> &NSView {
    unsafe { &*(obj as *const T as *const NSView) }
}

/// Check if a window with the given title is already visible. Prevents opening duplicate
/// About/Settings windows when the user clicks the menu multiple times.
pub(crate) fn is_window_already_open(title: &str) -> bool {
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
pub(crate) fn make_label(
    text: &str,
    font_size: f64,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
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
pub(crate) fn make_bold_label(
    text: &str,
    font_size: f64,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
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
pub(crate) fn make_link(
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
pub(crate) fn make_close_button(
    title: &str,
    window: &NSWindow,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
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
pub(crate) fn make_escape_button(window: &NSWindow, mtm: MainThreadMarker) -> Retained<NSButton> {
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
pub(crate) fn make_vertical_stack(
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
pub(crate) fn add_vibrancy_background(
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
pub(crate) fn center_window(window: &NSWindow, parent: Option<&NSWindow>) {
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

/// Load the app icon from the bundle or fall back to the resources dir.
pub(crate) fn load_app_icon() -> Retained<NSImage> {
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
