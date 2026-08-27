//! Turn preview pixels into the bitmap an image list slot takes.
//!
//! A `SysListView32` in icon mode draws from an `HIMAGELIST`, and every slot in one is the same
//! square size. Preview pixels are neither square nor a fixed size: the generator scales an
//! image's **longest** edge to what we asked for, so a landscape photo comes back wide and short.
//! So each thumbnail is composed into a square canvas, centred, on the pane's own background.
//!
//! Two conversions happen in the same pass, because doing them separately would mean walking a
//! megabyte twice:
//!
//! - **RGBA to BGRA**, which is the byte order a Windows DIB stores.
//! - **Top-down**, matching the negative `biHeight` the DIB section is created with, so no row
//!   flip is needed either.
//!
//! Pure, and compiled on every platform, so what Windows will show is asserted from a Mac.

/// Compose `width` × `height` RGBA pixels into a `side` × `side` top-down BGRA canvas, centred
/// on `background`.
///
/// A source larger than the canvas is centred and cropped rather than scaled: the generator is
/// asked for exactly `side` on the longest edge, so this only happens if it rounds up, and a
/// one-pixel crop is invisible where a rescale would cost a second resample.
///
/// Alpha is written opaque throughout. Every route into the grid is a photograph, and an image
/// list drawn from a partly transparent slot would show the listview's own background through
/// the picture rather than through the letterbox.
#[must_use]
pub fn compose_slot(
    source: &[u8],
    width: u32,
    height: u32,
    side: u32,
    background: (u8, u8, u8),
) -> Vec<u8> {
    let side = side as usize;
    let (red, green, blue) = background;
    let mut canvas = vec![0u8; side * side * 4];
    for pixel in canvas.as_chunks_mut::<4>().0 {
        pixel[0] = blue;
        pixel[1] = green;
        pixel[2] = red;
        pixel[3] = 0xff;
    }

    let source_width = width as usize;
    let source_height = height as usize;
    if source_width == 0 || source_height == 0 || source.len() < source_width * source_height * 4 {
        return canvas;
    }

    // How much of the source fits, and where it starts. A source smaller than the canvas is
    // centred with a positive offset; a larger one is cropped from its own centre.
    let copy_width = source_width.min(side);
    let copy_height = source_height.min(side);
    let destination_x = (side - copy_width) / 2;
    let destination_y = (side - copy_height) / 2;
    let source_x = (source_width - copy_width) / 2;
    let source_y = (source_height - copy_height) / 2;

    for row in 0..copy_height {
        let from = ((source_y + row) * source_width + source_x) * 4;
        let to = ((destination_y + row) * side + destination_x) * 4;
        for column in 0..copy_width {
            let source_pixel = &source[from + column * 4..from + column * 4 + 4];
            let target = &mut canvas[to + column * 4..to + column * 4 + 4];
            target[0] = source_pixel[2];
            target[1] = source_pixel[1];
            target[2] = source_pixel[0];
            target[3] = 0xff;
        }
    }
    canvas
}

/// How many bytes one image list slot costs, for the byte-budget cache that decides when to drop
/// one. A slot is the canvas above, so this is exact rather than an estimate.
#[must_use]
pub fn slot_bytes(side: u32) -> usize {
    (side as usize) * (side as usize) * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One BGRA pixel out of a composed canvas.
    fn pixel(canvas: &[u8], side: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let at = ((y as usize) * (side as usize) + x as usize) * 4;
        (canvas[at], canvas[at + 1], canvas[at + 2], canvas[at + 3])
    }

    /// One RGBA pixel, repeated.
    fn solid(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 0xff]);
        }
        out
    }

    #[test]
    fn a_landscape_thumbnail_is_letterboxed_top_and_bottom() {
        // 8 wide, 4 tall, into an 8 × 8 slot: two rows of background above and below.
        let source = solid(8, 4, (10, 20, 30));
        let canvas = compose_slot(&source, 8, 4, 8, (200, 200, 200));
        assert_eq!(canvas.len(), slot_bytes(8));
        // The letterbox.
        assert_eq!(pixel(&canvas, 8, 0, 0), (200, 200, 200, 0xff));
        assert_eq!(pixel(&canvas, 8, 0, 7), (200, 200, 200, 0xff));
        // The image, in BGRA rather than RGBA.
        assert_eq!(pixel(&canvas, 8, 0, 2), (30, 20, 10, 0xff));
        assert_eq!(pixel(&canvas, 8, 7, 5), (30, 20, 10, 0xff));
    }

    #[test]
    fn a_portrait_thumbnail_is_letterboxed_left_and_right() {
        let source = solid(4, 8, (10, 20, 30));
        let canvas = compose_slot(&source, 4, 8, 8, (0, 0, 0));
        assert_eq!(pixel(&canvas, 8, 0, 0), (0, 0, 0, 0xff));
        assert_eq!(pixel(&canvas, 8, 2, 0), (30, 20, 10, 0xff));
        assert_eq!(pixel(&canvas, 8, 7, 7), (0, 0, 0, 0xff));
    }

    /// Rows come out top-down, matching the negative `biHeight` the DIB section is created with.
    /// Getting this backwards shows every thumbnail upside down.
    #[test]
    fn rows_are_written_top_down() {
        // Two rows: the first red, the second green.
        let mut source = solid(2, 1, (255, 0, 0));
        source.extend(solid(2, 1, (0, 255, 0)));
        let canvas = compose_slot(&source, 2, 2, 2, (0, 0, 0));
        assert_eq!(pixel(&canvas, 2, 0, 0), (0, 0, 255, 0xff), "top row is red");
        assert_eq!(
            pixel(&canvas, 2, 0, 1),
            (0, 255, 0, 0xff),
            "bottom row is green"
        );
    }

    /// A corrupt or half-written delivery must produce a blank slot rather than a panic or a
    /// slot full of whatever was next in memory.
    #[test]
    fn a_short_or_empty_source_is_a_blank_slot() {
        for (source, width, height) in [
            (Vec::new(), 0, 0),
            (Vec::new(), 4, 4),
            (solid(4, 1, (1, 2, 3)), 4, 4),
        ] {
            let canvas = compose_slot(&source, width, height, 4, (7, 8, 9));
            assert_eq!(canvas.len(), slot_bytes(4));
            assert_eq!(pixel(&canvas, 4, 0, 0), (9, 8, 7, 0xff));
            assert_eq!(pixel(&canvas, 4, 3, 3), (9, 8, 7, 0xff));
        }
    }

    /// A source that came back a pixel bigger than asked for is cropped from its centre, which
    /// costs one row rather than a second resample of a megabyte.
    #[test]
    fn an_oversized_source_is_cropped_rather_than_rescaled() {
        let source = solid(6, 6, (10, 20, 30));
        let canvas = compose_slot(&source, 6, 6, 4, (0, 0, 0));
        assert_eq!(canvas.len(), slot_bytes(4));
        for x in 0..4 {
            for y in 0..4 {
                assert_eq!(pixel(&canvas, 4, x, y), (30, 20, 10, 0xff));
            }
        }
    }

    /// Every slot is opaque. A partly transparent one lets the listview's background through the
    /// picture rather than through the letterbox.
    #[test]
    fn every_pixel_is_opaque() {
        let mut source = solid(4, 4, (10, 20, 30));
        // A source claiming full transparency still composes opaque.
        for pixel in source.as_chunks_mut::<4>().0 {
            pixel[3] = 0;
        }
        let canvas = compose_slot(&source, 4, 4, 8, (1, 2, 3));
        for pixel in canvas.as_chunks::<4>().0 {
            assert_eq!(pixel[3], 0xff);
        }
    }
}
