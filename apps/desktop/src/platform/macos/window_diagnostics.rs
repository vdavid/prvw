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
use objc2_foundation::{NSPoint, NSRect};
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

/// One step of `POST /traffic-light-trace`, which measures whether the traffic lights ever
/// reach the screen at AppKit's default spot instead of our nudged one during a resize.
#[derive(Debug, Clone, Copy)]
pub enum TrafficLightTraceStep {
    /// Record where the buttons sit now (settled, so nudged) as the reference, and optionally
    /// put the window into live resize the way a drag on its edge does.
    Begin { live: bool },
    /// Grow the window by this many points, then flush Core Animation and report the button
    /// origins that flush carried to the window server.
    Resize { dw: f64, dh: f64 },
    /// Leave live resize (if `Begin` entered it) and report whether each button's visible spot
    /// still hit-tests to that button.
    End,
}

thread_local! {
    /// `Begin`'s reference origins (superview coordinates) and whether it entered live resize.
    static TRACE_BASELINE: std::cell::Cell<Option<([NSPoint; 3], bool)>> =
        const { std::cell::Cell::new(None) };
}

/// Run one trace step on the main thread and describe it in one line.
///
/// Each `Resize` is its own event-loop turn (the HTTP handler sends one command per step), so
/// anything AppKit or our own code queues for the end of a turn runs between steps, the way it
/// does between the drag events of a real live resize. Inside the step, `setFrame:display:`
/// relays out and draws synchronously, and `CATransaction.flush` then pushes that state to the
/// window server, which is what a live resize does at every step. The origins read just before
/// the flush are therefore what that frame shows.
pub fn traffic_light_trace_step(window: &Window, step: TrafficLightTraceStep) -> String {
    let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) else {
        return "(no AppKit window handle)".to_string();
    };
    unsafe {
        let content_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![content_view, window];
        if ns_window.is_null() {
            return "(no NSWindow)".to_string();
        }
        match step {
            TrafficLightTraceStep::Begin { live } => {
                let origins = button_origins(ns_window);
                let entered = live && send_if_responds(ns_window, objc2::sel!(_startLiveResize));
                TRACE_BASELINE.with(|b| b.set(Some((origins, entered))));
                format!(
                    "begin live={entered} reference {}",
                    describe_origins(&origins)
                )
            }
            TrafficLightTraceStep::Resize { dw, dh } => {
                let Some((baseline, _)) = TRACE_BASELINE.with(|b| b.get()) else {
                    return "(no Begin)".to_string();
                };
                let mut frame: NSRect = msg_send![ns_window, frame];
                frame.size.width += dw;
                frame.size.height += dh;
                // Grow downward so the title bar stays put on screen.
                frame.origin.y -= dh;
                let _: () = msg_send![ns_window, setFrame: frame, display: true, animate: false];
                let origins = button_origins(ns_window);
                let _: () = msg_send![objc2::class!(CATransaction), flush];
                let off: Vec<usize> = (0..3)
                    .filter(|&k| !same_point(origins[k], baseline[k]))
                    .collect();
                format!(
                    "resize {:.0}x{:.0} off={off:?} {}",
                    frame.size.width,
                    frame.size.height,
                    describe_origins(&origins)
                )
            }
            TrafficLightTraceStep::End => {
                let live = TRACE_BASELINE
                    .with(|b| b.take())
                    .is_some_and(|(_, live)| live);
                if live {
                    send_if_responds(ns_window, objc2::sel!(_endLiveResize));
                }
                let frame_view: *const AnyObject = msg_send![content_view, superview];
                let mut hits = Vec::new();
                for kind in 0u64..3 {
                    let button: *const AnyObject = msg_send![ns_window, standardWindowButton: kind];
                    if button.is_null() || frame_view.is_null() {
                        hits.push(false);
                        continue;
                    }
                    let bounds: NSRect = msg_send![button, bounds];
                    let nil_view: *const AnyObject = std::ptr::null();
                    let in_window: NSRect =
                        msg_send![button, convertRect: bounds, toView: nil_view];
                    // The frame view's superview coordinates are window coordinates.
                    let center = NSPoint::new(
                        in_window.origin.x + in_window.size.width / 2.0,
                        in_window.origin.y + in_window.size.height / 2.0,
                    );
                    let hit: *const AnyObject = msg_send![frame_view, hitTest: center];
                    let on_button = !hit.is_null() && {
                        let is_descendant: bool = msg_send![hit, isDescendantOf: button];
                        is_descendant
                    };
                    hits.push(on_button);
                }
                format!(
                    "end live={live} now {} centerHitsButton={hits:?}",
                    describe_origins(&button_origins(ns_window))
                )
            }
        }
    }
}

/// The three standard window buttons' frame origins, in their superview's coordinates.
unsafe fn button_origins(ns_window: *const AnyObject) -> [NSPoint; 3] {
    let mut out = [NSPoint::new(f64::NAN, f64::NAN); 3];
    for (kind, slot) in out.iter_mut().enumerate() {
        let button: *const AnyObject =
            unsafe { msg_send![ns_window, standardWindowButton: kind as u64] };
        if !button.is_null() {
            let frame: NSRect = unsafe { msg_send![button, frame] };
            *slot = frame.origin;
        }
    }
    out
}

fn same_point(a: NSPoint, b: NSPoint) -> bool {
    (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01
}

fn describe_origins(origins: &[NSPoint; 3]) -> String {
    let [c, m, z] = origins;
    format!(
        "close=({:.1},{:.1}) min=({:.1},{:.1}) zoom=({:.1},{:.1})",
        c.x, c.y, m.x, m.y, z.x, z.y
    )
}

/// Send a no-argument selector only when the receiver implements it. The live-resize pair is
/// private AppKit, so a macOS that drops it makes the trace run non-live instead of crashing.
unsafe fn send_if_responds(receiver: *const AnyObject, sel: objc2::runtime::Sel) -> bool {
    unsafe {
        let responds: bool = msg_send![receiver, respondsToSelector: sel];
        if responds {
            let _: () = objc2::runtime::MessageReceiver::send_message(receiver, sel, ());
        }
        responds
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
