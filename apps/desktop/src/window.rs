//! # Window (main viewer window)
//!
//! Everything about the main image-viewer window: creation, fullscreen, auto-fit resize,
//! title-bar vibrancy.
//!
//! Not to be confused with `features/settings/window.rs` (the Settings window) or
//! `features/about/` / `features/onboarding/` (those AppKit panels). This is `winit`'s
//! main window.
//!
//! ## Key patterns
//!
//! - **Window + wgpu surface in `resumed()`.** Not at startup. Required by winit 0.30
//!   on macOS.
//! - **Auto-fit resize.** `resize_to_fit_image` computes physical size and returns it
//!   synchronously — callers can pass it straight to `renderer.resize()` instead of
//!   waiting for the asynchronous `Resized` event.
//! - **Background material.** On macOS 26+ the whole window background is a single
//!   `NSGlassEffectView` (Liquid Glass) behind the wgpu Metal layer, rounded to the window
//!   corner radius. The Metal layer is masked to a rounded rect inset by `IMAGE_FRAME_INSET`
//!   so the glass shows through as a uniform frame around the image (`apply_glass_frame_mask`).
//!   Older macOS falls back to the legacy `NSVisualEffectView` vibrancy (a dark full-window
//!   layer plus a title-bar strip toggled by `set_titlebar_vibrancy_visible`).
//! - **Window corner radius.** `round_window_frame_to_glass` rounds the window's own frame
//!   view to match the glass so the system's default-radius corner stroke can't peek out.
//! - **Traffic lights.** Nudged off the rounded corner by `register_traffic_light_keeper`,
//!   which swizzles `NSView`'s frame setters so the offset is baked into the position AppKit
//!   itself commits — gated to the main window's three buttons. See that function for why
//!   re-nudging after the fact loses a race during live resize.
//! - **Title-bar double-click.** Forwarded to the native window `zoom:` (`zoom_window`) so the
//!   title bar fills/restores the screen like any macOS app. Our content view covers the title
//!   bar, so AppKit never sees the click — `app.rs` routes title-bar double-clicks here.
//! - **Native title/zoom labels.** When the title bar is on, two `NSTextField` labels
//!   (`add_titlebar_labels`, added on both the Liquid Glass and legacy paths) show the title and
//!   zoom readout in the title-bar area. They're contentView subviews — siblings of the wgpu Metal
//!   layer — with their layer `zPosition` raised above it
//!   (`TITLEBAR_LABEL_Z_POSITION`) so they composite in front of the transparent strip region.
//!   (They can't live inside the strip: it's behind the Metal layer, and a transparent Metal pixel
//!   occludes in-window content behind it.) They use the appearance-aware `labelColor` /
//!   `secondaryLabelColor`, so they auto-contrast in light/dark mode — the old glyphon white text
//!   was unreadable on light glass. `set_titlebar_text` updates them each redraw (cache-guarded);
//!   `set_titlebar_vibrancy_visible` hides them in lockstep with the strip (title-bar off,
//!   fullscreen). The title-bar-off case still uses glyphon pills over the image (see
//!   `render/CLAUDE.md`).
//!
//! ## Gotchas
//!
//! - **`request_inner_size` is async on macOS.** After calling it, `inner_size()` still
//!   returns the OLD value. That's why `resize_to_fit_image` returns the computed size.
//! - **Re-nudging the traffic lights after a relayout loses to AppKit.** During a live resize
//!   AppKit's button layout runs last and overwrites any offset applied from a `Resized`
//!   handler or a frame-change notification, so the buttons sit at the default until the next
//!   redraw. The fix swizzles the frame setters so the offset rides along with AppKit's own
//!   positioning — see `register_traffic_light_keeper`.
//! - **macOS 26 (Liquid Glass) renders the traffic lights as SwiftUI views.** They live in
//!   `_NSCoreHostingView<ThemeWidgetView>`, not the legacy `_NSThemeCloseWidget` that
//!   `standardWindowButton:` still returns. So positioning the standard button moves nothing
//!   during a live resize — you must offset the SwiftUI hosting views (matched by class name).
//!   Their superview is also flipped (top-left origin), so the vertical nudge sign flips vs.
//!   the legacy bottom-left buttons. Both quirks are handled in `register_traffic_light_keeper`
//!   / `traffic_light_delta`.
//! - **Fullscreen appearance hand-off.** Toggling fullscreen triggers a `Resized` event
//!   which calls `set_fullscreen_appearance` to swap the background (vibrancy → solid
//!   black in fullscreen).
//! - **Title-bar labels must be click-through.** The app forwards title-bar mouse events
//!   through winit (`App::pointer_in_title_bar` → `zoom_window` for the double-click), so the
//!   strip's subviews must not capture them. The labels are `ClickThroughLabel`s whose
//!   `hitTest:` returns null; a plain `NSTextField` would swallow double-click-to-zoom and
//!   window drags where the title/zoom text sits.

use crate::pixels::{
    Logical, from_logical_pos, from_logical_size, from_physical_size, to_logical_pos,
    to_logical_size,
};
// Brought to module scope for the `ClickThroughLabel` `define_class!` below, whose macro
// arms require the superclass and protocol as bare identifiers (not paths).
#[cfg(target_os = "macos")]
use objc2::MainThreadOnly;
#[cfg(target_os = "macos")]
use objc2_app_kit::NSTextField;
#[cfg(target_os = "macos")]
use objc2_foundation::NSObjectProtocol;
use std::path::Path;
use std::sync::Arc;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Fullscreen, Window, WindowAttributes};

const DEFAULT_WIDTH: f64 = 1024.0;
const DEFAULT_HEIGHT: f64 = 768.0;

/// Minimum window dimension (logical pixels) when auto-fitting to image size.
pub const MIN_WINDOW_DIM: f64 = 200.0;

/// Maximum fraction of the monitor's work area to use when auto-fitting.
pub const MAX_SCREEN_FRACTION: f64 = 0.9;

/// True when `PRVW_BACKGROUND_WINDOW` is set. The integration test harness sets it
/// so the app window opens unfocused and ordered to the back: the E2E tests drive the
/// app entirely through the QA HTTP server (synthetic `AppCommand`s, never OS input),
/// so a background window passes every test while no longer stealing the developer's
/// keystrokes when a swarm of test windows pops up during a run. `main()` pairs this
/// with `ActivationPolicy::Accessory` so the app never forces itself to the foreground.
pub fn background_window_requested() -> bool {
    std::env::var_os("PRVW_BACKGROUND_WINDOW").is_some()
}

/// Create the application window. Must be called in `resumed()`.
pub fn create_window(event_loop: &ActiveEventLoop, file_path: &Path) -> Arc<Window> {
    let title = window_title_for_path(file_path);

    let mut attrs = WindowAttributes::default()
        .with_title(title)
        .with_inner_size(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
    if background_window_requested() {
        // Don't take focus on creation (see `background_window_requested`).
        attrs = attrs.with_active(false);
    }

    let window = event_loop
        .create_window(attrs)
        .expect("Failed to create window");
    let window = Arc::new(window);

    // Disable macOS tab bar and native fullscreen (we have our own borderless fullscreen).
    // This removes "Show Tab Bar", "Show All Tabs", and the system "Enter Full Screen" from menus.
    #[cfg(target_os = "macos")]
    configure_macos_window(&window);

    window
}

/// Set macOS-specific window properties via NSWindow.
#[cfg(target_os = "macos")]
fn configure_macos_window(window: &Window) {
    use objc2::msg_send;
    use objc2_app_kit::NSWindow;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::AppKit(handle)) => handle,
        _ => return,
    };

    let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
    let ns_window: *const NSWindow = unsafe { msg_send![ns_view, window] };
    if ns_window.is_null() {
        return;
    }

    unsafe {
        let ns_window = &*ns_window;

        // Disable tabbing: removes "Show Tab Bar" and "Show All Tabs" from View menu
        // NSWindowTabbingMode.disallowed = 2
        let _: () = msg_send![ns_window, setTabbingMode: 2i64];

        // Remove native fullscreen from collection behavior.
        // This removes the system "Enter Full Screen" menu item.
        // We keep our own borderless fullscreen via winit (F / Enter / F11).
        let behavior: u64 = msg_send![ns_window, collectionBehavior];
        // NSWindowCollectionBehavior.fullScreenPrimary = 1 << 7 = 128
        let new_behavior = behavior & !(1 << 7);
        let _: () = msg_send![ns_window, setCollectionBehavior: new_behavior];

        // Transparent titlebar: content extends behind the title bar, giving the frosted
        // glass look that apps like Finder and Safari use.
        let _: () = msg_send![ns_window, setTitlebarAppearsTransparent: true];
        // NSWindowStyleMask.fullSizeContentView = 1 << 15 = 32768
        let mask: u64 = msg_send![ns_window, styleMask];
        let _: () = msg_send![ns_window, setStyleMask: mask | (1u64 << 15)];

        // Hide the native title text. The title string is still set (for Mission Control
        // and accessibility) but not drawn — we render our own overlay instead.
        // NSWindowTitleVisibility.hidden = 1
        let _: () = msg_send![ns_window, setTitleVisibility: 1i64];

        // Make the window non-opaque so the NSVisualEffectViews (BehindWindow blend mode)
        // can sample the desktop behind the window for true vibrancy.
        let _: () = msg_send![ns_window, setOpaque: false];
        let clear_color: *const objc2::runtime::AnyObject =
            msg_send![objc2::class!(NSColor), clearColor];
        let _: () = msg_send![ns_window, setBackgroundColor: clear_color];

        // Two vibrancy layers: the full-window dark one (HUDWindow material) provides the
        // dark blurred background around the image, and the title bar one (Titlebar material)
        // sits on top in the title bar area. Order matters: full-window first so it's at
        // the back. Both end up behind the wgpu CAMetalLayer (which uses zPosition).
        add_image_area_background(ns_view);
        // On Liquid Glass the full-window glass IS the whole background, so the title bar
        // area is glass too — no separate strip. The legacy path keeps the title bar
        // vibrancy for a darker strip behind the title text.
        if !liquid_glass_available() {
            add_titlebar_vibrancy(ns_view);
        }

        // The native title/zoom labels are independent of the strip (they're contentView
        // subviews composited above the Metal layer via `zPosition`), so they're added on BOTH
        // paths: Liquid Glass has no separate strip but still needs the readout.
        let mtm = objc2_foundation::MainThreadMarker::new_unchecked();
        add_titlebar_labels(ns_view, mtm);

        // Round the window's own frame view to match the glass, so the system's
        // default-radius corner stroke (drawn on the key window) doesn't peek out past
        // the rounder glass corners.
        if liquid_glass_available() {
            round_window_frame_to_glass(ns_view);
        }
        // The traffic lights are nudged off the rounded edge by `register_traffic_light_keeper`
        // (called from `initialize_viewer`), which swizzles the frame setters so the offset is
        // applied as AppKit positions the buttons — see that function.

        // Test mode: push the window behind everything so a swarm of E2E windows
        // can't sit on top of the developer's work (see `background_window_requested`).
        if background_window_requested() {
            let nil: *const objc2::runtime::AnyObject = std::ptr::null();
            let _: () = msg_send![ns_window, orderBack: nil];
        }
    }

    log::debug!(
        "Configured macOS window: tabbing disabled, native fullscreen removed, transparent titlebar"
    );
}

/// Outer corner radius (logical points) of the window's rounded shape on macOS 26+ Liquid
/// Glass. Matched by eye to the system Quick Look window.
#[cfg(target_os = "macos")]
const WINDOW_CORNER_RADIUS: f64 = 29.0;

/// Width (logical points) of the Liquid Glass frame between the window edge and the image.
/// The image is clipped to a rounded rect inset by this much, leaving the glass visible as
/// a uniform band around the picture.
#[cfg(target_os = "macos")]
const IMAGE_FRAME_INSET: f64 = 5.0;

/// Inner corner radius (logical points) of the image, concentric with the window so the
/// glass frame stays a uniform width around the curve.
#[cfg(target_os = "macos")]
const IMAGE_CORNER_RADIUS: f64 = WINDOW_CORNER_RADIUS - IMAGE_FRAME_INSET;

/// How far (logical points) to nudge the traffic lights from their default spot, so they
/// don't crowd the rounded window corner: 6 pt inward (right) and 2 pt down. Y is expressed in
/// bottom-left coordinates (down = negative); `traffic_light_delta` negates it for the flipped
/// superview of the Liquid Glass SwiftUI titlebar so the visual direction stays the same.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_X_OFFSET: f64 = 6.0;
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_Y_OFFSET: f64 = -2.0;

/// True when this macOS build provides `NSGlassEffectView` (macOS 26 Tahoe and later).
/// Checked at runtime by class lookup so the same binary still runs on macOS 13–25, where
/// it falls back to the legacy `NSVisualEffectView` vibrancy.
#[cfg(target_os = "macos")]
pub fn liquid_glass_available() -> bool {
    objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_some()
}

/// Add the background that fills the area around the image. On macOS 26+ this is a real
/// Liquid Glass surface (vivid wallpaper color pickup + rounded window corners); on older
/// systems it's the legacy dark vibrancy.
#[cfg(target_os = "macos")]
unsafe fn add_image_area_background(ns_view: *const objc2::runtime::AnyObject) {
    if liquid_glass_available() {
        unsafe { add_image_area_glass(ns_view) };
    } else {
        unsafe { add_image_area_vibrancy(ns_view) };
    }
}

/// Add a full-window `NSGlassEffectView` (macOS 26+ Liquid Glass) behind the wgpu layer.
/// Its `cornerRadius` defines the window's visible rounded shape, and the glass refracts
/// the desktop behind the window for vivid color pickup in the area around the image.
#[cfg(target_os = "macos")]
unsafe fn add_image_area_glass(ns_view: *const objc2::runtime::AnyObject) {
    use objc2::MainThreadOnly;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSGlassEffectView, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation,
    };
    use objc2_foundation::{MainThreadMarker, NSRect, NSString};

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), NSRect::default());
    unsafe {
        glass.setCornerRadius(WINDOW_CORNER_RADIUS);
        let identifier = NSString::from_str(IMAGE_AREA_VIBRANCY_IDENTIFIER);
        let _: () = msg_send![&*glass, setIdentifier: &*identifier];
        let _: () = msg_send![&*glass, setTranslatesAutoresizingMaskIntoConstraints: false];

        let glass_obj: *const AnyObject = &*glass as *const NSGlassEffectView as *const _;
        let _: () = msg_send![ns_view, addSubview: glass_obj];

        let make_constraint = |attr: NSLayoutAttribute,
                               parent_attr: NSLayoutAttribute,
                               constant: f64| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                    &glass, attr, NSLayoutRelation::Equal, Some(&*ns_view), parent_attr, 1.0, constant,
                )
        };
        for c in [
            make_constraint(NSLayoutAttribute::Top, NSLayoutAttribute::Top, 0.0),
            make_constraint(NSLayoutAttribute::Bottom, NSLayoutAttribute::Bottom, 0.0),
            make_constraint(NSLayoutAttribute::Leading, NSLayoutAttribute::Leading, 0.0),
            make_constraint(
                NSLayoutAttribute::Trailing,
                NSLayoutAttribute::Trailing,
                0.0,
            ),
        ] {
            c.setActive(true);
        }
    }
}

/// Add a full-window NSVisualEffectView with a dark material. This provides the dark
/// blurred background visible around the image (where the wgpu surface is transparent).
#[cfg(target_os = "macos")]
unsafe fn add_image_area_vibrancy(ns_view: *const objc2::runtime::AnyObject) {
    use objc2::MainThreadOnly;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation};
    use objc2_app_kit::{
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    };
    use objc2_foundation::{MainThreadMarker, NSRect, NSString};

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let zero_frame = NSRect::default();
    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), zero_frame);
    unsafe {
        // HUDWindow material: dark, translucent with blur. Suits the "almost black with
        // glass" look the user wants around the image.
        effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        let identifier = NSString::from_str(IMAGE_AREA_VIBRANCY_IDENTIFIER);
        let _: () = msg_send![&*effect, setIdentifier: &*identifier];

        let _: () = msg_send![&*effect, setTranslatesAutoresizingMaskIntoConstraints: false];

        let effect_obj: *const AnyObject = &*effect as *const NSVisualEffectView as *const _;
        let _: () = msg_send![ns_view, addSubview: effect_obj];

        // Pin to all four edges of the contentView.
        let make_constraint = |attr: NSLayoutAttribute,
                               parent_attr: NSLayoutAttribute,
                               constant: f64| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                    &effect, attr,
                    NSLayoutRelation::Equal,
                    Some(&*ns_view),
                    parent_attr, 1.0, constant,
                )
        };
        // After addSubview / setActive, AppKit owns retains on the view and constraints,
        // so we drop our local Retained handles at end of scope. The view tree keeps
        // everything alive for the window's lifetime.
        for c in [
            make_constraint(NSLayoutAttribute::Top, NSLayoutAttribute::Top, 0.0),
            make_constraint(NSLayoutAttribute::Bottom, NSLayoutAttribute::Bottom, 0.0),
            make_constraint(NSLayoutAttribute::Leading, NSLayoutAttribute::Leading, 0.0),
            make_constraint(
                NSLayoutAttribute::Trailing,
                NSLayoutAttribute::Trailing,
                0.0,
            ),
        ] {
            c.setActive(true);
        }
    }
}

/// Identifiers set on the vibrancy views so we can find them later by `identifier`
/// (NSView's `tag` is read-only on plain NSViews).
#[cfg(target_os = "macos")]
const TITLEBAR_VIBRANCY_IDENTIFIER: &str = "prvw.titlebar_vibrancy";
#[cfg(target_os = "macos")]
const IMAGE_AREA_VIBRANCY_IDENTIFIER: &str = "prvw.image_area_vibrancy";
/// Identifiers on the native title/zoom labels riding inside the title-bar strip, so
/// `set_titlebar_text` can find them and update their `stringValue`.
#[cfg(target_os = "macos")]
const TITLEBAR_TITLE_IDENTIFIER: &str = "prvw.titlebar_title";
#[cfg(target_os = "macos")]
const TITLEBAR_ZOOM_IDENTIFIER: &str = "prvw.titlebar_zoom";

/// Point size of the bold system font used for the native title/zoom labels. Matches the
/// 13.5pt the glyphon overlay used for the title-bar-off case.
#[cfg(target_os = "macos")]
const TITLEBAR_LABEL_FONT_SIZE: f64 = 13.5;

/// `zPosition` for the native title/zoom label layers. Above the wgpu CAMetalLayer's `1.0` (set
/// by `push_metal_layer_above_vibrancy`) so the labels — added as contentView subviews, siblings
/// of the Metal layer — composite in front of it. A transparent Metal pixel still occludes
/// in-window content behind it, so the labels must sit in front, not behind.
#[cfg(target_os = "macos")]
const TITLEBAR_LABEL_Z_POSITION: f64 = 2.0;

/// Add an NSVisualEffectView pinned to the top 32px (the title bar area).
#[cfg(target_os = "macos")]
unsafe fn add_titlebar_vibrancy(ns_view: *const objc2::runtime::AnyObject) {
    use objc2::MainThreadOnly;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation};
    use objc2_app_kit::{
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    };
    use objc2_foundation::{MainThreadMarker, NSRect, NSString};

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    const TITLE_BAR_HEIGHT: f64 = 32.0;

    // Use Auto Layout to pin the view to the top of the contentView. Skipping the frame
    // approach because winit's NSView uses flipped coordinates, which makes the "top
    // versus bottom" Y calculation error-prone.
    let zero_frame = NSRect::default();
    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), zero_frame);
    unsafe {
        effect.setMaterial(NSVisualEffectMaterial::Titlebar);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        // Identifier so set_titlebar_vibrancy_visible can find it.
        let identifier = NSString::from_str(TITLEBAR_VIBRANCY_IDENTIFIER);
        let _: () = msg_send![&*effect, setIdentifier: &*identifier];

        let _: () = msg_send![&*effect, setTranslatesAutoresizingMaskIntoConstraints: false];

        // Plain addSubview (no positioned:) → goes to the END of subviews → renders on
        // top of the image area vibrancy (which was added earlier).
        let effect_obj: *const AnyObject = &*effect as *const NSVisualEffectView as *const _;
        let _: () = msg_send![ns_view, addSubview: effect_obj];

        // Pin: top, leading, trailing to contentView; height = TITLE_BAR_HEIGHT.
        let make_constraint = |attr: NSLayoutAttribute,
                               parent_attr: NSLayoutAttribute,
                               constant: f64| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &effect, attr,
                NSLayoutRelation::Equal,
                Some(&*ns_view),
                parent_attr, 1.0, constant,
            )
        };
        let top = make_constraint(NSLayoutAttribute::Top, NSLayoutAttribute::Top, 0.0);
        let leading = make_constraint(NSLayoutAttribute::Leading, NSLayoutAttribute::Leading, 0.0);
        let trailing = make_constraint(
            NSLayoutAttribute::Trailing,
            NSLayoutAttribute::Trailing,
            0.0,
        );
        let height = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &effect, NSLayoutAttribute::Height,
            NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
            1.0, TITLE_BAR_HEIGHT,
        );
        // After addSubview / setActive, AppKit owns retains on the view and constraints,
        // so we drop our local Retained handles at end of scope.
        top.setActive(true);
        leading.setActive(true);
        trailing.setActive(true);
        height.setActive(true);
    }
}

#[cfg(target_os = "macos")]
objc2::define_class!(
    /// `NSTextField` label that's transparent to mouse events: `hitTest:` always returns null
    /// so clicks and drags fall through to the strip and winit's content view.
    ///
    /// Why click-through: the app forwards title-bar mouse events through winit (the content
    /// view covers the title bar, so AppKit never sees them — `App::pointer_in_title_bar` routes
    /// a title-bar double-click to `zoom_window`). A default label captures mouse events within
    /// its text bounds, which would swallow double-click-to-zoom and window drags right where
    /// the title/zoom text sits.
    #[unsafe(super(NSTextField))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwClickThroughLabel"]
    pub(crate) struct ClickThroughLabel;

    unsafe impl NSObjectProtocol for ClickThroughLabel {}

    impl ClickThroughLabel {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: objc2_foundation::NSPoint) -> *mut objc2_app_kit::NSView {
            std::ptr::null_mut()
        }
    }
);

#[cfg(target_os = "macos")]
impl ClickThroughLabel {
    /// Alloc/init a `ClickThroughLabel`. The `labelWithString:` convenience constructor would
    /// return a plain `NSTextField`, not this subclass, so we alloc/init directly.
    fn new(mtm: objc2_foundation::MainThreadMarker) -> objc2::rc::Retained<Self> {
        use objc2::msg_send;
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Add the title and zoom-readout labels as subviews of the contentView (`ns_view`), positioned
/// in the top title-bar strip. They're `ClickThroughLabel`s (mouse-transparent — see that type),
/// non-editable, non-selectable, transparent-background, colored with the appearance-aware
/// semantic colors (`labelColor` / `secondaryLabelColor`) so they auto-contrast in light and dark
/// mode with no observer code.
///
/// They sit on the contentView (siblings of the Metal layer) with a `zPosition` above it, not
/// inside the `effect` strip: the strip is behind the Metal layer and a transparent Metal pixel
/// occludes in-window content behind it, so a label inside the strip would be invisible. The view
/// hierarchy owns the retains after `addSubview`, so the local `Retained` handles drop at end of
/// scope.
#[cfg(target_os = "macos")]
unsafe fn add_titlebar_labels(
    ns_view: *const objc2::runtime::AnyObject,
    mtm: objc2_foundation::MainThreadMarker,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSColor, NSFont, NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation,
        NSLayoutRelation, NSLineBreakMode,
    };
    use objc2_foundation::NSString;

    /// Build one label: non-editable, non-selectable, bordered/bezeled off, transparent
    /// background, bold system font, with the given identifier.
    unsafe fn make_label(
        identifier: &str,
        mtm: objc2_foundation::MainThreadMarker,
    ) -> objc2::rc::Retained<ClickThroughLabel> {
        let label = ClickThroughLabel::new(mtm);
        label.setEditable(false);
        label.setSelectable(false);
        label.setBordered(false);
        label.setDrawsBackground(false);
        label.setBezeled(false);
        label.setFont(Some(&NSFont::boldSystemFontOfSize(
            TITLEBAR_LABEL_FONT_SIZE,
        )));
        unsafe {
            let id = NSString::from_str(identifier);
            let _: () = msg_send![&*label, setIdentifier: &*id];
            let _: () = msg_send![&*label, setTranslatesAutoresizingMaskIntoConstraints: false];
        }
        label
    }

    unsafe {
        let title = make_label(TITLEBAR_TITLE_IDENTIFIER, mtm);
        title.setTextColor(Some(&NSColor::labelColor()));
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);

        let zoom = make_label(TITLEBAR_ZOOM_IDENTIFIER, mtm);
        zoom.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let title_obj: *const AnyObject = &*title as *const ClickThroughLabel as *const _;
        let zoom_obj: *const AnyObject = &*zoom as *const ClickThroughLabel as *const _;
        let _: () = msg_send![ns_view, addSubview: title_obj];
        let _: () = msg_send![ns_view, addSubview: zoom_obj];

        // Composite the labels in front of the wgpu Metal layer (a sibling layer under the
        // contentView's root). Layer-back each label, then raise its `zPosition` above the
        // Metal layer's `1.0`.
        for label_obj in [title_obj, zoom_obj] {
            let _: () = msg_send![label_obj, setWantsLayer: true];
            let layer: *const AnyObject = msg_send![label_obj, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setZPosition: TITLEBAR_LABEL_Z_POSITION];
            }
        }

        // The title yields first: lower horizontal compression resistance than the zoom means
        // the title middle-truncates while the zoom stays intact when space runs out.
        title.setContentCompressionResistancePriority_forOrientation(
            249.0_f32,
            NSLayoutConstraintOrientation::Horizontal,
        );
        zoom.setContentCompressionResistancePriority_forOrientation(
            751.0_f32,
            NSLayoutConstraintOrientation::Horizontal,
        );

        // Constraints pin to the contentView. The labels sit in the top title-bar strip:
        // `centerY = contentView.top + 17` sits them in the strip, nudged 1pt below the strip's
        // vertical middle (32pt strip) to align with the traffic lights. Leading 88 clears the
        // traffic lights; trailing −12 is the zoom's right margin.
        let parent: &AnyObject = &*ns_view;

        let title_leading = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &title, NSLayoutAttribute::Leading,
            NSLayoutRelation::Equal,
            Some(parent), NSLayoutAttribute::Leading, 1.0, 88.0,
        );
        let title_center_y = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &title, NSLayoutAttribute::CenterY,
            NSLayoutRelation::Equal,
            Some(parent), NSLayoutAttribute::Top, 1.0, 17.0,
        );

        // Zoom: trailing = contentView.trailing − 12, centerY in the strip, leading ≥ title.trailing + 12.
        let zoom_trailing = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &zoom, NSLayoutAttribute::Trailing,
            NSLayoutRelation::Equal,
            Some(parent), NSLayoutAttribute::Trailing, 1.0, -12.0,
        );
        let zoom_center_y = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &zoom, NSLayoutAttribute::CenterY,
            NSLayoutRelation::Equal,
            Some(parent), NSLayoutAttribute::Top, 1.0, 17.0,
        );
        let gap = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &zoom, NSLayoutAttribute::Leading,
            NSLayoutRelation::GreaterThanOrEqual,
            Some(&*title), NSLayoutAttribute::Trailing, 1.0, 12.0,
        );

        for c in [
            &title_leading,
            &title_center_y,
            &zoom_trailing,
            &zoom_center_y,
            &gap,
        ] {
            c.setActive(true);
        }
    }
}

/// Update the native title/zoom labels in the title-bar strip. Finds both labels by their
/// identifier and sets `stringValue`. Cache-guarded: skips a label whose value is unchanged,
/// avoiding a needless Auto Layout pass on every redraw.
#[cfg(target_os = "macos")]
pub fn set_titlebar_text(window: &Window, title: &str, zoom: &str) {
    set_label_text_by_id(window, TITLEBAR_TITLE_IDENTIFIER, title);
    set_label_text_by_id(window, TITLEBAR_ZOOM_IDENTIFIER, zoom);
}

/// Find an `NSTextField` by its `identifier` (searching the contentView's subtree) and set
/// its `stringValue`, skipping the set when the value already matches. The labels are nested
/// inside the title-bar strip view, not direct children of the contentView, so the search
/// recurses through subviews.
#[cfg(target_os = "macos")]
fn set_label_text_by_id(window: &Window, identifier: &str, value: &str) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let target_id = NSString::from_str(identifier);
        let label = find_subview_by_id(ns_view, &target_id);
        if label.is_null() {
            return;
        }
        let new_value = NSString::from_str(value);
        let current: *const NSString = msg_send![label, stringValue];
        if !current.is_null() {
            let same: bool = msg_send![&*new_value, isEqualToString: current];
            if same {
                return;
            }
        }
        let _: () = msg_send![label, setStringValue: &*new_value];
    }
}

/// Depth-first search of `view`'s subtree for a subview whose `identifier` matches `target_id`.
/// Returns null if none is found.
#[cfg(target_os = "macos")]
unsafe fn find_subview_by_id(
    view: *const objc2::runtime::AnyObject,
    target_id: &objc2_foundation::NSString,
) -> *const objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    unsafe {
        let subviews: *const AnyObject = msg_send![view, subviews];
        if subviews.is_null() {
            return std::ptr::null();
        }
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let subview: *const AnyObject = msg_send![subviews, objectAtIndex: i];
            let id: *const NSString = msg_send![subview, identifier];
            if !id.is_null() {
                let matches: bool = msg_send![target_id, isEqualToString: id];
                if matches {
                    return subview;
                }
            }
            let found = find_subview_by_id(subview, target_id);
            if !found.is_null() {
                return found;
            }
        }
        std::ptr::null()
    }
}

/// Show or hide the title bar vibrancy view and its title/zoom labels together. The labels are
/// contentView subviews (not children of the strip — see `add_titlebar_labels`), so they need
/// toggling explicitly alongside the strip; this keeps them in lockstep for the title-bar-off and
/// fullscreen cases.
#[cfg(target_os = "macos")]
pub fn set_titlebar_vibrancy_visible(window: &Window, visible: bool) {
    set_subview_hidden_by_id(window, TITLEBAR_VIBRANCY_IDENTIFIER, !visible);
    set_subview_hidden_by_id(window, TITLEBAR_TITLE_IDENTIFIER, !visible);
    set_subview_hidden_by_id(window, TITLEBAR_ZOOM_IDENTIFIER, !visible);
}

/// Switch the window's appearance for fullscreen vs windowed.
/// In fullscreen: hide the dark vibrancy and use a solid black background.
/// In windowed: show the vibrancy (which has a translucent dark blur).
#[cfg(target_os = "macos")]
pub fn set_fullscreen_appearance(window: &Window, fullscreen: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    set_subview_hidden_by_id(window, IMAGE_AREA_VIBRANCY_IDENTIFIER, fullscreen);

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };
    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let bg: *const AnyObject = if fullscreen {
            msg_send![objc2::class!(NSColor), blackColor]
        } else {
            msg_send![objc2::class!(NSColor), clearColor]
        };
        let _: () = msg_send![ns_window, setBackgroundColor: bg];
    }
}

/// Find a subview by its `identifier` and set its `hidden` flag.
#[cfg(target_os = "macos")]
fn set_subview_hidden_by_id(window: &Window, identifier: &str, hidden: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let subviews: *const AnyObject = msg_send![ns_view, subviews];
        if subviews.is_null() {
            return;
        }
        let count: usize = msg_send![subviews, count];
        let target_id = NSString::from_str(identifier);
        for i in 0..count {
            let subview: *const AnyObject = msg_send![subviews, objectAtIndex: i];
            let id: *const NSString = msg_send![subview, identifier];
            if !id.is_null() {
                let matches: bool = msg_send![&*target_id, isEqualToString: id];
                if matches {
                    let _: () = msg_send![subview, setHidden: hidden];
                    return;
                }
            }
        }
    }
}

/// Force the wgpu CAMetalLayer to render on top of the NSVisualEffectView's layer
/// (added by `add_titlebar_vibrancy`) using `zPosition`. Both layers are siblings under
/// the contentView's root layer; setting wgpu's zPosition higher pushes it in front of
/// the vibrancy in the compositing order.
#[cfg(target_os = "macos")]
pub fn push_metal_layer_above_vibrancy(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let root_layer: *const AnyObject = msg_send![ns_view, layer];
        if root_layer.is_null() {
            return;
        }
        let metal_layer = find_sublayer_responding_to(root_layer, objc2::sel!(setColorspace:));
        if metal_layer.is_null() {
            log::warn!("No CAMetalLayer found, can't set zPosition");
            return;
        }

        // Force wgpu in front of the NSVisualEffectView's layer (default zPosition = 0).
        // setZPosition: takes a CGFloat (f64 on macOS).
        let _: () = msg_send![metal_layer, setZPosition: 1.0_f64];
        log::debug!("Set CAMetalLayer.zPosition = 1.0 (wgpu renders on top of vibrancy)");
    }
}

/// Clip the wgpu CAMetalLayer to a rounded rect inset from the window edge, so the
/// full-window Liquid Glass shows through as a uniform frame around the image. The inner
/// corners are concentric with the window (`IMAGE_CORNER_RADIUS`), and continuous-curved
/// to match the system squircle. No-op on pre-26 macOS (no glass frame there).
///
/// Recreated on every window resize because a CALayer mask does not autoresize with its
/// host layer.
#[cfg(target_os = "macos")]
pub fn apply_glass_frame_mask(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    if !liquid_glass_available() {
        return;
    }

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let root_layer: *const AnyObject = msg_send![ns_view, layer];
        if root_layer.is_null() {
            return;
        }
        let metal_layer = find_sublayer_responding_to(root_layer, objc2::sel!(setColorspace:));
        if metal_layer.is_null() {
            return;
        }

        let bounds: NSRect = msg_send![metal_layer, bounds];
        let inset = IMAGE_FRAME_INSET;
        let w = (bounds.size.width - 2.0 * inset).max(0.0);
        let h = (bounds.size.height - 2.0 * inset).max(0.0);
        let frame = NSRect::new(
            NSPoint::new(bounds.origin.x + inset, bounds.origin.y + inset),
            NSSize::new(w, h),
        );

        let mask: *const AnyObject = msg_send![objc2::class!(CALayer), layer];
        if mask.is_null() {
            return;
        }
        let _: () = msg_send![mask, setFrame: frame];
        let _: () = msg_send![mask, setCornerRadius: IMAGE_CORNER_RADIUS];
        // Match the system squircle. kCACornerCurveContinuous == @"continuous".
        let continuous = NSString::from_str("continuous");
        let _: () = msg_send![mask, setCornerCurve: &*continuous];
        let _: () = msg_send![mask, setMasksToBounds: true];
        // The mask's alpha clips the host layer; an opaque fill makes the rounded rect the
        // visible region.
        unsafe extern "C" {
            fn CGColorCreateGenericGray(gray: f64, alpha: f64) -> *const core::ffi::c_void;
            fn CFRelease(cf: *const core::ffi::c_void);
        }
        let cg_black = CGColorCreateGenericGray(0.0, 1.0);
        // Raw objc_msgSend to bypass objc2's encoding check: our CGColorRef is
        // `*const c_void` (encodes as `^v`) but ObjC expects `^{CGColor=}`. Same trick as
        // `display_profile::set_colorspace_on_layer`.
        let sel = objc2::sel!(setBackgroundColor:);
        let send: unsafe extern "C" fn(
            *const AnyObject,
            objc2::runtime::Sel,
            *const core::ffi::c_void,
        ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
        send(mask, sel, cg_black);
        CFRelease(cg_black);
        // Crisp rounded corners at Retina: match the host layer's contents scale.
        let scale: f64 = msg_send![metal_layer, contentsScale];
        let _: () = msg_send![mask, setContentsScale: scale];

        let _: () = msg_send![metal_layer, setMask: mask];
    }
}

#[cfg(target_os = "macos")]
unsafe fn find_sublayer_responding_to(
    layer: *const objc2::runtime::AnyObject,
    sel: objc2::runtime::Sel,
) -> *const objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    unsafe {
        // Check the layer itself first (in case it IS the Metal layer).
        let responds: bool = msg_send![layer, respondsToSelector: sel];
        if responds {
            return layer;
        }
        let sublayers: *const AnyObject = msg_send![layer, sublayers];
        if sublayers.is_null() {
            return std::ptr::null();
        }
        let count: usize = msg_send![sublayers, count];
        for i in 0..count {
            let sublayer: *const AnyObject = msg_send![sublayers, objectAtIndex: i];
            let sub_responds: bool = msg_send![sublayer, respondsToSelector: sel];
            if sub_responds {
                return sublayer;
            }
        }
        std::ptr::null()
    }
}

/// Round the window's frame view (the content view's superview, an `NSThemeFrame`) to the
/// same radius and continuous curve as the glass. Without this, macOS strokes the window's
/// own corner at the system default radius on the key window, which peeks out past the
/// rounder glass corner as a thin artifact.
#[cfg(target_os = "macos")]
unsafe fn round_window_frame_to_glass(content_view: *const objc2::runtime::AnyObject) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    unsafe {
        let frame_view: *const AnyObject = msg_send![content_view, superview];
        if frame_view.is_null() {
            return;
        }
        // Ensure the frame view is layer-backed before reaching for its layer.
        let _: () = msg_send![frame_view, setWantsLayer: true];
        let layer: *const AnyObject = msg_send![frame_view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = msg_send![layer, setCornerRadius: WINDOW_CORNER_RADIUS];
        let continuous = NSString::from_str("continuous");
        let _: () = msg_send![layer, setCornerCurve: &*continuous];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

/// The main viewer window, captured so the swizzled setters know which window's traffic
/// lights to nudge. Stored as a raw pointer compared by identity — never dereferenced.
#[cfg(target_os = "macos")]
static MAIN_WINDOW: std::sync::atomic::AtomicPtr<objc2::runtime::AnyObject> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// The original `NSView` `setFrameOrigin:` / `setFrame:` implementations, saved when we
/// swizzle them so the replacements can chain to the real behavior.
#[cfg(target_os = "macos")]
static ORIGINAL_SET_FRAME_ORIGIN: std::sync::OnceLock<objc2::runtime::Imp> =
    std::sync::OnceLock::new();
#[cfg(target_os = "macos")]
static ORIGINAL_SET_FRAME: std::sync::OnceLock<objc2::runtime::Imp> = std::sync::OnceLock::new();

// Guards against applying the offset twice when `setFrame:` chains into `setFrameOrigin:`
// internally: the outer setter adds the offset, the inner one sees the guard and passes
// through unchanged.
#[cfg(target_os = "macos")]
thread_local! {
    static NUDGING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True when `view` is one of the main window's three traffic-light buttons — the only views
/// whose frame we want to nudge. Checked on every `setFrameOrigin:` / `setFrame:`, so it must
/// be cheap and exact: we gate on the stored main window, then compare against its standard
/// buttons. Everything else (other windows, other views) passes through untouched.
#[cfg(target_os = "macos")]
unsafe fn is_main_traffic_light(view: *const objc2::runtime::AnyObject) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use std::sync::atomic::Ordering;

    let win: *const AnyObject = unsafe { msg_send![view, window] };
    if win.is_null() || !std::ptr::eq(win, MAIN_WINDOW.load(Ordering::Acquire)) {
        return false;
    }
    if liquid_glass_available() {
        // macOS 26 (Liquid Glass) renders the traffic lights as SwiftUI views hosted in
        // `_NSCoreHostingView<ThemeWidgetView>` — those are what AppKit repositions on every
        // relayout, not the legacy `_NSThemeCloseWidget` that `standardWindowButton:` returns.
        // Match by class name (it embeds the Swift type name "ThemeWidgetView").
        let cls: *const objc2::runtime::AnyClass = unsafe { msg_send![view, class] };
        let name = unsafe { (*cls).name() };
        name.to_bytes().windows(15).any(|w| w == b"ThemeWidgetView")
    } else {
        // Older macOS: nudge the three standard window buttons directly.
        // NSWindowButton: close = 0, miniaturize = 1, zoom = 2.
        (0u64..3).any(|kind| {
            let button: *const AnyObject = unsafe { msg_send![win, standardWindowButton: kind] };
            std::ptr::eq(button, view)
        })
    }
}

/// The (dx, dy) to add to a traffic-light view's frame origin. X is always rightward.
/// `TRAFFIC_LIGHT_Y_OFFSET` expresses the vertical nudge as "down in bottom-left coordinates"
/// (the legacy window-button path), so it's negated when the view's superview is flipped
/// (top-left origin, y growing downward) — as the Liquid Glass SwiftUI titlebar's hosting
/// views are — to keep the visual direction (downward) the same in both coordinate systems.
#[cfg(target_os = "macos")]
unsafe fn traffic_light_delta(view: *const objc2::runtime::AnyObject) -> (f64, f64) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let sup: *const AnyObject = unsafe { msg_send![view, superview] };
    let flipped: bool = !sup.is_null() && unsafe { msg_send![sup, isFlipped] };
    let dy = if flipped {
        -TRAFFIC_LIGHT_Y_OFFSET
    } else {
        TRAFFIC_LIGHT_Y_OFFSET
    };
    (TRAFFIC_LIGHT_X_OFFSET, dy)
}

/// Keep the traffic lights nudged off the rounded corner across every relayout, flicker-free.
///
/// macOS repositions the standard window buttons to their default spot on every relayout
/// (resize, fullscreen, title change). Re-nudging them *after the fact* (on the `Resized`
/// event or a frame-change notification) loses a race: during a live resize AppKit's layout
/// runs last and overwrites the offset, so the buttons sit at the default until the next
/// redraw. Instead we swizzle `NSView`'s `setFrameOrigin:` / `setFrame:` and add the offset
/// *as AppKit positions the buttons* — so the offset is baked into the value AppKit itself
/// commits, and nothing runs after to undo it. Gated precisely to the main window's three
/// traffic lights (see `is_main_traffic_light`), so no other view or window is affected.
///
/// Call once per process, after the main window exists. The swizzle is installed once; later
/// calls just refresh the stored main-window pointer.
#[cfg(target_os = "macos")]
pub fn register_traffic_light_keeper(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use objc2_foundation::{NSPoint, NSRect};
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        MAIN_WINDOW.store(ns_window as *mut AnyObject, Ordering::Release);

        let close_button: *const AnyObject = msg_send![ns_window, standardWindowButton: 0u64];
        if close_button.is_null() {
            return;
        }

        static SWIZZLED: OnceLock<()> = OnceLock::new();
        SWIZZLED.get_or_init(|| {
            // `setFrameOrigin:` / `setFrame:` resolve to `NSView`'s implementation (the
            // buttons don't override them), so swizzling the methods reachable from the
            // button's class swizzles them for all views — the gate keeps the effect to our
            // three buttons.
            let btn_class: &AnyClass = {
                let cls: *const AnyClass = msg_send![close_button, class];
                &*cls
            };

            unsafe extern "C-unwind" fn set_frame_origin(
                this: *mut AnyObject,
                cmd: Sel,
                mut origin: NSPoint,
            ) {
                unsafe {
                    let adjust = !NUDGING.with(|g| g.get()) && is_main_traffic_light(this);
                    if adjust {
                        NUDGING.with(|g| g.set(true));
                        let (dx, dy) = traffic_light_delta(this);
                        origin.x += dx;
                        origin.y += dy;
                    }
                    let orig: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, NSPoint) =
                        std::mem::transmute(*ORIGINAL_SET_FRAME_ORIGIN.get().unwrap());
                    orig(this, cmd, origin);
                    if adjust {
                        NUDGING.with(|g| g.set(false));
                    }
                }
            }

            unsafe extern "C-unwind" fn set_frame(
                this: *mut AnyObject,
                cmd: Sel,
                mut frame: NSRect,
            ) {
                unsafe {
                    let adjust = !NUDGING.with(|g| g.get()) && is_main_traffic_light(this);
                    if adjust {
                        NUDGING.with(|g| g.set(true));
                        let (dx, dy) = traffic_light_delta(this);
                        frame.origin.x += dx;
                        frame.origin.y += dy;
                    }
                    let orig: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, NSRect) =
                        std::mem::transmute(*ORIGINAL_SET_FRAME.get().unwrap());
                    orig(this, cmd, frame);
                    if adjust {
                        NUDGING.with(|g| g.set(false));
                    }
                }
            }

            if let Some(method) = btn_class.instance_method(sel!(setFrameOrigin:)) {
                let new: Imp = std::mem::transmute(set_frame_origin as *const std::ffi::c_void);
                let old = method.set_implementation(new);
                let _ = ORIGINAL_SET_FRAME_ORIGIN.set(old);
            }
            if let Some(method) = btn_class.instance_method(sel!(setFrame:)) {
                let new: Imp = std::mem::transmute(set_frame as *const std::ffi::c_void);
                let old = method.set_implementation(new);
                let _ = ORIGINAL_SET_FRAME.set(old);
            }
            log::debug!("Swizzled NSView frame setters for the traffic-light keeper");
        });
    }
}

/// Toggle the window's standard "zoom" — the green-button / title-bar-double-click behavior
/// that fills the screen (or restores the previous size). We intercept the title-bar
/// double-click ourselves, because the transparent full-size-content-view window puts our
/// `winit` content view over the whole window (title bar included), so AppKit never sees the
/// double-click. Forwarding it to `zoom:` makes the title bar behave like any native macOS
/// app's.
#[cfg(target_os = "macos")]
pub fn zoom_window(window: &Window) {
    use objc2::msg_send;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
        let ns_window: *const objc2::runtime::AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let nil: *const objc2::runtime::AnyObject = std::ptr::null();
        let _: () = msg_send![ns_window, zoom: nil];
    }
}

/// Set the native window title. Named "keeping buttons" because changing the title makes
/// macOS relayout the standard window buttons; the traffic-light offset survives that because
/// `register_traffic_light_keeper` swizzles the frame setters (the buttons stay nudged without
/// any work here). Kept as a single call site for the title so that contract stays visible.
pub fn set_title_keeping_buttons(window: &Window, title: &str) {
    window.set_title(title);
}

/// Build the window title from a file path (filename only, not the full path).
pub fn window_title_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Prvw")
        .to_string()
}

/// Build a window title with position info: `3 / 60 – photo.jpg`
pub fn window_title_with_position(path: &Path, current: usize, total: usize) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Prvw");
    if total > 1 {
        format!("{} / {} \u{2013} {name}", current + 1, total)
    } else {
        name.to_string()
    }
}

/// Build a loading title: `3 / 60 – Loading...`
pub fn window_title_loading(current: usize, total: usize) -> String {
    if total > 1 {
        format!("{} / {} \u{2013} Loading...", current + 1, total)
    } else {
        "Loading...".to_string()
    }
}

/// Toggle fullscreen on the window.
pub fn toggle_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        log::debug!("Fullscreen: borderless -> windowed");
        window.set_fullscreen(None);
    } else {
        log::debug!("Fullscreen: windowed -> borderless");
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    }
}

/// Set fullscreen on or off directly.
pub fn set_fullscreen(window: &Window, on: bool) {
    if on {
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    } else {
        window.set_fullscreen(None);
    }
}

/// Check if the window is currently fullscreen.
pub fn is_fullscreen(window: &Window) -> bool {
    window.fullscreen().is_some()
}

/// Monitor work area in logical pixels.
pub struct MonitorBounds {
    pub x: Logical<f64>,
    pub y: Logical<f64>,
    pub width: Logical<f64>,
    pub height: Logical<f64>,
}

impl MonitorBounds {
    /// Get the current monitor's bounds in logical pixels. Returns `None` if no monitor.
    pub fn from_window(window: &Window) -> Option<Self> {
        let scale = window.scale_factor();
        window.current_monitor().map(|m| {
            let (x, y) = from_logical_pos(m.position().to_logical::<f64>(scale));
            let (width, height) = from_logical_size(m.size().to_logical::<f64>(scale));
            Self {
                x,
                y,
                width,
                height,
            }
        })
    }

    /// Maximum window size (90% of monitor in each dimension).
    pub fn max_window_size(&self) -> (Logical<f64>, Logical<f64>) {
        (
            self.width * MAX_SCREEN_FRACTION,
            self.height * MAX_SCREEN_FRACTION,
        )
    }
}

/// Clamp a new window position so it doesn't go MORE off-screen than the old position.
///
/// - `target`: desired (x, y) for the new position
/// - `new_size`: (width, height) of the new outer frame
/// - `old_pos`: (x, y) of the current outer frame
/// - `old_size`: (width, height) of the current outer frame
///
/// Returns the clamped (x, y).
pub fn clamp_to_screen(
    target: (Logical<f64>, Logical<f64>),
    new_size: (Logical<f64>, Logical<f64>),
    old_pos: (Logical<f64>, Logical<f64>),
    old_size: (Logical<f64>, Logical<f64>),
    bounds: &MonitorBounds,
) -> (Logical<f64>, Logical<f64>) {
    // Unwrap to raw f64 for complex clamping arithmetic, then re-wrap.
    let (bx, by, bw, bh) = (bounds.x.0, bounds.y.0, bounds.width.0, bounds.height.0);
    let (ox, oy) = (old_pos.0.0, old_pos.1.0);
    let (ow, oh) = (old_size.0.0, old_size.1.0);
    let (nw, nh) = (new_size.0.0, new_size.1.0);
    let (tx, ty) = (target.0.0, target.1.0);

    let off_left = (bx - ox).max(0.0);
    let off_right = ((ox + ow) - (bx + bw)).max(0.0);
    let off_top = (by - oy).max(0.0);
    let off_bottom = ((oy + oh) - (by + bh)).max(0.0);

    let min_x = bx - off_left;
    let max_x = bx + bw + off_right - nw;
    let min_y = by - off_top;
    let max_y = by + bh + off_bottom - nh;

    let fx = if min_x <= max_x {
        tx.clamp(min_x, max_x)
    } else {
        (min_x + max_x) / 2.0
    };
    let fy = if min_y <= max_y {
        ty.clamp(min_y, max_y)
    } else {
        (min_y + max_y) / 2.0
    };
    (Logical(fx), Logical(fy))
}

/// Resize the window to fit the given image dimensions, then center it on screen.
///
/// Returns the physical size the window was set to, so the caller can update the renderer
/// immediately (without waiting for the async `Resized` event).
///
/// The window size is the image size clamped to:
/// - minimum 200px in each dimension
/// - maximum 90% of the monitor's work area in each dimension
///
/// Returns `None` if the window is fullscreen (no resize performed).
pub fn resize_to_fit_image(
    window: &Window,
    image_width: u32,
    image_height: u32,
    content_offset_y: Logical<f32>,
) -> Option<PhysicalSize<u32>> {
    if is_fullscreen(window) {
        return None;
    }

    let scale_factor = window.scale_factor();
    let offset = content_offset_y.0 as f64;

    // Get the monitor's work area (excluding dock/menu bar)
    let (max_w, max_h) = MonitorBounds::from_window(window)
        .map(|b| {
            let (w, h) = b.max_window_size();
            (w.0, h.0)
        })
        .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));

    // Apply the minimum floor first, then scale down proportionally to fit within the
    // screen cap. Scaling must happen on the un-clamped dimensions to preserve aspect ratio —
    // clamping first would make both axes fit independently, losing the ratio.
    // The offset is added after scaling — it's a fixed overhead, not part of the image.
    let img_w = (image_width as f64).max(MIN_WINDOW_DIM);
    let img_h = (image_height as f64).max(MIN_WINDOW_DIM);
    let scale = (max_w / img_w).min((max_h - offset) / img_h).min(1.0);
    let final_w = (img_w * scale).max(MIN_WINDOW_DIM);
    let final_h = (img_h * scale + offset).max(MIN_WINDOW_DIM);

    let new_size = to_logical_size(Logical(final_w), Logical(final_h));
    let (pw, ph) = from_physical_size(new_size.to_physical::<u32>(scale_factor));

    let _ = window.request_inner_size(new_size);

    log::debug!(
        "Auto-fit window: {}x{} image -> {}x{} logical ({}x{} physical)",
        image_width,
        image_height,
        final_w as u32,
        final_h as u32,
        pw.0,
        ph.0
    );

    // Center the window on the current monitor
    if let Some(bounds) = MonitorBounds::from_window(window) {
        let cx = Logical(bounds.x.0 + (bounds.width.0 - final_w) / 2.0);
        let cy = Logical(bounds.y.0 + (bounds.height.0 - final_h) / 2.0);
        window.set_outer_position(to_logical_pos(cx, cy));
    }

    Some(PhysicalSize::new(pw.0, ph.0))
}
