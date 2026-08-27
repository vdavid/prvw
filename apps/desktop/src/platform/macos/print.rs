//! macOS print: print the current image via the system print dialog.
//!
//! Loads the original file (color-managed by ColorSync via its embedded ICC profile, same
//! as Copy — see `ui_common::load_image_from_path`), draws it aspect-fit onto a single page,
//! and runs `NSPrintOperation` as a window-modal **sheet**.
//!
//! Why a sheet and not `runOperation`: the app-modal `runOperation` spins a nested run loop,
//! which segfaults inside winit's event loop on autorelease-pool cleanup (see
//! `platform/macos/CLAUDE.md`). `runOperationModalForWindow:…` attaches an async sheet driven
//! by the existing run loop instead — no nested loop, same family as our non-modal windows.

use std::ffi::c_void;
use std::path::Path;
use std::ptr;

use objc2::rc::Retained;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSCompositingOperation, NSImage, NSPrintInfo, NSPrintOperation, NSView, NSWindow,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize};

use crate::printing;

use super::ui_common::load_image_from_path;

/// Ivars for [`PrintView`]: the image to draw. Retained here so it outlives the print
/// operation — `NSPrintOperation` retains the view, the view retains the image.
struct PrintViewIvars {
    image: Retained<NSImage>,
}

define_class!(
    /// A one-page print view that draws its image aspect-fit and centered in the page's
    /// printable area. Non-flipped (the default), so `drawInRect:` renders right-side-up.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwPrintView"]
    #[ivars = PrintViewIvars]
    struct PrintView;

    unsafe impl NSObjectProtocol for PrintView {}

    impl PrintView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let bounds: NSRect = unsafe { msg_send![self, bounds] };
            let image = &self.ivars().image;
            let Some(dest) = aspect_fit_rect(bounds, image.size()) else {
                return;
            };
            image.drawInRect_fromRect_operation_fraction(
                dest,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
                NSCompositingOperation::SourceOver,
                1.0,
            );
        }

        /// Force a single page: the whole image fits one sheet.
        #[unsafe(method(knowsPageRange:))]
        fn knows_page_range(&self, range: *mut NSRange) -> bool {
            if !range.is_null() {
                // SAFETY: AppKit hands us a valid NSRange slot to fill in.
                unsafe { *range = NSRange::new(1, 1) };
            }
            true
        }

        #[unsafe(method(rectForPage:))]
        fn rect_for_page(&self, _page: isize) -> NSRect {
            unsafe { msg_send![self, bounds] }
        }
    }
);

/// The `NSRect` adapter over [`crate::printing::aspect_fit`], which Windows shares. AppKit's
/// page origin is bottom-left and GDI's is top-left; the fit is centred, so the same numbers
/// answer for both (see that module's docs).
fn aspect_fit_rect(bounds: NSRect, image_size: NSSize) -> Option<NSRect> {
    let page = printing::Rect::new(
        bounds.origin.x,
        bounds.origin.y,
        bounds.size.width,
        bounds.size.height,
    );
    let fitted = printing::aspect_fit(page, image_size.width, image_size.height)?;
    Some(NSRect::new(
        NSPoint::new(fitted.x, fitted.y),
        NSSize::new(fitted.width, fitted.height),
    ))
}

impl PrintView {
    fn new(mtm: MainThreadMarker, image: Retained<NSImage>, frame: NSRect) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(PrintViewIvars { image });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Print the image at `path`, presenting the system print sheet on `parent_window`.
///
/// Returns the running `NSPrintOperation` so the caller can keep it alive for the duration
/// of the (async) sheet, or `None` if the file couldn't be loaded. Does nothing useful
/// without a parent window — the sheet needs one to attach to.
pub(crate) fn print_image_file(
    path: &Path,
    parent_window: &NSWindow,
) -> Option<Retained<NSPrintOperation>> {
    let mtm = MainThreadMarker::new()?;
    let image = load_image_from_path(path)?;

    // Size the print view to the default page's printable area (paper minus margins). The
    // image is then aspect-fit inside it. Switching paper/printer in the dialog won't reflow
    // the fixed size — fine for a simple "fit to page" print.
    let print_info = NSPrintInfo::sharedPrintInfo();
    let page = print_info.imageablePageBounds();
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), page.size);
    let view = PrintView::new(mtm, image, frame);

    let operation =
        NSPrintOperation::printOperationWithView_printInfo(view.as_super(), &print_info);
    operation.setShowsPrintPanel(true);
    operation.setShowsProgressPanel(true);

    // Window-modal sheet (async). nil delegate + nil selector: we have no completion work,
    // and the operation is kept alive by the caller until the sheet finishes.
    // SAFETY: valid window; nil delegate/selector and a null context are explicitly allowed.
    unsafe {
        operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
            parent_window,
            None,
            None,
            ptr::null_mut::<c_void>(),
        );
    }

    Some(operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// The adapter carries origin and size across in the right order. The fit itself is
    /// `printing::aspect_fit`, tested there for every platform at once.
    #[test]
    fn the_nsrect_adapter_carries_the_fit_across() {
        let dest =
            aspect_fit_rect(rect(10.0, 20.0, 200.0, 200.0), NSSize::new(100.0, 50.0)).unwrap();
        // scale = min(200/100, 200/50) = 2.0 → 200×100, centred: y = 20 + (200−100)/2 = 70.
        assert_eq!(dest.origin.x, 10.0);
        assert_eq!(dest.origin.y, 70.0);
        assert_eq!(dest.size.width, 200.0);
        assert_eq!(dest.size.height, 100.0);
    }

    /// A zero-area image yields no draw rect rather than a NaN/divide-by-zero.
    #[test]
    fn degenerate_image_yields_none() {
        assert!(aspect_fit_rect(rect(0.0, 0.0, 100.0, 100.0), NSSize::new(0.0, 0.0)).is_none());
    }

    /// A real file loads into an NSImage; a missing one returns None instead of a broken handle.
    #[test]
    fn loads_real_file_and_rejects_missing() {
        let path = fixture("p3_red_64x64.jpg");
        assert!(path.exists(), "fixture missing: {}", path.display());
        assert!(load_image_from_path(&path).is_some());
        assert!(load_image_from_path(&fixture("does-not-exist.jpg")).is_none());
    }
}
