//! The browse-mode `NSSplitView` (Phase 0 spike).
//!
//! Builds the split view as a sibling subview of winit's contentView, layer-backed above the
//! Metal layer, pinned to the contentView edges, with a stable `identifier` — hidden by default.
//! Left pane: an `NSScrollView` wrapping a stub `NSOutlineView`. Right pane: an `NSScrollView`
//! with a "(grid)" placeholder label.
//!
//! ## Focus / keyboard — the spike's reason to exist
//!
//! When a native AppKit view is first responder, AppKit's responder chain — not winit — gets the
//! key events. winit's `WindowEvent::KeyboardInput` goes quiet for printable/navigation keys
//! while a pane holds focus. So the panes are `define_class!` `BrowsePane`s (an `NSScrollView`
//! subclass) that override `keyDown:` to route Tab / Esc / Enter into `AppCommand`s via
//! `crate::commands::send_command`, and `acceptsFirstResponder` → true so they can hold focus.
//! Tab toggles focus between the two panes through the window's `makeFirstResponder:`.
//!
//! Every `Retained<>` here is either owned by the view hierarchy after `addSubview`, stored in
//! `BrowseSplitView` (which `App` keeps for the window's life), or `mem::forget`-leaked like the
//! Settings delegate — so nothing drops early and segfaults the autorelease pool.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSScrollView, NSSplitView,
};
use objc2_foundation::{NSObjectProtocol, NSString};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::commands::{self, AppCommand};

/// `identifier` set on the split view so `window` helpers can find/hide it by id, exactly like
/// the title-bar labels and vibrancy strips.
const BROWSER_SPLIT_IDENTIFIER: &str = "prvw.browser_split";

/// `zPosition` for the split view's layer. Above the wgpu CAMetalLayer's `1.0`
/// (`window::push_metal_layer_above_vibrancy`) so the native browse UI composites in front of the
/// transparent Metal layer rather than being occluded behind it. Matches `TITLEBAR_LABEL_Z_POSITION`.
const BROWSER_SPLIT_Z_POSITION: f64 = 2.0;

/// Tells `BrowsePane::key_down` which side it is, so Tab can hand focus to the other pane.
#[derive(Clone, Copy)]
enum PaneSide {
    Tree,
    Grid,
}

struct BrowsePaneIvars {
    side: std::cell::Cell<PaneSide>,
}

define_class!(
    /// An `NSScrollView` pane that participates in the responder chain and routes the spike's
    /// navigation keys (Tab / Esc / Enter) into `AppCommand`s. Without `acceptsFirstResponder`
    /// → true a scroll view won't reliably become first responder; without the `keyDown:`
    /// override the keys would beep (no responder handles them) instead of switching mode.
    #[unsafe(super(NSScrollView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowsePane"]
    #[ivars = BrowsePaneIvars]
    struct BrowsePane;

    unsafe impl NSObjectProtocol for BrowsePane {}

    impl BrowsePane {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &AnyObject) {
            // `keyCode` is a hardware code; the three we care about are stable: Tab=48,
            // Return=36, keypad Enter=76, Escape=53.
            let key_code: u16 = unsafe { msg_send![event, keyCode] };
            match key_code {
                48 => {
                    // Tab → toggle focus to the other pane via the window key-view loop.
                    let to = match self.ivars().side.get() {
                        PaneSide::Tree => PaneSide::Grid,
                        PaneSide::Grid => PaneSide::Tree,
                    };
                    log::debug!("BrowsePane keyDown: Tab → focus other pane");
                    unsafe { focus_sibling_pane(self, to) };
                }
                36 | 76 | 53 => {
                    // Enter or Esc → leave browse mode (spike: both return to image mode).
                    log::debug!("BrowsePane keyDown: Enter/Esc → leave browse mode");
                    commands::send_command(AppCommand::EnterImageMode);
                }
                other => {
                    log::debug!("BrowsePane keyDown: keyCode {other} (passing to super)");
                    // Let AppKit handle arrows/selection etc. within the pane.
                    let _: () = unsafe { msg_send![super(self), keyDown: event] };
                }
            }
        }
    }
);

impl BrowsePane {
    fn new(mtm: MainThreadMarker, side: PaneSide) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(BrowsePaneIvars {
            side: std::cell::Cell::new(side),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Make the sibling pane (identified by `to`) the window's first responder. Called from a Tab
/// keyDown; walks up to the window and over to the other pane handle stored on the split view.
/// SAFETY: `pane` is a live `BrowsePane` on the main thread; the handles it reaches were retained
/// for the window's lifetime.
unsafe fn focus_sibling_pane(pane: &BrowsePane, to: PaneSide) {
    unsafe {
        let window: *const AnyObject = msg_send![pane, window];
        if window.is_null() {
            return;
        }
        // Find the split view (our superview) and its two arranged panes by order.
        let split: *const AnyObject = msg_send![pane, superview];
        if split.is_null() {
            return;
        }
        let subviews: *const AnyObject = msg_send![split, subviews];
        if subviews.is_null() {
            return;
        }
        let count: usize = msg_send![subviews, count];
        // Pane 0 = tree, pane 1 = grid (insertion order in `create`).
        let target_index = match to {
            PaneSide::Tree => 0usize,
            PaneSide::Grid => 1usize,
        };
        if target_index >= count {
            return;
        }
        let target: *const AnyObject = msg_send![subviews, objectAtIndex: target_index];
        let _: bool = msg_send![window, makeFirstResponder: target];
    }
}

/// Owns the split view and both panes for the window's lifetime. `App` stores this in
/// `browser::State` so the `Retained<>`s never drop while the window lives. The view hierarchy
/// also retains them after `addSubview`, but holding our own handles lets us hide/show and focus
/// without re-walking the subtree.
pub struct BrowseSplitView {
    split: Retained<NSSplitView>,
    tree_pane: Retained<BrowsePane>,
    #[allow(dead_code)] // Held for the window's lifetime; the spike doesn't read it back yet.
    grid_pane: Retained<BrowsePane>,
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

    /// Make the tree pane the window's first responder, so keyboard focus lands in browse mode.
    pub fn focus_tree(&self, window: &Window) {
        let Some(ns_view) = content_view_ptr(window) else {
            return;
        };
        unsafe {
            let ns_window: *const AnyObject = msg_send![ns_view, window];
            if ns_window.is_null() {
                return;
            }
            let pane: *const AnyObject = &*self.tree_pane as *const BrowsePane as *const _;
            let made: bool = msg_send![ns_window, makeFirstResponder: pane];
            log::debug!("focus_tree: makeFirstResponder(tree pane) → {made}");
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
    use crate::platform::macos::ui_common::{FlippedView, as_view};

    unsafe {
        // ── Split view ───────────────────────────────────────────────────────────
        let split = NSSplitView::initWithFrame(
            NSSplitView::alloc(mtm),
            objc2_foundation::NSRect::default(),
        );
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

        // ── Left pane: scroll view wrapping a stub NSOutlineView ───────────────────
        let tree_pane = BrowsePane::new(mtm, PaneSide::Tree);
        let _: () = msg_send![&*tree_pane, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*tree_pane, setHasVerticalScroller: true];
        let _: () = msg_send![&*tree_pane, setDrawsBackground: false];
        let outline: Retained<AnyObject> = msg_send![objc2::class!(NSOutlineView), new];
        let _: () = msg_send![&*tree_pane, setDocumentView: &*outline];

        // ── Right pane: scroll view with a "(grid)" placeholder label ──────────────
        let grid_pane = BrowsePane::new(mtm, PaneSide::Grid);
        let _: () = msg_send![&*grid_pane, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*grid_pane, setHasVerticalScroller: true];
        let _: () = msg_send![&*grid_pane, setDrawsBackground: false];
        let grid_placeholder = FlippedView::new_as_nsview(mtm);
        let label = make_placeholder_label(mtm, "(grid)");
        grid_placeholder.addSubview(as_view::<objc2_app_kit::NSTextField>(&label));
        let _: () = msg_send![&*grid_pane, setDocumentView: &*grid_placeholder];

        // Add panes to the split view (order matters: index 0 = tree, 1 = grid; see
        // `focus_sibling_pane`).
        split.addSubview(as_view::<BrowsePane>(&tree_pane));
        split.addSubview(as_view::<BrowsePane>(&grid_pane));

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

        // `outline` and `grid_placeholder`/`label` are owned by the view hierarchy after
        // `setDocumentView` / `addSubview`, so their local handles drop at end of scope safely.
        log::debug!("Browse split view created (hidden)");

        BrowseSplitView {
            split,
            tree_pane,
            grid_pane,
        }
    }
}

/// A centered, non-editable placeholder label for the stub panes.
fn make_placeholder_label(
    mtm: MainThreadMarker,
    text: &str,
) -> Retained<objc2_app_kit::NSTextField> {
    use objc2_app_kit::{NSColor, NSTextField};
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    unsafe {
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let _: () = msg_send![&*label, setTranslatesAutoresizingMaskIntoConstraints: false];
    }
    label
}
