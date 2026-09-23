//! The tag dots in the window's bottom-left corner.
//!
//! One dot per color the image carries, Finder-sized and overlapping the way Finder draws a
//! file's tags, leftmost on top. Colorless tags draw nothing, and neither does an image with no
//! colored tags. Each dot is two overlay pills: a dark halo that keeps a pale dot legible on a
//! bright photo and separates overlapping dots, then the color on top. Pure geometry, so the
//! layout is testable without a GPU.
//!
//! Nothing else draws in the bottom-left corner. In fullscreen the window is the screen, so this
//! is the screen's corner there.

use super::{Tag, TagColor};
use crate::pixels::Logical;
use crate::render::text::StandalonePill;

/// A dot's diameter in logical pixels: the size Finder draws a tag dot in its list view.
pub const DOT_SIZE: f32 = 10.0;

/// How far along each next dot starts. Less than [`DOT_SIZE`], so the dots overlap.
const DOT_STEP: f32 = 6.0;

/// The halo's width around each dot.
const HALO_WIDTH: f32 = 1.5;

/// Space between the window edges and the outside of the halo.
const MARGIN: f32 = 10.0;

/// Translucent black: a thin outline over a bright photo, next to invisible over a dark one.
const HALO_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.45];

/// The pills for `tags`' dots, in draw order. `window_height` is the surface's logical height.
#[must_use]
pub fn build(tags: &[Tag], window_height: Logical<f32>) -> Vec<StandalonePill> {
    let halo_size = DOT_SIZE + 2.0 * HALO_WIDTH;
    let halo_top = window_height.0 - MARGIN - halo_size;
    let colors = dot_colors(tags);
    let mut pills = Vec::with_capacity(colors.len() * 2);
    // Right to left, so each dot's halo cuts into the one to its right and the leftmost ends on
    // top.
    for (position, color) in colors.iter().enumerate().rev() {
        let halo_left = MARGIN + position as f32 * DOT_STEP;
        pills.push(circle(halo_left, halo_top, halo_size, HALO_COLOR));
        pills.push(circle(
            halo_left + HALO_WIDTH,
            halo_top + HALO_WIDTH,
            DOT_SIZE,
            dot_color(*color),
        ));
    }
    pills
}

/// A filled circle: a pill whose corner radius is half its size.
fn circle(left: f32, top: f32, size: f32, color: [f32; 4]) -> StandalonePill {
    StandalonePill {
        x: Logical(left),
        y: Logical(top),
        width: Logical(size),
        height: Logical(size),
        corner_radius: Logical(size / 2.0),
        color,
        border_width: Logical(0.0),
    }
}

/// The colors to draw, in the file's own order, each once: a custom red tag next to Finder's
/// red draws one red dot, which is what Finder does.
fn dot_colors(tags: &[Tag]) -> Vec<TagColor> {
    let mut colors: Vec<TagColor> = Vec::with_capacity(tags.len());
    for color in tags.iter().filter_map(|tag| tag.color) {
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    colors
}

/// A dot's fill. These are Cmdr's dark-appearance tag colors, whatever the system appearance:
/// the dots sit on a photo, never on a light window, and the brighter set holds up over both a
/// dark image and the dark halo. Written as sRGB and converted, because the overlay pipeline
/// blends in linear light (the surface is an sRGB format).
fn dot_color(color: TagColor) -> [f32; 4] {
    let [r, g, b] = match color {
        TagColor::Red => [0xef, 0x6f, 0x6a],
        TagColor::Orange => [0xf0, 0x9a, 0x4c],
        TagColor::Yellow => [0xf0, 0xc5, 0x4e],
        TagColor::Green => [0x6f, 0xc4, 0x63],
        TagColor::Blue => [0x5e, 0xa0, 0xee],
        TagColor::Purple => [0xbd, 0x86, 0xe0],
        TagColor::Gray => [0xa8, 0xa8, 0xad],
    };
    [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0]
}

/// The sRGB transfer function, inverted.
fn srgb_to_linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(colors: &[Option<TagColor>]) -> Vec<Tag> {
        colors
            .iter()
            .enumerate()
            .map(|(i, color)| Tag {
                name: format!("tag {i}"),
                color: *color,
            })
            .collect()
    }

    /// The fills, which are every second pill (each dot draws its halo first).
    fn fills(pills: &[StandalonePill]) -> Vec<&StandalonePill> {
        pills.iter().skip(1).step_by(2).collect()
    }

    const HEIGHT: Logical<f32> = Logical(600.0);

    #[test]
    fn no_colored_tags_draw_nothing() {
        assert!(build(&[], HEIGHT).is_empty());
        assert!(build(&tags(&[None, None]), HEIGHT).is_empty());
    }

    #[test]
    fn one_dot_is_a_halo_and_a_fill_in_the_bottom_left_corner() {
        let pills = build(&tags(&[Some(TagColor::Red)]), HEIGHT);
        assert_eq!(pills.len(), 2);
        let (halo, fill) = (&pills[0], &pills[1]);

        // Round: the radius is half the size.
        assert_eq!(fill.width.0, DOT_SIZE);
        assert_eq!(fill.height.0, DOT_SIZE);
        assert_eq!(fill.corner_radius.0, DOT_SIZE / 2.0);
        assert_eq!(halo.width.0, DOT_SIZE + 2.0 * HALO_WIDTH);
        assert_eq!(halo.corner_radius.0, halo.width.0 / 2.0);

        // Concentric, with the halo's outside `MARGIN` from the left and bottom edges.
        assert_eq!(halo.x.0, MARGIN);
        assert_eq!(halo.y.0 + halo.height.0, HEIGHT.0 - MARGIN);
        assert_eq!(fill.x.0, halo.x.0 + HALO_WIDTH);
        assert_eq!(fill.y.0, halo.y.0 + HALO_WIDTH);

        assert_eq!(fill.color, dot_color(TagColor::Red));
        assert_eq!(halo.color, HALO_COLOR);
    }

    /// Finder's look: dots overlap, the leftmost on top. Pills draw in order, so the leftmost
    /// dot comes last.
    #[test]
    fn several_dots_overlap_with_the_leftmost_on_top() {
        let pills = build(
            &tags(&[
                Some(TagColor::Red),
                Some(TagColor::Blue),
                Some(TagColor::Green),
            ]),
            HEIGHT,
        );
        assert_eq!(pills.len(), 6);
        let fills = fills(&pills);
        let xs: Vec<f32> = fills.iter().map(|pill| pill.x.0).collect();
        assert!(xs.windows(2).all(|pair| pair[0] > pair[1]), "{xs:?}");
        assert_eq!(xs[0] - xs[1], DOT_STEP);
        const { assert!(DOT_STEP < DOT_SIZE, "the dots have to overlap") };

        let colors: Vec<[f32; 4]> = fills.iter().map(|pill| pill.color).collect();
        assert_eq!(
            colors,
            vec![
                dot_color(TagColor::Green),
                dot_color(TagColor::Blue),
                dot_color(TagColor::Red)
            ]
        );
    }

    #[test]
    fn colorless_tags_are_skipped_and_a_repeated_color_draws_once() {
        assert_eq!(
            dot_colors(&tags(&[
                None,
                Some(TagColor::Red),
                None,
                Some(TagColor::Blue),
                Some(TagColor::Red),
            ])),
            vec![TagColor::Red, TagColor::Blue]
        );
    }

    /// The palette is Cmdr's dark set, converted to linear light: `#ef6f6a` red is `0xef`
    /// (239) in sRGB, which is about 0.863 linear.
    #[test]
    fn colors_are_opaque_and_linear() {
        let red = dot_color(TagColor::Red);
        assert!((red[0] - 0.863).abs() < 0.002, "{red:?}");
        assert_eq!(red[3], 1.0);
        for color in TagColor::ALL {
            let [r, g, b, a] = dot_color(color);
            assert_eq!(a, 1.0);
            assert!([r, g, b].iter().all(|c| (0.0..=1.0).contains(c)));
        }
        // Seven distinct colors, so no two tags read the same.
        for (i, a) in TagColor::ALL.iter().enumerate() {
            for b in &TagColor::ALL[i + 1..] {
                assert_ne!(dot_color(*a), dot_color(*b), "{a:?} and {b:?}");
            }
        }
    }
}
