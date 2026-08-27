//! The browse-mode thumbnail grid: a real `NSCollectionView` gallery in the right pane.
//!
//! A vertically-scrolling `NSCollectionView` (flow layout) of the selected folder's images, each
//! cell an `NSImageView` showing a QuickLook thumbnail plus a filename label. Mirrors the tree's
//! discipline: **nothing reads the disk on the main thread** (folder listing runs on a background
//! worker — `grid_listing`), and **thumbnails ride the previews' `QLThumbnailGenerator` worker**
//! (a second `quicklook::RequestTable` into the same shared `quicklookd` cache, delivering RGBA8
//! that the main thread wraps in an `NSImage` via `quicklook::nsimage_from_rgba8`).
//!
//! ## Ownership and where the mutable state lives
//!
//! `NSCollectionView` holds its data source/delegate weakly (`assign`), so [`BrowseGrid`] keeps the
//! `Retained<GridDataSource>` alive for the window's life (autorelease-segfault rule). All the
//! grid's mutable state — the [`grid_model::GridModel`] (folder image list + selection + folder
//! generation), the visible-range [`grid_scheduler::Scheduler`], the byte-budget
//! [`thumbnail_cache::ThumbnailCache`], the generated `NSImage` map, and the
//! `quicklook::RequestTable` — lives in `RefCell` ivars on `GridDataSource`, because both the
//! AppKit callbacks (`&self`) and the app-driving methods on `BrowseGrid` need it and it's all
//! main-thread-only. `BrowseGrid`'s methods delegate into the data source.
//!
//! ## Thumbnail flow
//!
//! 1. The tree selects a folder → `BrowseSelectFolder` → the executor calls
//!    [`BrowseGrid::list_folder`], which enqueues a background listing.
//! 2. The listing returns via `BrowseFolderListed` → [`BrowseGrid::folder_listed`] populates the
//!    model, reseeds the scheduler/cache on the visible range, and `reloadData`s the collection
//!    view. An empty folder shows the "(No images)" overlay.
//! 3. On scroll (and after a reload), `BrowseGrid::pump_visible_range` feeds the visible
//!    range to the scheduler + cache and pumps the scheduler into the QL worker at
//!    `GRID_THUMBNAIL_PX`.
//! 4. Completions arrive as `BrowseThumbnailsAvailable`; [`BrowseGrid::thumbnails_available`]
//!    drops stale-generation deliveries, builds the `NSImage`, stores it in the map + cache,
//!    feeds `evict_to_budget`'s returned indices to `Scheduler::uncache` (dropping their
//!    `NSImage`s), and reloads the affected items so cells pick up their image.
//!
//! ## Keyboard: the collection view holds first responder, arrows are native
//!
//! In idle-winit browse mode the focused native view holds the window's first responder, so the
//! collection view handles its own arrow selection + scroll natively. The [`BrowseCollectionView`]
//! subclass overrides `keyDown:` to intercept only Tab/Enter/Esc (routed via `AppCommand`) and
//! calls `super` for everything else. `apply_focus` (`BrowseGrid::make_first_responder`) keeps the
//! collection view first responder when the grid pane is focused.
//!
//! ## Selection emphasis follows focus
//!
//! A selected [`GridItem`] draws a rounded rect: accent-blue when the grid is the focused pane
//! (the collection view is first responder), gray when selected but unfocused, nothing when not
//! selected. The item reads its own first-responder state at paint time; `refresh_focus_emphasis`
//! repaints visible selected items on a Tab focus flip. Single-click selects instantly (the open
//! gesture is detected in the item's `mouseDown:` via `clickCount == 2`, so no click-delay).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSCollectionView, NSCollectionViewFlowLayout, NSCollectionViewItem,
    NSCollectionViewScrollDirection, NSCollectionViewScrollPosition, NSColor, NSEvent,
    NSImageScaling, NSImageView, NSIndexPathNSCollectionViewAdditions, NSScrollView, NSTextField,
    NSView,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexPath, NSInteger, NSPoint, NSRect, NSSet, NSSize, NSString,
};

use super::grid_listing::FolderLister;
use super::grid_model::{self, GridModel};
use super::grid_scheduler::Scheduler;
use super::thumbnail_cache::{self, GRID_THUMBNAIL_PX, ThumbnailCache};
use crate::navigation::SortBy;
use crate::previews::quicklook::{self, RequestTable};
use crate::previews::request::SubmitRequest;

// ─── Styling constants (tweak these for the gallery look) ───────────────────
// All in logical points. The cell-size slider is a later phase; these tune the
// fixed ~160pt gallery so it reads like Finder/Photos. The cell is a vertical stack:
// a square thumbnail on top, the filename label below.

/// Cell side in points. A sensible fixed gallery size; the live-resize slider is a later phase.
const CELL_PT: f64 = 168.0;
// `CELL_IMAGE_PT` / `CELL_LABEL_PT` are only referenced from `GridItem::loadView`, an AppKit
// override — `GridItem` is instantiated by the collection view (via `makeItemWithIdentifier:`),
// never constructed in Rust, so the dead-code lint can't see the use. Genuinely used at runtime.
/// The image-view side inside a cell, leaving room below for the filename label.
#[allow(dead_code)]
const CELL_IMAGE_PT: f64 = 140.0;
/// Height reserved for the filename label under the thumbnail.
#[allow(dead_code)]
const CELL_LABEL_PT: f64 = 18.0;
/// Vertical gap between the thumbnail and its filename label.
#[allow(dead_code)]
const CELL_IMAGE_LABEL_GAP_PT: f64 = 6.0;
/// Filename label point size (small system font, like Finder icon-view labels).
#[allow(dead_code)]
const CELL_LABEL_FONT_PT: f64 = 11.0;
/// Spacing between cells (both axes). A touch more than the section inset gives the
/// grid comfortable, Photos-like breathing room.
const CELL_SPACING: f64 = 16.0;
/// Section inset around the whole grid (inside the gallery surface).
const SECTION_INSET_PT: f64 = 14.0;

/// Corner radius of a cell's selection ring.
const SELECTION_CORNER_RADIUS: f64 = 8.0;
/// Padding between the cell's content bounds and its selection ring, so the ring frames
/// the thumbnail + label with a little air rather than hugging the cell edge.
#[allow(dead_code)]
const SELECTION_INSET_PT: f64 = 2.0;
/// Alpha applied to the accent fill of a focused-pane selection (softens the saturated blue).
const SELECTION_FOCUSED_ALPHA: f64 = 0.85;

/// Point size of the centered "(No images)" empty-state label.
const EMPTY_LABEL_FONT_PT: f64 = 15.0;

/// Reuse identifier for grid items.
const ITEM_IDENTIFIER: &str = "PrvwGridItem";

/// How many items ahead/behind the visible range the prefetcher warms. One screen's worth is a
/// good default; the scheduler's own `MARGIN` is the hard cap on generation.
const PREFETCH_MARGIN: usize = 24;

// ─── BrowseCollectionView: keyDown override for Tab/Enter/Esc ───────────────

define_class!(
    /// `NSCollectionView` subclass whose `keyDown:` intercepts only Tab/Enter/Esc (routed via
    /// `AppCommand`); every other key falls through to `super` so native arrow selection + scroll
    /// stay immediate. The grid pane's first responder is this view (synced by `apply_focus`), so
    /// its `keyDown:` fires for browse keys.
    // SAFETY: NSCollectionView subclass, no Drop, no ivars. Main-thread only.
    #[unsafe(super(NSCollectionView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwBrowseCollectionView"]
    struct BrowseCollectionView;

    unsafe impl NSObjectProtocol for BrowseCollectionView {}

    impl BrowseCollectionView {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key_code: u16 = unsafe { msg_send![event, keyCode] };
            if let Some(command) = super::browse_keydown_command(key_code) {
                log::debug!("Browse grid keyDown intercepted key_code={key_code}");
                crate::commands::send_command(command);
            } else {
                unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                }
            }
        }
    }
);

impl BrowseCollectionView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// ─── GridItem: the NSCollectionViewItem subclass ───────────────────────────

define_class!(
    /// One grid cell: an `NSImageView` (proportional scaling) with a filename label below it. We
    /// override `loadView` to build the view tree programmatically (no nib), and set
    /// `imageView`/`textField` so the base `NSCollectionViewItem` manages selection highlighting.
    // SAFETY: NSCollectionViewItem subclass, no Drop. Main-thread only.
    #[unsafe(super(NSCollectionViewItem))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwGridItem"]
    struct GridItem;

    unsafe impl NSObjectProtocol for GridItem {}

    impl GridItem {
        /// Build the cell's view tree. `NSViewController` calls this lazily the first time `view`
        /// is read. We make a flipped container holding a centered image view and a label.
        #[unsafe(method(loadView))]
        fn load_view(&self) {
            use crate::platform::macos::ui_common::FlippedView;
            let mtm = MainThreadMarker::from(self);
            // Flipped container (Y=0 at top) so the thumbnail-on-top, label-below layout reads
            // top-down like the visual order, matching the rest of our AppKit UI.
            let container = FlippedView::new_as_nsview(mtm);
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CELL_PT, CELL_PT));
            unsafe {
                let _: () = msg_send![&*container, setFrame: frame];
                let _: () = msg_send![&*container, setWantsLayer: true];
            }

            // Image view: a centered square at the top, scales proportionally up or down.
            let image_view = NSImageView::new(mtm);
            image_view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            let image_frame = NSRect::new(
                NSPoint::new((CELL_PT - CELL_IMAGE_PT) / 2.0, 0.0),
                NSSize::new(CELL_IMAGE_PT, CELL_IMAGE_PT),
            );
            unsafe {
                let _: () = msg_send![&*image_view, setFrame: image_frame];
                let iv_mask = 2u64 | 16u64; // width-sizable | height-sizable
                let _: () = msg_send![&*image_view, setAutoresizingMask: iv_mask];
            }
            container.addSubview(&image_view);

            // Filename label below the thumbnail, centered, single line, middle-truncating.
            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setBordered(false);
            label.setDrawsBackground(false);
            label.setEditable(false);
            label.setSelectable(false);
            label.setAlignment(objc2_app_kit::NSTextAlignment(2)); // center
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            label.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(CELL_LABEL_FONT_PT)));
            let label_frame = NSRect::new(
                NSPoint::new(0.0, CELL_IMAGE_PT + CELL_IMAGE_LABEL_GAP_PT),
                NSSize::new(CELL_PT, CELL_LABEL_PT),
            );
            unsafe {
                let _: () = msg_send![&*label, setFrame: label_frame];
                let _: () = msg_send![&*label, setLineBreakMode: 5isize]; // truncate middle
                let lbl_mask = 2u64 | 8u64; // width-sizable | min-Y-margin-flexible (pin to top)
                let _: () = msg_send![&*label, setAutoresizingMask: lbl_mask];
            }
            container.addSubview(&label);

            unsafe {
                let _: () = msg_send![self, setView: &*container];
                let _: () = msg_send![self, setImageView: &*image_view];
                let _: () = msg_send![self, setTextField: &*label];
            }
        }

        /// Repaint the selection emphasis when AppKit sets `isSelected` (on click / programmatic
        /// selection). Blue when also focused, gray when selected-but-unfocused, none otherwise —
        /// the focus state is read live from the collection view's first responder.
        #[unsafe(method(setSelected:))]
        fn set_selected(&self, selected: bool) {
            unsafe {
                let _: () = msg_send![super(self), setSelected: selected];
            }
            self.refresh_emphasis();
        }

        /// Detect a double-click here (instead of a click gesture recognizer, which delays the
        /// single click ~600 ms to disambiguate). A single click selects instantly via `super`'s
        /// native handling; a double-click also fires the open command. Routed to `super` either
        /// way so native selection still happens.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let click_count: isize = unsafe { msg_send![event, clickCount] };
            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
            if click_count == 2 {
                crate::commands::send_command(crate::commands::AppCommand::BrowseOpenSelected);
            }
        }

        /// ObjC entry point for `BrowseGrid::refresh_focus_emphasis` to repaint a visible cell on a
        /// Tab focus flip (dispatched by selector since `visibleItems` yields base
        /// `NSCollectionViewItem`s). Delegates to the Rust `refresh_emphasis`.
        #[unsafe(method(refreshEmphasis))]
        fn refresh_emphasis_objc(&self) {
            self.refresh_emphasis();
        }
    }
);

impl GridItem {
    /// Repaint the selection emphasis to match `isSelected` and the grid's focus. Called from
    /// `setSelected:` (AppKit) and from `BrowseGrid::refresh_focus_emphasis` on a focus flip.
    /// `#[allow(dead_code)]`: reached only via the AppKit override + the grid's refresh, which the
    /// lint can't see.
    #[allow(dead_code)]
    fn refresh_emphasis(&self) {
        let selected: bool = unsafe { msg_send![self, isSelected] };
        let focused = self.grid_pane_is_focused();
        self.apply_selection_style(selected, focused);
    }

    /// Whether the grid pane is the focused pane — read from the data source's state-driven
    /// `focused` flag (mirrored from `browser::State::focused_pane`), NOT from the native first
    /// responder. Inferring focus from `window.firstResponder` is racy during a click→command
    /// focus flip (the `BrowseSelectFolder` dispatch is async), which left the grid drawn blue
    /// after a tree click. State is the single source of truth (see `grid.rs` module docs).
    fn grid_pane_is_focused(&self) -> bool {
        unsafe {
            let cv: *const AnyObject = msg_send![self, collectionView];
            if cv.is_null() {
                return false;
            }
            let ds: *const AnyObject = msg_send![cv, dataSource];
            if ds.is_null() {
                return false;
            }
            let responds: bool = msg_send![ds, respondsToSelector: sel!(gridPaneIsFocused)];
            if !responds {
                return false;
            }
            msg_send![ds, gridPaneIsFocused]
        }
    }

    /// Draw the selection as a rounded rect: accent-blue when selected AND focused, gray when
    /// selected AND not focused, transparent when not selected (no indicator).
    fn apply_selection_style(&self, selected: bool, focused: bool) {
        let view: Option<Retained<NSView>> = unsafe { msg_send![self, view] };
        let Some(view) = view else { return };
        let Some(layer) = view.layer() else { return };
        let color = if !selected {
            NSColor::clearColor()
        } else if focused {
            NSColor::selectedContentBackgroundColor()
                .colorWithAlphaComponent(SELECTION_FOCUSED_ALPHA)
        } else {
            NSColor::unemphasizedSelectedContentBackgroundColor()
        };
        let layer_ptr = &*layer as *const _ as *const AnyObject;
        unsafe {
            let _: () = msg_send![layer_ptr, setCornerRadius: SELECTION_CORNER_RADIUS];
            // `setBackgroundColor:` on a CALayer wants a `CGColorRef`. A plain
            // `msg_send![layer, setBackgroundColor: cg]` mis-encodes the CGColorRef as an ObjC
            // object and panics, so fire it through a hand-typed objc_msgSend (same trap
            // `split_view::set_layer_background` works around).
            let cg = color.CGColor();
            let cg_ptr: *const std::ffi::c_void = Retained::as_ptr(&cg).cast();
            let set_bg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
                *const std::ffi::c_void,
            ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            set_bg(layer_ptr, sel!(setBackgroundColor:), cg_ptr);
        }
    }
}

// ─── GridDataSource: data source + delegate, owns the mutable grid state ────

struct GridDataSourceIvars {
    /// The headless model: folder image list, sort, selected index, folder generation.
    model: RefCell<GridModel>,
    /// Visible-range-centered generation scheduler (Phase 2 plumbing).
    scheduler: RefCell<Scheduler>,
    /// Byte-budget eviction bookkeeping (Phase 2 plumbing).
    cache: RefCell<ThumbnailCache>,
    /// Generated thumbnails as `NSImage`, keyed by folder index. AppKit owns the bitmaps; this map
    /// keeps them alive while resident and drops them on eviction.
    images: RefCell<HashMap<usize, Retained<objc2_app_kit::NSImage>>>,
    /// Background folder lister (its own OS thread). Kept alive for the data source's life.
    lister: FolderLister,
    /// QL request worker for grid thumbnails — a second path into the shared `quicklookd` cache.
    requests: RequestTable,
    /// Whether the grid pane is the focused pane, mirrored from `browser::State::focused_pane` by
    /// `sync_native` (via `BrowseGrid::set_focused`). The single source of truth for the grid's
    /// selection-emphasis color: blue when focused, gray when not. We DON'T infer it from the
    /// native first responder — the click→`BrowseSelectFolder` dispatch is async, so reading the
    /// first responder during a focus flip is racy. State-driven matches the `sync_native` model.
    focused: Cell<bool>,
}

define_class!(
    /// `NSCollectionView` data source + delegate (+ prefetching) for the thumbnail grid. Owns the
    /// grid's mutable state in `RefCell` ivars (main-thread only; AppKit calls re-entrantly).
    // SAFETY: NSObject subclass, no Drop. Main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwGridDataSource"]
    #[ivars = GridDataSourceIvars]
    struct GridDataSource;

    unsafe impl NSObjectProtocol for GridDataSource {}

    // Raw-selector style (like `outline::TreeDataSource`): the collection view sends these
    // selectors; we don't formally conform to the protocol traits because a `define_class!` method
    // can't return `Retained<T>` (no `Encode`) the way the typed `NSCollectionViewDataSource` trait
    // demands. We register the object as the data source/delegate/prefetch source via raw
    // `setDataSource:`/`setDelegate:`/`setPrefetchDataSource:` (see `BrowseGrid::create`).
    impl GridDataSource {
        // ── NSCollectionViewDataSource ──
        #[unsafe(method(collectionView:numberOfItemsInSection:))]
        fn number_of_items(&self, _cv: &NSCollectionView, _section: NSInteger) -> NSInteger {
            self.ivars().model.borrow().len() as NSInteger
        }

        #[unsafe(method(collectionView:itemForRepresentedObjectAtIndexPath:))]
        fn item_for_index_path(
            &self,
            cv: &NSCollectionView,
            index_path: &NSIndexPath,
        ) -> *mut NSCollectionViewItem {
            let identifier = NSString::from_str(ITEM_IDENTIFIER);
            let item: Retained<NSCollectionViewItem> =
                cv.makeItemWithIdentifier_forIndexPath(&identifier, index_path);
            let index = index_path.item() as usize;
            self.configure_item(&item, index);
            Retained::into_raw(item)
        }

        /// State-driven focus flag for the grid items to read at paint time (see
        /// `GridItem::grid_pane_is_focused`). Raw selector because `GridItem` dispatches it
        /// dynamically via the collection view's `dataSource` pointer.
        #[unsafe(method(gridPaneIsFocused))]
        fn grid_pane_is_focused(&self) -> bool {
            self.ivars().focused.get()
        }

        // ── NSCollectionViewDelegate: selection ──
        #[unsafe(method(collectionView:didSelectItemsAtIndexPaths:))]
        fn did_select_items(&self, _cv: &NSCollectionView, index_paths: &NSSet<NSIndexPath>) {
            let Some(first) = first_index(index_paths) else {
                return;
            };
            self.ivars().model.borrow_mut().set_selected(first);
            crate::commands::send_command(crate::commands::AppCommand::BrowseGridSelected(first));
        }

        // ── NSCollectionViewPrefetching: warm a margin ahead/behind ──
        #[unsafe(method(collectionView:prefetchItemsAtIndexPaths:))]
        fn prefetch_items(&self, _cv: &NSCollectionView, index_paths: &NSArray<NSIndexPath>) {
            // Widen the scheduler's visible range to cover the prefetch indices so their
            // thumbnails generate ahead of being scrolled into view. The pump is driven by the
            // executor after this returns.
            let count = index_paths.count();
            if count == 0 {
                return;
            }
            let mut lo = usize::MAX;
            let mut hi = 0usize;
            for i in 0..count {
                let ip = index_paths.objectAtIndex(i);
                let item = ip.item() as usize;
                lo = lo.min(item);
                hi = hi.max(item);
            }
            let len = self.ivars().model.borrow().len();
            let range = grid_model::clamp_visible_range(lo..hi + 1, len);
            self.ivars().scheduler.borrow_mut().set_visible_range(range.clone());
            self.ivars().cache.borrow_mut().set_visible_range(range);
        }

    }
);

impl GridDataSource {
    fn new(mtm: MainThreadMarker, sort_by: SortBy, max_parallel: usize) -> Retained<Self> {
        let ivars = GridDataSourceIvars {
            model: RefCell::new(GridModel::new(sort_by)),
            scheduler: RefCell::new(Scheduler::new(max_parallel)),
            cache: RefCell::new(ThumbnailCache::new()),
            images: RefCell::new(HashMap::new()),
            lister: FolderLister::start(),
            requests: RequestTable::new(
                || crate::commands::AppCommand::BrowseThumbnailsAvailable,
                "prvw-gridgen",
            ),
            focused: Cell::new(false),
        };
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Configure a (possibly recycled) cell for `index`: set its image (cached thumbnail or a
    /// neutral placeholder), filename label, and selection style.
    fn configure_item(&self, item: &NSCollectionViewItem, index: usize) {
        let model = self.ivars().model.borrow();
        let name = model
            .path(index)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        drop(model);

        if let Some(image_view) = item.imageView() {
            let images = self.ivars().images.borrow();
            if let Some(img) = images.get(&index) {
                image_view.setImage(Some(img));
            } else {
                // Neutral placeholder until the thumbnail arrives.
                image_view.setImage(None);
            }
        }
        if let Some(label) = item.textField() {
            label.setStringValue(&NSString::from_str(&name));
        }
    }
}

/// Pull the smallest item index out of a selection set (single-select grid, but the delegate hands
/// a set). `None` when empty.
fn first_index(set: &NSSet<NSIndexPath>) -> Option<usize> {
    let all = set.allObjects();
    let count = all.count();
    let mut min: Option<usize> = None;
    for i in 0..count {
        let ip = all.objectAtIndex(i);
        let item = ip.item() as usize;
        min = Some(min.map_or(item, |m| m.min(item)));
    }
    min
}

// ─── BrowseGrid: the owned scroll + collection view ────────────────────────

/// Owns the grid's scroll view, collection view, the flow layout, the data source/delegate, and
/// the "(No images)" overlay, for the window's lifetime. `BrowseSplitView` stores this so nothing
/// drops early (autorelease-segfault rule). Also the handle the app drives.
pub struct BrowseGrid {
    scroll: Retained<NSScrollView>,
    collection: Retained<BrowseCollectionView>,
    /// Kept alive: the collection view holds the data source/delegate weakly (`assign`).
    data_source: Retained<GridDataSource>,
    /// Centered "(No images)" label, shown when the listed folder has no supported images.
    empty_label: Retained<NSTextField>,
    /// The window's backing scale factor, for thumbnail request scale.
    scale: f64,
}

// SAFETY: all fields are main-thread-only AppKit objects, stored not shared across threads.
unsafe impl Send for BrowseGrid {}

impl BrowseGrid {
    /// Build the collection view inside an `NSScrollView`. Returns the owner; the caller adds
    /// `scroll_view()` to the grid pane. `scale` is the window's backing scale factor.
    pub fn create(mtm: MainThreadMarker, sort_by: SortBy, scale: f64) -> Self {
        // Half the cores, floor 1 — same courtesy cap as previews (quicklookd does the work).
        let max_parallel = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(4);
        let data_source = GridDataSource::new(mtm, sort_by, max_parallel);

        unsafe {
            // Flow layout: fixed item size, vertical scroll, comfortable spacing.
            let layout = NSCollectionViewFlowLayout::new(mtm);
            layout.setItemSize(NSSize::new(CELL_PT, CELL_PT));
            layout.setMinimumLineSpacing(CELL_SPACING);
            layout.setMinimumInteritemSpacing(CELL_SPACING);
            layout.setScrollDirection(NSCollectionViewScrollDirection::Vertical);
            layout.setSectionInset(NSEdgeInsets {
                top: SECTION_INSET_PT,
                left: SECTION_INSET_PT,
                bottom: SECTION_INSET_PT,
                right: SECTION_INSET_PT,
            });

            let collection = BrowseCollectionView::new(mtm);
            collection.setCollectionViewLayout(Some(&layout));
            collection.setSelectable(true);
            collection.setAllowsMultipleSelection(false);
            collection.setAllowsEmptySelection(true);
            let _: () = msg_send![&*collection, setBackgroundColors:
                &*NSArray::from_slice(&[&*NSColor::clearColor()])];

            // Register the item class for reuse. `GridItem::class()` forces the `define_class!`
            // type to register with the objc runtime (lazy otherwise) and hands back the class —
            // looking it up by name can race the registration and return null.
            let identifier = NSString::from_str(ITEM_IDENTIFIER);
            let item_class = GridItem::class();
            let _: () = msg_send![&*collection,
                registerClass: item_class, forItemWithIdentifier: &*identifier];

            // Wire data source + delegate + prefetching (all held weakly → BrowseGrid keeps them).
            // Passed as `&AnyObject` via raw sends because `GridDataSource` implements the protocol
            // selectors by name without formally conforming to the typed traits (see the
            // `define_class!` note). The collection view only sends the selectors; it doesn't check
            // Rust-level conformance.
            let ds_obj: &AnyObject = &data_source;
            let _: () = msg_send![&*collection, setDataSource: ds_obj];
            let _: () = msg_send![&*collection, setDelegate: ds_obj];
            let _: () = msg_send![&*collection, setPrefetchDataSource: ds_obj];

            // Double-click to open is detected in `GridItem::mouseDown:` (clickCount == 2), not a
            // click gesture recognizer — the recognizer delays the single click ~600 ms to
            // disambiguate, which made selection feel laggy. Single click selects instantly.

            // Scroll view host.
            let scroll = NSScrollView::new(mtm);
            let _: () = msg_send![&*scroll, setDrawsBackground: false];
            let _: () = msg_send![&*scroll, setHasVerticalScroller: true];
            let _: () = msg_send![&*scroll, setHasHorizontalScroller: false];
            let _: () = msg_send![&*scroll, setAutohidesScrollers: true];
            let _: () = msg_send![&*scroll, setBorderType: 0isize]; // NSNoBorder
            let cv_obj: &AnyObject = &collection;
            let _: () = msg_send![&*scroll, setDocumentView: cv_obj];

            // "(No images)" overlay, hidden by default. Centered over the grid; shown only for an
            // empty folder. Plain label on a transparent view so the gallery background shows.
            let empty_label = crate::platform::macos::ui_common::make_label(
                "(No images)",
                EMPTY_LABEL_FONT_PT,
                mtm,
            );
            empty_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            let _: () = msg_send![&*empty_label, setHidden: true];

            BrowseGrid {
                scroll,
                collection,
                data_source,
                empty_label,
                scale,
            }
        }
    }

    /// The scroll view to add to the grid pane.
    pub fn scroll_view(&self) -> &NSScrollView {
        &self.scroll
    }

    /// Make the collection view the window's first responder. Called by `apply_focus` when the
    /// grid pane is focused: the collection view then handles its own arrow selection + scroll
    /// natively, and `GridItem`s read it as first responder to draw blue (focused) emphasis.
    pub fn make_first_responder(&self) {
        unsafe {
            let window: *const AnyObject = msg_send![&*self.collection, window];
            if window.is_null() {
                return;
            }
            let cv_obj: *const AnyObject = Retained::as_ptr(&self.collection).cast();
            let accepted: bool = msg_send![window, makeFirstResponder: cv_obj];
            log::debug!("Grid make_first_responder accepted={accepted}");
        }
    }

    /// Set whether the grid pane is the focused pane (mirrored from `browser::State::focused_pane`)
    /// and repaint the visible selected items so their emphasis matches: accent-blue when focused,
    /// gray when not. Called by `apply_focus` on every `sync_native`, so a tree click (which moves
    /// focus away from the grid) grays the grid item immediately — and a grid click blues it.
    /// State-driven, not first-responder-inferred (the async click→command flip made FR-reading
    /// racy).
    pub fn set_focused(&self, focused: bool) {
        self.data_source.ivars().focused.set(focused);
        self.refresh_focus_emphasis();
    }

    /// Repaint the visible selected items' emphasis to match the grid's current focus flag (blue
    /// when focused, gray otherwise). Called from `set_focused` on a focus flip, since AppKit
    /// doesn't re-run `setSelected:` just because the focused pane changed.
    pub fn refresh_focus_emphasis(&self) {
        let visible = self.collection.visibleItems();
        let count = visible.count();
        for i in 0..count {
            // `visibleItems` yields base `NSCollectionViewItem`s; every cell is a `GridItem`, which
            // implements `refreshEmphasis`. Dispatch by selector (guarded by respondsToSelector: so
            // a stray non-GridItem can't crash).
            let item = visible.objectAtIndex(i);
            let responds: bool =
                unsafe { msg_send![&*item, respondsToSelector: sel!(refreshEmphasis)] };
            if responds {
                let _: () = unsafe { msg_send![&*item, refreshEmphasis] };
            }
        }
    }

    /// The "(No images)" overlay label, for the pane to position centered over the grid.
    pub fn empty_label(&self) -> &NSTextField {
        &self.empty_label
    }

    /// True when the listed folder has no supported images. Drives the grid-non-focusable rule
    /// (Tab skips the grid) and the "(No images)" overlay.
    pub fn is_empty(&self) -> bool {
        self.data_source.ivars().model.borrow().is_empty()
    }

    /// The selected image's path, if any (for opening into image mode).
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.data_source
            .ivars()
            .model
            .borrow()
            .selected_path()
            .map(std::path::Path::to_path_buf)
    }

    /// The selected grid index, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.data_source.ivars().model.borrow().selected()
    }

    /// All image paths in the listed folder, in display order (for handing the folder to image
    /// mode on open).
    pub fn images(&self) -> Vec<PathBuf> {
        self.data_source.ivars().model.borrow().images().to_vec()
    }

    /// Begin listing `folder`'s images on the background worker. The result arrives as
    /// `AppCommand::BrowseFolderListed` → [`Self::folder_listed`]. Never reads the disk here.
    pub fn list_folder(&self, folder: PathBuf) {
        self.data_source.ivars().lister.list(folder);
    }

    /// Apply a completed background folder listing: populate the model (sorted), reset the
    /// scheduler/cache for the new folder, reload the collection view, toggle the empty overlay,
    /// and kick off thumbnail generation for the initial visible range. Always applies the latest
    /// listing that arrives; the model's folder generation guards thumbnail completions, so a stale
    /// in-flight thumbnail from a previous folder is dropped when it lands.
    ///
    /// `preselect` is the image to preselect (browse-open positioning: the image the user came
    /// from, so Esc/Enter right after open round-trips to it). When it's `Some` and present in the
    /// folder, that image is selected + scrolled into view; otherwise the first image is selected.
    pub fn folder_listed(&self, images: Vec<PathBuf>, preselect: Option<&std::path::Path>) {
        let len = images.len();
        // Resolve the preselect index against the SORTED image list, so it matches the grid's
        // display order. `set_images` sorts in place, so read the sorted list back afterward.
        {
            let mut model = self.data_source.ivars().model.borrow_mut();
            model.set_images(images);
        }
        let preselect_index = {
            let model = self.data_source.ivars().model.borrow();
            super::grid_preselect_index(model.images(), preselect)
        };
        // New folder: clear generated images + reseed the scheduler/cache from scratch.
        self.data_source.ivars().images.borrow_mut().clear();
        let initial_visible = grid_model::clamp_visible_range(0..first_screen_count(), len);
        self.data_source
            .ivars()
            .scheduler
            .borrow_mut()
            .set_folder(len, initial_visible.clone());
        self.data_source
            .ivars()
            .cache
            .borrow_mut()
            .set_visible_range(initial_visible);

        self.collection.reloadData();
        self.refresh_empty_overlay();
        // Preselect the came-from image when revealing into it (scroll it into view); else select
        // the first image so the model + native selection stay coherent and visible.
        if len > 0 {
            match preselect_index {
                Some(index) => self.select_index(index, true),
                None => self.select_index(0, false),
            }
        }
        self.pump_visible_range();
    }

    /// Apply a live folder re-scan to the grid (browse-mode live sync). Unlike
    /// [`Self::folder_listed`] (a fresh folder pick, which resets the selection to the first image),
    /// this **preserves the selection by path** across adds/removes — `GridModel::apply_rescan` keeps
    /// the cursor on the same file, or lands on the next/previous surviving image when the selected
    /// one was deleted, or clears it for an emptied folder. Then it refreshes thumbnails for the
    /// change: drops the cached `NSImage` for every `modified` path so it regenerates with fresh
    /// bytes, drops thumbnails for removed indices, reloads the collection view, and re-pumps
    /// generation (which schedules thumbnails for any added images in the visible range). Returns
    /// `true` when the selected image changed identity (so the executor can re-warm it).
    ///
    /// `modified` are the paths flagged `Modify` by the watcher (content changed in place); their
    /// thumbnails must be regenerated even though the path is unchanged. Reuses the same machinery
    /// as the fresh-listing path; the only behavioral difference is selection-by-path.
    pub fn apply_rescan(&self, images: Vec<PathBuf>, modified: &[PathBuf]) -> bool {
        let selected_before = self.selected_path();
        let len = images.len();
        {
            let mut model = self.data_source.ivars().model.borrow_mut();
            model.apply_rescan(images, selected_before.as_deref());
        }
        let selected_after = self.selected_path();

        // New folder contents: clear all generated thumbnails and reseed the scheduler/cache from
        // scratch on the current visible range. A full reset is the same robust approach the
        // fresh-listing path takes — the visible range re-pumps regeneration, and a re-saved
        // (`modified`) image can't keep a stale bitmap because the whole map is cleared. (The
        // map/cache are keyed by index, which shifts on add/remove, so a targeted drop would be
        // fragile; the clear-and-repump is cheap — thumbnails come from the shared QL cache.)
        let _ = modified; // The full clear below covers modified paths too.
        self.data_source.ivars().images.borrow_mut().clear();
        let initial_visible = grid_model::clamp_visible_range(0..first_screen_count(), len);
        self.data_source
            .ivars()
            .scheduler
            .borrow_mut()
            .set_folder(len, initial_visible.clone());
        self.data_source
            .ivars()
            .cache
            .borrow_mut()
            .set_visible_range(initial_visible);

        self.collection.reloadData();
        self.refresh_empty_overlay();
        // Re-assert the preserved selection in the native collection view (reloadData clears it).
        if let Some(index) = self.selected_index() {
            self.select_index(index, false);
        }
        self.pump_visible_range();

        selected_before != selected_after
    }

    /// Recompute the visible range from the collection view and feed it to the scheduler + cache,
    /// then pump generation. Called on scroll (via the executor) and after a reload.
    pub fn pump_visible_range(&self) {
        let range = self.current_visible_range();
        self.data_source
            .ivars()
            .scheduler
            .borrow_mut()
            .set_visible_range(range.clone());
        let evicted = {
            let mut cache = self.data_source.ivars().cache.borrow_mut();
            cache.set_visible_range(range);
            cache.evict_to_budget()
        };
        self.drop_evicted(&evicted);
        self.pump();
    }

    /// Drain the scheduler into the QL worker at `GRID_THUMBNAIL_PX`, stamped with the current
    /// folder generation so stale completions are dropped.
    fn pump(&self) {
        let generation = self.data_source.ivars().model.borrow().generation();
        loop {
            let next = self.data_source.ivars().scheduler.borrow_mut().poll_next();
            let Some((index, request_id)) = next else {
                break;
            };
            let path = self
                .data_source
                .ivars()
                .model
                .borrow()
                .path(index)
                .map(std::path::Path::to_path_buf);
            let Some(path) = path else {
                self.data_source
                    .ivars()
                    .scheduler
                    .borrow_mut()
                    .mark_failed(index);
                continue;
            };
            self.data_source.ivars().requests.submit(SubmitRequest {
                request_id,
                index,
                folder_generation: generation,
                path: &path,
                size_pt: f64::from(GRID_THUMBNAIL_PX),
                scale: self.scale,
                proxy: crate::commands::event_loop_proxy(),
            });
        }
    }

    /// Apply queued QL completions: drop stale-generation deliveries, wrap RGBA8 in an `NSImage`,
    /// store it, update the cache's byte bookkeeping, evict to budget (feeding evicted indices to
    /// the scheduler's `uncache` and dropping their `NSImage`s), and reload the affected items.
    pub fn thumbnails_available(&self, mtm: MainThreadMarker) {
        let batch = self.data_source.ivars().requests.drain_pending();
        let generation = self.data_source.ivars().model.borrow().generation();
        let mut ready: Vec<usize> = Vec::new();
        for delivery in batch {
            if delivery.folder_generation != generation {
                // Stale folder — drop and tell the scheduler so it isn't stuck "in flight".
                self.data_source
                    .ivars()
                    .scheduler
                    .borrow_mut()
                    .mark_failed(delivery.index);
                continue;
            }
            match delivery.result {
                Ok(pixels) => {
                    let Some(image) = quicklook::nsimage_from_rgba8(
                        pixels.width,
                        pixels.height,
                        &pixels.rgba,
                        mtm,
                    ) else {
                        self.data_source
                            .ivars()
                            .scheduler
                            .borrow_mut()
                            .mark_failed(delivery.index);
                        continue;
                    };
                    self.data_source
                        .ivars()
                        .images
                        .borrow_mut()
                        .insert(delivery.index, image);
                    self.data_source
                        .ivars()
                        .scheduler
                        .borrow_mut()
                        .mark_ready(delivery.index);
                    self.data_source
                        .ivars()
                        .cache
                        .borrow_mut()
                        .insert(delivery.index, thumbnail_cache::EST_THUMBNAIL_BYTES);
                    ready.push(delivery.index);
                }
                Err(()) => {
                    self.data_source
                        .ivars()
                        .scheduler
                        .borrow_mut()
                        .mark_failed(delivery.index);
                }
            }
        }
        // Inserts may have pushed the cache over budget; evict and resync the scheduler.
        let evicted = self
            .data_source
            .ivars()
            .cache
            .borrow_mut()
            .evict_to_budget();
        self.drop_evicted(&evicted);
        // Reload the items that just got an image (skip ones we immediately evicted).
        let to_reload: Vec<usize> = ready
            .into_iter()
            .filter(|i| self.data_source.ivars().images.borrow().contains_key(i))
            .collect();
        self.reload_items(&to_reload);
        // A completion may have freed a parallelism slot; keep the queue moving.
        self.pump();
    }

    /// Drop evicted indices from the scheduler's `cached` set and release their `NSImage`s.
    fn drop_evicted(&self, evicted: &[usize]) {
        if evicted.is_empty() {
            return;
        }
        let mut images = self.data_source.ivars().images.borrow_mut();
        let mut scheduler = self.data_source.ivars().scheduler.borrow_mut();
        for &idx in evicted {
            images.remove(&idx);
            scheduler.uncache(idx);
        }
    }

    /// Select `index` programmatically and optionally scroll it to visible. Keeps the model and the
    /// native selection coherent (Phase 5 will route arrow keys here).
    pub fn select_index(&self, index: usize, scroll: bool) {
        let resolved = self
            .data_source
            .ivars()
            .model
            .borrow_mut()
            .set_selected(index);
        let Some(index) = resolved else {
            return;
        };
        let ip = NSIndexPath::indexPathForItem_inSection(index as NSInteger, 0);
        let set = NSSet::from_retained_slice(&[ip]);
        self.collection.setSelectionIndexPaths(&set);
        if scroll {
            self.collection.scrollToItemsAtIndexPaths_scrollPosition(
                &set,
                NSCollectionViewScrollPosition::CenteredVertically,
            );
        }
    }

    /// Reload specific item indices so their cells pick up a freshly-generated thumbnail.
    fn reload_items(&self, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let paths: Vec<Retained<NSIndexPath>> = indices
            .iter()
            .map(|&i| NSIndexPath::indexPathForItem_inSection(i as NSInteger, 0))
            .collect();
        let set = NSSet::from_retained_slice(&paths);
        self.collection.reloadItemsAtIndexPaths(&set);
        // reloadItemsAtIndexPaths clears the selection highlight; reassert it.
        if let Some(sel) = self.selected_index() {
            self.select_index(sel, false);
        }
    }

    /// Show/hide the "(No images)" overlay and toggle whether the collection view can be selected
    /// (an empty grid is non-focusable so Tab keeps focus on the tree).
    fn refresh_empty_overlay(&self) {
        let empty = self.is_empty();
        unsafe {
            let _: () = msg_send![&*self.empty_label, setHidden: !empty];
        }
        self.collection.setSelectable(!empty);
    }

    /// The collection view's current visible item range, as a half-open `[start, end)` index
    /// range clamped to the model. Widened by `PREFETCH_MARGIN` so generation warms a margin
    /// ahead/behind even without the prefetch callback firing.
    fn current_visible_range(&self) -> std::ops::Range<usize> {
        let len = self.data_source.ivars().model.borrow().len();
        if len == 0 {
            return 0..0;
        }
        let visible = self.collection.indexPathsForVisibleItems();
        let all = visible.allObjects();
        let count = all.count();
        if count == 0 {
            // Nothing realized yet (just after reload): warm the top of the folder.
            return grid_model::clamp_visible_range(0..first_screen_count(), len);
        }
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for i in 0..count {
            let ip = all.objectAtIndex(i);
            let item = ip.item() as usize;
            lo = lo.min(item);
            hi = hi.max(item);
        }
        let lo = lo.saturating_sub(PREFETCH_MARGIN);
        let hi = (hi + 1 + PREFETCH_MARGIN).min(len);
        grid_model::clamp_visible_range(lo..hi, len)
    }
}

/// A rough first-screen item count to warm before the collection view has realized any cells.
/// Generous enough to cover a tall window; the scheduler's `MARGIN` caps the real queue.
fn first_screen_count() -> usize {
    60
}

#[cfg(test)]
mod tests {
    // The grid's pure logic (model, sort, selection, empty detection, visible-range clamping) is
    // tested in `grid_model`. The `NSCollectionView` view wiring here is covered by the smoke run
    // + live QA. No headless test seam exists for the objc2 plumbing.
}
