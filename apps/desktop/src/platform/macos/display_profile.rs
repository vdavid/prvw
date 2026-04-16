//! macOS display color profile detection and CAMetalLayer colorspace management.
//!
//! Uses CoreGraphics FFI to query the active display's ICC profile and set the
//! Metal layer's colorspace so the compositor applies the correct color management.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

// CoreGraphics and CoreFoundation opaque types (C pointers).
#[allow(non_camel_case_types)]
type CGColorSpaceRef = *const std::ffi::c_void;
#[allow(non_camel_case_types)]
type CFDataRef = *const std::ffi::c_void;
#[allow(non_camel_case_types)]
type CGDirectDisplayID = u32;

unsafe extern "C" {
    /// Get the main display ID.
    fn CGMainDisplayID() -> CGDirectDisplayID;

    /// Get the color space associated with a display.
    fn CGDisplayCopyColorSpace(display: CGDirectDisplayID) -> CGColorSpaceRef;

    /// Extract the raw ICC profile data from a color space.
    fn CGColorSpaceCopyICCData(space: CGColorSpaceRef) -> CFDataRef;

    /// Create a color space from ICC profile data.
    fn CGColorSpaceCreateWithICCData(data: CFDataRef) -> CGColorSpaceRef;

    /// Release a CoreFoundation object.
    fn CFRelease(cf: *const std::ffi::c_void);

    /// Get the byte pointer of CFData.
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;

    /// Get the length of CFData.
    fn CFDataGetLength(data: CFDataRef) -> isize;

    /// Create CFData from raw bytes.
    fn CFDataCreate(
        allocator: *const std::ffi::c_void,
        bytes: *const u8,
        length: isize,
    ) -> CFDataRef;
}

/// Get the ICC profile bytes for the display the window is currently on.
/// Returns `None` if the display profile can't be queried (headless, SSH, etc.).
pub fn get_display_icc(window: &Window) -> Option<Vec<u8>> {
    // Get the display ID from the window's current monitor.
    // winit's MonitorHandle doesn't expose the CGDirectDisplayID directly,
    // so we use the main display as the default and try to match by position.
    let display_id = display_id_for_window(window);

    unsafe {
        let color_space = CGDisplayCopyColorSpace(display_id);
        if color_space.is_null() {
            log::warn!("CGDisplayCopyColorSpace returned null for display {display_id}");
            return None;
        }

        let icc_data = CGColorSpaceCopyICCData(color_space);
        CFRelease(color_space);

        if icc_data.is_null() {
            log::warn!("CGColorSpaceCopyICCData returned null for display {display_id}");
            return None;
        }

        let ptr = CFDataGetBytePtr(icc_data);
        let len = CFDataGetLength(icc_data) as usize;
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        CFRelease(icc_data);

        log::info!(
            "Display ICC profile: {len} bytes from display {display_id}{}",
            describe_icc(&bytes)
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        Some(bytes)
    }
}

/// Set the CAMetalLayer's colorspace to match the given ICC profile.
/// This tells the macOS compositor what color space our rendered pixels are in.
pub fn set_layer_colorspace(window: &Window, icc_bytes: &[u8]) {
    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;

        // wgpu creates a WgpuMetalLayer subview inside the NSView. The view's own layer
        // might not be the CAMetalLayer. Walk the subview hierarchy to find it.
        // First try the view's layer directly.
        let layer: *const AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            log::warn!("NSView has no layer, can't set colorspace");
            return;
        }

        // Check if this layer is a CAMetalLayer by checking if it responds to setColorspace:
        let responds: bool = msg_send![layer, respondsToSelector: objc2::sel!(setColorspace:)];
        if !responds {
            // The view's layer isn't a CAMetalLayer. Try to find it in sublayers.
            log::debug!("View's layer doesn't respond to setColorspace:, searching sublayers");
            let sublayers: *const AnyObject = msg_send![layer, sublayers];
            if sublayers.is_null() {
                log::warn!("No sublayers found, can't set colorspace");
                return;
            }
            let count: usize = msg_send![sublayers, count];
            let mut found = false;
            for i in 0..count {
                let sublayer: *const AnyObject = msg_send![sublayers, objectAtIndex: i];
                let sub_responds: bool =
                    msg_send![sublayer, respondsToSelector: objc2::sel!(setColorspace:)];
                if sub_responds {
                    log::debug!("Found CAMetalLayer in sublayer {i}");
                    set_colorspace_on_layer(sublayer, icc_bytes);
                    found = true;
                    break;
                }
            }
            if !found {
                log::warn!("No CAMetalLayer found in view hierarchy, can't set colorspace");
            }
            return;
        }

        set_colorspace_on_layer(layer, icc_bytes);
    }
}

/// Mark the wgpu Metal layer as non-opaque so transparent areas of the rendered surface
/// composite with whatever is behind it (e.g. an NSVisualEffectView for vibrancy).
pub fn set_metal_layer_transparent(window: &Window) {
    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let layer: *const AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            return;
        }

        let metal_layer = find_metal_layer(layer);
        if metal_layer.is_null() {
            log::warn!("No CAMetalLayer found, can't disable opacity");
            return;
        }

        let _: () = msg_send![metal_layer, setOpaque: false];
        log::debug!("Set CAMetalLayer.opaque = false (vibrancy passthrough enabled)");
    }
}

/// Walk the layer hierarchy to find the wgpu CAMetalLayer (identified by its response to
/// `setColorspace:`, which is a CAMetalLayer-specific selector).
unsafe fn find_metal_layer(layer: *const AnyObject) -> *const AnyObject {
    unsafe {
        let responds: bool = msg_send![layer, respondsToSelector: objc2::sel!(setColorspace:)];
        if responds {
            return layer;
        }
        let sublayers: *const AnyObject = msg_send![layer, sublayers];
        if sublayers.is_null() {
            return std::ptr::null();
        }
        let count: usize = msg_send![sublayers, count];
        for i in 0..count {
            let sublayer: *const AnyObject = msg_send![sublayers, objectAtIndex: i];
            let sub_responds: bool =
                msg_send![sublayer, respondsToSelector: objc2::sel!(setColorspace:)];
            if sub_responds {
                return sublayer;
            }
        }
        std::ptr::null()
    }
}

/// Set the colorspace on a CAMetalLayer pointer.
unsafe fn set_colorspace_on_layer(layer: *const AnyObject, icc_bytes: &[u8]) {
    unsafe {
        // Create CGColorSpace from ICC data
        let cf_data = CFDataCreate(
            std::ptr::null(),
            icc_bytes.as_ptr(),
            icc_bytes.len() as isize,
        );
        if cf_data.is_null() {
            log::warn!("CFDataCreate failed for ICC profile");
            return;
        }

        let color_space = CGColorSpaceCreateWithICCData(cf_data);
        CFRelease(cf_data);

        if color_space.is_null() {
            log::warn!("CGColorSpaceCreateWithICCData failed");
            return;
        }

        // [layer setColorspace:color_space]
        // Use raw objc_msgSend to bypass type encoding check — our CGColorSpaceRef is
        // *const c_void (encodes as ^v) but ObjC expects ^{CGColorSpace=}.
        let sel = objc2::sel!(setColorspace:);
        let send: unsafe extern "C" fn(*const AnyObject, objc2::runtime::Sel, CGColorSpaceRef) =
            std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
        send(layer, sel, color_space);
        CFRelease(color_space);

        log::info!(
            "Set CAMetalLayer colorspace{}",
            describe_icc(icc_bytes)
                .map(|d| format!(" to {d}"))
                .unwrap_or_default()
        );
    }
}

/// Get the CGDirectDisplayID for the display the window is currently on.
/// Uses `[[NSWindow screen] deviceDescription][@"NSScreenNumber"]` which is the authoritative
/// source — it's exactly what `NSWindowDidChangeScreenNotification` updates.
fn display_id_for_window(window: &Window) -> CGDirectDisplayID {
    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return unsafe { CGMainDisplayID() };
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return unsafe { CGMainDisplayID() };
    };

    unsafe {
        use objc2::runtime::AnyClass;

        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return CGMainDisplayID();
        }

        // [window screen] -> NSScreen
        let screen: *const AnyObject = msg_send![ns_window, screen];
        if screen.is_null() {
            return CGMainDisplayID();
        }

        // [screen deviceDescription] -> NSDictionary
        let device_desc: *const AnyObject = msg_send![screen, deviceDescription];
        if device_desc.is_null() {
            return CGMainDisplayID();
        }

        // deviceDescription[@"NSScreenNumber"] -> NSNumber containing the CGDirectDisplayID
        let ns_string_class = AnyClass::get(c"NSString").unwrap();
        let key: *const AnyObject =
            msg_send![ns_string_class, stringWithUTF8String: c"NSScreenNumber".as_ptr()];
        let screen_number: *const AnyObject = msg_send![device_desc, objectForKey: key];
        if screen_number.is_null() {
            return CGMainDisplayID();
        }

        // [screenNumber unsignedIntValue] -> CGDirectDisplayID (u32)
        let display_id: u32 = msg_send![screen_number, unsignedIntValue];
        display_id
    }
}

/// Extract a human-readable description from raw ICC bytes, for logging.
fn describe_icc(icc: &[u8]) -> Option<String> {
    // Find the 'desc' tag in the ICC tag table and extract the ASCII description.
    if icc.len() < 132 {
        return None;
    }
    let tag_count = u32::from_be_bytes([icc[128], icc[129], icc[130], icc[131]]) as usize;
    for t in 0..tag_count.min(30) {
        let base = 132 + t * 12;
        if base + 12 > icc.len() {
            break;
        }
        if &icc[base..base + 4] == b"desc" {
            let offset =
                u32::from_be_bytes([icc[base + 4], icc[base + 5], icc[base + 6], icc[base + 7]])
                    as usize;
            let size =
                u32::from_be_bytes([icc[base + 8], icc[base + 9], icc[base + 10], icc[base + 11]])
                    as usize;
            if offset + size > icc.len() || size < 13 {
                return None;
            }
            // 'desc' type: 4 bytes sig + 4 reserved + 4 length + ASCII string
            let type_sig = &icc[offset..offset + 4];
            if type_sig == b"desc" {
                let str_len = u32::from_be_bytes([
                    icc[offset + 8],
                    icc[offset + 9],
                    icc[offset + 10],
                    icc[offset + 11],
                ]) as usize;
                let str_start = offset + 12;
                let str_end = (str_start + str_len).min(offset + size).min(icc.len());
                return String::from_utf8(icc[str_start..str_end].to_vec())
                    .ok()
                    .map(|s| s.trim_end_matches('\0').to_string());
            }
        }
    }
    None
}

/// Register an NSNotificationCenter observer for `NSWindowDidChangeScreenNotification`.
/// When the window moves to a different screen, sends `AppCommand::DisplayChanged` via the
/// global event loop proxy.
pub fn register_screen_change_observer(window: &Window) {
    use objc2::runtime::AnyClass;
    use objc2::sel;
    use objc2_app_kit::NSWindow;

    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const NSWindow = msg_send![ns_view, window];
        if ns_window.is_null() {
            log::warn!("Can't register screen change observer: no NSWindow");
            return;
        }

        // Get NSNotificationCenter.defaultCenter
        let nc_class = AnyClass::get(c"NSNotificationCenter").unwrap();
        let center: *const AnyObject = msg_send![nc_class, defaultCenter];

        // Create the notification name NSString
        let ns_string_class = AnyClass::get(c"NSString").unwrap();
        let name: *const AnyObject = msg_send![
            ns_string_class,
            stringWithUTF8String: c"NSWindowDidChangeScreenNotification".as_ptr()
        ];

        // We use a block-based observer to avoid needing a custom delegate class.
        // addObserverForName:object:queue:usingBlock: takes an NSOperationQueue (nil = current)
        // and a block. We use a C function pointer via objc2's block support.
        // Create a one-off ObjC class with a handler method.
        use std::sync::OnceLock;
        static OBSERVER_CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

        let cls = OBSERVER_CLASS.get_or_init(|| {
            use objc2::runtime::ClassBuilder;
            let superclass = AnyClass::get(c"NSObject").unwrap();
            let mut builder = ClassBuilder::new(c"PrvwScreenChangeObserver", superclass).unwrap();

            // The handler method: called when NSWindowDidChangeScreenNotification fires.
            // Uses *mut AnyObject for the receiver to satisfy objc2's MethodImplementation trait.
            unsafe extern "C-unwind" fn screen_did_change(
                _this: *mut AnyObject,
                _cmd: objc2::runtime::Sel,
                _notification: *mut AnyObject,
            ) {
                log::debug!("NSWindowDidChangeScreenNotification fired");
                crate::commands::send_command(crate::commands::AppCommand::DisplayChanged);
            }

            builder.add_method(
                sel!(screenDidChange:),
                screen_did_change
                    as unsafe extern "C-unwind" fn(
                        *mut AnyObject,
                        objc2::runtime::Sel,
                        *mut AnyObject,
                    ),
            );

            builder.register()
        });

        let cls = *cls;
        let observer: *const AnyObject = msg_send![cls, new];

        // Register: [center addObserver:observer selector:@selector(screenDidChange:)
        //            name:@"NSWindowDidChangeScreenNotification" object:ns_window]
        let _: () = msg_send![
            center,
            addObserver: observer,
            selector: sel!(screenDidChange:),
            name: name,
            object: ns_window
        ];

        // Leak the observer intentionally — it lives for the app's lifetime.
        // (The window and notification center outlive it.)
        log::debug!("Registered NSWindowDidChangeScreenNotification observer");
    }
}
