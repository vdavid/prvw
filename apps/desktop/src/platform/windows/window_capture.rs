//! Debug-only: photograph the main window the way a person sees it, for the QA server's
//! `screenshot_window` tool.
//!
//! `PrintWindow` rather than a `BitBlt` off the screen DC, because the E2E harness deliberately
//! leaves its windows unfocused and behind everything (`window::background_window_requested`).
//! Blitting the screen would capture whatever sits on top; `PrintWindow` asks the window itself
//! to draw, so an occluded or partly off-screen window still comes back whole.

use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, PW_RENDERFULLCONTENT};

/// A captured window: top-down 32-bit BGRA, tightly packed (32 bits per pixel is always
/// DWORD-aligned, so there's no row padding to strip).
pub struct Frame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Ask the window to draw itself into a bitmap, and hand back its pixels.
///
/// `hwnd` is the raw handle from `winit`'s window handle, passed as a `u64` so the QA layer's
/// command carries one type for both platforms.
pub fn capture(hwnd: u64) -> Result<Frame, String> {
    let hwnd = HWND(hwnd as *mut c_void);

    let mut rect = RECT::default();
    // SAFETY: `hwnd` came from a live winit window, and `rect` is ours to write.
    unsafe { GetWindowRect(hwnd, &mut rect) }
        .map_err(|why| format!("Couldn't measure the window: {why}"))?;
    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    if width == 0 || height == 0 {
        return Err("The window has no size to capture.".to_string());
    }

    // A negative height asks for a top-down DIB, which is the row order PNG wants. 32 bits per
    // pixel with `BI_RGB` means BGRA, and the alpha byte is whatever was there before: GDI
    // doesn't write it, which is why the encoder forces it opaque.
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `info` describes the buffer we're asking for, `bits` receives the pointer to it,
    // and `DIB_RGB_COLORS` is what makes the `None` device context legal (a colour table would
    // need one).
    let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }
        .map_err(|why| format!("Couldn't make room for the capture: {why}"))?;
    // Declared before the device context so it's dropped after: GDI refuses to delete a bitmap
    // that's still selected into a live DC.
    let bitmap = OwnedBitmap(bitmap);

    // SAFETY: a null argument asks for a memory DC compatible with the screen, which is what a
    // window capture wants.
    let device_context = unsafe { CreateCompatibleDC(None) };
    if device_context.is_invalid() {
        return Err("Couldn't make a device context for the capture.".to_string());
    }
    let device_context = OwnedDc(device_context);

    // SAFETY: both handles are live, and the default bitmap this replaces needs no restoring
    // because the DC is deleted whole.
    unsafe { SelectObject(device_context.0, bitmap.0.into()) };

    // `PW_RENDERFULLCONTENT` is what makes this work for a window whose client area is drawn by
    // the GPU: without it, a DirectComposition-backed surface comes back black.
    // SAFETY: the window and the device context are both live, and the DC has our bitmap in it.
    let drawn = unsafe {
        PrintWindow(
            hwnd,
            device_context.0,
            PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
        )
    };
    if !drawn.as_bool() {
        return Err("The window wouldn't draw itself into the capture.".to_string());
    }

    // GDI batches drawing calls, and reading a DIB's memory goes around the batch. A false
    // return means the batch was already empty, which is not a problem.
    // SAFETY: no arguments to get wrong.
    let _ = unsafe { GdiFlush() };

    let len = width as usize * height as usize * 4;
    // SAFETY: `CreateDIBSection` succeeded, so `bits` points at exactly `len` bytes for the
    // dimensions we asked for, and the bitmap outlives the copy.
    let bgra = unsafe { std::slice::from_raw_parts(bits as *const u8, len) }.to_vec();

    Ok(Frame {
        bgra,
        width,
        height,
    })
}

/// Deletes its bitmap on drop.
struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        // Nothing useful to do about a refusal while unwinding a capture.
        // SAFETY: the handle is ours and nothing else deleted it.
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}

/// Deletes its memory device context on drop.
struct OwnedDc(HDC);

impl Drop for OwnedDc {
    fn drop(&mut self) {
        // Same: a refusal here has no recovery worth writing.
        // SAFETY: the handle came from `CreateCompatibleDC`, which is what `DeleteDC` takes.
        let _ = unsafe { DeleteDC(self.0) };
    }
}
