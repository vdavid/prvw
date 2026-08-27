//! macOS display colour profile detection and CAMetalLayer colorspace management.
//!
//! Uses CoreGraphics FFI to query the active display's ICC profile and set the
//! Metal layer's colorspace so the compositor applies the correct colour management.
//! The shared half of this feature, and what every platform owes it, is in
//! [`super`].

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

#[allow(non_camel_case_types)]
type CFStringRef = *const std::ffi::c_void;

unsafe extern "C" {
    /// Get the main display ID.
    fn CGMainDisplayID() -> CGDirectDisplayID;

    /// Get the color space associated with a display.
    fn CGDisplayCopyColorSpace(display: CGDirectDisplayID) -> CGColorSpaceRef;

    /// Extract the raw ICC profile data from a color space.
    fn CGColorSpaceCopyICCData(space: CGColorSpaceRef) -> CFDataRef;

    /// Create a color space from ICC profile data.
    fn CGColorSpaceCreateWithICCData(data: CFDataRef) -> CGColorSpaceRef;

    /// Create a named color space (for EDR: kCGColorSpaceExtendedLinearDisplayP3).
    fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;

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

    /// `kCGColorSpaceExtendedLinearDisplayP3`: Display-P3 primaries with a
    /// linear (gamma 1.0) transfer function and signed / above-1.0 values.
    /// The HDR RAW path in `decoding::raw` bypasses `moxcms` and applies a
    /// direct `linear Rec.2020 → linear Display P3` matrix
    /// (`color::profiles::REC2020_TO_LINEAR_DISPLAY_P3_D65`), so the f16
    /// texture we hand to Metal is already linear — naming the linear
    /// colorspace here matches that contract and keeps above-1.0
    /// highlights addressable by the EDR compositor. An earlier version
    /// of this path fed gamma-encoded moxcms output through
    /// `kCGColorSpaceExtendedDisplayP3`; we abandoned that when we
    /// discovered moxcms was clipping at 1.0 regardless, which defeated
    /// the whole HDR pipeline.
    static kCGColorSpaceExtendedLinearDisplayP3: CFStringRef;
}

/// `MTLPixelFormatRGBA16Float` from Metal's `MTLPixelFormat` enum.
/// Reference: <https://developer.apple.com/documentation/metal/mtlpixelformat>.
const MTL_PIXEL_FORMAT_RGBA16_FLOAT: u64 = 115;
/// `MTLPixelFormatBGRA8Unorm_sRGB`: the SDR default wgpu/Metal pair.
const MTL_PIXEL_FORMAT_BGRA8_UNORM_SRGB: u64 = 81;

/// Get the ICC profile bytes for the display the window is currently on.
/// Returns `None` if the display profile can't be queried (headless, SSH, etc.).
pub fn display_icc(window: &Window) -> Option<Vec<u8>> {
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
            super::describe_for_log(&bytes)
        );
        super::usable_profile(bytes)
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

/// Configure the wgpu `CAMetalLayer` for EDR output. Sets three properties
/// in lockstep so they stay consistent with each other and with the wgpu
/// surface configuration the caller has already applied:
///
/// - `wantsExtendedDynamicRangeContent` — tells the compositor to allocate
///   a float-capable backing store and route the window through the EDR
///   path on XDR / OLED displays.
/// - `pixelFormat` — `MTLPixelFormatRGBA16Float` (115) when EDR is active,
///   `MTLPixelFormatBGRA8Unorm_sRGB` (81) when not. Must match the wgpu
///   `SurfaceConfiguration.format` the caller set.
/// - `colorspace` — `kCGColorSpaceExtendedLinearDisplayP3` for EDR so the
///   compositor reads our linear, above-1.0 values as HDR headroom.
///   On SDR, the caller's existing `set_layer_colorspace` (ICC-based) is
///   the right source of truth; pass the display ICC through `icc_for_sdr`
///   and this function restores it.
///
/// The `icc_for_sdr` argument is only consulted when `edr_active == false`.
/// When the caller has no ICC bytes to restore (for example, ICC
/// management is off), pass an empty slice — the colorspace is left
/// whatever it was.
///
/// `headroom_now` is only logged. It's the caller's `App::edr_headroom`
/// rather than a fresh query, so the diagnostic below can't disagree with
/// the number the decode path actually ran on.
pub fn set_layer_edr_state(
    window: &Window,
    edr_active: bool,
    icc_for_sdr: &[u8],
    headroom_now: f32,
) {
    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let root_layer: *const AnyObject = msg_send![ns_view, layer];
        if root_layer.is_null() {
            log::warn!("NSView has no layer, can't set EDR state");
            return;
        }

        let metal_layer = find_metal_layer(root_layer);
        if metal_layer.is_null() {
            log::warn!("No CAMetalLayer found, can't set EDR state");
            return;
        }

        // wantsExtendedDynamicRangeContent: BOOL
        let _: () = msg_send![metal_layer, setWantsExtendedDynamicRangeContent: edr_active];

        // Read the property back. On rare occasions the OS refuses the
        // request (older macOS on non-Metal displays, remote desktop,
        // specific driver bugs); a YES-requested / NO-granted mismatch is
        // the clearest signal we're on such a path.
        let granted: bool = msg_send![metal_layer, wantsExtendedDynamicRangeContent];
        log::info!(
            "render: CAMetalLayer EDR state confirmed: wantsExtendedDynamicRangeContent={} \
             (was requested: {}, OS granted: {}, NSScreen headroom: {:.2})",
            if granted { "YES" } else { "NO" },
            if edr_active { "YES" } else { "NO" },
            if granted == edr_active { "YES" } else { "NO" },
            headroom_now
        );

        // pixelFormat: MTLPixelFormat (NSUInteger -> u64)
        let pixel_format: u64 = if edr_active {
            MTL_PIXEL_FORMAT_RGBA16_FLOAT
        } else {
            MTL_PIXEL_FORMAT_BGRA8_UNORM_SRGB
        };
        let _: () = msg_send![metal_layer, setPixelFormat: pixel_format];

        if edr_active {
            // Linear Display P3, signed / above-1.0 values. Matches the
            // linear f16 output from the HDR path's direct-matrix color
            // conversion (see `decoding::raw` and
            // `color::profiles::rec2020_to_linear_display_p3_inplace`).
            // The "Extended" variant is what keeps above-1.0 values
            // alive — plain `kCGColorSpaceDisplayP3` clamps at 1.0.
            let color_space = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearDisplayP3);
            if color_space.is_null() {
                log::warn!(
                    "CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearDisplayP3) returned null"
                );
            } else {
                assign_colorspace(metal_layer, color_space);
                CFRelease(color_space);
                log::info!(
                    "render: CAMetalLayer EDR on (wantsExtendedDynamicRangeContent=YES, pixelFormat=RGBA16Float, colorspace=extendedLinearDisplayP3)"
                );
            }
        } else {
            // Restore the display ICC colorspace when we have one. When
            // `icc_for_sdr` is empty (ICC management disabled), leave
            // whatever colorspace the layer had — the compositor still
            // interprets RGB as sRGB by default.
            if !icc_for_sdr.is_empty() {
                set_colorspace_on_layer(metal_layer, icc_for_sdr);
            }
            log::info!(
                "render: CAMetalLayer EDR off (wantsExtendedDynamicRangeContent=NO, pixelFormat=BGRA8Unorm_sRGB)"
            );
        }
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

/// Set the colorspace on a CAMetalLayer pointer, and say so at `info`.
unsafe fn set_colorspace_on_layer(layer: *const AnyObject, icc_bytes: &[u8]) {
    if unsafe { assign_icc_colorspace(layer, icc_bytes) } {
        log::info!(
            "Set CAMetalLayer colorspace{}",
            super::describe_icc(icc_bytes)
                .map(|d| format!(" to {d}"))
                .unwrap_or_default()
        );
    }
}

/// Build a `CGColorSpace` from ICC bytes and hand it to the layer. Answers whether it took.
/// Silent, so the callers that run per frame or per resize can stay quiet.
unsafe fn assign_icc_colorspace(layer: *const AnyObject, icc_bytes: &[u8]) -> bool {
    unsafe {
        let cf_data = CFDataCreate(
            std::ptr::null(),
            icc_bytes.as_ptr(),
            icc_bytes.len() as isize,
        );
        if cf_data.is_null() {
            log::warn!("CFDataCreate failed for ICC profile");
            return false;
        }

        let color_space = CGColorSpaceCreateWithICCData(cf_data);
        CFRelease(cf_data);

        if color_space.is_null() {
            log::warn!("CGColorSpaceCreateWithICCData failed");
            return false;
        }
        assign_colorspace(layer, color_space);
        CFRelease(color_space);
        true
    }
}

/// `[layer setColorspace:space]`, through raw `objc_msgSend`.
///
/// **Gotcha:** the typed `msg_send!` can't express this one. Our `CGColorSpaceRef` is
/// `*const c_void`, which encodes as `^v`, and ObjC expects `^{CGColorSpace=}`.
unsafe fn assign_colorspace(layer: *const AnyObject, color_space: CGColorSpaceRef) {
    unsafe {
        let sel = objc2::sel!(setColorspace:);
        let send: unsafe extern "C" fn(*const AnyObject, objc2::runtime::Sel, CGColorSpaceRef) =
            std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
        send(layer, sel, color_space);
    }
}

/// Put the layer's colourspace back to the one this app decided on, after wgpu set its own.
///
/// **Why this exists.** Since wgpu 30, `Surface::configure` writes the `CAMetalLayer`
/// colourspace itself, from `SurfaceConfiguration::color_space`, and that field can only name
/// standard colour spaces. Neither of Prvw's two answers is in that vocabulary:
///
/// - **SDR** wants the *display's own ICC profile*. The pixels arriving have already been
///   transformed into it, so any other colourspace makes the compositor transform them a second
///   time, which is exactly the double-transform display matching exists to avoid.
/// - **EDR** wants **linear** Display P3, to match the direct Rec.2020 → linear P3 matrix the HDR
///   path applies. wgpu offers `ExtendedSrgbLinear` (linear, but BT.709 primaries) and
///   `ExtendedDisplayP3` (P3, but gamma-encoded), and neither is that pair.
///
/// So the renderer says it again after every `Surface::configure`. Quiet, because that includes
/// every window resize; the loud version is [`set_layer_colorspace`].
pub fn restore_layer_colorspace(window: &Window, edr_active: bool, icc_for_sdr: &[u8]) {
    if !edr_active && icc_for_sdr.is_empty() {
        return;
    }
    let Ok(handle) = window.window_handle().map(|h| h.as_raw()) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle else {
        return;
    };

    unsafe {
        let ns_view = handle.ns_view.as_ptr() as *const AnyObject;
        let root_layer: *const AnyObject = msg_send![ns_view, layer];
        if root_layer.is_null() {
            return;
        }
        let metal_layer = find_metal_layer(root_layer);
        if metal_layer.is_null() {
            return;
        }

        if edr_active {
            let color_space = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearDisplayP3);
            if color_space.is_null() {
                return;
            }
            assign_colorspace(metal_layer, color_space);
            CFRelease(color_space);
            log::trace!("Restored the CAMetalLayer's extendedLinearDisplayP3 colorspace");
        } else {
            assign_icc_colorspace(metal_layer, icc_for_sdr);
            log::trace!("Restored the CAMetalLayer's display-ICC colorspace");
        }
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
