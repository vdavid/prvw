//! `IShellItemImageFactory`: the Windows half of [`super::generator::Route::System`].
//!
//! This is the same service Explorer's own thumbnail view uses, backed by the per-user
//! `thumbcache_*.db` files. A folder someone has already scrolled through in Explorer costs a
//! cache read here rather than a decode, which is the whole reason the route exists — it's what
//! `quicklookd` is to macOS.
//!
//! ## Apartment, and why the pump rule doesn't bite
//!
//! Shell thumbnail providers are in-process COM objects registered as apartment-threaded, so
//! every worker enters an STA ([`Apartment`]) rather than the MTA. That matters twice over: an
//! MTA caller would have COM marshal each call into a single host STA and serialise the whole
//! pool behind it, and an out-of-process provider would then be answering on a thread that never
//! pumps.
//!
//! A synchronous outbound COM call from an STA runs a modal message loop **inside** the call, so
//! `AGENTS.md`'s "never open a nested message loop" rule is exactly why this can only happen on a
//! worker: that loop on the event-loop thread would be `about_to_wait` starving and the
//! slideshow freezing. On a `prvw-previewgen-*` thread it's nobody's pump but its own.
//!
//! ## Gotcha: ask for a thumbnail, never an icon
//!
//! `SIIGBF_THUMBNAILONLY` is the counterpart of the QuickLook `RepresentationTypes` gotcha in
//! `super::quicklook`, and it's there for the same reason. Without it the shell happily returns
//! the file type's generic icon for anything it can't render, and the app would blow that up to
//! fill the window as a placeholder. With it, such a file fails instead and the "Loading…" pill
//! covers the gap.
//!
//! ## Gotcha: never `SIIGBF_SCALEUP`
//!
//! The requested `SIZE` is a box the thumbnail is fitted into with its aspect ratio kept, and a
//! small image comes back small. `SIIGBF_SCALEUP` changes that to "always fill the box", which
//! pads with transparent margins — and the placeholder is drawn against source dimensions read
//! from the file's header, so padded pixels would show as an off-centre, wrongly-scaled image
//! that snaps into place when the real decode lands.

use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{
    IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY,
};
use windows::core::{Interface, PCWSTR};

use super::generator::dib_to_rgba8;
use super::request::PreviewPixels;

/// The biggest bitmap worth turning into a preview. Matches the macOS bridge's ceiling: past it
/// the allocation is worth more than the placeholder.
const MAX_EDGE_PX: i32 = 8192;

/// A thread's COM apartment, entered for as long as the worker runs.
pub struct Apartment {
    /// Whether this guard is the one that has to leave. A thread COM was already initialised on
    /// (winit's, in another mode) isn't ours to uninitialise.
    owned: bool,
}

impl Apartment {
    /// Enter the single-threaded apartment shell providers expect.
    pub fn enter() -> Self {
        // SAFETY: no arguments to get wrong, and the return value distinguishes every outcome.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            // Someone got here first with a different model. Every call below still works; it
            // just marshals. Not ours to undo, so don't.
            log::debug!("Preview worker joined an existing COM apartment of another model");
            return Self { owned: false };
        }
        if result.is_err() {
            log::warn!("Preview worker couldn't enter a COM apartment: {result:?}");
            return Self { owned: false };
        }
        Self { owned: true }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: paired with the successful `CoInitializeEx` above, on this same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Ask the shell for `path`'s thumbnail, at most `pixels` on its longest edge.
///
/// `path` is already in the spelling Win32 shell APIs take (`paths::shell_path` did that);
/// handing one a `\\?\` prefix would fail outright.
pub fn thumbnail(path: &str, pixels: u32) -> Option<PreviewPixels> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let edge = i32::try_from(pixels).ok()?.clamp(1, MAX_EDGE_PX);

    // SAFETY: `wide` is NUL-terminated and outlives the call; a null bind context is documented
    // as "use the default".
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }.ok()?;
    let factory: IShellItemImageFactory = item.cast().ok()?;

    // SAFETY: the factory is live, and the out-parameter is a bitmap handle we own from here.
    let bitmap = unsafe {
        factory.GetImage(
            SIZE { cx: edge, cy: edge },
            // Extracting is the point: a cache hit is fast either way, and `SIIGBF_INCACHEONLY`
            // would leave a folder nobody has browsed in Explorer with no previews at all.
            SIIGBF_THUMBNAILONLY,
        )
    }
    .ok()?;

    let preview = read_bitmap(bitmap);
    // SAFETY: `GetImage` transfers the bitmap to us, and nothing else refers to it now.
    let _ = unsafe { DeleteObject(bitmap.into()) };
    preview
}

/// Copy an `HBITMAP`'s pixels out as RGBA8.
fn read_bitmap(bitmap: HBITMAP) -> Option<PreviewPixels> {
    let mut header = BITMAP::default();
    // SAFETY: `bitmap` is a live GDI bitmap, and `header` is ours to fill, sized as declared.
    let read = unsafe {
        GetObjectW(
            bitmap.into(),
            size_of::<BITMAP>() as i32,
            Some(std::ptr::from_mut(&mut header).cast()),
        )
    };
    if read == 0 {
        return None;
    }
    let (width, height) = (header.bmWidth, header.bmHeight);
    if width <= 0 || height <= 0 || width > MAX_EDGE_PX || height > MAX_EDGE_PX {
        return None;
    }

    // A negative height asks for a top-down DIB, which is the row order the renderer uploads.
    // 32 bits per pixel with `BI_RGB` means B, G, R, A, and 4 bytes per pixel is always
    // DWORD-aligned, so there's no row padding to strip.
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buffer = vec![0u8; width as usize * height as usize * 4];

    // `GetDIBits` needs a DC only to name a device; the bitmap carries its own bits.
    // SAFETY: a null argument asks for a DC compatible with the screen.
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        return None;
    }
    // SAFETY: `info` describes exactly the layout `buffer` is sized for, and the bitmap is live.
    let rows = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(buffer.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: the DC is ours and nothing is selected into it.
    let _ = unsafe { DeleteDC(dc) };
    if rows != height {
        return None;
    }

    dib_to_rgba8(&mut buffer);
    Some(PreviewPixels {
        width: width as u32,
        height: height as u32,
        rgba: buffer,
    })
}
