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
//! - **Traffic lights.** Nudged off the rounded corner; re-applied against a stored baseline
//!   (`offset_traffic_lights`) because macOS resets them on every titlebar relayout. The first
//!   paint retries the nudge until it sticks (the buttons aren't laid out at window-setup time).
//! - **Title-bar double-click.** Forwarded to the native window `zoom:` (`zoom_window`) so the
//!   title bar fills/restores the screen like any macOS app. Our content view covers the title
//!   bar, so AppKit never sees the click — `app.rs` routes title-bar double-clicks here.
//!
//! ## Gotchas
//!
//! - **`request_inner_size` is async on macOS.** After calling it, `inner_size()` still
//!   returns the OLD value. That's why `resize_to_fit_image` returns the computed size.
//! - **Traffic lights aren't laid out at window-setup time.** macOS gives the standard window
//!   buttons their real frames only after the window is first displayed, so the offset applied
//!   during `configure_macos_window` (and the initial title set) bails out on zero-size frames.
//!   The first `RedrawRequested` retries via `reposition_traffic_lights` until it succeeds.
//! - **Fullscreen appearance hand-off.** Toggling fullscreen triggers a `Resized` event
//!   which calls `set_fullscreen_appearance` to swap the background (vibrancy → solid
//!   black in fullscreen).

use crate::pixels::{
    Logical, from_logical_pos, from_logical_size, from_physical_size, to_logical_pos,
    to_logical_size,
};
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

        // Round the window's own frame view to match the glass, so the system's
        // default-radius corner stroke (drawn on the key window) doesn't peek out past
        // the rounder glass corners.
        if liquid_glass_available() {
            round_window_frame_to_glass(ns_view);
        }
        // Nudge the traffic lights inward so they sit comfortably off the rounded edge.
        offset_traffic_lights(ns_window as *const NSWindow as *const objc2::runtime::AnyObject);

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
/// don't crowd the rounded window corner: 6 pt inward (right) and 2 pt down. The frame view
/// uses standard AppKit coordinates (origin bottom-left), so "down" is a negative y delta.
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

/// Show or hide the title bar vibrancy view.
#[cfg(target_os = "macos")]
pub fn set_titlebar_vibrancy_visible(window: &Window, visible: bool) {
    set_subview_hidden_by_id(window, TITLEBAR_VIBRANCY_IDENTIFIER, !visible);
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

/// The traffic lights' default origins, captured the first time we offset them. macOS
/// re-lays the buttons out to these defaults whenever the window relayouts, so we re-apply
/// the offset against this stored baseline (rather than the current frame) to avoid the
/// offset compounding every time.
#[cfg(target_os = "macos")]
static ORIGINAL_TRAFFIC_LIGHT_ORIGIN: std::sync::OnceLock<[(f64, f64); 3]> =
    std::sync::OnceLock::new();

/// Shift the three traffic lights from their default positions by the traffic-light
/// offsets. Idempotent: always sets `default + offset`, so calling it repeatedly (on every
/// redraw) keeps them put instead of creeping further each time.
///
/// Returns `true` once the offset is actually applied. Returns `false` while the buttons
/// aren't laid out yet (frame width 0) — at startup the buttons get their real frames only
/// after the window is first displayed, so the first calls (during window setup and the
/// initial title set) bail out here. The caller retries on the first paint (see the
/// `traffic_lights_positioned` flag in `app.rs`) to nudge them in time for the first frame.
#[cfg(target_os = "macos")]
unsafe fn offset_traffic_lights(ns_window: *const objc2::runtime::AnyObject) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSPoint, NSRect};

    // NSWindowButton: close = 0, miniaturize = 1, zoom = 2.
    let mut buttons = [std::ptr::null::<AnyObject>(); 3];
    let mut frames = [NSRect::default(); 3];
    for kind in 0usize..3 {
        unsafe {
            let button: *const AnyObject = msg_send![ns_window, standardWindowButton: kind as u64];
            if button.is_null() {
                return false;
            }
            buttons[kind] = button;
            frames[kind] = msg_send![button, frame];
        }
    }
    // Skip until the buttons are actually laid out — capturing a zero-size frame as the
    // baseline would place them wrong forever.
    if frames[0].size.width <= 0.0 {
        return false;
    }
    // Capture the defaults on first sight; the buttons are at their natural spot here. If
    // they're already offset (a later call after our own move), the stored baseline keeps
    // us from compounding.
    let base = ORIGINAL_TRAFFIC_LIGHT_ORIGIN.get_or_init(|| {
        [
            (frames[0].origin.x, frames[0].origin.y),
            (frames[1].origin.x, frames[1].origin.y),
            (frames[2].origin.x, frames[2].origin.y),
        ]
    });
    for kind in 0usize..3 {
        unsafe {
            let origin = NSPoint::new(
                base[kind].0 + TRAFFIC_LIGHT_X_OFFSET,
                base[kind].1 + TRAFFIC_LIGHT_Y_OFFSET,
            );
            let _: () = msg_send![buttons[kind], setFrameOrigin: origin];
        }
    }
    true
}

/// Re-apply the traffic-light offset (see `offset_traffic_lights`). Called on window resize
/// because macOS resets the buttons to their default positions when it relayouts, and on the
/// first paint to nudge them once the buttons are laid out. Returns `true` once the offset is
/// actually applied (the buttons are laid out).
#[cfg(target_os = "macos")]
pub fn reposition_traffic_lights(window: &Window) -> bool {
    use objc2::msg_send;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return false;
    };
    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
        let ns_window: *const objc2::runtime::AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return false;
        }
        offset_traffic_lights(ns_window)
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

/// Set the native window title, then immediately re-apply the traffic-light offset. macOS
/// re-lays the standard buttons out to their defaults synchronously when the title changes,
/// so re-nudging them in the same call (rather than waiting for the next redraw) keeps them
/// from visibly flashing to the corner on every navigation.
pub fn set_title_keeping_buttons(window: &Window, title: &str) {
    window.set_title(title);
    #[cfg(target_os = "macos")]
    reposition_traffic_lights(window);
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
