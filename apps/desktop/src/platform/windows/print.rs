//! Windows print: the system print dialog, then the image drawn to fill one page.
//!
//! The counterpart of `platform::macos::print`, and it does the same thing by different means:
//! re-read the original file (`ui_common::decode_srgb`, shared with Copy), lay it out with
//! `printing::aspect_fit`, and hand it to the print system. Where macOS attaches an
//! `NSPrintOperation` sheet, Windows puts up `PrintDlgW` and then draws onto the printer DC it
//! hands back.
//!
//! ## The dialog runs on a worker, and that's the whole design
//!
//! `PrintDlgW` is modal: it opens a message loop and doesn't return until the person picks a
//! printer or cancels. On winit's thread that loop is `AGENTS.md`'s starved pump —
//! `about_to_wait` stops running, the slideshow timer freezes, `EventLoopProxy` events stall.
//! `platform::windows::msg_hook` states the rule and `open_dialog` already follows it for the
//! file picker, so this does too: [`print_image_file`] hands the whole job to a thread and
//! returns.
//!
//! Win32 is fine with that. A common dialog is modal to **its own** thread, and naming the main
//! window as `hwndOwner` still disables it for the duration and keeps the dialog in front of it,
//! which is the behaviour a person expects. The decode and the spooling then happen on the same
//! worker, which they'd have to anyway: a 50 MP RAW is seconds of work.
//!
//! ## One page, filled
//!
//! `GetDeviceCaps(HORZRES/VERTRES)` is the printable area in device pixels, and the image is
//! aspect-fitted into it. Not `PHYSICALWIDTH`, which is the whole sheet including the margins
//! the hardware can't reach: drawing there puts part of the photo where no ink goes.

use std::path::Path;
use std::time::Instant;

use windows::Win32::Foundation::{GlobalFree, HWND};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteDC, GET_DEVICE_CAPS_INDEX,
    GetDeviceCaps, HDC, HORZRES, SRCCOPY, StretchDIBits, VERTRES,
};
use windows::Win32::Storage::Xps::{DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_HIDEPRINTTOFILE, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW, PrintDlgW,
};
use windows::core::PCWSTR;

use super::ui_common::decode_srgb;
use crate::decoding::RawPipelineFlags;
use crate::printing::{self, Rect};

/// Print the image at `path`, with the system dialog owned by `owner_hwnd`.
///
/// Returns as soon as the worker is running; the outcome shows up in the log, the way Copy's
/// does. `owner_hwnd` travels as a `u64` because `HWND` is a raw pointer and so not `Send` — the
/// same crossing `window_capture::capture` makes.
pub(crate) fn print_image_file(
    path: &Path,
    owner_hwnd: u64,
    raw_flags: RawPipelineFlags,
    relative_colorimetric: bool,
) {
    let path = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("prvw-print".to_string())
        .spawn(move || {
            let owner = HWND(owner_hwnd as *mut std::ffi::c_void);
            match print_on_worker(&path, owner, raw_flags, relative_colorimetric) {
                Ok(true) => log::info!("Printed {}", path.display()),
                Ok(false) => log::debug!("Print: the dialog was dismissed"),
                Err(why) => log::warn!("Print: {why} ({})", path.display()),
            }
        });
    if let Err(err) = spawned {
        log::warn!("Print: couldn't start the print worker: {err}");
    }
}

/// The whole print, on the worker thread. `Ok(false)` means the person cancelled, which is not a
/// failure and gets no warning.
fn print_on_worker(
    path: &Path,
    owner: HWND,
    raw_flags: RawPipelineFlags,
    relative_colorimetric: bool,
) -> Result<bool, String> {
    let Some(dc) = show_dialog(owner)? else {
        return Ok(false);
    };
    let printed = draw_one_page(dc, path, raw_flags, relative_colorimetric);
    // The DC is ours from `PD_RETURNDC` onwards, whatever happened to the drawing.
    // SAFETY: nothing else refers to it, and the document is already ended or aborted.
    let _ = unsafe { DeleteDC(dc) };
    printed.map(|()| true)
}

/// Put up the print dialog and return the printer DC it hands back, or `None` if dismissed.
fn show_dialog(owner: HWND) -> Result<Option<HDC>, String> {
    let mut dialog = PRINTDLGW {
        lStructSize: size_of::<PRINTDLGW>() as u32,
        hwndOwner: owner,
        // `PD_RETURNDC` is what we're here for: a DC for the chosen printer, with the person's
        // paper, orientation, and quality already applied. The three `NO`/`HIDE` flags drop
        // choices a one-page image print can't honour, rather than showing them dead.
        Flags: PD_RETURNDC
            | PD_NOPAGENUMS
            | PD_NOSELECTION
            | PD_HIDEPRINTTOFILE
            | PD_USEDEVMODECOPIESANDCOLLATE,
        nCopies: 1,
        ..Default::default()
    };

    // SAFETY: `dialog` is fully initialised and declares its own size; the call fills in the
    // handles below, which are ours to free from here.
    let chosen = unsafe { PrintDlgW(&mut dialog) }.as_bool();
    // The dialog allocates these whether or not the person went ahead, and they're ours either
    // way. SAFETY: both are `GlobalAlloc` blocks nothing else holds now.
    for handle in [dialog.hDevMode, dialog.hDevNames] {
        if !handle.is_invalid() {
            let _ = unsafe { GlobalFree(Some(handle)) };
        }
    }
    if !chosen {
        // SAFETY: no arguments. Zero means "the person cancelled", anything else is a real fault.
        let error = unsafe { CommDlgExtendedError() };
        if error.0 == 0 {
            return Ok(None);
        }
        return Err(format!("the print dialog failed (0x{:04x})", error.0));
    }
    if dialog.hDC.is_invalid() {
        return Err("the print dialog returned no printer".to_string());
    }
    Ok(Some(dialog.hDC))
}

/// Decode the file and blit it onto one page of `dc`.
fn draw_one_page(
    dc: HDC,
    path: &Path,
    raw_flags: RawPipelineFlags,
    relative_colorimetric: bool,
) -> Result<(), String> {
    let start = Instant::now();
    let (width, height, mut pixels) = decode_srgb(path, raw_flags, relative_colorimetric)
        .ok_or_else(|| "the file wouldn't decode".to_string())?;
    printing::flatten_onto_white_bgra(&mut pixels);

    let page = Rect::new(
        0.0,
        0.0,
        f64::from(caps(dc, HORZRES)),
        f64::from(caps(dc, VERTRES)),
    );
    let placement = printing::aspect_fit(page, f64::from(width), f64::from(height))
        .ok_or_else(|| "the page or the image has no area to draw in".to_string())?;

    // A negative height means a top-down DIB, which is the row order the decoder produces.
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

    let name: Vec<u16> = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let document = DOCINFOW {
        cbSize: size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: the DC came from `PD_RETURNDC` and `document` outlives the call.
    if unsafe { StartDocW(dc, &document) } <= 0 {
        return Err("the printer wouldn't start the job".to_string());
    }
    // SAFETY: the document is open on this DC.
    if unsafe { StartPage(dc) } <= 0 {
        // SAFETY: ending a document with no page open is how a failed job is closed.
        let _ = unsafe { EndDoc(dc) };
        return Err("the printer wouldn't start the page".to_string());
    }
    // SAFETY: `info` describes exactly the buffer `pixels` holds, and the rect is on the page.
    let drawn = unsafe {
        StretchDIBits(
            dc,
            placement.x.round() as i32,
            placement.y.round() as i32,
            placement.width.round() as i32,
            placement.height.round() as i32,
            0,
            0,
            width as i32,
            height as i32,
            Some(pixels.as_ptr().cast()),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        )
    };
    // SAFETY: the page and the document are both open on this DC.
    let ended = unsafe { EndPage(dc) } > 0 && unsafe { EndDoc(dc) } > 0;
    if drawn == 0 || !ended {
        return Err("the printer rejected the page".to_string());
    }
    log::debug!(
        "Print: {width}x{height} laid out at {}x{} in {} ms",
        placement.width.round(),
        placement.height.round(),
        start.elapsed().as_millis()
    );
    Ok(())
}

/// One `GetDeviceCaps` value, floored at zero so a driver that answers nonsense can't turn into
/// a negative page.
fn caps(dc: HDC, index: GET_DEVICE_CAPS_INDEX) -> i32 {
    // SAFETY: `dc` is a live printer DC and every index here is a documented one.
    unsafe { GetDeviceCaps(Some(dc), index) }.max(0)
}
