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
//! 3. On scroll (and after a reload), `BrowseGrid::on_visible_range_changed` feeds the visible
//!    range to the scheduler + cache and pumps the scheduler into the QL worker at
//!    `GRID_THUMBNAIL_PX`.
//! 4. Completions arrive as `BrowseThumbnailsAvailable`; [`BrowseGrid::thumbnails_available`]
//!    drops stale-generation deliveries, builds the `NSImage`, stores it in the map + cache,
//!    feeds `evict_to_budget`'s returned indices to `Scheduler::uncache` (dropping their
//!    `NSImage`s), and reloads the affected items so cells pick up their image.
//!
//! ## Keyboard is app-driven (same as the tree)
//!
//! winit keeps the keyboard even with the collection view up, so selection/open arrive as
//! `AppCommand`s, not through the responder chain. Mouse (click to select, double-click to open,
//! scroll) is fully native. A double-click gesture recognizer fires `BrowseOpenSelected`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSClickGestureRecognizer, NSCollectionView, NSCollectionViewFlowLayout, NSCollectionViewItem,
    NSCollectionViewScrollDirection, NSCollectionViewScrollPosition, NSColor, NSImageScaling,
    NSImageView, NSIndexPathNSCollectionViewAdditions, NSScrollView, NSTextField, NSView,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexPath, NSInteger, NSPoint, NSRect, NSSet, NSSize, NSString,
};

use super::grid_listing::FolderLister;
use super::grid_model::{self, GridModel};
use super::grid_scheduler::Scheduler;
use super::thumbnail_cache::{self, GRID_THUMBNAIL_PX, ThumbnailCache};
use crate::navigation::SortBy;
use crate::previews::quicklook::{self, RequestTable, SubmitRequest};

/// Cell side in points. A sensible fixed gallery size; the live-resize slider is a later phase.
const CELL_PT: f64 = 160.0;
// `CELL_IMAGE_PT` / `CELL_LABEL_PT` are only referenced from `GridItem::loadView`, an AppKit
// override — `GridItem` is instantiated by the collection view (via `makeItemWithIdentifier:`),
// never constructed in Rust, so the dead-code lint can't see the use. Genuinely used at runtime.
/// The image-view side inside a cell, leaving room below for the filename label.
#[allow(dead_code)]
const CELL_IMAGE_PT: f64 = 132.0;
/// Height reserved for the filename label under the thumbnail.
#[allow(dead_code)]
const CELL_LABEL_PT: f64 = 24.0;
/// Spacing between cells (both axes) and the section inset.
const CELL_SPACING: f64 = 12.0;
/// Reuse identifier for grid items.
const ITEM_IDENTIFIER: &str = "PrvwGridItem";

/// How many items ahead/behind the visible range the prefetcher warms. One screen's worth is a
/// good default; the scheduler's own `MARGIN` is the hard cap on generation.
const PREFETCH_MARGIN: usize = 24;

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
            let mtm = MainThreadMarker::from(self);
            let container = NSView::new(mtm);
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CELL_PT, CELL_PT));
            unsafe {
                let _: () = msg_send![&*container, setFrame: frame];
                let _: () = msg_send![&*container, setWantsLayer: true];
            }

            // Image view: fills the top square area, scales proportionally up or down.
            let image_view = NSImageView::new(mtm);
            image_view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            let image_frame = NSRect::new(
                NSPoint::new(0.0, CELL_LABEL_PT),
                NSSize::new(CELL_PT, CELL_IMAGE_PT),
            );
            unsafe {
                let _: () = msg_send![&*image_view, setFrame: image_frame];
                let iv_mask = 2u64 | 16u64; // width-sizable | height-sizable
                let _: () = msg_send![&*image_view, setAutoresizingMask: iv_mask];
            }
            container.addSubview(&image_view);

            // Filename label below the thumbnail, centered, single line, truncating.
            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setBordered(false);
            label.setDrawsBackground(false);
            label.setEditable(false);
            label.setSelectable(false);
            label.setAlignment(objc2_app_kit::NSTextAlignment(2)); // center
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            label.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(11.0)));
            let label_frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(CELL_PT, CELL_LABEL_PT),
            );
            unsafe {
                let _: () = msg_send![&*label, setFrame: label_frame];
                let _: () = msg_send![&*label, setLineBreakMode: 5isize]; // truncate middle
                let lbl_mask = 2u64; // width-sizable
                let _: () = msg_send![&*label, setAutoresizingMask: lbl_mask];
            }
            container.addSubview(&label);

            unsafe {
                let _: () = msg_send![self, setView: &*container];
                let _: () = msg_send![self, setImageView: &*image_view];
                let _: () = msg_send![self, setTextField: &*label];
            }
        }

        /// Tint the cell background when selected so the selection reads clearly (the base item's
        /// `isSelected` is set by AppKit on click; we recolor on `setSelected:`).
        #[unsafe(method(setSelected:))]
        fn set_selected(&self, selected: bool) {
            unsafe {
                let _: () = msg_send![super(self), setSelected: selected];
            }
            self.apply_selection_style(selected);
        }
    }
);

impl GridItem {
    // Called only from `setSelected:` (an AppKit override on a class AppKit instantiates), so the
    // dead-code lint can't see the use — it's reached at runtime on every click/selection change.
    #[allow(dead_code)]
    fn apply_selection_style(&self, selected: bool) {
        // The view exists once loaded; recolor its layer background.
        let view: Option<Retained<NSView>> = unsafe { msg_send![self, view] };
        let Some(view) = view else { return };
        let Some(layer) = view.layer() else { return };
        let color = if selected {
            NSColor::selectedContentBackgroundColor().colorWithAlphaComponent(0.85)
        } else {
            NSColor::clearColor()
        };
        let layer_ptr = &*layer as *const _ as *const AnyObject;
        unsafe {
            let _: () = msg_send![layer_ptr, setCornerRadius: 6.0f64];
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

        // ── Double-click to open ──
        /// Target of the double-click gesture recognizer. Opens the selected image in image mode.
        /// (Single-click selection is native; this is the open gesture.)
        #[unsafe(method(handleDoubleClick:))]
        fn handle_double_click(&self, _sender: *mut AnyObject) {
            crate::commands::send_command(crate::commands::AppCommand::BrowseOpenSelected);
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
    collection: Retained<NSCollectionView>,
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
                top: CELL_SPACING,
                left: CELL_SPACING,
                bottom: CELL_SPACING,
                right: CELL_SPACING,
            });

            let collection = NSCollectionView::new(mtm);
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

            // Double-click to open: a click gesture recognizer set to 2 clicks, targeting the
            // data source (it forwards to the open command). Single-click selection stays native.
            let recognizer = NSClickGestureRecognizer::new(mtm);
            recognizer.setNumberOfClicksRequired(2);
            let _: () = msg_send![&*recognizer, setTarget: &*data_source];
            let _: () = msg_send![&*recognizer, setAction: Some(sel!(handleDoubleClick:))];
            collection.addGestureRecognizer(&recognizer);

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
            let empty_label =
                crate::platform::macos::ui_common::make_label("(No images)", 15.0, mtm);
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
    pub fn folder_listed(&self, images: Vec<PathBuf>) {
        let len = images.len();
        {
            let mut model = self.data_source.ivars().model.borrow_mut();
            model.set_images(images);
        }
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
        // Select the first image so the model + native selection agree (Phase 5 refines the
        // browse-open positioning; here we just keep them coherent and visible).
        if len > 0 {
            self.select_index(0, false);
        }
        self.pump_visible_range();
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
        let size = objc2_core_foundation::CGSize {
            width: f64::from(GRID_THUMBNAIL_PX),
            height: f64::from(GRID_THUMBNAIL_PX),
        };
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
                size,
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
