//! macOS file-open handler via ObjC method injection.
//!
//! Winit 0.30 registers its own NSApplicationDelegate (`WinitApplicationDelegate`) and panics
//! if we replace it. But winit's delegate doesn't implement `application:openURLs:`, so AppKit
//! falls through to NSDocumentController, which shows "cannot open files in X format" because
//! there's no NSDocument subclass.
//!
//! The fix: use the ObjC runtime to add `application:openURLs:` directly to winit's delegate
//! class. This way AppKit dispatches file-open events to our implementation without us
//! replacing the delegate.

use crate::commands::AppCommand;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{ffi, sel};
use std::cell::RefCell;
use std::ffi::CString;
use std::path::PathBuf;
use winit::event_loop::EventLoopProxy;

// Thread-local storage for the event loop proxy.
thread_local! {
    static EVENT_PROXY: RefCell<Option<EventLoopProxy<AppCommand>>> = const { RefCell::new(None) };
}

/// Store the event loop proxy. Call before `register()`.
pub fn set_proxy(proxy: EventLoopProxy<AppCommand>) {
    EVENT_PROXY.with(|p| {
        *p.borrow_mut() = Some(proxy);
    });
}

/// The `application:openURLs:` implementation that will be added to winit's delegate class.
/// This is a C-style function matching the ObjC method signature.
extern "C" fn application_open_urls(
    _this: &AnyObject,
    _cmd: Sel,
    _app: &AnyObject,
    urls: &AnyObject, // NSArray<NSURL>
) {
    unsafe {
        use objc2::msg_send;

        let count: usize = msg_send![urls, count];
        log::debug!("application:openURLs: received {count} file(s)");

        for i in 0..count {
            let url: *const AnyObject = msg_send![urls, objectAtIndex: i];
            if url.is_null() {
                continue;
            }
            let path_str: *const AnyObject = msg_send![url, path];
            if path_str.is_null() {
                continue;
            }
            // Convert NSString to Rust String
            let utf8: *const u8 = msg_send![path_str, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let c_str = std::ffi::CStr::from_ptr(utf8 as *const std::ffi::c_char);
            let path = PathBuf::from(c_str.to_string_lossy().into_owned());

            if path.is_file() {
                log::info!("File open via delegate: {}", path.display());
                EVENT_PROXY.with(|proxy| {
                    if let Some(proxy) = proxy.borrow().as_ref() {
                        let _ = proxy.send_event(AppCommand::OpenFile(path.clone()));
                    }
                });
            }
        }
    }
}

/// Add `application:openURLs:` to winit's `WinitApplicationDelegate` class.
/// Must be called after `EventLoop::new()` (which creates the class) and before `run_app()`.
pub fn register() {
    unsafe {
        let class_name = CString::new("WinitApplicationDelegate").unwrap();
        let class = objc2::runtime::AnyClass::get(class_name.as_c_str());
        let Some(class) = class else {
            log::warn!("WinitApplicationDelegate class not found, file opens won't work");
            return;
        };

        // Cast to mutable — class_addMethod requires a mutable class pointer.
        // SAFETY: we're adding a method before the event loop runs, so no concurrent access.
        let class_ptr = class as *const AnyClass as *mut AnyClass;

        let sel = sel!(application:openURLs:);
        // ObjC type encoding for `void (NSApplication*, NSArray<NSURL>*)` = "v@:@@"
        let types = CString::new("v@:@@").unwrap();

        let imp: unsafe extern "C-unwind" fn() = std::mem::transmute::<
            extern "C" fn(&AnyObject, Sel, &AnyObject, &AnyObject),
            unsafe extern "C-unwind" fn(),
        >(application_open_urls);

        let added = ffi::class_addMethod(class_ptr as *mut _, sel, imp, types.as_ptr());

        if added != Bool::NO {
            log::debug!("Added application:openURLs: to WinitApplicationDelegate");
        } else {
            log::warn!("Failed to add application:openURLs: (method may already exist)");
        }
    }
}
