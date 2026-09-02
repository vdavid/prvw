//! The browse-mode `NSSplitView`: a real folder-tree sidebar on the left, a thumbnail grid on the
//! right.
//!
//! Built as a sibling subview of winit's contentView, layer-backed above the Metal layer, pinned
//! to the contentView edges, with a stable `identifier` — hidden by default (image mode is the
//! startup screen). Same compositing pattern as `window::add_titlebar_labels`.
//!
//! ## Left pane: the source-list tree
//!
//! An `NSVisualEffectView` `.sidebar` material fills the pane (Finder-sidebar look); the real
//! `NSOutlineView` (see `outline::BrowseTree`) sits inside it, inset `TITLE_BAR_HEIGHT` from the
//! top so its rows clear the traffic lights.
//!
//! ## Right pane: the thumbnail grid on a rounded gallery surface
//!
//! The `NSCollectionView` gallery (see `grid::BrowseGrid`) sits inside a layer-backed,
//! rounded-corner **gallery surface** — a `controlBackgroundColor`-filled view inset from the pane
//! edges (`GALLERY_INSET_PT`, with a larger `GALLERY_TOP_INSET_PT` to clear the title-bar strip),
//! so the grid reads as an intentional gallery rather than a flush void (the "rounded corners like
//! Finder's content area" cue). The grid scroll view fills the surface; `masksToBounds` clips its
//! content to the rounded corners. The grid's "(No images)" overlay is centered on the surface
//! (shown only for an empty folder). Tune the inset/radius via the `GALLERY_*` constants.
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
//! ## Focus: native first responder follows `focused_pane`
//!
//! In idle-winit browse mode the focused native view holds the window's first responder, so it
//! handles its own arrows/page-keys/type-select natively and only Tab/Enter/Esc are intercepted by
//! its `keyDown:` override. `apply_focus` is the sync point: it `makeFirstResponder:`s the focused
//! pane's control (outline view or collection view) and refreshes the grid's per-item emphasis. The
//! tree (a source list) draws accent-blue selection automatically while it's first responder and
//! gray otherwise, so its emphasis needs no extra code.
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
use super::grid::BrowseGrid;
use super::outline::BrowseTree;
use crate::navigation::SortBy;

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

// ─── Gallery surface (the grid pane's rounded background) ───────────────────
// The thumbnail grid sits on an intentional, slightly-inset rounded-corner surface — the
// user's "rounded corners like the left side of Finder windows" cue. Tune these to taste.

/// Inset of the gallery surface from the grid pane's leading/trailing/bottom edges, in logical px.
/// The small gap lets the window background frame the gallery so it reads as a deliberate surface.
const GALLERY_INSET_PT: f64 = 10.0;
/// Top inset of the gallery surface, in logical px. Larger than the side inset so the surface
/// clears the traffic-light / title-bar strip (mirrors the sidebar's `SIDEBAR_TOP_INSET`) plus a
/// little air below it.
const GALLERY_TOP_INSET_PT: f64 = crate::TITLE_BAR_HEIGHT as f64 + GALLERY_INSET_PT;
/// Corner radius of the gallery surface, in logical px. Echoes a Finder content area.
const GALLERY_CORNER_RADIUS: f64 = 10.0;

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
    /// The two pane container views. Held for the window's lifetime (autorelease discipline) but no
    /// longer read after build — focus emphasis now lives in the native controls (tree source-list
    /// selection, grid per-item rect), not a pane background.
    _tree_pane: Retained<NSView>,
    _grid_pane: Retained<NSView>,
    /// The folder tree the app drives for keyboard navigation.
    tree: BrowseTree,
    /// The thumbnail grid the app drives (listing, selection, thumbnail generation).
    grid: BrowseGrid,
    /// Translucent overlay covering the tree pane. Hidden by default; shown when a directory scan
    /// the user is waiting on outlives `LOADING_OVERLAY_DELAY` (slow SMB share).
    loading_overlay: Retained<NSView>,
    /// The overlay's centered label. Held so `refresh_loading_overlay` can keep its running scan
    /// count up to date; dropping it would take the label with it (autorelease discipline).
    loading_label: Retained<objc2_app_kit::NSTextField>,
}

// SAFETY: All fields are AppKit objects only ever touched on the main thread (App runs the winit
// loop on the main thread). They're stored, not shared across threads.
unsafe impl Send for BrowseSplitView {}

impl BrowseSplitView {
    /// Build the split view and add it (hidden) as a sibling subview of winit's contentView.
    /// `sort_by` is the order the grid lists folder images in.
    pub fn create(window: &Window, sort_by: SortBy) -> Self {
        let mtm = MainThreadMarker::new().expect("create() must run on the main thread");
        let ns_view = content_view_ptr(window).expect("winit window must have an AppKit view");
        let scale = window.scale_factor();

        unsafe { build(mtm, ns_view, sort_by, scale) }
    }

    /// Put the running scan status on the grid's centered label, or `None` to hand it back to the
    /// "(No images)" rule.
    pub fn set_grid_scan_status(&self, text: Option<&str>) {
        self.grid.set_scan_status(text);
    }

    /// The thumbnail grid, for the app to drive (folder listing, thumbnails, selection, open).
    pub fn grid(&self) -> &BrowseGrid {
        &self.grid
    }

    /// True while the tree's async reveal walk is in flight (browse-open positioning). Exposed for
    /// the QA snapshot so tests can poll for the reveal to settle.
    pub fn reveal_pending(&self) -> bool {
        self.tree.reveal_pending()
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

    /// Sync the native first responder to the focused pane and refresh emphasis on both panes.
    /// `makeFirstResponder:` the focused pane's native control (the `NSOutlineView` for Tree, the
    /// `NSCollectionView` for Grid) so it draws native selection emphasis and its `keyDown:`
    /// override fires for that pane; then refresh the grid's per-item emphasis (the tree's
    /// source-list emphasis follows first responder automatically). Called on entering browse and
    /// whenever `ToggleBrowseFocus` flips the pane.
    pub fn apply_focus(&self, focused: PaneSide) {
        // Make the focused pane's control first responder. The window draws accent-blue emphasis
        // on the first-responder source list (tree) for free; the grid reads its own
        // first-responder state for per-item blue/gray.
        match focused {
            PaneSide::Tree => self.tree.make_first_responder(),
            PaneSide::Grid => self.grid.make_first_responder(),
        }
        // Drive the grid's selected-item emphasis from `focused_pane` (blue iff the grid is the
        // focused pane, gray otherwise) and repaint. State-driven, so a tree click grays the grid
        // item even though the async click→`BrowseSelectFolder` flip makes the native first
        // responder unreliable to read. The tree (a source list) repaints its own emphasis on the
        // first-responder change.
        self.grid.set_focused(matches!(focused, PaneSide::Grid));
    }

    /// Reveal `folder` in the tree: expand from its containing root down to it and select +
    /// scroll-to-mid (async — see `outline::BrowseTree::reveal_to_folder`). Used for browse-open
    /// positioning and dir-arg launch.
    pub fn reveal_folder_in_tree(&self, folder: &std::path::Path) {
        self.tree.reveal_to_folder(folder);
    }

    /// Apply a completed background directory scan: store the children and reload that tree node.
    /// Then refresh the loading overlay (it may now hide if no scan is left pending).
    pub fn tree_children_loaded(&self, path: &std::path::Path, children: Vec<std::path::PathBuf>) {
        self.tree.children_loaded(path, children);
        // No count here: whatever scan is still in flight gets its number back on the next wakeup.
        self.refresh_loading_overlay(None);
    }

    /// The source-list root paths (home + volumes), for the live-folder-sync tree watch.
    pub fn tree_root_paths(&self) -> Vec<std::path::PathBuf> {
        self.tree.root_paths()
    }

    /// Re-scan a watched (expanded) tree folder's subdirectories after it changed on disk and
    /// reload its node (live folder sync, Part B), preserving expansion/selection.
    pub fn reload_tree_node(&self, folder: &std::path::Path) {
        self.tree.reload_node(folder);
    }

    /// The longest-running still-in-flight tree scan (folder + start time), for the loading
    /// overlay's timer and its live entry count.
    pub fn earliest_in_flight_scan(&self) -> Option<(std::path::PathBuf, std::time::Instant)> {
        self.tree.earliest_in_flight_scan()
    }

    /// Show or hide the tree-pane loading overlay based on whether a scan is overdue (pending
    /// longer than `tree_model::LOADING_OVERLAY_DELAY`), and give it the scan's live entry count
    /// when the caller has one. Idempotent — safe to call every wakeup.
    pub fn refresh_loading_overlay(&self, scanned_count: Option<usize>) {
        let started = self.earliest_in_flight_scan().map(|(_, started)| started);
        let overdue = super::tree_model::scan_overdue(started, Instant::now());
        let text = match scanned_count {
            Some(count) => format!(
                "Scanning\u{2026} {}",
                crate::folder_scan::images_so_far(count)
            ),
            None => "Loading\u{2026}".to_string(),
        };
        self.loading_label
            .setStringValue(&NSString::from_str(&text));
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
unsafe fn build(
    mtm: MainThreadMarker,
    ns_view: *const AnyObject,
    sort_by: SortBy,
    scale: f64,
) -> BrowseSplitView {
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

        // ── Right pane: a rounded gallery surface hosting the NSCollectionView grid ─
        let grid_pane = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*grid_pane, setWantsLayer: true];

        // The gallery surface: a layer-backed, rounded-corner container inset from the pane edges,
        // filled with a semantic surface color so the grid reads as an intentional gallery (not a
        // flush void). The grid scroll view fills it; rounded corners clip its content.
        let gallery = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*gallery, setWantsLayer: true];
        let _: () = msg_send![&*gallery, setTranslatesAutoresizingMaskIntoConstraints: false];
        if let Some(layer) = gallery.layer() {
            let layer_ptr = &*layer as *const _ as *const AnyObject;
            let _: () = msg_send![layer_ptr, setCornerRadius: GALLERY_CORNER_RADIUS];
            let _: () = msg_send![layer_ptr, setMasksToBounds: true];
            // `controlBackgroundColor` is the semantic content-area surface — light gray in light
            // mode, near-black in dark mode — so it adapts automatically.
            set_layer_background(layer_ptr, &NSColor::controlBackgroundColor());
        }
        grid_pane.addSubview(&gallery);
        pin_gallery(&gallery, &grid_pane);

        let grid = BrowseGrid::create(mtm, sort_by, scale);
        let grid_scroll = grid.scroll_view();
        let _: () = msg_send![grid_scroll, setTranslatesAutoresizingMaskIntoConstraints: false];
        gallery.addSubview(as_view::<objc2_app_kit::NSScrollView>(grid_scroll));
        pin_edges(
            as_view::<objc2_app_kit::NSScrollView>(grid_scroll),
            &gallery,
            0.0,
        );

        // The "(No images)" overlay (owned by the grid), centered over the gallery surface, hidden
        // until a folder lists empty.
        let empty_label = grid.empty_label();
        let _: () = msg_send![empty_label, setTranslatesAutoresizingMaskIntoConstraints: false];
        gallery.addSubview(as_view::<objc2_app_kit::NSTextField>(empty_label));
        center_in(as_view::<objc2_app_kit::NSTextField>(empty_label), &gallery);

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
            _tree_pane: tree_pane,
            _grid_pane: grid_pane,
            tree,
            grid,
            loading_overlay,
            loading_label,
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

/// Pin the gallery surface inside the grid pane: a uniform `GALLERY_INSET_PT` on the leading,
/// trailing, and bottom edges, but a larger `GALLERY_TOP_INSET_PT` on top so it clears the
/// traffic-light / title-bar strip. SAFETY: both views are live on the main thread.
unsafe fn pin_gallery(gallery: &AnyObject, pane: &AnyObject) {
    unsafe {
        pin(
            gallery,
            NSLayoutAttribute::Top,
            pane,
            NSLayoutAttribute::Top,
            GALLERY_TOP_INSET_PT,
        );
        pin(
            gallery,
            NSLayoutAttribute::Leading,
            pane,
            NSLayoutAttribute::Leading,
            GALLERY_INSET_PT,
        );
        pin(
            pane,
            NSLayoutAttribute::Trailing,
            gallery,
            NSLayoutAttribute::Trailing,
            GALLERY_INSET_PT,
        );
        pin(
            pane,
            NSLayoutAttribute::Bottom,
            gallery,
            NSLayoutAttribute::Bottom,
            GALLERY_INSET_PT,
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
