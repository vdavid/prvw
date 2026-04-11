//! macOS NSApplicationDelegate for handling Apple Events (file open requests).
//!
//! When a user double-clicks an image file while Prvw is already running, macOS sends an
//! `application:openURLs:` event instead of launching a new instance. This module registers
//! a delegate to handle that event and forward file paths to the main event loop.
//!
//! Based on winit's documented approach: winit guarantees it won't register its own delegate,
//! so we can safely register ours alongside winit's event loop.

use crate::qa_server::AppCommand;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSURL};
use std::cell::RefCell;
use std::path::PathBuf;
use winit::event_loop::EventLoopProxy;

// Thread-local storage for the event loop proxy. The delegate's callback runs on the main thread
// and needs access to the proxy, but define_class! doesn't support generic ivars.
thread_local! {
    static EVENT_PROXY: RefCell<Option<EventLoopProxy<AppCommand>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. PrvwAppDelegate doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwAppDelegate"]
    pub struct PrvwAppDelegate;

    unsafe impl NSObjectProtocol for PrvwAppDelegate {}

    unsafe impl NSApplicationDelegate for PrvwAppDelegate {
        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let urls_vec: Vec<Retained<NSURL>> = urls.to_vec();
            for url in &urls_vec {
                if let Some(path_nsstring) = url.path() {
                    let path_string: String = path_nsstring.to_string();
                    let path = PathBuf::from(path_string);
                    if path.is_file() {
                        log::info!("Apple Event: opening {}", path.display());
                        EVENT_PROXY.with(|proxy| {
                            if let Some(proxy) = proxy.borrow().as_ref() {
                                let _ = proxy.send_event(AppCommand::OpenFile(path.clone()));
                            }
                        });
                    }
                }
            }
        }
    }
);

impl PrvwAppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Register the macOS application delegate. Must be called after `EventLoop::new()` and before
/// `run_app()`. Returns the delegate (caller must keep it alive for the app's lifetime).
pub fn register(proxy: EventLoopProxy<AppCommand>) -> Retained<PrvwAppDelegate> {
    EVENT_PROXY.with(|p| {
        *p.borrow_mut() = Some(proxy);
    });

    let mtm = MainThreadMarker::new().expect("Must be called from the main thread");
    let delegate = PrvwAppDelegate::new(mtm);
    let app = NSApplication::sharedApplication(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    log::debug!("Registered macOS application delegate for file open events");
    delegate
}
