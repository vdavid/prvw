//! macOS print: print the current image via the system print dialog.
//!
//! Loads the original file (color-managed by ColorSync via its embedded ICC profile, same
//! as Copy — see `ui_common::load_image_from_path`), draws it aspect-fit onto a single page,
//! turning it a quarter turn when that fills more of the paper, and runs `NSPrintOperation`
//! as a window-modal **sheet**.
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
    NSAffineTransformNSAppKitAdditions, NSCompositingOperation, NSGraphicsContext, NSImage,
    NSPrintInfo, NSPrintOperation, NSView, NSWindow,
};
use objc2_foundation::{NSAffineTransform, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize};

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
            let Some(placement) = fit_to_page_rect(bounds, image.size()) else {
                return;
            };
            let whole_image = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            if !placement.auto_rotated {
                image.drawInRect_fromRect_operation_fraction(
                    placement.rect,
                    whole_image,
                    NSCompositingOperation::SourceOver,
                    1.0,
                );
                return;
            }

            // Turn the page under the image rather than the image on the page: AppKit composites
            // through the transform, so the printer gets one resampling pass at its own
            // resolution instead of a rotated bitmap we resampled ourselves. Windows can't do
            // this — GDI won't rotate a blit — and turns the pixels instead.
            //
            // The page is y-up (the view is non-flipped), so a negative angle turns the image
            // clockwise, matching `printing::rotate_quarter_turn_clockwise`. Inside the turned
            // space the image's own width is the placement's height, so it's drawn centred on
            // the origin the transform was translated to.
            let turned = NSRect::new(
                NSPoint::new(-placement.rect.size.height / 2.0, -placement.rect.size.width / 2.0),
                NSSize::new(placement.rect.size.height, placement.rect.size.width),
            );
            let transform = NSAffineTransform::transform();
            transform.translateXBy_yBy(
                placement.rect.origin.x + placement.rect.size.width / 2.0,
                placement.rect.origin.y + placement.rect.size.height / 2.0,
            );
            transform.rotateByDegrees(-90.0);
            NSGraphicsContext::saveGraphicsState_class();
            transform.concat();
            image.drawInRect_fromRect_operation_fraction(
                turned,
                whole_image,
                NSCompositingOperation::SourceOver,
                1.0,
            );
            NSGraphicsContext::restoreGraphicsState_class();
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

/// Where the image goes on an `NSRect` page, in AppKit's terms.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RectPlacement {
    rect: NSRect,
    auto_rotated: bool,
}

/// The `NSRect` adapter over [`crate::printing::fit_to_page`], which Windows shares. AppKit's
/// page origin is bottom-left and GDI's is top-left; the fit is centred, so the same numbers
/// answer for both (see that module's docs).
///
/// `image_size` is `NSImage`'s size, which ImageIO already corrected for the file's EXIF
/// orientation — the final displayed size the turn has to be decided against, and the same size
/// `drawInRect:` draws to.
fn fit_to_page_rect(bounds: NSRect, image_size: NSSize) -> Option<RectPlacement> {
    let page = printing::Rect::new(
        bounds.origin.x,
        bounds.origin.y,
        bounds.size.width,
        bounds.size.height,
    );
    let placement = printing::fit_to_page(page, image_size.width, image_size.height)?;
    Some(RectPlacement {
        rect: NSRect::new(
            NSPoint::new(placement.rect.x, placement.rect.y),
            NSSize::new(placement.rect.width, placement.rect.height),
        ),
        auto_rotated: placement.auto_rotated,
    })
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
    /// `printing::fit_to_page`, tested there for every platform at once.
    #[test]
    fn the_nsrect_adapter_carries_the_fit_across() {
        let placement =
            fit_to_page_rect(rect(10.0, 20.0, 200.0, 200.0), NSSize::new(100.0, 50.0)).unwrap();
        // Square page: turning buys nothing, so this is the plain fit.
        // scale = min(200/100, 200/50) = 2.0 → 200×100, centred: y = 20 + (200−100)/2 = 70.
        assert!(!placement.auto_rotated);
        assert_eq!(placement.rect.origin.x, 10.0);
        assert_eq!(placement.rect.origin.y, 70.0);
        assert_eq!(placement.rect.size.width, 200.0);
        assert_eq!(placement.rect.size.height, 100.0);
    }

    /// A landscape image on a portrait page comes back turned, with the rect it turns into.
    #[test]
    fn the_adapter_carries_the_turn_across() {
        let placement =
            fit_to_page_rect(rect(0.0, 0.0, 100.0, 200.0), NSSize::new(100.0, 50.0)).unwrap();
        assert!(placement.auto_rotated);
        // Turned it's a 50×100 image on a 100×200 page: scale 2.0 → 100 wide, 200 tall.
        assert_eq!(placement.rect.size.width, 100.0);
        assert_eq!(placement.rect.size.height, 200.0);
    }

    /// A zero-area image yields no draw rect rather than a NaN/divide-by-zero.
    #[test]
    fn degenerate_image_yields_none() {
        assert!(fit_to_page_rect(rect(0.0, 0.0, 100.0, 100.0), NSSize::new(0.0, 0.0)).is_none());
    }

    /// The turn has to be decided against the size a person sees, so this pins the assumption it
    /// rests on: `NSImage` reports an EXIF-corrected size. The fixture is a 90×60 JPEG carrying
    /// orientation 6, and ImageIO is expected to hand back 60×90. If this ever fails, print would
    /// silently start turning upright photos onto their side.
    #[test]
    fn nsimage_reports_the_exif_corrected_size() {
        let image = load_image_from_path(&fixture("orientation6_90x60.jpg"))
            .expect("the fixture should load");
        assert_eq!(
            (image.size().width, image.size().height),
            (60.0, 90.0),
            "NSImage stopped applying EXIF orientation; the print turn is now decided on raw pixels"
        );
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
