//! Windows clipboard: copy the current image to the system clipboard.
//!
//! The counterpart to `platform::macos::clipboard`, and it makes the same promise: whoever
//! pastes gets the **original file** where they can take one, and sRGB pixels where they can't.
//! `crate::clipboard` builds the byte layouts (and explains why there are three of them); this
//! module owns the memory blocks and the Win32 calls.
//!
//! ## The pixels come from the file, never from the screen
//!
//! `App` already holds a decoded buffer, and it is the wrong one to copy: it's been transformed
//! to the display's profile and may be half-float HDR, so pasting it would shift colours in
//! whatever opens it next. So this decodes the original file again, targeting sRGB
//! (`ui_common::decode_srgb`, shared with Print), exactly as macOS re-reads the file through
//! ImageIO. A RAW file gets Prvw's own pipeline here rather than the OS's, so unlike macOS a
//! copied RAW matches what the viewer showed.
//!
//! ## Off the event-loop thread
//!
//! That decode is a full decode: milliseconds for a JPEG, seconds for a 50 MP RAW. Doing it on
//! the winit thread would freeze the window mid-copy, so [`copy_image_file`] hands the whole job
//! to a worker and returns. Win32 is fine with that: `OpenClipboard` belongs to the calling
//! thread, and none of this needs a window.
//!
//! **No message loop anywhere in here**, which is the point worth stating out loud given how
//! easily a Win32 modal loop starves winit's pump (`AGENTS.md`). Clipboard data that isn't
//! delay-rendered is copied into the system's own storage as it's set, so nothing has to stay
//! around to answer `WM_RENDERFORMAT` and no pump is involved.
//!
//! ## Ownership
//!
//! Every block starts out ours and stops being ours the instant `SetClipboardData` accepts it.
//! Freeing one afterwards corrupts the clipboard; not freeing one it rejected leaks for the life
//! of the process. [`GlobalBlock`] is the seam: it frees on drop, and handing it over consumes
//! it.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP};
use windows::core::w;

use super::ui_common::decode_srgb;
use crate::clipboard::{self, WindowsBitmaps};
use crate::decoding::RawPipelineFlags;

/// How many times to ask for the clipboard before giving up. Another process holds it while it
/// writes, which is brief but real, and `OpenClipboard` fails outright rather than waiting.
const OPEN_ATTEMPTS: u32 = 10;

/// Wait between those attempts. Ten of these is a quarter second, which is far longer than any
/// well-behaved app holds the clipboard and short enough that the worker doesn't linger.
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(25);

/// `DROPEFFECT_COPY` from `ole2.h`. It rides along under the "Preferred DropEffect" format to
/// tell Explorer that pasting should copy the file rather than move it. Named here because
/// nothing else in the app calls into the OLE namespace.
const DROPEFFECT_COPY: u32 = 1;

/// Copy the image at `path` to the clipboard, as the original file plus sRGB pixels.
///
/// Returns as soon as the worker is running; the outcome shows up in the log. `raw_flags` and
/// `relative_colorimetric` are the app's current decode settings, so a copied RAW matches the
/// viewer's own rendering.
pub(crate) fn copy_image_file(
    path: &Path,
    raw_flags: RawPipelineFlags,
    relative_colorimetric: bool,
) {
    let path = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("clipboard-copy".to_string())
        .spawn(move || copy_on_worker(&path, raw_flags, relative_colorimetric));
    if let Err(err) = spawned {
        log::warn!("Copy image: couldn't start the clipboard worker: {err}");
    }
}

/// The whole copy, on the worker thread. The bitmap is optional on purpose: a file that won't
/// decode (corrupt, or a format the pixels came from somewhere else) still belongs on the
/// clipboard as a file, which is the representation Explorer and chat apps want anyway.
fn copy_on_worker(path: &Path, raw_flags: RawPipelineFlags, relative_colorimetric: bool) {
    let start = Instant::now();
    let bitmaps = decode_srgb(path, raw_flags, relative_colorimetric)
        .map(|(width, height, rgba)| clipboard::windows_bitmaps(width, height, &rgba));
    if bitmaps.is_none() {
        log::warn!(
            "Copy image: {} wouldn't decode, so only the file goes on the clipboard",
            path.display()
        );
    }
    match write(path, bitmaps.as_ref()) {
        Ok(()) => log::info!(
            "Copied image to clipboard in {} ms: {}",
            start.elapsed().as_millis(),
            path.display()
        ),
        Err(err) => log::warn!("Copy image: {err} ({})", path.display()),
    }
}

/// Put the file and the bitmaps on the clipboard, in the order a consumer that takes the first
/// format it recognises should see them: the lossless original first, pixels after.
fn write(path: &Path, bitmaps: Option<&WindowsBitmaps>) -> Result<(), String> {
    let _session = ClipboardSession::open()?;

    // Everything already there is someone else's; the clipboard holds one thing at a time.
    // SAFETY: the session guarantees the clipboard is open and owned by this thread.
    unsafe { EmptyClipboard() }.map_err(|err| format!("EmptyClipboard failed: {err}"))?;

    let mut offered = 0;
    // No file list when the path has no shell form (`clipboard::shell_path` says when). The
    // pixels below still go up, so the copy is worth less rather than being lost.
    if let Some(list) = clipboard::hdrop(&[path])
        && let Some(block) = GlobalBlock::holding(&list)
    {
        offered += u32::from(block.give_to_clipboard(CF_HDROP.0.into()));
        // Only meaningful next to CF_HDROP, and only to Explorer: it's the difference between a
        // paste that copies the file and one that moves it.
        if let Some(effect) = GlobalBlock::holding(&DROPEFFECT_COPY.to_le_bytes()) {
            // SAFETY: a static, null-terminated wide string. Registering a format that already
            // exists returns the existing id, so this is safe to call on every copy.
            let format = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
            if format != 0 {
                effect.give_to_clipboard(format);
            }
        }
    }
    if let Some(bitmaps) = bitmaps {
        if let Some(block) = GlobalBlock::holding(&bitmaps.dib) {
            offered += u32::from(block.give_to_clipboard(CF_DIB.0.into()));
        }
        if let Some(v5) = &bitmaps.dib_v5
            && let Some(block) = GlobalBlock::holding(v5)
        {
            offered += u32::from(block.give_to_clipboard(CF_DIBV5.0.into()));
        }
    }

    if offered == 0 {
        return Err("nothing could be put on the clipboard".to_string());
    }
    Ok(())
}

/// The clipboard, open and owned by this thread until the guard drops.
///
/// A guard rather than a pair of calls because every path out of [`write`] has to close it:
/// leaving it open blocks every other process from copying anything.
struct ClipboardSession;

impl ClipboardSession {
    /// Take the clipboard, retrying while another process still has it.
    fn open() -> Result<Self, String> {
        let mut last = String::new();
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: `None` asks for no owner window, which is what a worker thread with no
            // window of its own wants. Nothing here delay-renders, so no owner is ever called
            // back.
            match unsafe { OpenClipboard(None) } {
                Ok(()) => return Ok(Self),
                Err(err) => last = err.to_string(),
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(OPEN_RETRY_DELAY);
            }
        }
        Err(format!("couldn't open the clipboard: {last}"))
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        // SAFETY: the clipboard is open on this thread, which is the guard's whole invariant.
        if let Err(err) = unsafe { CloseClipboard() } {
            log::warn!("Copy image: CloseClipboard failed: {err}");
        }
    }
}

/// A movable memory block holding one clipboard format's bytes, still owned by us.
struct GlobalBlock(HGLOBAL);

impl GlobalBlock {
    /// Allocate a block and copy `bytes` into it. `None` when the allocation fails, which for a
    /// very large image is a real possibility rather than a formality.
    fn holding(bytes: &[u8]) -> Option<Self> {
        // SAFETY: `GMEM_MOVEABLE` is what `SetClipboardData` requires, and the length is the
        // slice's own. A failed allocation comes back as `Err`.
        let handle = match unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) } {
            Ok(handle) => handle,
            Err(err) => {
                log::warn!("Copy image: couldn't allocate {} bytes: {err}", bytes.len());
                return None;
            }
        };
        let block = Self(handle);

        // SAFETY: locking a block we just allocated returns a pointer to at least `bytes.len()`
        // bytes, and the copy stays inside it. `block` frees the allocation if we bail out.
        unsafe {
            let destination = GlobalLock(handle);
            if destination.is_null() {
                log::warn!("Copy image: GlobalLock failed");
                return None;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination.cast::<u8>(), bytes.len());
            // Returns an error when the lock count reaches zero, which is the normal outcome
            // here and not a failure, so there's nothing to check.
            let _ = GlobalUnlock(handle);
        }
        Some(block)
    }

    /// Hand the block to the clipboard under `format`. Consumes it: on success the system owns
    /// the memory and freeing it would corrupt the clipboard, and on failure it's freed here.
    /// Returns whether the clipboard took it.
    fn give_to_clipboard(self, format: u32) -> bool {
        let block = std::mem::ManuallyDrop::new(self);
        // SAFETY: called with the clipboard open (the `ClipboardSession` in `write`), on a
        // `GMEM_MOVEABLE` block this thread owns. Ownership transfers on success.
        match unsafe { SetClipboardData(format, Some(HANDLE(block.0.0))) } {
            Ok(_) => true,
            Err(err) => {
                log::warn!("Copy image: SetClipboardData({format}) failed: {err}");
                // SAFETY: the clipboard refused the block, so it's still ours to free, and
                // `ManuallyDrop` means nothing else will.
                unsafe {
                    let _ = GlobalFree(Some(block.0));
                }
                false
            }
        }
    }
}

impl Drop for GlobalBlock {
    fn drop(&mut self) {
        // SAFETY: only reached while the block is still ours; `give_to_clipboard` takes it out
        // of drop's reach before the clipboard can own it.
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}
