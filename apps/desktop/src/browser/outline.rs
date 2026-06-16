//! The browse-mode folder tree: a real `NSOutlineView` source list in the left pane.
//!
//! A one-column, directories-only `NSOutlineView` styled like Finder's sidebar
//! (`NSTableViewSelectionHighlightStyleSourceList` for the rounded-pill selection, inside an
//! `NSVisualEffectView` `.sidebar` material). Roots are the home folder plus every mounted
//! volume (flat sibling roots — see "Roots" below). Children are computed lazily per node and
//! cached. Selecting a folder fires `AppCommand::BrowseSelectFolder`.
//!
//! ## Item identity is load-bearing
//!
//! `NSOutlineView` tracks items by **pointer identity**: it calls `child:ofItem:` /
//! `isItemExpandable:` / `viewForTableColumn:item:` with the very pointers the data source
//! handed back earlier, and compares them by address. If the data source returned a fresh
//! object for the same logical node on each call, the outline view would think every node is
//! new and the tree would misbehave (wrong expansion state, lost selection, duplicate rows).
//!
//! So each node is a [`NodeObject`] (a tiny `NSObject` subclass holding the node's absolute
//! `PathBuf`), and the data source returns the **same** `Retained<NodeObject>` for the same
//! path across calls via a `RefCell<HashMap<PathBuf, Retained<NodeObject>>>` cache in the
//! delegate's ivars. `node_for_path` is the single place that mints-or-reuses a node.
//!
//! ## Roots: flat, not grouped
//!
//! Finder shows volumes under a "Locations" group-row header. A grouped source list needs the
//! data source to return group-row pseudo-items and implement `isGroupItem:`, which fights the
//! path-keyed node model (a group row has no path). We use **flat sibling roots** instead: the
//! home folder first, then each mounted volume as a top-level row. Simpler, identity-stable, and
//! reads cleanly. Grouped rows can come back in the styling phase if wanted.
//!
//! ## Keyboard is app-driven, not the responder chain
//!
//! winit keeps the keyboard even with the outline view up (see `docs/specs/image-browser.md` →
//! "Input architecture"), so arrow keys don't reach `NSOutlineView`'s own `keyDown:`. Instead
//! the app routes Up/Down/Left/Right through winit → `AppCommand` and drives the outline view
//! programmatically via [`BrowseTree`] (`move_selection`, `expand_selected`, `collapse_selected`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSImageView, NSOutlineView, NSScrollView, NSTableColumn, NSTableViewStyle, NSTextField, NSView,
    NSWorkspace,
};
use objc2_foundation::{NSNotification, NSString};

use super::tree_model;

// ─── NodeObject: the per-path item, pointer-identity stable ────────────────

/// Ivars for [`NodeObject`]: the node's absolute path. Stored as a `PathBuf` so the Rust side
/// can read it back without re-parsing an `NSString`.
struct NodeIvars {
    path: PathBuf,
}

define_class!(
    /// One tree node, identified by its absolute path. The `NSOutlineView` data source returns
    /// the same instance for the same path (see the module docs on item identity), so AppKit's
    /// pointer-identity tracking stays coherent.
    // SAFETY: NSObject has no subclassing requirements; this type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowseNode"]
    #[ivars = NodeIvars]
    struct NodeObject;

    unsafe impl NSObjectProtocol for NodeObject {}
);

impl NodeObject {
    fn new(mtm: MainThreadMarker, path: PathBuf) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(NodeIvars { path });
        unsafe { msg_send![super(this), init] }
    }

    fn path(&self) -> &Path {
        &self.ivars().path
    }
}

// ─── Data source + delegate ────────────────────────────────────────────────

/// Ivars for [`TreeDataSource`]: the roots, the node-identity cache, and the lazily-built
/// child-directory cache. All `RefCell` because AppKit calls the data source re-entrantly on
/// the main thread; no cross-thread sharing happens.
struct TreeDataSourceIvars {
    /// Top-level rows (home + volumes), in display order. Built once at construction.
    roots: Vec<tree_model::Root>,
    /// Path → node, so the same path always maps to the same `NodeObject` pointer.
    nodes: RefCell<HashMap<PathBuf, Retained<NodeObject>>>,
    /// Path → its child directories, computed on first `child:ofItem:` for that path. Avoids
    /// re-reading the directory on every outline-view query (it queries a lot during layout).
    children: RefCell<HashMap<PathBuf, Vec<PathBuf>>>,
}

define_class!(
    /// `NSOutlineView` data source + delegate for the folder tree. Owns the node-identity cache
    /// and lazy child enumeration; reports selection changes as `AppCommand`s.
    // SAFETY: NSObject subclass, no Drop. Touched only on the main thread (MainThreadOnly).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowseTreeDataSource"]
    #[ivars = TreeDataSourceIvars]
    struct TreeDataSource;

    unsafe impl NSObjectProtocol for TreeDataSource {}

    // ── NSOutlineViewDataSource ──
    impl TreeDataSource {
        /// Number of children of `item` (or of the root when `item` is null).
        #[unsafe(method(outlineView:numberOfChildrenOfItem:))]
        fn number_of_children(
            &self,
            _outline: &NSOutlineView,
            item: *mut AnyObject,
        ) -> isize {
            if item.is_null() {
                return self.ivars().roots.len() as isize;
            }
            let node = unsafe { &*(item as *const NodeObject) };
            self.children_of(node.path()).len() as isize
        }

        /// The `index`-th child of `item` (or root). Returns a stable `NodeObject` pointer.
        #[unsafe(method(outlineView:child:ofItem:))]
        fn child_of_item(
            &self,
            _outline: &NSOutlineView,
            index: isize,
            item: *mut AnyObject,
        ) -> *mut AnyObject {
            let child_path: Option<PathBuf> = if item.is_null() {
                self.ivars()
                    .roots
                    .get(index as usize)
                    .map(|r| r.path.clone())
            } else {
                let node = unsafe { &*(item as *const NodeObject) };
                self.children_of(node.path()).get(index as usize).cloned()
            };
            match child_path {
                // `node_for_path` returns a cached `Retained`; we hand AppKit a borrowed pointer.
                // The cache keeps the object alive for the data source's (window's) lifetime.
                Some(path) => Retained::as_ptr(&self.node_for_path(path)) as *mut AnyObject,
                None => std::ptr::null_mut(),
            }
        }

        /// Expandable iff the node has at least one child directory.
        #[unsafe(method(outlineView:isItemExpandable:))]
        fn is_item_expandable(
            &self,
            _outline: &NSOutlineView,
            item: *mut AnyObject,
        ) -> objc2::runtime::Bool {
            if item.is_null() {
                return objc2::runtime::Bool::YES;
            }
            let node = unsafe { &*(item as *const NodeObject) };
            objc2::runtime::Bool::new(!self.children_of(node.path()).is_empty())
        }

        /// The cell view for a row: a folder icon + the folder's display name.
        #[unsafe(method(outlineView:viewForTableColumn:item:))]
        fn view_for_item(
            &self,
            outline: &NSOutlineView,
            _column: *mut NSTableColumn,
            item: *mut AnyObject,
        ) -> *mut NSView {
            let node = unsafe { &*(item as *const NodeObject) };
            let mtm = MainThreadMarker::from(self);
            let view = self.make_cell(mtm, outline, node);
            Retained::into_raw(view)
        }
    }

    // ── NSOutlineViewDelegate ──
    impl TreeDataSource {
        /// Selection changed → record + log the folder via an `AppCommand`.
        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn selection_did_change(&self, notification: &NSNotification) {
            let outline: Retained<NSOutlineView> = unsafe {
                let obj: *mut AnyObject = msg_send![notification, object];
                Retained::retain(obj as *mut NSOutlineView).expect("notification has an object")
            };
            let row: isize = unsafe { msg_send![&*outline, selectedRow] };
            if row < 0 {
                return;
            }
            let item: *mut AnyObject = unsafe { msg_send![&*outline, itemAtRow: row] };
            if item.is_null() {
                return;
            }
            let node = unsafe { &*(item as *const NodeObject) };
            crate::commands::send_command(crate::commands::AppCommand::BrowseSelectFolder(
                node.path().to_path_buf(),
            ));
        }
    }
);

impl TreeDataSource {
    fn new(mtm: MainThreadMarker, roots: Vec<tree_model::Root>) -> Retained<Self> {
        let ivars = TreeDataSourceIvars {
            roots,
            nodes: RefCell::new(HashMap::new()),
            children: RefCell::new(HashMap::new()),
        };
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Mint-or-reuse the node for `path`. The returned `Retained` is cached, so repeated calls
    /// hand back the **same** object instance — the pointer-identity rule the outline view needs.
    fn node_for_path(&self, path: PathBuf) -> Retained<NodeObject> {
        let mut nodes = self.ivars().nodes.borrow_mut();
        nodes
            .entry(path.clone())
            .or_insert_with(|| {
                let mtm = MainThreadMarker::from(self);
                NodeObject::new(mtm, path)
            })
            .clone()
    }

    /// The child directories of `path`, computed once and cached.
    fn children_of(&self, path: &Path) -> Vec<PathBuf> {
        if let Some(cached) = self.ivars().children.borrow().get(path) {
            return cached.clone();
        }
        let computed = tree_model::child_directories(path);
        self.ivars()
            .children
            .borrow_mut()
            .insert(path.to_path_buf(), computed.clone());
        computed
    }

    /// Build the one-line cell view: a small folder icon + the folder's display name.
    fn make_cell(
        &self,
        mtm: MainThreadMarker,
        outline: &NSOutlineView,
        node: &NodeObject,
    ) -> Retained<NSView> {
        // Reuse a recycled cell if the outline view has one (cell reuse keeps scrolling cheap).
        let identifier = NSString::from_str("PrvwBrowseCell");
        let display_name = self.display_name(node.path());

        unsafe {
            let reused: *mut NSView =
                msg_send![outline, makeViewWithIdentifier: &*identifier, owner: self];
            let container: Retained<NSView> = if reused.is_null() {
                build_cell_container(mtm, &identifier)
            } else {
                Retained::retain(reused).expect("reused cell view")
            };

            // The container holds [icon, label] as its first two subviews (see `build_cell_container`).
            let subviews: Retained<objc2_foundation::NSArray<NSView>> =
                msg_send![&*container, subviews];
            let count: usize = msg_send![&*subviews, count];
            if count >= 2 {
                let icon_view: *mut NSImageView = msg_send![&*subviews, objectAtIndex: 0usize];
                let label: *mut NSTextField = msg_send![&*subviews, objectAtIndex: 1usize];

                let icon = folder_icon(node.path());
                let _: () = msg_send![icon_view, setImage: &*icon];

                let ns_name = NSString::from_str(&display_name);
                let _: () = msg_send![label, setStringValue: &*ns_name];
            }
            container
        }
    }

    /// The label for a node: the matching root's display name for a top-level row (so a volume
    /// shows its localized name, not the mount point's last component), else the folder name.
    fn display_name(&self, path: &Path) -> String {
        if let Some(root) = self.ivars().roots.iter().find(|r| r.path == path) {
            return root.name.clone();
        }
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }
}

/// Build a fresh cell container: a horizontal row of [folder icon, name label]. Identifier is
/// set so `makeViewWithIdentifier:` recycles it. Auto Layout pins the icon left, label after it.
fn build_cell_container(mtm: MainThreadMarker, identifier: &NSString) -> Retained<NSView> {
    use objc2_app_kit::{NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation};

    unsafe {
        let container = NSView::new(mtm);
        let _: () = msg_send![&*container, setIdentifier: identifier];

        let icon = NSImageView::new(mtm);
        let _: () = msg_send![&*icon, setTranslatesAutoresizingMaskIntoConstraints: false];

        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        label.setBordered(false);
        label.setDrawsBackground(false);
        label.setEditable(false);
        label.setSelectable(false);
        let _: () = msg_send![&*label, setTranslatesAutoresizingMaskIntoConstraints: false];

        let label_view: &NSView = as_nsview(&*label);
        container.addSubview(&icon);
        container.addSubview(label_view);

        let pin = |item: &AnyObject,
                   attr: NSLayoutAttribute,
                   to: &AnyObject,
                   to_attr: NSLayoutAttribute,
                   c: f64| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                item, attr, NSLayoutRelation::Equal, Some(to), to_attr, 1.0, c,
            )
            .setActive(true);
        };
        let icon_size = |item: &AnyObject| {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                item, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 16.0,
            )
            .setActive(true);
        };

        // Icon: leading edge, vertically centered, 16×16.
        pin(
            &icon,
            NSLayoutAttribute::Leading,
            &container,
            NSLayoutAttribute::Leading,
            2.0,
        );
        pin(
            &icon,
            NSLayoutAttribute::CenterY,
            &container,
            NSLayoutAttribute::CenterY,
            0.0,
        );
        icon_size(&icon);
        pin(
            &icon,
            NSLayoutAttribute::Height,
            &icon,
            NSLayoutAttribute::Width,
            0.0,
        );
        // Label: after the icon, vertically centered, to the trailing edge.
        pin(
            label_view,
            NSLayoutAttribute::Leading,
            &icon,
            NSLayoutAttribute::Trailing,
            6.0,
        );
        pin(
            label_view,
            NSLayoutAttribute::CenterY,
            &container,
            NSLayoutAttribute::CenterY,
            0.0,
        );
        pin(
            &container,
            NSLayoutAttribute::Trailing,
            label_view,
            NSLayoutAttribute::Trailing,
            4.0,
        );

        container
    }
}

/// The Finder folder icon for `path` (`NSWorkspace iconForFile:`). Called on the main thread
/// during outline-view layout.
fn folder_icon(path: &Path) -> Retained<objc2_app_kit::NSImage> {
    let ws = NSWorkspace::sharedWorkspace();
    let ns_path = NSString::from_str(&path.to_string_lossy());
    ws.iconForFile(&ns_path)
}

/// Upcast an AppKit control to `&NSView` (all controls are `#[repr(C)]` NSView subclasses).
unsafe fn as_nsview<T>(obj: &T) -> &NSView {
    unsafe { &*(obj as *const T as *const NSView) }
}

// ─── BrowseTree: the owned outline + scroll view ───────────────────────────

/// Owns the outline view, its scroll view, and the data source/delegate for the window's
/// lifetime. `BrowseSplitView` stores this so nothing drops early (autorelease-segfault rule).
/// Also the handle the app drives for keyboard navigation.
pub struct BrowseTree {
    scroll: Retained<NSScrollView>,
    outline: Retained<NSOutlineView>,
    /// Kept alive: the outline view holds the data source/delegate weakly (`assign`).
    _data_source: Retained<TreeDataSource>,
}

// SAFETY: all fields are main-thread-only AppKit objects, stored not shared across threads.
unsafe impl Send for BrowseTree {}

impl BrowseTree {
    /// Build the source-list outline view inside an `NSScrollView`. Returns the owner; the
    /// caller adds `scroll_view()` to the sidebar pane.
    pub fn create(mtm: MainThreadMarker) -> Self {
        let roots = tree_model::enumerate_roots();
        log::info!(
            "Browse tree roots: {}",
            roots
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let data_source = TreeDataSource::new(mtm, roots);

        unsafe {
            let outline = NSOutlineView::new(mtm);
            // Source-list look: rounded-pill selection, like Finder's sidebar. `setStyle:` with
            // `SourceList` is the modern replacement for the deprecated
            // `setSelectionHighlightStyle:` — it applies the source-list selection + inset.
            outline.setStyle(NSTableViewStyle::SourceList);
            let _: () = msg_send![&*outline, setHeaderView: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*outline, setRowSizeStyle: 1isize]; // NSTableViewRowSizeStyleSmall
            let _: () = msg_send![&*outline, setIndentationPerLevel: 14.0f64];
            let _: () = msg_send![&*outline, setFloatsGroupRows: false];
            // Transparent background so the sidebar vibrancy shows through.
            let _: () = msg_send![&*outline, setBackgroundColor: &*clear_color()];

            // One column, auto-sized to the outline width.
            let column = NSTableColumn::initWithIdentifier(
                NSTableColumn::alloc(mtm),
                &NSString::from_str("name"),
            );
            let _: () = msg_send![&*column, setEditable: false];
            outline.addTableColumn(&column);
            let _: () = msg_send![&*outline, setOutlineTableColumn: &*column];

            // Wire data source + delegate. The outline view holds them weakly, so the
            // `Retained` in `BrowseTree` is what keeps them alive.
            let ds_obj: &AnyObject = &data_source;
            let _: () = msg_send![&*outline, setDataSource: ds_obj];
            let _: () = msg_send![&*outline, setDelegate: ds_obj];

            // Scroll view host.
            let scroll = NSScrollView::new(mtm);
            let _: () = msg_send![&*scroll, setDrawsBackground: false];
            let _: () = msg_send![&*scroll, setHasVerticalScroller: true];
            let _: () = msg_send![&*scroll, setHasHorizontalScroller: false];
            let _: () = msg_send![&*scroll, setAutohidesScrollers: true];
            let _: () = msg_send![&*scroll, setBorderType: 0isize]; // NSNoBorder
            let outline_obj: &AnyObject = &outline;
            let _: () = msg_send![&*scroll, setDocumentView: outline_obj];

            BrowseTree {
                scroll,
                outline,
                _data_source: data_source,
            }
        }
    }

    /// The scroll view to add to the sidebar pane.
    pub fn scroll_view(&self) -> &NSScrollView {
        &self.scroll
    }

    /// Move the selection by `delta` (+1 Down, -1 Up) across the currently visible rows,
    /// clamped at the ends. App-driven (winit owns the keyboard); see the module docs.
    pub fn move_selection(&self, delta: i32) {
        unsafe {
            let count: isize = msg_send![&*self.outline, numberOfRows];
            let selected: isize = msg_send![&*self.outline, selectedRow];
            let current = if selected < 0 {
                None
            } else {
                Some(selected as usize)
            };
            let Some(next) = tree_model::next_selectable_row(current, count as usize, delta) else {
                return;
            };
            self.select_row(next);
        }
    }

    /// Expand the selected row (Right arrow), if it has children.
    pub fn expand_selected(&self) {
        unsafe {
            let row: isize = msg_send![&*self.outline, selectedRow];
            if row < 0 {
                return;
            }
            let item: *mut AnyObject = msg_send![&*self.outline, itemAtRow: row];
            if !item.is_null() {
                let _: () = msg_send![&*self.outline, expandItem: item];
            }
        }
    }

    /// Collapse the selected row (Left arrow). If the row is a leaf or already collapsed,
    /// collapse its parent instead so Left walks up the tree (Finder behavior).
    pub fn collapse_selected(&self) {
        unsafe {
            let row: isize = msg_send![&*self.outline, selectedRow];
            if row < 0 {
                return;
            }
            let item: *mut AnyObject = msg_send![&*self.outline, itemAtRow: row];
            if item.is_null() {
                return;
            }
            let expanded: bool = msg_send![&*self.outline, isItemExpanded: item];
            if expanded {
                let _: () = msg_send![&*self.outline, collapseItem: item];
            } else {
                let parent: *mut AnyObject = msg_send![&*self.outline, parentForItem: item];
                if !parent.is_null() {
                    let _: () = msg_send![&*self.outline, collapseItem: parent];
                    // Move selection to the parent we just collapsed.
                    let parent_row: isize = msg_send![&*self.outline, rowForItem: parent];
                    if parent_row >= 0 {
                        self.select_row(parent_row as usize);
                    }
                }
            }
        }
    }

    /// Select `row` and scroll it into view. Fires the selection-changed delegate, which
    /// records the folder.
    fn select_row(&self, row: usize) {
        unsafe {
            let index_set = index_set_with(row);
            let _: () = msg_send![&*self.outline, selectRowIndexes: &*index_set, byExtendingSelection: false];
            let _: () = msg_send![&*self.outline, scrollRowToVisible: row as isize];
        }
    }
}

/// An `NSIndexSet` containing the single index `i`.
fn index_set_with(i: usize) -> Retained<AnyObject> {
    unsafe {
        let cls = objc2::class!(NSIndexSet);
        msg_send![cls, indexSetWithIndex: i]
    }
}

/// A clear `NSColor` (transparent outline background so the sidebar vibrancy shows).
fn clear_color() -> Retained<objc2_app_kit::NSColor> {
    objc2_app_kit::NSColor::clearColor()
}
