//! The browse-mode `NSSplitView` (Phase 0 spike).
//!
//! Builds the split view as a sibling subview of winit's contentView, layer-backed above the
//! Metal layer, pinned to the contentView edges, with a stable `identifier` — hidden by default.
//! Left pane: a sidebar-ish placeholder (stub rows). Right pane: a centered "(grid)" label.
//!
//! ## No responder chain — keyboard is app-driven
//!
//! The panes are plain `NSView`s. They do **not** subclass `keyDown:` and do not depend on
//! first-responder status, because winit keeps delivering `WindowEvent::KeyboardInput` even
//! while this split view is up (the live spike proved it: `makeFirstResponder` returns `true`
//! but winit re-asserts its content view, so a native pane's `keyDown:` never fires). So all
//! browse-mode keys flow through winit → `input::browse_key_to_command` → `AppCommand`, and the
//! split view only renders focus: `set_focused_pane` recolors the two panes so the focused one
//! is visibly highlighted. See `docs/specs/image-browser.md` → "Input architecture".
//!
//! Every `Retained<>` here is either owned by the view hierarchy after `addSubview` or stored in
//! `BrowseSplitView` (which `App` keeps for the window's life) — so nothing drops early and
//! segfaults the autorelease pool.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSColor, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSSplitView, NSView,
};
use objc2_foundation::{NSRect, NSString};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::PaneSide;

/// `identifier` set on the split view so `window` helpers can find/hide it by id, exactly like
/// the title-bar labels and vibrancy strips.
const BROWSER_SPLIT_IDENTIFIER: &str = "prvw.browser_split";

/// `zPosition` for the split view's layer. Above the wgpu CAMetalLayer's `1.0`
/// (`window::push_metal_layer_above_vibrancy`) so the native browse UI composites in front of the
/// transparent Metal layer rather than being occluded behind it. Matches `TITLEBAR_LABEL_Z_POSITION`.
const BROWSER_SPLIT_Z_POSITION: f64 = 2.0;

/// Initial divider position (logical px from the left). Gives the tree pane a sidebar-like width
/// so neither pane collapses to zero on first show.
const INITIAL_DIVIDER_X: f64 = 240.0;

/// Owns the split view and both panes for the window's lifetime. `App` stores this in
/// `browser::State` so the `Retained<>`s never drop while the window lives. The view hierarchy
/// also retains them after `addSubview`, but holding our own handles lets us hide/show and
/// recolor the focus highlight without re-walking the subtree.
pub struct BrowseSplitView {
    split: Retained<NSSplitView>,
    tree_pane: Retained<NSView>,
    grid_pane: Retained<NSView>,
}

// SAFETY: All fields are AppKit objects only ever touched on the main thread (App runs the winit
// loop on the main thread). They're stored, not shared across threads.
unsafe impl Send for BrowseSplitView {}

impl BrowseSplitView {
    /// Build the split view and add it (hidden) as a sibling subview of winit's contentView.
    pub fn create(window: &Window) -> Self {
        let mtm = MainThreadMarker::new().expect("create() must run on the main thread");
        let ns_view = content_view_ptr(window).expect("winit window must have an AppKit view");

        unsafe { build(mtm, ns_view) }
    }

    /// Hide or show the split view.
    pub fn set_hidden(&self, _window: &Window, hidden: bool) {
        unsafe {
            let split: *const AnyObject = &*self.split as *const NSSplitView as *const _;
            let _: () = msg_send![split, setHidden: hidden];
        }
    }

    /// Recolor the panes so the focused one is visibly highlighted (stub-level focus ring).
    /// Called on entering browse mode and whenever `ToggleBrowseFocus` flips the pane.
    pub fn set_focused_pane(&self, focused: PaneSide) {
        unsafe {
            set_pane_focus(&self.tree_pane, matches!(focused, PaneSide::Tree));
            set_pane_focus(&self.grid_pane, matches!(focused, PaneSide::Grid));
        }
    }
}

/// Pull winit's contentView pointer out of the window handle.
fn content_view_ptr(window: &Window) -> Option<*const AnyObject> {
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    Some(handle.ns_view.as_ptr() as *const AnyObject)
}

/// Build the split view + panes and wire them into the contentView. SAFETY: `ns_view` is winit's
/// live contentView on the main thread (`mtm`).
unsafe fn build(mtm: MainThreadMarker, ns_view: *const AnyObject) -> BrowseSplitView {
    use crate::platform::macos::ui_common::{FlippedView, as_view, make_label};

    unsafe {
        // ── Split view ───────────────────────────────────────────────────────────
        let split = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), NSRect::default());
        split.setVertical(true); // left | right panes side by side
        // Identifier + layer-back + zPosition above the Metal layer, like the title-bar labels.
        let identifier = NSString::from_str(BROWSER_SPLIT_IDENTIFIER);
        let _: () = msg_send![&*split, setIdentifier: &*identifier];
        let _: () = msg_send![&*split, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*split, setWantsLayer: true];
        let split_layer: *const AnyObject = msg_send![&*split, layer];
        if !split_layer.is_null() {
            let _: () = msg_send![split_layer, setZPosition: BROWSER_SPLIT_Z_POSITION];
        }
        // Hidden by default — image mode is the startup screen.
        let _: () = msg_send![&*split, setHidden: true];

        // ── Left pane: sidebar-ish placeholder with a few stub rows ────────────────
        // `NSSplitView` sizes its arranged subviews itself, so the panes must KEEP
        // autoresizing-translation ON. The earlier spike disabled it (and gave the panes no
        // size constraints), which collapsed both to zero and rendered the gray void. Plain
        // layer-backed `NSView`s with a background color make the panes clearly visible.
        let tree_pane = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*tree_pane, setWantsLayer: true];
        add_stub_rows(mtm, &tree_pane);

        // ── Right pane: centered "(grid)" label ────────────────────────────────────
        let grid_pane = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*grid_pane, setWantsLayer: true];
        let label = make_label("(grid)", 15.0, mtm);
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let _: () = msg_send![&*label, setTranslatesAutoresizingMaskIntoConstraints: false];
        grid_pane.addSubview(as_view::<objc2_app_kit::NSTextField>(&label));
        // Center the label in its pane.
        center_in(&label, &grid_pane);

        // Add panes to the split view (order matters: index 0 = tree, 1 = grid).
        split.addSubview(&tree_pane);
        split.addSubview(&grid_pane);

        // ── Add the split view as a contentView sibling, pinned to all edges ───────
        let split_obj: *const AnyObject = &*split as *const NSSplitView as *const _;
        let _: () = msg_send![ns_view, addSubview: split_obj];

        let parent: &AnyObject = &*ns_view;
        let make_constraint = |attr: NSLayoutAttribute, parent_attr: NSLayoutAttribute| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &split, attr, NSLayoutRelation::Equal, Some(parent), parent_attr, 1.0, 0.0,
            )
        };
        for c in [
            make_constraint(NSLayoutAttribute::Top, NSLayoutAttribute::Top),
            make_constraint(NSLayoutAttribute::Bottom, NSLayoutAttribute::Bottom),
            make_constraint(NSLayoutAttribute::Leading, NSLayoutAttribute::Leading),
            make_constraint(NSLayoutAttribute::Trailing, NSLayoutAttribute::Trailing),
        ] {
            c.setActive(true);
        }

        // Give the tree pane a sidebar width so neither side starts collapsed.
        let _: () = msg_send![&*split, setPosition: INITIAL_DIVIDER_X, ofDividerAtIndex: 0usize];

        log::debug!("Browse split view created (hidden)");

        BrowseSplitView {
            split,
            tree_pane,
            grid_pane,
        }
    }
}

/// Add a few stub "folder rows" to the tree pane so it reads as a sidebar, not a blank box.
/// SAFETY: `pane` is a live layer-backed `NSView` on the main thread.
unsafe fn add_stub_rows(mtm: MainThreadMarker, pane: &NSView) {
    use crate::platform::macos::ui_common::{as_view, make_label};
    unsafe {
        let rows = ["Pictures", "Downloads", "Desktop"];
        let mut prev: Option<Retained<objc2_app_kit::NSTextField>> = None;
        for text in rows {
            let row = make_label(text, 13.0, mtm);
            // Left-align the stub rows (the shared label factory centers by default).
            row.setAlignment(objc2_app_kit::NSTextAlignment(0)); // NSTextAlignmentLeft = 0
            let _: () = msg_send![&*row, setTranslatesAutoresizingMaskIntoConstraints: false];
            pane.addSubview(as_view::<objc2_app_kit::NSTextField>(&row));

            // Pin to the leading edge; stack vertically from the top.
            pin(
                &row,
                NSLayoutAttribute::Leading,
                pane,
                NSLayoutAttribute::Leading,
                16.0,
            );
            match &prev {
                Some(p) => pin(
                    &row,
                    NSLayoutAttribute::Top,
                    p,
                    NSLayoutAttribute::Bottom,
                    12.0,
                ),
                None => pin(
                    &row,
                    NSLayoutAttribute::Top,
                    pane,
                    NSLayoutAttribute::Top,
                    16.0,
                ),
            }
            prev = Some(row);
        }
    }
}

/// Activate one Auto Layout constraint pinning `item.attr` to `to.to_attr` at `constant`.
/// SAFETY: both views are live on the main thread.
unsafe fn pin(
    item: &AnyObject,
    attr: NSLayoutAttribute,
    to: &AnyObject,
    to_attr: NSLayoutAttribute,
    constant: f64,
) {
    unsafe {
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            item, attr, NSLayoutRelation::Equal, Some(to), to_attr, 1.0, constant,
        );
        c.setActive(true);
    }
}

/// Center `item` (both axes) inside `container`. SAFETY: both views are live on the main thread.
unsafe fn center_in(item: &AnyObject, container: &AnyObject) {
    unsafe {
        pin(
            item,
            NSLayoutAttribute::CenterX,
            container,
            NSLayoutAttribute::CenterX,
            0.0,
        );
        pin(
            item,
            NSLayoutAttribute::CenterY,
            container,
            NSLayoutAttribute::CenterY,
            0.0,
        );
    }
}

/// Recolor a pane's layer to show whether it's focused. Focused → a bright accent-tinted
/// background; unfocused → a dim neutral one. Distinct enough that a human can SEE Tab switch
/// focus (stub-level; the real panes get native selection rings later).
/// SAFETY: `pane` is a live layer-backed `NSView` on the main thread.
unsafe fn set_pane_focus(pane: &NSView, focused: bool) {
    let Some(layer) = pane.layer() else {
        return;
    };
    let color = if focused {
        // System accent, lightened, so the focused pane clearly pops.
        NSColor::controlAccentColor().colorWithAlphaComponent(0.30)
    } else {
        // Dim neutral fill so the unfocused pane stays visible but recedes.
        NSColor::secondaryLabelColor().colorWithAlphaComponent(0.06)
    };
    // The typed `CGColor()` gives a real CGColorRef. Set it on the layer with a raw
    // `objc_msgSend`: `msg_send![layer, setBackgroundColor: cg]` mis-encodes the CGColorRef as
    // `@` (ObjC object) instead of `^{CGColor=}` and panics — the same trap `settings::window`
    // works around. `CALayer::setBackgroundColor` is also unavailable here (it's gated behind
    // a `objc2-quartz-core` feature we don't pull in), so the raw send is the route.
    unsafe {
        let cg_color = color.CGColor();
        let cg_ptr: *const std::ffi::c_void = Retained::as_ptr(&cg_color).cast();
        let set_bg: unsafe extern "C" fn(
            *const AnyObject,
            objc2::runtime::Sel,
            *const std::ffi::c_void,
        ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
        set_bg(
            &*layer as *const _ as *const AnyObject,
            objc2::sel!(setBackgroundColor:),
            cg_ptr,
        );
    }
}
