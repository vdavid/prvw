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
//! ## Children load asynchronously — the data source never touches the disk
//!
//! Reading a directory on the main thread freezes winit's event loop (the whole app) whenever the
//! filesystem is slow — a stale SMB mount can block for ~10 s. So the data source serves children
//! **only from an in-memory cache** ([`tree_model::ChildCache`]) and never calls `read_dir` inline.
//! On a cache miss it marks the path in flight, enqueues a scan on a background [`TreeScanner`]
//! thread (`std::thread` + `mpsc`, the same pattern as `navigation::preloader` — no tokio), and
//! reports zero children for now. The scanner reads the directory off-thread and posts the result
//! back via `AppCommand::BrowseTreeChildrenLoaded`; the executor stores it in the cache and calls
//! `reloadItem:reloadChildren:` so the outline view re-queries the node. `isItemExpandable:`
//! assumes every directory is expandable (no disk read) until a scan proves it empty.
//!
//! ## Keyboard: the outline view holds first responder, arrows are native
//!
//! In idle-winit browse mode the focused native view holds the window's first responder (see
//! `docs/specs/image-browser.md` → "Input architecture"), so the outline view handles its own
//! arrows/expand/collapse/type-select natively. The [`BrowseOutlineView`] subclass overrides
//! `keyDown:` to intercept only Tab/Enter/Esc (routed via `AppCommand`) and calls `super` for
//! everything else, so native row navigation stays immediate. `apply_focus`
//! (`BrowseTree::make_first_responder`) is what keeps the outline view first responder when the
//! tree pane is focused — which also gives it accent-blue source-list selection emphasis for free.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSEvent, NSImageView, NSOutlineView, NSScrollView, NSTableColumn, NSTableViewStyle,
    NSTextField, NSView, NSWorkspace,
};
use objc2_foundation::{NSNotification, NSRect, NSString};

use super::tree_model::{self, ChildCache};

// ─── BrowseOutlineView: keyDown override for Tab/Enter/Esc ──────────────────

define_class!(
    /// `NSOutlineView` subclass whose `keyDown:` intercepts only Tab/Enter/Esc (routed via
    /// `AppCommand`); every other key falls through to `super` so native row navigation,
    /// expand/collapse, and type-select stay immediate. The tree pane's first responder is this
    /// view (synced by `apply_focus`), so its `keyDown:` fires for browse keys.
    // SAFETY: NSOutlineView subclass, no Drop, no ivars. Main-thread only.
    #[unsafe(super(NSOutlineView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowseOutlineView"]
    struct BrowseOutlineView;

    unsafe impl NSObjectProtocol for BrowseOutlineView {}

    impl BrowseOutlineView {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key_code: u16 = unsafe { msg_send![event, keyCode] };
            if let Some(command) = super::browse_keydown_command(key_code) {
                log::debug!("Browse tree keyDown intercepted key_code={key_code}");
                crate::commands::send_command(command);
            } else {
                unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                }
            }
        }
    }
);

impl BrowseOutlineView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// ─── Background scanner: directory I/O off the main thread ─────────────────

/// A background directory scanner. Owns a single `std::thread` (the same pattern as
/// `navigation::preloader`: an OS thread + an `mpsc` channel, no tokio) that reads directories so
/// the main thread never blocks on a slow filesystem. Each request is a path; the worker computes
/// its child directories and posts them back to the main thread via the global `EventLoopProxy`
/// as `AppCommand::BrowseTreeChildrenLoaded`.
struct TreeScanner {
    request_tx: mpsc::Sender<PathBuf>,
}

impl TreeScanner {
    /// Spawn the scanner worker. It runs until the `Sender` (held by the data source, alive for
    /// the window's life) drops, closing the channel and ending the loop.
    fn start() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("prvw-tree-scan".into())
            .spawn(move || {
                while let Ok(path) = request_rx.recv() {
                    let children = tree_model::child_directories(&path);
                    log::debug!(
                        "Tree scan done: {} ({} subdir(s))",
                        path.display(),
                        children.len()
                    );
                    // Post back to the main thread. `send_command` uses the global proxy set in
                    // `resumed()`; if it's gone the app is shutting down and we just drop the work.
                    crate::commands::send_command(
                        crate::commands::AppCommand::BrowseTreeChildrenLoaded { path, children },
                    );
                }
                log::debug!("Tree scanner worker exiting");
            })
            .expect("Failed to spawn tree scanner worker thread");
        log::info!("Tree scanner started (dedicated OS thread)");
        TreeScanner { request_tx }
    }

    /// Enqueue a directory scan. Fire-and-forget; the result comes back as an `AppCommand`.
    fn scan(&self, path: PathBuf) {
        if self.request_tx.send(path).is_err() {
            log::warn!("Tree scanner worker is gone — dropping scan request");
        }
    }
}

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

/// Ivars for [`TreeDataSource`]: the roots, the node-identity cache, the async child-load state
/// machine, and the background scanner. All `RefCell` because AppKit calls the data source
/// re-entrantly on the main thread; no cross-thread sharing happens (the scanner talks back via
/// the global `EventLoopProxy`, not by sharing these).
struct TreeDataSourceIvars {
    /// Top-level rows (home + volumes), in display order. Built once at construction.
    roots: Vec<tree_model::Root>,
    /// Path → node, so the same path always maps to the same `NodeObject` pointer.
    nodes: RefCell<HashMap<PathBuf, Retained<NodeObject>>>,
    /// Per-path child-directory load state (`NotLoaded` → `InFlight` → `Loaded`). The data source
    /// serves children only from here and NEVER reads a directory inline — a slow filesystem on
    /// the main thread would freeze the app. A miss enqueues a background scan via `scanner`.
    children: RefCell<ChildCache>,
    /// Background directory scanner (its own OS thread). Kept alive for the data source's life.
    scanner: TreeScanner,
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
            // Serve from cache only; a miss enqueues a background scan and reports 0 for now. The
            // `reloadItem:reloadChildren:` after the scan completes re-queries us with the count.
            self.loaded_or_request(node.path())
                .map_or(0, |c| c.len() as isize)
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
                // Loaded by the time AppKit asks for a specific child (it only asks for indices
                // `number_of_children` reported, and that's non-zero only once loaded).
                self.loaded_children(node.path())
                    .and_then(|c| c.get(index as usize).cloned())
            };
            match child_path {
                // `node_for_path` returns a cached `Retained`; we hand AppKit a borrowed pointer.
                // The cache keeps the object alive for the data source's (window's) lifetime.
                Some(path) => Retained::as_ptr(&self.node_for_path(path)) as *mut AnyObject,
                None => std::ptr::null_mut(),
            }
        }

        /// Expandable without reading the disk. Every directory is assumed expandable until a
        /// scan proves otherwise — reading the dir here to count children would block the main
        /// thread on slow filesystems (the freeze we're avoiding). A disclosure triangle that
        /// opens to nothing on a truly-empty dir is fine. Once the scan has loaded, we report the
        /// real answer (so an empty folder loses its triangle on the reload).
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
            // Derive from cache when present; assume expandable (YES) on a miss/in-flight so the
            // triangle shows. No disk read on any path.
            let expandable = match self.loaded_children(node.path()) {
                Some(children) => !children.is_empty(),
                None => true,
            };
            objc2::runtime::Bool::new(expandable)
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
            children: RefCell::new(ChildCache::new()),
            scanner: TreeScanner::start(),
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

    /// The source-list roots (home + volumes), for computing a reveal path chain.
    fn roots(&self) -> &[tree_model::Root] {
        &self.ivars().roots
    }

    /// The loaded child directories of `path`, or `None` if a scan hasn't finished yet (a miss or
    /// still in flight). Read-only — never reads the disk and never starts a scan.
    fn loaded_children(&self, path: &Path) -> Option<Vec<PathBuf>> {
        self.ivars()
            .children
            .borrow()
            .loaded(path)
            .map(<[PathBuf]>::to_vec)
    }

    /// Like [`loaded_children`](Self::loaded_children), but on a cache miss it marks the path in
    /// flight and enqueues a background scan (so the result comes back via
    /// `AppCommand::BrowseTreeChildrenLoaded`). Returns the children only if already loaded; never
    /// reads the disk on the main thread.
    fn loaded_or_request(&self, path: &Path) -> Option<Vec<PathBuf>> {
        let mut cache = self.ivars().children.borrow_mut();
        if let Some(children) = cache.loaded(path) {
            return Some(children.to_vec());
        }
        // Miss or in flight: ask the cache to start a scan (no-op if already in flight).
        if cache.begin_scan(path, Instant::now()) {
            log::debug!("Tree scan queued: {}", path.display());
            self.ivars().scanner.scan(path.to_path_buf());
        }
        None
    }

    /// Store a finished scan's children. Called from the executor on
    /// `AppCommand::BrowseTreeChildrenLoaded` before it reloads the node. Returns the node so the
    /// caller can hand it to `reloadItem:reloadChildren:` (or `None` if the path isn't a known
    /// node — e.g. a stale scan after the tree changed).
    fn complete_scan(&self, path: &Path, children: Vec<PathBuf>) -> Option<Retained<NodeObject>> {
        self.ivars()
            .children
            .borrow_mut()
            .complete_scan(path, children);
        // Only return a node if we already minted one for this path (roots and expanded folders
        // have one). A path we've never shown has no node and needs no reload.
        self.ivars().nodes.borrow().get(path).cloned()
    }

    /// The earliest still-in-flight scan start time, for the loading-overlay timer. `None` when no
    /// scan is pending.
    fn earliest_in_flight(&self) -> Option<Instant> {
        self.ivars().children.borrow().earliest_in_flight()
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

/// An in-progress "reveal a path" walk. Browse-open positioning must expand the tree from a root
/// down through every ancestor to the current image's folder, then select it — but child
/// directories load **asynchronously** (the data source never reads the disk on the main thread).
/// So the walk can't expand synchronously; it advances one level each time a
/// `BrowseTreeChildrenLoaded` for the level it's waiting on arrives.
///
/// `chain` is the root-to-target path list from `tree_model::reveal_path_chain`; `position` is the
/// index in `chain` whose children the walk is currently waiting on. When `chain[position]`'s
/// children arrive, the walk expands `chain[position + 1]` (triggering its scan) and advances; at
/// the last element it selects + scrolls the target and the reveal is done.
struct RevealWalk {
    chain: Vec<PathBuf>,
    position: usize,
}

/// Owns the outline view, its scroll view, and the data source/delegate for the window's
/// lifetime. `BrowseSplitView` stores this so nothing drops early (autorelease-segfault rule).
/// Also the handle the app drives for keyboard navigation.
pub struct BrowseTree {
    scroll: Retained<NSScrollView>,
    outline: Retained<BrowseOutlineView>,
    /// Kept alive: the outline view holds the data source/delegate weakly (`assign`).
    _data_source: Retained<TreeDataSource>,
    /// The in-progress async reveal walk (browse-open positioning), or `None` when idle. Advanced
    /// by `children_loaded` as each ancestor's scan completes. `RefCell` because the reveal is
    /// driven from `&self` methods on the main thread (no cross-thread sharing).
    reveal: RefCell<Option<RevealWalk>>,
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
            let outline = BrowseOutlineView::new(mtm);
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
                reveal: RefCell::new(None),
            }
        }
    }

    /// The scroll view to add to the sidebar pane.
    pub fn scroll_view(&self) -> &NSScrollView {
        &self.scroll
    }

    /// Apply a completed background scan: store the children in the cache, then reload that node so
    /// the outline view re-queries it and shows the rows. Called from the executor on
    /// `AppCommand::BrowseTreeChildrenLoaded`. A `None` node (path never shown, or a stale scan)
    /// just updates the cache with no UI reload needed. After the reload, advances any in-progress
    /// reveal walk that was waiting on this path's children (browse-open positioning).
    pub fn children_loaded(&self, path: &Path, children: Vec<PathBuf>) {
        if let Some(node) = self._data_source.complete_scan(path, children) {
            unsafe {
                // `reloadItem:reloadChildren:` re-queries `numberOfChildrenOfItem:` /
                // `child:ofItem:` for this node (now served from the freshly-loaded cache),
                // redrawing its subtree.
                let item: *const AnyObject = Retained::as_ptr(&node).cast();
                let _: () = msg_send![&*self.outline, reloadItem: item, reloadChildren: true];
            }
        }
        // Even a scan with no live node (e.g. the root's first scan before it's expanded) can be
        // the level a reveal is waiting on — advance regardless of the reload above.
        self.advance_reveal(path);
    }

    /// Reveal `folder` in the tree: compute the root-to-folder chain from the tree's own roots
    /// (`tree_model::reveal_path_chain`) and start the async walk. No-op when `folder` is under no
    /// root (nothing to reveal). The public entry the app calls on browse-open / dir-arg launch.
    pub fn reveal_to_folder(&self, folder: &Path) {
        let Some(chain) = tree_model::reveal_path_chain(self._data_source.roots(), folder) else {
            log::debug!("Tree reveal: {} is under no root, skipping", folder.display());
            return;
        };
        self.reveal_path(chain);
    }

    /// Start an async walk that expands the tree from the containing root down through every
    /// ancestor to `chain`'s last element (the target folder), then selects + scrolls it. `chain`
    /// is `tree_model::reveal_path_chain`'s root-to-target list. Because children load
    /// asynchronously, this only kicks off the first level; `children_loaded` advances it as each
    /// scan completes. A single-element chain (target is a root) selects immediately. An empty
    /// chain is a no-op. Replaces any in-progress reveal.
    pub fn reveal_path(&self, chain: Vec<PathBuf>) {
        if chain.is_empty() {
            return;
        }
        log::debug!(
            "Tree reveal start: {} level(s), target {}",
            chain.len(),
            chain.last().map(|p| p.display().to_string()).unwrap_or_default()
        );
        // The target is the last element. If the chain is just the root, it's already a top-level
        // row — select it now, no expansion needed.
        if chain.len() == 1 {
            self.select_and_scroll_to(&chain[0]);
            return;
        }
        *self.reveal.borrow_mut() = Some(RevealWalk {
            chain,
            position: 0,
        });
        // Kick off the first level: expand the root so its children scan (or, if already loaded,
        // advance straight through).
        let first = self.reveal.borrow().as_ref().map(|w| w.chain[0].clone());
        if let Some(first) = first {
            self.expand_or_advance(&first);
        }
    }

    /// Advance the reveal walk when `loaded_path`'s children have arrived. No-op when no reveal is
    /// active or the loaded level isn't the one the walk is waiting on (a stale or unrelated scan).
    /// Steps the position forward and expands the next ancestor (which triggers its scan); the
    /// terminal target select happens in `expand_or_advance`.
    fn advance_reveal(&self, loaded_path: &Path) {
        let next = {
            let mut guard = self.reveal.borrow_mut();
            let Some(walk) = guard.as_mut() else {
                return;
            };
            if walk.chain.get(walk.position).map(PathBuf::as_path) != Some(loaded_path) {
                return; // Not the level we're waiting on (stale/unrelated scan).
            }
            // The walk only ever waits on a non-target level (the target is never expanded, so its
            // scan never fires), so there's always a next level here. Step to it.
            walk.position += 1;
            walk.chain.get(walk.position).cloned()
        };
        if let Some(next) = next {
            self.expand_or_advance(&next);
        } else {
            // Defensive: a scan landed for the target itself (only if something expanded it). Just
            // finish the reveal on the target.
            let target = self.reveal.borrow().as_ref().and_then(|w| w.chain.last().cloned());
            if let Some(target) = target {
                self.select_and_scroll_to(&target);
            }
            *self.reveal.borrow_mut() = None;
        }
    }

    /// One step of the reveal walk for `path`: if it's the target (last element), select + scroll
    /// and finish. Otherwise expand it so the outline view queries its children (firing the async
    /// scan); if those children are already cached (a re-reveal), advance immediately so the walk
    /// doesn't stall waiting for a scan that won't re-fire.
    fn expand_or_advance(&self, path: &Path) {
        let is_target = self
            .reveal
            .borrow()
            .as_ref()
            .is_some_and(|w| w.chain.last().map(PathBuf::as_path) == Some(path));
        if is_target {
            self.select_and_scroll_to(path);
            *self.reveal.borrow_mut() = None;
            return;
        }
        // Expand the node so AppKit queries `numberOfChildrenOfItem:`, which enqueues the scan via
        // `loaded_or_request` (no disk read on the main thread). `expandItem:` mints/reuses the
        // node through the data source's identity cache.
        self.expand_node(path);
        // If this level's children are already loaded (the data source served them synchronously),
        // no `BrowseTreeChildrenLoaded` will arrive to advance us — step now.
        if self._data_source.loaded_children(path).is_some() {
            self.advance_reveal(path);
        }
    }

    /// Expand the tree node for `path` (programmatic disclosure). Reuses the data source's
    /// identity-stable node so the outline view tracks it correctly.
    fn expand_node(&self, path: &Path) {
        let node = self._data_source.node_for_path(path.to_path_buf());
        unsafe {
            let item: *const AnyObject = Retained::as_ptr(&node).cast();
            let _: () = msg_send![&*self.outline, expandItem: item];
        }
    }

    /// Select the row for `path` and scroll it to roughly mid-view. Used at the end of a reveal
    /// walk. The selection fires `outlineViewSelectionDidChange:` → `BrowseSelectFolder`, which
    /// lists the folder's images in the grid (so the reveal drives the grid listing). Scrolls via
    /// the enclosing scroll view's clip so the row lands centered rather than just barely visible.
    fn select_and_scroll_to(&self, path: &Path) {
        let node = self._data_source.node_for_path(path.to_path_buf());
        unsafe {
            let item: *const AnyObject = Retained::as_ptr(&node).cast();
            let row: isize = msg_send![&*self.outline, rowForItem: item];
            if row < 0 {
                log::debug!("Tree reveal: row not found for {}", path.display());
                return;
            }
            // Select the row (no row in an NSIndexSet helper — build one). `selectRowIndexes:` with
            // a single index, not extending the selection.
            let index_set = objc2_foundation::NSIndexSet::indexSetWithIndex(row as usize);
            let _: () =
                msg_send![&*self.outline, selectRowIndexes: &*index_set, byExtendingSelection: false];
            self.scroll_row_to_middle(row);
            log::debug!("Tree reveal done: selected row {row} for {}", path.display());
        }
    }

    /// Scroll so `row` sits roughly mid-view: center its rect in the scroll view's clip rather than
    /// the default `scrollRowToVisible:` (which only scrolls just enough to make it visible).
    fn scroll_row_to_middle(&self, row: isize) {
        unsafe {
            let row_rect: NSRect = msg_send![&*self.outline, rectOfRow: row];
            let clip_height: f64 = {
                let clip: *const AnyObject = msg_send![&*self.scroll, contentView];
                if clip.is_null() {
                    // Fall back to a plain "make visible" if there's no clip view.
                    let _: () = msg_send![&*self.outline, scrollRowToVisible: row];
                    return;
                }
                let bounds: NSRect = msg_send![clip, bounds];
                bounds.size.height
            };
            // Target origin centers the row's rect in the visible clip height, clamped at 0.
            let target_y = (row_rect.origin.y + row_rect.size.height / 2.0 - clip_height / 2.0)
                .max(0.0);
            let point = objc2_foundation::NSPoint::new(0.0, target_y);
            let clip: *const AnyObject = msg_send![&*self.scroll, contentView];
            let _: () = msg_send![clip, scrollToPoint: point];
            let _: () = msg_send![&*self.scroll, reflectScrolledClipView: clip];
        }
    }

    /// The earliest still-in-flight scan start time, for the loading-overlay timer. `None` when no
    /// scan is pending (overlay stays hidden).
    pub fn earliest_in_flight_scan(&self) -> Option<Instant> {
        self._data_source.earliest_in_flight()
    }

    /// Make the outline view the window's first responder. Called by `apply_focus` when the tree
    /// pane is focused: the outline view then handles its own arrows/expand/collapse/type-select
    /// natively, and a source list draws accent-blue selection emphasis while it's first responder
    /// (gray otherwise) — so tree emphasis follows focus for free.
    pub fn make_first_responder(&self) {
        unsafe {
            let window: *const AnyObject = msg_send![&*self.outline, window];
            if window.is_null() {
                return;
            }
            let outline_obj: *const AnyObject = Retained::as_ptr(&self.outline).cast();
            let accepted: bool = msg_send![window, makeFirstResponder: outline_obj];
            log::debug!("Tree make_first_responder accepted={accepted}");
        }
    }
}

/// A clear `NSColor` (transparent outline background so the sidebar vibrancy shows).
fn clear_color() -> Retained<objc2_app_kit::NSColor> {
    objc2_app_kit::NSColor::clearColor()
}
