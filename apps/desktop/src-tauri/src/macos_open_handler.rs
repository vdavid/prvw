//! macOS Apple Event handler for "open documents" events.
//!
//! When a user double-clicks an image file while Prvw is already running, macOS sends a
//! `kAEOpenDocuments` Apple Event to the running instance. This module registers a handler
//! via `NSAppleEventManager` (not via NSApplicationDelegate, which would conflict with winit).
//!
//! The handler extracts file URLs from the event descriptor and sends them to the main
//! app via `mpsc::Sender<AppCommand>`.

use crate::AppCommand;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSObject, NSObjectProtocol};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

// Apple Event constants (from CoreServices/AE headers)
// kCoreEventClass = 'aevt' = 0x61657674
// kAEOpenDocuments = 'odoc' = 0x6F646F63
const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
const K_AE_OPEN_DOCUMENTS: u32 = 0x6F64_6F63;
// keyDirectObject = '----' = 0x2D2D2D2D
const KEY_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;

// Thread-local storage for the command sender.
thread_local! {
    static COMMAND_TX: RefCell<Option<Sender<AppCommand>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwOpenHandler"]
    pub struct PrvwOpenHandler;

    unsafe impl NSObjectProtocol for PrvwOpenHandler {}

    impl PrvwOpenHandler {
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            // The file list is in the direct object parameter ('----')
            let Some(file_list) = event.paramDescriptorForKeyword(KEY_DIRECT_OBJECT) else {
                log::warn!("Apple Event: no direct object parameter");
                return;
            };

            let count = file_list.numberOfItems();
            log::debug!("Apple Event: received open request for {count} file(s)");

            for i in 1..=count {
                // Apple Event descriptors are 1-indexed
                let Some(desc) = file_list.descriptorAtIndex(i) else {
                    continue;
                };
                let Some(url) = desc.fileURLValue() else {
                    continue;
                };
                let Some(path_nsstring) = url.path() else {
                    continue;
                };
                let path = PathBuf::from(path_nsstring.to_string());
                if path.is_file() {
                    log::info!("Apple Event: opening {}", path.display());
                    COMMAND_TX.with(|tx| {
                        if let Some(tx) = tx.borrow().as_ref() {
                            let _ = tx.send(AppCommand::OpenFile(path.clone()));
                        }
                    });
                }
            }
        }
    }
);

impl PrvwOpenHandler {
    fn new(mtm: objc2_foundation::MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Register the Apple Event handler for `kAEOpenDocuments`. Must be called on the main thread.
/// Returns the handler object (must be kept alive).
pub fn register(command_tx: Sender<AppCommand>) -> Retained<PrvwOpenHandler> {
    COMMAND_TX.with(|tx| {
        *tx.borrow_mut() = Some(command_tx);
    });

    let mtm =
        objc2_foundation::MainThreadMarker::new().expect("Must be called from the main thread");
    let handler = PrvwOpenHandler::new(mtm);

    let manager = NSAppleEventManager::sharedAppleEventManager();
    unsafe {
        manager.setEventHandler_andSelector_forEventClass_andEventID(
            handler.as_ref() as &AnyObject,
            sel!(handleOpenDocuments:withReplyEvent:),
            K_CORE_EVENT_CLASS,
            K_AE_OPEN_DOCUMENTS,
        );
    }

    log::debug!("Registered Apple Event handler for kAEOpenDocuments");
    handler
}
