//! The parts of "print this image" that no print system owns.
//!
//! Two print paths exist — `NSPrintOperation` on macOS, `PrintDlgW` plus GDI on Windows — and
//! both need the same two answers: where on the page the image goes, and (for GDI) what the
//! pixels have to look like when they get there. Keeping those here means the maths is written
//! once and asserted from any host, the way `paths` and `scroll` do for their platform rules.
//!
//! ## The two coordinate systems agree, here
//!
//! AppKit's page origin is bottom-left and GDI's is top-left, which normally makes a shared
//! layout function a trap. [`aspect_fit`] is safe from it because the fit is **centred**: the
//! margin above the image equals the margin below it, so flipping the axis leaves every number
//! unchanged. Anything that ever aligns to an edge instead has to grow a per-platform answer.

/// A rectangle on the page, in whatever unit the caller's page is measured in (points on macOS,
/// device pixels on Windows). Origin is the corner the caller's coordinate system starts from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Fit an `image_width × image_height` image inside `page`, centred, keeping its aspect ratio.
///
/// `min` of the two axis ratios keeps the whole image visible (letterboxed), never cropped. It
/// does enlarge a small image to fill the page, which is what "print this photo" means — unlike
/// a preview, where upscaling would only cost memory. Returns `None` for a degenerate image or
/// page, so the caller skips drawing rather than dividing by zero.
pub fn aspect_fit(page: Rect, image_width: f64, image_height: f64) -> Option<Rect> {
    if image_width <= 0.0 || image_height <= 0.0 || page.width <= 0.0 || page.height <= 0.0 {
        return None;
    }
    let scale = (page.width / image_width).min(page.height / image_height);
    let width = image_width * scale;
    let height = image_height * scale;
    Some(Rect::new(
        page.x + (page.width - width) / 2.0,
        page.y + (page.height - height) / 2.0,
        width,
        height,
    ))
}

/// Turn straight-alpha RGBA8 into the opaque BGRA a GDI printer DC draws.
///
/// Two things happen, and both are about paper. GDI's 32-bit `BI_RGB` layout is B, G, R, and a
/// byte it ignores, where the decoder hands back R, G, B, A. And a printer has no transparency
/// to offer: an alpha byte dropped on the floor would print a transparent PNG's background as
/// black, so each channel is composited onto white first. That matches what macOS gets for free
/// from `drawInRect:` compositing `SourceOver` onto the page.
///
/// Windows is the only caller; it lives here so the tests below run on any host, which is the
/// only way it gets checked before meeting a Windows box.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn flatten_onto_white_bgra(buffer: &mut [u8]) {
    for pixel in buffer.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        let (r, g, b) = (
            u32::from(pixel[0]),
            u32::from(pixel[1]),
            u32::from(pixel[2]),
        );
        // `over` white: c·a + 255·(1−a), rounded, all in integers.
        let over = |c: u32| ((c * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        pixel[0] = over(b);
        pixel[1] = over(g);
        pixel[2] = over(r);
        pixel[3] = 0xff;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Rect = Rect::new(0.0, 0.0, 200.0, 200.0);

    /// A wide image into a square page is width-limited and letterboxed vertically.
    #[test]
    fn a_wide_image_is_width_limited() {
        // scale = min(200/100, 200/50) = 2.0 → 200×100, centred: y = (200−100)/2 = 50.
        let fitted = aspect_fit(PAGE, 100.0, 50.0).unwrap();
        assert_eq!(fitted, Rect::new(0.0, 50.0, 200.0, 100.0));
    }

    /// A tall one is height-limited and letterboxed horizontally.
    #[test]
    fn a_tall_image_is_height_limited() {
        let fitted = aspect_fit(PAGE, 50.0, 100.0).unwrap();
        assert_eq!(fitted, Rect::new(50.0, 0.0, 100.0, 200.0));
    }

    /// The page origin offsets the result: a printable area rarely starts at 0,0.
    #[test]
    fn the_page_origin_carries_through() {
        let fitted = aspect_fit(Rect::new(10.0, 20.0, 100.0, 100.0), 100.0, 100.0).unwrap();
        assert_eq!(fitted, Rect::new(10.0, 20.0, 100.0, 100.0));
    }

    /// Centring is what lets AppKit's y-up page and GDI's y-down page share this function: the
    /// two margins are equal, so flipping the axis changes nothing. A test, because the day
    /// someone adds an edge alignment this stops being true.
    #[test]
    fn the_fit_is_symmetric_on_both_axes() {
        let fitted = aspect_fit(PAGE, 100.0, 50.0).unwrap();
        assert_eq!(fitted.y, PAGE.height - fitted.y - fitted.height);
        let fitted = aspect_fit(PAGE, 50.0, 100.0).unwrap();
        assert_eq!(fitted.x, PAGE.width - fitted.x - fitted.width);
    }

    /// A photo smaller than the paper still fills it. Printing a 200 px thumbnail as a 200 px
    /// speck in the middle of A4 is nobody's intent.
    #[test]
    fn a_small_image_is_enlarged_to_the_page() {
        let fitted = aspect_fit(PAGE, 20.0, 20.0).unwrap();
        assert_eq!(fitted, Rect::new(0.0, 0.0, 200.0, 200.0));
    }

    /// Nothing to draw beats a NaN rect.
    #[test]
    fn degenerate_input_yields_nothing() {
        assert!(aspect_fit(PAGE, 0.0, 0.0).is_none());
        assert!(aspect_fit(PAGE, 100.0, -1.0).is_none());
        assert!(aspect_fit(Rect::new(0.0, 0.0, 0.0, 200.0), 100.0, 100.0).is_none());
    }

    #[test]
    fn opaque_pixels_only_change_channel_order() {
        let mut buffer = vec![10, 20, 30, 255, 40, 50, 60, 255];
        flatten_onto_white_bgra(&mut buffer);
        assert_eq!(buffer, vec![30, 20, 10, 255, 60, 50, 40, 255]);
    }

    /// A fully transparent pixel prints as paper, not as ink.
    #[test]
    fn transparent_pixels_become_white() {
        let mut buffer = vec![0, 0, 0, 0];
        flatten_onto_white_bgra(&mut buffer);
        assert_eq!(buffer, vec![255, 255, 255, 255]);
    }

    /// Half alpha lands halfway between the colour and the paper, and the endpoints stay exact.
    #[test]
    fn partial_alpha_composites_onto_white() {
        let mut buffer = vec![0, 100, 200, 128];
        flatten_onto_white_bgra(&mut buffer);
        // b: (200·128 + 255·127)/255 ≈ 227.4, g: ≈ 177.2, r: 127 exactly.
        assert_eq!(buffer, vec![227, 177, 127, 255]);
    }
}
