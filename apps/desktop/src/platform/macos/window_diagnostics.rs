//! Debug-only dump of the main window's AppKit view/layer tree.
//!
//! Exists to answer "where does AppKit think the traffic lights are, and where do they
//! draw?" — the two diverge when a nudge moves a drawing view but not the control that
//! hit-tests. Reachable from the QA server (`GET /window-diagnostics`) so a run can be
//! inspected live, before and after a window zoom.
//!
//! Every frame is reported in WINDOW coordinates (bottom-left origin) via
//! `convertRect:toView:nil`, so views living in flipped and unflipped superviews are
//! directly comparable.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;
use std::fmt::Write as _;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// Depth limit for the view walk. The theme frame's traffic lights sit 2–4 levels down;
/// deeper is our own content and just noise here.
const MAX_DEPTH: usize = 5;

/// Dump the window's frame-view tree, the standard window buttons, and the layer geometry
/// that shapes the window's rounded corner. Main thread only (it talks to AppKit).
pub fn dump(window: &Window) -> String {
    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return "(no AppKit window handle)".to_string();
    };
    let mut out = String::new();
    unsafe {
        let content_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![content_view, window];
        if ns_window.is_null() {
            return "(no NSWindow)".to_string();
        }

        let frame: NSRect = msg_send![ns_window, frame];
        let style_mask: u64 = msg_send![ns_window, styleMask];
        let is_zoomed: bool = msg_send![ns_window, isZoomed];
        let is_key: bool = msg_send![ns_window, isKeyWindow];
        let collection_behavior: u64 = msg_send![ns_window, collectionBehavior];
        let _ = writeln!(
            out,
            "NSWindow frame=({:.1},{:.1} {:.1}x{:.1}) styleMask=0x{style_mask:x} collectionBehavior=0x{collection_behavior:x} zoomed={is_zoomed} key={is_key}",
            frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
        );

        // The three standard window buttons: what AppKit hit-tests.
        for (kind, name) in [(0u64, "close"), (1, "miniaturize"), (2, "zoom")] {
            let button: *const AnyObject = msg_send![ns_window, standardWindowButton: kind];
            if button.is_null() {
                let _ = writeln!(out, "standardWindowButton[{name}] = nil");
                continue;
            }
            let posts: bool = msg_send![button, postsFrameChangedNotifications];
            let _ = writeln!(
                out,
                "standardWindowButton[{name}] posts={posts} {}",
                describe_view(button, content_view)
            );
        }

        let frame_view: *const AnyObject = msg_send![content_view, superview];
        if !frame_view.is_null() {
            let _ = writeln!(out, "\n-- frame view tree --");
            walk(frame_view, content_view, 0, &mut out);
            let _ = writeln!(out, "\n-- frame view layer --");
            let layer: *const AnyObject = msg_send![frame_view, layer];
            let _ = writeln!(out, "{}", describe_layer(layer));
        }

        let _ = writeln!(out, "\n-- content view layer tree --");
        let root: *const AnyObject = msg_send![content_view, layer];
        let _ = writeln!(out, "root {}", describe_layer(root));
        if !root.is_null() {
            let sublayers: *const AnyObject = msg_send![root, sublayers];
            if !sublayers.is_null() {
                let count: usize = msg_send![sublayers, count];
                for i in 0..count {
                    let sub: *const AnyObject = msg_send![sublayers, objectAtIndex: i];
                    let _ = writeln!(out, "  [{i}] {}", describe_layer(sub));
                    let mask: *const AnyObject = msg_send![sub, mask];
                    if !mask.is_null() {
                        let _ = writeln!(out, "      mask {}", describe_layer(mask));
                    }
                }
            }
        }
    }
    out
}

/// Send `performClick:` to the green traffic light, so a zoom runs through the button's own
/// action instead of a direct `zoom:` on the window. Lets a QA run exercise the path a real
/// click takes without synthesizing OS mouse input.
pub fn click_zoom_button(window: &Window) {
    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    unsafe {
        let content_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![content_view, window];
        if ns_window.is_null() {
            return;
        }
        let button: *const AnyObject = msg_send![ns_window, standardWindowButton: 2u64];
        if button.is_null() {
            return;
        }
        let nil: *const AnyObject = std::ptr::null();
        let _: () = msg_send![button, performClick: nil];
    }
}

/// Recursively print a view subtree. The content view's own subtree is skipped (it's our
/// wgpu/browse content, not window chrome).
unsafe fn walk(
    view: *const AnyObject,
    content_view: *const AnyObject,
    depth: usize,
    out: &mut String,
) {
    unsafe {
        let indent = "  ".repeat(depth);
        let _ = writeln!(out, "{indent}{}", describe_view(view, content_view));
        if std::ptr::eq(view, content_view) || depth >= MAX_DEPTH {
            return;
        }
        let subviews: *const AnyObject = msg_send![view, subviews];
        if subviews.is_null() {
            return;
        }
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let sub: *const AnyObject = msg_send![subviews, objectAtIndex: i];
            walk(sub, content_view, depth + 1, out);
        }
    }
}

/// One line describing a view: class, frame in its own superview's coordinates, the same
/// frame converted to window coordinates, flipped-ness, and hidden state.
unsafe fn describe_view(view: *const AnyObject, content_view: *const AnyObject) -> String {
    unsafe {
        let cls: *const objc2::runtime::AnyClass = msg_send![view, class];
        let name = (*cls).name().to_string_lossy().to_string();
        let frame: NSRect = msg_send![view, frame];
        let bounds: NSRect = msg_send![view, bounds];
        let nil_view: *const AnyObject = std::ptr::null();
        // `convertRect:toView:nil` gives window coordinates regardless of flipped-ness.
        let in_window: NSRect = msg_send![view, convertRect: bounds, toView: nil_view];
        let hidden: bool = msg_send![view, isHidden];
        let flipped: bool = msg_send![view, isFlipped];
        let sup: *const AnyObject = msg_send![view, superview];
        let sup_flipped: bool = !sup.is_null() && msg_send![sup, isFlipped];
        let marker = if std::ptr::eq(view, content_view) {
            "  <-- winit content view (subtree skipped)"
        } else {
            ""
        };
        format!(
            "{name}@{view:p} frame=({:.1},{:.1} {:.1}x{:.1}) inWindow=({:.1},{:.1} {:.1}x{:.1}) flipped={flipped} supFlipped={sup_flipped} hidden={hidden}{marker}",
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
            in_window.origin.x,
            in_window.origin.y,
            in_window.size.width,
            in_window.size.height,
        )
    }
}

/// One line describing a layer: class, frame, corner radius/curve, masking, zPosition.
unsafe fn describe_layer(layer: *const AnyObject) -> String {
    unsafe {
        if layer.is_null() {
            return "(nil layer)".to_string();
        }
        let cls: *const objc2::runtime::AnyClass = msg_send![layer, class];
        let name = (*cls).name().to_string_lossy().to_string();
        let frame: NSRect = msg_send![layer, frame];
        let radius: f64 = msg_send![layer, cornerRadius];
        let masks: bool = msg_send![layer, masksToBounds];
        let z: f64 = msg_send![layer, zPosition];
        let hidden: bool = msg_send![layer, isHidden];
        let curve: *const objc2_foundation::NSString = msg_send![layer, cornerCurve];
        let curve = if curve.is_null() {
            "nil".to_string()
        } else {
            (*curve).to_string()
        };
        let corner_mask: u64 = msg_send![layer, maskedCorners];
        format!(
            "{name} frame=({:.1},{:.1} {:.1}x{:.1}) cornerRadius={radius:.1} curve={curve} maskedCorners=0x{corner_mask:x} masksToBounds={masks} z={z} hidden={hidden}",
            frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
        )
    }
}
