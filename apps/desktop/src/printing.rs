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
    for pixel in buffer.as_chunks_mut::<4>().0 {
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

/// Where the image lands on the page, and whether it gets a quarter turn on the way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The area the image covers, in the caller's page coordinates. Already accounts for the
    /// turn: when `auto_rotated`, its width comes from the image's height and vice versa.
    pub rect: Rect,
    /// Draw the image turned a quarter turn clockwise to fill this rect.
    pub auto_rotated: bool,
}

/// Lay the image out on the page, turning it a quarter turn when that prints it bigger.
///
/// A 3:2 landscape photo on portrait A4 covers 47% of the sheet upright and 94% turned, so
/// printing it upright throws away about half the paper. macOS Preview turns it, and so does
/// this: a photo the person asked to print should fill what they paid for.
///
/// **The rule is "does turning it print bigger", nothing else.** Compare the two fits and keep
/// the larger. That's one comparison instead of a table of orientation cases, and it gets the
/// case that would otherwise ruin the feature for free: someone who already picked landscape in
/// the print dialog is handed a landscape page, their landscape photo fits it best upright, and
/// they don't get the double turn. Ties (a square image, square paper, a page and an image whose
/// aspects are reciprocal) stay upright, since turning buys nothing.
///
/// `image_width` and `image_height` must be the image's **final displayed** size, after EXIF
/// orientation. Both print paths satisfy that: `decoding::load_image` rotates the buffer and
/// reports the rotated dimensions, and `NSImage` reports an orientation-corrected `size`.
pub fn fit_to_page(page: Rect, image_width: f64, image_height: f64) -> Option<Placement> {
    let upright = aspect_fit(page, image_width, image_height)?;
    let turned = aspect_fit(page, image_height, image_width)?;
    if turned.width * turned.height > upright.width * upright.height {
        Some(Placement {
            rect: turned,
            auto_rotated: true,
        })
    } else {
        Some(Placement {
            rect: upright,
            auto_rotated: false,
        })
    }
}

/// Turn a 4-bytes-per-pixel, top-down image a quarter turn clockwise. Returns the turned
/// buffer; its dimensions are the caller's `height` by `width`.
///
/// Clockwise means the top edge ends up on the right, matching EXIF orientation 6 — the turn a
/// camera records when it's held on its side, so the app only ever rotates photos one way.
///
/// Windows needs this because GDI can't turn a blit: `StretchDIBits` scales and nothing more,
/// and `SetWorldTransform` on a printer DC is at the driver's discretion. Turning the pixels is
/// a transpose of a buffer that's about to be spooled anyway, on the print worker thread.
/// macOS turns the page's coordinate system instead and never calls this. It lives here so the
/// tests below run on any host, the same reason [`flatten_onto_white_bgra`] does.
/// `None` if `pixels` isn't exactly `width` x `height` — a caller that got its own dimensions
/// wrong should print nothing and say so, rather than spool a sheet of black.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn rotate_quarter_turn_clockwise(pixels: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    const BYTES_PER_PIXEL: usize = 4;
    let (width, height) = (width as usize, height as usize);
    let size = width.checked_mul(height)?.checked_mul(BYTES_PER_PIXEL)?;
    if size == 0 || pixels.len() != size {
        return None;
    }
    let mut turned = vec![0u8; size];
    // Source (x, y) lands at (height - 1 - y, x) in a buffer that is `height` pixels wide.
    for y in 0..height {
        for x in 0..width {
            let source = (y * width + x) * BYTES_PER_PIXEL;
            let destination = (x * height + (height - 1 - y)) * BYTES_PER_PIXEL;
            turned[destination..destination + BYTES_PER_PIXEL]
                .copy_from_slice(&pixels[source..source + BYTES_PER_PIXEL]);
        }
    }
    Some(turned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Rect = Rect::new(0.0, 0.0, 200.0, 200.0);

    /// A4-ish portrait paper, in the device pixels a 300 dpi printer reports.
    const PORTRAIT_PAGE: Rect = Rect::new(0.0, 0.0, 2480.0, 3508.0);
    /// The same sheet after someone picked landscape in the print dialog.
    const LANDSCAPE_PAGE: Rect = Rect::new(0.0, 0.0, 3508.0, 2480.0);

    /// The whole point: a 3:2 landscape photo on portrait paper turns, and the turn buys most of
    /// the sheet back. Upright it fits 2,480 x 1,653 (47% of the page); turned it fits
    /// 2,338 x 3,508 (94%).
    #[test]
    fn a_landscape_photo_turns_on_a_portrait_page() {
        let placement = fit_to_page(PORTRAIT_PAGE, 6000.0, 4000.0).unwrap();
        assert!(placement.auto_rotated);
        // Height-limited once turned: scale = 3508/6000, so the page's full height is used.
        assert_eq!(placement.rect.height, 3508.0);
        assert!(placement.rect.width < PORTRAIT_PAGE.width);
    }

    /// A portrait photo on portrait paper is already right; turning it would shrink it.
    #[test]
    fn a_portrait_photo_stays_upright_on_a_portrait_page() {
        let placement = fit_to_page(PORTRAIT_PAGE, 4000.0, 6000.0).unwrap();
        assert!(!placement.auto_rotated);
        assert_eq!(
            placement.rect,
            aspect_fit(PORTRAIT_PAGE, 4000.0, 6000.0).unwrap()
        );
    }

    /// Someone who already chose landscape in the print dialog gets their landscape photo
    /// upright. Turning it here would be the double rotation that makes the feature infuriating.
    #[test]
    fn a_landscape_photo_stays_upright_on_a_landscape_page() {
        let placement = fit_to_page(LANDSCAPE_PAGE, 6000.0, 4000.0).unwrap();
        assert!(!placement.auto_rotated);
        assert_eq!(placement.rect.width, 3508.0);
    }

    /// A square image fits identically either way, so it never turns. Ties belong upright.
    #[test]
    fn a_square_image_never_turns() {
        assert!(
            !fit_to_page(PORTRAIT_PAGE, 4000.0, 4000.0)
                .unwrap()
                .auto_rotated
        );
        assert!(
            !fit_to_page(LANDSCAPE_PAGE, 4000.0, 4000.0)
                .unwrap()
                .auto_rotated
        );
        assert!(!fit_to_page(PAGE, 4000.0, 4000.0).unwrap().auto_rotated);
    }

    /// Square paper has no orientation to disagree with, so nothing turns on it either.
    #[test]
    fn square_paper_leaves_everything_upright() {
        assert!(!fit_to_page(PAGE, 100.0, 50.0).unwrap().auto_rotated);
        assert!(!fit_to_page(PAGE, 50.0, 100.0).unwrap().auto_rotated);
    }

    /// The decision is "does turning it print bigger", not "do the aspects disagree" — which is
    /// the same answer here and a cheaper thing to reason about than a pile of orientation cases.
    #[test]
    fn turning_is_chosen_only_when_it_prints_bigger() {
        for (image_width, image_height) in [(6000.0, 4000.0), (4000.0, 6000.0), (100.0, 3000.0)] {
            for page in [PORTRAIT_PAGE, LANDSCAPE_PAGE, PAGE] {
                let placement = fit_to_page(page, image_width, image_height).unwrap();
                let upright = aspect_fit(page, image_width, image_height).unwrap();
                let turned = aspect_fit(page, image_height, image_width).unwrap();
                let (chosen, rejected) = if placement.auto_rotated {
                    (turned, upright)
                } else {
                    (upright, turned)
                };
                assert_eq!(placement.rect, chosen);
                assert!(
                    chosen.width * chosen.height >= rejected.width * rejected.height,
                    "{image_width}x{image_height} on {page:?} picked the smaller fit"
                );
            }
        }
    }

    /// A portrait photo whose EXIF tag already turned it upright arrives here as portrait, so it
    /// stays put on portrait paper. The raw sensor frame was landscape; fitting against that
    /// would print it sideways, which is the subtle way to get this wrong.
    #[test]
    fn the_final_displayed_size_is_what_decides() {
        // The camera stored 6000x4000 with orientation 6; the decoder hands us 4000x6000.
        assert!(
            !fit_to_page(PORTRAIT_PAGE, 4000.0, 6000.0)
                .unwrap()
                .auto_rotated
        );
        assert!(
            fit_to_page(PORTRAIT_PAGE, 6000.0, 4000.0)
                .unwrap()
                .auto_rotated
        );
    }

    /// Nothing to draw still beats a NaN rect once there's a rotation to pick.
    #[test]
    fn a_degenerate_image_still_yields_nothing() {
        assert!(fit_to_page(PAGE, 0.0, 100.0).is_none());
        assert!(fit_to_page(Rect::new(0.0, 0.0, 0.0, 10.0), 100.0, 50.0).is_none());
    }

    /// The quarter turn moves the top-left pixel to the top-right corner, and swaps the sides.
    #[test]
    fn a_quarter_turn_clockwise_moves_the_first_pixel_to_the_top_right() {
        // A 2x1 image: [A][B] across.
        let pixels = vec![1, 1, 1, 255, 2, 2, 2, 255];
        let turned = rotate_quarter_turn_clockwise(&pixels, 2, 1).unwrap();
        // Turned clockwise it's 1x2: A on top, B below.
        assert_eq!(turned, vec![1, 1, 1, 255, 2, 2, 2, 255]);

        // A 1x2 image: [A] over [B].
        let pixels = vec![1, 1, 1, 255, 2, 2, 2, 255];
        let turned = rotate_quarter_turn_clockwise(&pixels, 1, 2).unwrap();
        // Turned clockwise it's 2x1: B on the left, A on the right.
        assert_eq!(turned, vec![2, 2, 2, 255, 1, 1, 1, 255]);
    }

    /// A buffer that doesn't match its stated size prints nothing rather than a sheet of black.
    #[test]
    fn a_buffer_that_isnt_its_stated_size_is_refused() {
        assert!(rotate_quarter_turn_clockwise(&[0; 8], 3, 1).is_none());
        assert!(rotate_quarter_turn_clockwise(&[], 0, 0).is_none());
    }

    /// Four quarter turns are the identity, which is the cheap way to assert the index maths.
    #[test]
    fn four_quarter_turns_come_back_to_the_start() {
        let original: Vec<u8> = (0..(3 * 5 * 4) as u8).collect();
        let (mut pixels, mut width, mut height) = (original.clone(), 3u32, 5u32);
        for _ in 0..4 {
            pixels = rotate_quarter_turn_clockwise(&pixels, width, height).unwrap();
            (width, height) = (height, width);
        }
        assert_eq!((width, height), (3, 5));
        assert_eq!(pixels, original);
    }

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
