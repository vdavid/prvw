//! Prune AppKit's auto-injected menu items.
//!
//! macOS appends standard items to menus it recognizes by title — "Writing Tools",
//! "AutoFill", "Start Dictation", "Emoji & Symbols" to the Edit menu, and "Enter Full
//! Screen" to the View menu. Prvw is a viewer with no text input, so none of them belong.
//!
//! These items are added *lazily* (after the menu is built, before each display), so a
//! one-time removal doesn't stick. We install an `NSMenuDelegate` whose `menuNeedsUpdate:`
//! fires right before the menu opens and removes the unwanted items every time. AppKit
//! holds the delegate weakly, so the caller must keep the returned objects alive for the
//! app's lifetime.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send};
use objc2_foundation::{NSObjectProtocol, NSString};

/// What a pruner does to its menu, evaluated per item by title.
struct PruneRule {
    titles: Vec<String>,
    /// `true`: remove every item whose title isn't in `titles` (used for Edit — keep only
    /// our own item). `false`: remove only items whose title *is* in `titles` (used for
    /// View — drop "Enter Full Screen" but leave everything else).
    keep_only: bool,
}

define_class!(
    /// Menu delegate that strips AppKit's auto-injected items before each open.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwMenuPruner"]
    #[ivars = PruneRule]
    struct MenuPruner;

    unsafe impl NSObjectProtocol for MenuPruner {}

    impl MenuPruner {
        // NSMenuDelegate. Declared by selector (no formal protocol conformance) so we
        // don't need the typed `NSMenu` bindings just for this one callback.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: *mut AnyObject) {
            // SAFETY: AppKit hands us a valid NSMenu on the main thread.
            unsafe { prune(menu, self.ivars()) }
        }
    }
);

impl MenuPruner {
    fn new(mtm: MainThreadMarker, rule: PruneRule) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(rule);
        unsafe { msg_send![super(this), init] }
    }
}

/// Remove items from `menu` per `rule`. Separators have an empty title, so under
/// `keep_only` they're removed too (AppKit slips one in above its injected items).
unsafe fn prune(menu: *mut AnyObject, rule: &PruneRule) {
    if menu.is_null() {
        return;
    }
    unsafe {
        let count: isize = msg_send![menu, numberOfItems];
        for i in (0..count).rev() {
            let item: *mut AnyObject = msg_send![menu, itemAtIndex: i];
            if item.is_null() {
                continue;
            }
            let title_ptr: *mut NSString = msg_send![item, title];
            let title = if title_ptr.is_null() {
                String::new()
            } else {
                (*title_ptr).to_string()
            };
            let in_list = rule.titles.iter().any(|t| t == &title);
            let remove = if rule.keep_only { !in_list } else { in_list };
            if remove {
                let _: () = msg_send![menu, removeItemAtIndex: i];
            }
        }
    }
}

/// Look up a top-level menu's submenu (NSMenu) by its bar title, e.g. "Edit".
unsafe fn submenu_by_title(main_menu: *mut AnyObject, title: &str) -> *mut AnyObject {
    unsafe {
        let ns_title = NSString::from_str(title);
        let item: *mut AnyObject = msg_send![main_menu, itemWithTitle: &*ns_title];
        if item.is_null() {
            return std::ptr::null_mut();
        }
        msg_send![item, submenu]
    }
}

/// Install pruners on the Edit and View menus. Returns the delegate objects, which the
/// caller MUST keep alive (AppKit references delegates weakly). Run after
/// `Menu::init_for_nsapp`.
pub(crate) fn install() -> Vec<Retained<AnyObject>> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let mut delegates = Vec::new();

    // SAFETY: walks the AppKit main-menu tree on the main thread; pointers are null-checked.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let main_menu: *mut AnyObject = msg_send![app, mainMenu];
        if main_menu.is_null() {
            return delegates;
        }

        let specs = [
            (
                "Edit",
                PruneRule {
                    titles: vec!["Copy image".to_string()],
                    keep_only: true,
                },
            ),
            (
                "View",
                PruneRule {
                    titles: vec![
                        "Enter Full Screen".to_string(),
                        "Exit Full Screen".to_string(),
                    ],
                    keep_only: false,
                },
            ),
        ];

        for (bar_title, rule) in specs {
            let submenu = submenu_by_title(main_menu, bar_title);
            if submenu.is_null() {
                continue;
            }
            // Prune now, then again before every open via the delegate.
            prune(submenu, &rule);
            let pruner = MenuPruner::new(mtm, rule);
            let _: () = msg_send![submenu, setDelegate: &*pruner];
            delegates.push(Retained::cast_unchecked::<AnyObject>(pruner));
        }
    }

    delegates
}
