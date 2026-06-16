//! The browse-mode `NSSplitView`: a real folder-tree sidebar on the left, a grid stub on the right.
//!
//! Built as a sibling subview of winit's contentView, layer-backed above the Metal layer, pinned
//! to the contentView edges, with a stable `identifier` — hidden by default (image mode is the
//! startup screen). Same compositing pattern as `window::add_titlebar_labels`.
//!
//! ## Left pane: the source-list tree
//!
//! An `NSVisualEffectView` `.sidebar` material fills the pane (Finder-sidebar look); the real
//! `NSOutlineView` (see `outline::BrowseTree`) sits inside it, inset `TITLE_BAR_HEIGHT` from the
//! top so its rows clear the traffic lights. The right (grid) pane stays a centered "(grid)"
//! stub — a later phase wires `NSCollectionView` there.
//!
//! ## The two spike fixes baked in here
//!
//! - **Divider opens at ~240pt without a drag.** `setPosition:ofDividerAtIndex:` is a no-op at
//!   build time because the split view has no frame yet. `BrowseSplitViewInner` (an `NSSplitView`
//!   subclass) sets the position on its first `layout` pass — the first time it has a real frame —
//!   then latches a flag so the user can drag freely afterward.
//! - **Sidebar clears the traffic lights.** The sidebar vibrancy fills the full pane height, but
//!   the outline scroll view is inset `TITLE_BAR_HEIGHT` (32pt) from the top so no row sits under
//!   the traffic-light strip — the same metric `window.rs` reserves for the title bar.
//!
//! ## No responder chain — keyboard is app-driven
//!
//! The panes are plain views; they don't subclass `keyDown:`. winit keeps delivering keyboard
//! input even with this split view up, so browse keys flow through winit → `AppCommand` and drive
//! the outline view programmatically (`BrowseTree::move_selection` / `expand_selected` /
//! `collapse_selected`). `set_focused_pane` only recolors the panes for a visible focus cue.
//!
//! Every `Retained<>` here is owned by the view hierarchy after `addSubview` or stored in
//! `BrowseSplitView` (which `App` keeps for the window's life) — so nothing drops early and
//! segfaults the autorelease pool.

use std::cell::Cell;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSSplitView, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
};
use objc2_foundation::{NSObjectProtocol, NSRect, NSString};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::PaneSide;
use super::outline::BrowseTree;

/// `identifier` set on the split view so `window` helpers can find/hide it by id, exactly like
/// the title-bar labels and vibrancy strips.
const BROWSER_SPLIT_IDENTIFIER: &str = "prvw.browser_split";

/// `zPosition` for the split view's layer. Above the wgpu CAMetalLayer's `1.0`
/// (`window::push_metal_layer_above_vibrancy`) so the native browse UI composites in front of the
/// transparent Metal layer rather than being occluded behind it. Matches `TITLEBAR_LABEL_Z_POSITION`.
const BROWSER_SPLIT_Z_POSITION: f64 = 2.0;

/// Initial divider position (logical px from the left): the sidebar opens at this width on first
/// layout, no manual drag needed. See the divider-fix note in the module docs.
const INITIAL_DIVIDER_X: f64 = 240.0;

/// Top inset for the sidebar's content (the outline scroll view), in logical px. Keeps tree rows
/// clear of the traffic lights. Mirrors `crate::TITLE_BAR_HEIGHT`.
const SIDEBAR_TOP_INSET: f64 = crate::TITLE_BAR_HEIGHT as f64;

// ─── BrowseSplitViewInner: sets the divider on first layout ────────────────

/// Ivars for [`BrowseSplitViewInner`]: a one-shot flag that latches after the first layout pass
/// sets the initial divider position. `Cell` because `layout` takes `&self` and AppKit calls it
/// on the main thread only.
struct SplitInnerIvars {
    divider_set: Cell<bool>,
}

define_class!(
    /// `NSSplitView` subclass that sets the initial divider position on its first `layout` — the
    /// first moment it has a real frame, since `setPosition:ofDividerAtIndex:` is a no-op on a
    /// zero-frame split view at build time.
    // SAFETY: NSSplitView subclass, no Drop. Main-thread only.
    #[unsafe(super(NSSplitView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowseSplitView"]
    #[ivars = SplitInnerIvars]
    struct BrowseSplitViewInner;

    unsafe impl NSObjectProtocol for BrowseSplitViewInner {}
);

impl BrowseSplitViewInner {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(SplitInnerIvars {
            divider_set: Cell::new(false),
        });
        let this: Retained<Self> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::default()] };
        this
    }
}

// ─── BrowseSplitView: the owned handles ────────────────────────────────────

/// Owns the split view, both panes, and the folder tree for the window's lifetime. `App` stores
/// this in `browser::State` so the `Retained<>`s never drop while the window lives.
pub struct BrowseSplitView {
    split: Retained<BrowseSplitViewInner>,
    tree_pane: Retained<NSView>,
    grid_pane: Retained<NSView>,
    /// The folder tree the app drives for keyboard navigation.
    tree: BrowseTree,
    /// Translucent "Loading…" overlay covering the tree pane. Hidden by default; shown when a
    /// directory scan the user is waiting on outlives `LOADING_OVERLAY_DELAY` (slow SMB share).
    loading_overlay: Retained<NSView>,
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

    /// Hide or show the split view. On the first show, set the initial divider position — by
    /// now the split view is in the live hierarchy and has a real frame, so
    /// `setPosition:ofDividerAtIndex:` takes (it's a no-op at build time on a zero-frame view).
    /// We set it once and latch, so a later show won't yank a divider the user has dragged.
    pub fn set_hidden(&self, _window: &Window, hidden: bool) {
        unsafe {
            let _: () = msg_send![&*self.split, setHidden: hidden];
            if !hidden && !self.split.ivars().divider_set.get() {
                self.split.ivars().divider_set.set(true);
                // Force the edge constraints to resolve so the split view has a real frame
                // (they otherwise resolve on the next runloop pass, after this returns, when
                // `setPosition:` would still see a zero frame and no-op).
                let _: () = msg_send![&*self.split, layoutSubtreeIfNeeded];
                let _: () = msg_send![
                    &*self.split,
                    setPosition: INITIAL_DIVIDER_X,
                    ofDividerAtIndex: 0usize
                ];
            }
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

    /// Move the tree selection by `delta` (+1 Down, -1 Up). Driven by browse-mode arrow keys.
    pub fn move_tree_selection(&self, delta: i32) {
        self.tree.move_selection(delta);
    }

    /// Expand the selected tree row (Right arrow).
    pub fn expand_tree_selection(&self) {
        self.tree.expand_selected();
    }

    /// Collapse the selected tree row (Left arrow).
    pub fn collapse_tree_selection(&self) {
        self.tree.collapse_selected();
    }

    /// Apply a completed background directory scan: store the children and reload that tree node.
    /// Then refresh the loading overlay (it may now hide if no scan is left pending).
    pub fn tree_children_loaded(&self, path: &std::path::Path, children: Vec<std::path::PathBuf>) {
        self.tree.children_loaded(path, children);
        self.refresh_loading_overlay();
    }

    /// The earliest still-in-flight tree scan start time, for the loading-overlay timer.
    pub fn earliest_in_flight_scan(&self) -> Option<std::time::Instant> {
        self.tree.earliest_in_flight_scan()
    }

    /// Show or hide the tree-pane "Loading…" overlay based on whether a scan is overdue (pending
    /// longer than `tree_model::LOADING_OVERLAY_DELAY`). Idempotent — safe to call every wakeup.
    pub fn refresh_loading_overlay(&self) {
        let overdue =
            super::tree_model::scan_overdue(self.earliest_in_flight_scan(), Instant::now());
        unsafe {
            let _: () = msg_send![&*self.loading_overlay, setHidden: !overdue];
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
        let split = BrowseSplitViewInner::new(mtm);
        split.setVertical(true); // left | right panes side by side
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

        // ── Left pane: sidebar vibrancy + the real folder tree ─────────────────────
        // `NSSplitView` sizes its arranged subviews itself, so the panes KEEP autoresizing
        // translation ON (the default). The tree's scroll view is inset below the title bar.
        let tree_pane = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*tree_pane, setWantsLayer: true];

        // Sidebar material fills the pane (Finder-sidebar look).
        let sidebar =
            NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), NSRect::default());
        sidebar.setMaterial(NSVisualEffectMaterial::Sidebar);
        sidebar.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        sidebar.setState(NSVisualEffectState::FollowsWindowActiveState);
        let _: () = msg_send![&*sidebar, setTranslatesAutoresizingMaskIntoConstraints: false];
        tree_pane.addSubview(as_view::<NSVisualEffectView>(&sidebar));
        pin_edges(as_view::<NSVisualEffectView>(&sidebar), &tree_pane, 0.0);

        // The folder tree, inside its scroll view, inset from the top so rows clear the
        // traffic lights.
        let tree = BrowseTree::create(mtm);
        let scroll = tree.scroll_view();
        let _: () = msg_send![scroll, setTranslatesAutoresizingMaskIntoConstraints: false];
        tree_pane.addSubview(as_view::<objc2_app_kit::NSScrollView>(scroll));
        let scroll_view = as_view::<objc2_app_kit::NSScrollView>(scroll);
        pin(
            &tree_pane,
            NSLayoutAttribute::Top,
            scroll_view,
            NSLayoutAttribute::Top,
            -SIDEBAR_TOP_INSET,
        );
        pin(
            scroll_view,
            NSLayoutAttribute::Leading,
            &tree_pane,
            NSLayoutAttribute::Leading,
            0.0,
        );
        pin(
            &tree_pane,
            NSLayoutAttribute::Trailing,
            scroll_view,
            NSLayoutAttribute::Trailing,
            0.0,
        );
        pin(
            &tree_pane,
            NSLayoutAttribute::Bottom,
            scroll_view,
            NSLayoutAttribute::Bottom,
            0.0,
        );

        // ── Loading overlay: translucent "Loading…" over the tree pane, hidden by default ─
        // Shown only when a scan the user is waiting on outlives `LOADING_OVERLAY_DELAY` (a slow
        // SMB share). Sits above the scroll view in the tree pane, pinned to all edges, so it
        // covers the rows while a scan is in flight. The 1s delay (driven by the wakeup timer in
        // `app.rs`) keeps it from flashing for fast local dirs.
        let loading_overlay = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*loading_overlay, setWantsLayer: true];
        let _: () =
            msg_send![&*loading_overlay, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*loading_overlay, setHidden: true];
        if let Some(overlay_layer) = loading_overlay.layer() {
            // A near-opaque window-background fill so the rows underneath read as "busy".
            let bg = NSColor::windowBackgroundColor().colorWithAlphaComponent(0.8);
            set_layer_background(&*overlay_layer as *const _ as *const AnyObject, &bg);
        }
        let loading_label = make_label("Loading…", 13.0, mtm);
        loading_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let _: () = msg_send![&*loading_label, setTranslatesAutoresizingMaskIntoConstraints: false];
        loading_overlay.addSubview(as_view::<objc2_app_kit::NSTextField>(&loading_label));
        center_in(&loading_label, &loading_overlay);
        tree_pane.addSubview(&loading_overlay);
        pin_edges(&loading_overlay, &tree_pane, 0.0);

        // ── Right pane: centered "(grid)" stub (a later phase wires NSCollectionView) ─
        let grid_pane = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*grid_pane, setWantsLayer: true];
        let label = make_label("(grid)", 15.0, mtm);
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let _: () = msg_send![&*label, setTranslatesAutoresizingMaskIntoConstraints: false];
        grid_pane.addSubview(as_view::<objc2_app_kit::NSTextField>(&label));
        center_in(&label, &grid_pane);

        // Add panes to the split view (order matters: index 0 = tree, 1 = grid).
        split.addSubview(&tree_pane);
        split.addSubview(&grid_pane);

        // ── Add the split view as a contentView sibling, pinned to all edges ───────
        let split_obj: *const AnyObject = &*split as *const BrowseSplitViewInner as *const _;
        let _: () = msg_send![ns_view, addSubview: split_obj];

        let parent: &AnyObject = &*ns_view;
        for (attr, parent_attr) in [
            (NSLayoutAttribute::Top, NSLayoutAttribute::Top),
            (NSLayoutAttribute::Bottom, NSLayoutAttribute::Bottom),
            (NSLayoutAttribute::Leading, NSLayoutAttribute::Leading),
            (NSLayoutAttribute::Trailing, NSLayoutAttribute::Trailing),
        ] {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &split, attr, NSLayoutRelation::Equal, Some(parent), parent_attr, 1.0, 0.0,
            )
            .setActive(true);
        }

        log::debug!("Browse split view created (hidden) with real folder tree");

        BrowseSplitView {
            split,
            tree_pane,
            grid_pane,
            tree,
            loading_overlay,
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
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            item, attr, NSLayoutRelation::Equal, Some(to), to_attr, 1.0, constant,
        )
        .setActive(true);
    }
}

/// Pin `item` to fill `container` (all four edges) at the given inset.
unsafe fn pin_edges(item: &AnyObject, container: &AnyObject, inset: f64) {
    unsafe {
        pin(
            item,
            NSLayoutAttribute::Top,
            container,
            NSLayoutAttribute::Top,
            inset,
        );
        pin(
            item,
            NSLayoutAttribute::Leading,
            container,
            NSLayoutAttribute::Leading,
            inset,
        );
        pin(
            container,
            NSLayoutAttribute::Trailing,
            item,
            NSLayoutAttribute::Trailing,
            inset,
        );
        pin(
            container,
            NSLayoutAttribute::Bottom,
            item,
            NSLayoutAttribute::Bottom,
            inset,
        );
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

/// Recolor a pane's layer to show whether it's focused. Focused → a faint accent tint; unfocused
/// → transparent (the sidebar vibrancy / grid background shows through). Distinct enough that a
/// human can SEE Tab switch focus; the real selection rings live in the native controls.
/// SAFETY: `pane` is a live layer-backed `NSView` on the main thread.
unsafe fn set_pane_focus(pane: &NSView, focused: bool) {
    let Some(layer) = pane.layer() else {
        return;
    };
    let color = if focused {
        NSColor::controlAccentColor().colorWithAlphaComponent(0.12)
    } else {
        NSColor::clearColor()
    };
    unsafe { set_layer_background(&*layer as *const _ as *const AnyObject, &color) };
}

/// Set a layer's `backgroundColor` from an `NSColor`, given the layer as a raw `AnyObject` pointer.
/// SAFETY: `layer` is a live `CALayer` on the main thread.
///
/// The typed `CGColor()` gives a real CGColorRef. Set it with a raw `objc_msgSend`:
/// `msg_send![layer, setBackgroundColor: cg]` mis-encodes the CGColorRef as `@` (ObjC object)
/// instead of `^{CGColor=}` and panics — the same trap `settings::window` works around.
/// `CALayer::setBackgroundColor` is also unavailable here (gated behind an `objc2-quartz-core`
/// feature we don't pull in), so the raw send is the route. Taking a raw pointer keeps this free
/// of the (unimported) `CALayer` type name.
unsafe fn set_layer_background(layer: *const AnyObject, color: &NSColor) {
    unsafe {
        let cg_color = color.CGColor();
        let cg_ptr: *const std::ffi::c_void = Retained::as_ptr(&cg_color).cast();
        let set_bg: unsafe extern "C" fn(
            *const AnyObject,
            objc2::runtime::Sel,
            *const std::ffi::c_void,
        ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
        set_bg(layer, objc2::sel!(setBackgroundColor:), cg_ptr);
    }
}
