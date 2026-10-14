//! Gain-map parameters as the app holds them: one shape whatever the file's metadata flavor
//! (ISO 21496-1, Ultra HDR XMP, or Apple's maker note), in ISO 21496-1's terms, and the math the
//! shader's mode 2 runs on them.
//!
//! The math is a port of `ultrahdr-core` 0.6.0, not a re-derivation (spec Findings, 2026-10-13),
//! so its edge cases match the reference implementation:
//!
//! - [`gain`] is `GainMapLut::new`'s per-code formula, clamped to `[2⁻³⁰, 2³⁰]`.
//! - [`sample_gain`] is the integer-scale sampler `apply_gainmap` uses: Shepard's
//!   inverse-distance weighting over the four neighbouring texels, applied to the _linear gains_,
//!   with texels top-left aligned (image pixel `x` sits at map coordinate `x × map / image`).
//! - [`apply`] is `apply_gain_row_presampled`: `(base + base_offset) × gain − alternate_offset`.
//! - [`weight`] ports `calculate_weight` for an SDR base and takes libavif's formula for an HDR
//!   base, where upstream always answers 1.0 (pinned by a test in `color::hdr`).
//!
//! `render/shader.wgsl` ports the same functions; `color::hdr::fragment::shade` strings them
//! together the way the shader does, and the GPU parity tests compare the two.

/// A row-major 3×3 matrix, the shape `color::profiles` uses.
pub type Mat3 = [[f32; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mul(m: &Mat3, v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

/// The three color conversions mode 2 runs per fragment (`fragment::GainMapInputs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainMapSpaces {
    /// Linear Display P3 (the base, as the shader samples it) into the gain's application space.
    pub p3_to_app: Mat3,
    /// The application space into `HdrDisplaySpace::for_host()`.
    pub app_to_display: Mat3,
    /// Linear Display P3 straight into `HdrDisplaySpace::for_host()`: exactly the identity on
    /// macOS, so a base with no gain applied reaches the surface bit for bit.
    pub p3_to_display: Mat3,
}

impl GainMapSpaces {
    /// The application space is the base's primaries (from its ICC colorants when it's a
    /// matrix-shaper profile), or the alternate rendition's (the gain-map image's profile) when
    /// the metadata says so. A LUT-based or unreadable profile falls back to Display P3.
    ///
    /// Why it matters at all: a single-channel gain scales R, G, and B by the same factor, which
    /// commutes with any linear matrix, so for the common map the space changes nothing. A
    /// three-channel map or unequal per-channel offsets do depend on it.
    pub fn new(base_icc: &[u8], map_icc: Option<&[u8]>, use_base_color_space: bool) -> Self {
        use moxcms::ColorProfile;
        let p3 = ColorProfile::new_display_p3();
        let display = match crate::color::profiles::HdrDisplaySpace::for_host() {
            crate::color::profiles::HdrDisplaySpace::LinearDisplayP3 => None,
            crate::color::profiles::HdrDisplaySpace::LinearSrgb => Some(ColorProfile::new_srgb()),
        };
        let matrix_shaper = |icc: &[u8]| {
            ColorProfile::new_from_slice(icc)
                .ok()
                .filter(ColorProfile::is_matrix_shaper)
        };
        let base = matrix_shaper(base_icc);
        if let Some(base) = &base {
            let to_p3 = to_mat3(base.transform_matrix(&p3));
            if to_p3.iter().flatten().any(|&v| v < -1e-3) {
                log::debug!("Gain-map base is wider than Display P3; colors outside it clip");
            }
        }
        let alternate = (!use_base_color_space)
            .then(|| map_icc.and_then(matrix_shaper))
            .flatten();
        let app = match alternate.or(base) {
            Some(app) => app,
            None => {
                log::debug!("Gain-map base has no matrix-shaper profile; applying the gain in P3");
                p3.clone()
            }
        };
        let to_display = |from: &ColorProfile| match &display {
            Some(display) => to_mat3(from.transform_matrix(display)),
            None => to_mat3(from.transform_matrix(&p3)),
        };
        Self {
            p3_to_app: to_mat3(p3.transform_matrix(&app)),
            app_to_display: to_display(&app),
            p3_to_display: match &display {
                Some(display) => to_mat3(p3.transform_matrix(display)),
                None => IDENTITY,
            },
        }
    }
}

/// A conversion within this of the identity, in every coefficient, is the identity: two
/// profiles naming the same primaries (Apple's Display P3 and `moxcms`') differ in their
/// colorants' last digits (about 1e-5), and the almost-identity leaks each channel into the
/// others. A 1e-4 leak from a saturated channel into a black one is 1e-4 linear, about a third of
/// an sRGB code near black (where codes are 3e-4 apart); real conversions (sRGB to P3) move
/// coefficients by about 1e-1.
const IDENTITY_TOLERANCE: f64 = 1e-4;

/// A `moxcms` conversion matrix in our shape, snapped to the exact identity within
/// [`IDENTITY_TOLERANCE`]. Scene-linear sources (`decoding::scene_linear`) go through it too.
pub fn to_mat3(m: moxcms::Matrix3d) -> Mat3 {
    let near_identity = m.v.iter().enumerate().all(|(r, row)| {
        row.iter().enumerate().all(|(c, &v)| {
            let identity = if r == c { 1.0 } else { 0.0 };
            (v - identity).abs() < IDENTITY_TOLERANCE
        })
    });
    if near_identity {
        return IDENTITY;
    }
    m.v.map(|row| row.map(|v| v as f32))
}

/// The largest exponent a gain may reach, either way: `ultrahdr-core` clamps the gain to
/// `[2⁻³⁰, 2³⁰]`, which for finite inputs is the same as clamping its log2.
const MAX_LOG2_GAIN: f32 = 30.0;

/// How much of the gain map to apply at the display's current headroom: 0 shows the base
/// rendition, 1 the alternate.
pub fn weight(headroom_now: f32, p: &GainMapParams) -> f32 {
    let h = headroom_now.max(1.0).log2();
    if p.base_headroom > p.alternate_headroom {
        // HDR base (libavif): 1.0 at headroom 1, falling to 0.0 at the base's own headroom.
        return ((h - p.base_headroom) / (p.alternate_headroom - p.base_headroom)).clamp(0.0, 1.0);
    }
    let lo = p.base_headroom.max(0.0);
    let hi = p.alternate_headroom.max(0.0);
    if hi <= lo {
        return 1.0;
    }
    ((h - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// The linear gain a map value `v` (0 to 1, the texel as `Rgba8Unorm` reads it) stands for in
/// `channel`, at `weight`.
pub fn gain(v: f32, channel: usize, p: &GainMapParams, weight: f32) -> f32 {
    let c = &p.channels[channel];
    let recovery = if c.gamma != 1.0 && c.gamma > 0.0 {
        v.powf(1.0 / c.gamma)
    } else {
        v
    };
    let log2_gain = c.min + recovery * (c.max - c.min);
    (log2_gain * weight)
        .clamp(-MAX_LOG2_GAIN, MAX_LOG2_GAIN)
        .exp2()
}

/// How close, in image pixels, a texture coordinate has to land to a pixel centre to count as
/// that pixel (`sample_gain`). Far above float rounding (about 1e-4 px at 10,000 px), far below
/// anything a zoom puts between two pixels that matters.
const PIXEL_SNAP: f32 = 1.0 / 64.0;

/// A gain map's texels as the GPU holds them: RGBA8, a single-channel map replicated into R, G,
/// and B.
#[derive(Clone, Copy, Debug)]
pub struct MapView<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

impl MapView<'_> {
    fn texel(&self, x: u32, y: u32, channel: usize) -> f32 {
        f32::from(self.rgba[((y * self.width + x) * 4) as usize + channel]) / 255.0
    }
}

/// The per-channel gain at texture coordinate `uv` (0 to 1 across the base image of
/// `image_size`), with the base's texels centred at `(x + 0.5) / width` as the GPU puts them.
pub fn sample_gain(
    map: &MapView,
    image_size: [u32; 2],
    uv: [f32; 2],
    p: &GainMapParams,
    weight: f32,
) -> [f32; 3] {
    let axis = |a: usize, map_len: u32| {
        // The image pixel this fragment is, then where that pixel sits on the map.
        //
        // Shepard's weighting jumps at a cell edge (the far neighbours change), so a pixel that
        // lands a hair on the wrong side of one takes the wrong texels. Two roundings do that:
        // `(x + 0.5) / w × w − 0.5` comes out just under `x` at most widths that aren't powers
        // of two, and a GPU's division isn't correctly rounded. So a fragment within
        // `PIXEL_SNAP` of a pixel centre is that pixel, and its map cell comes from integer
        // arithmetic, which is exactly upstream's `x × map / image`. `x × map` fits in a `u32`
        // for any size a texture can have, as it must in the shader.
        let size = image_size[a];
        let raw = uv[a] * size as f32 - 0.5;
        let nearest = raw.round();
        let (cell, fraction) = if (raw - nearest).abs() < PIXEL_SNAP {
            let scaled = nearest.max(0.0) as u32 * map_len;
            (scaled / size, (scaled % size) as f32 / size as f32)
        } else {
            let at = raw.max(0.0) * map_len as f32 / size as f32;
            (at.floor() as u32, at - at.floor())
        };
        let i0 = cell.min(map_len - 1);
        let i1 = (i0 + 1).min(map_len - 1);
        (i0, i1, fraction)
    };
    let (x0, x1, fx) = axis(0, map.width);
    let (y0, y1, fy) = axis(1, map.height);
    let w = shepard_weights(fx, fy, x1 - x0, y1 - y0);
    std::array::from_fn(|c| {
        let g = |x, y| gain(map.texel(x, y, c), c, p, weight);
        g(x0, y0) * w[0] + g(x0, y1) * w[1] + g(x1, y0) * w[2] + g(x1, y1) * w[3]
    })
}

/// Shepard's weights for the top-left, bottom-left, top-right, and bottom-right texels, at
/// `(fx, fy)` inside the cell. `right`/`down` are 0 where the neighbour was clamped back onto
/// the same texel at the map's edge, which moves that neighbour's distance the way
/// `ultrahdr-core`'s edge tables do.
fn shepard_weights(fx: f32, fy: f32, right: u32, down: u32) -> [f32; 4] {
    let d_tl = (fx * fx + fy * fy).sqrt();
    if d_tl == 0.0 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    let dx_r = fx - right as f32;
    let dy_b = fy - down as f32;
    let d_bl = (fx * fx + dy_b * dy_b).sqrt();
    let d_tr = (dx_r * dx_r + fy * fy).sqrt();
    let d_br = (dx_r * dx_r + dy_b * dy_b).sqrt();
    let [w_tl, w_bl, w_tr, w_br] = [d_tl, d_bl, d_tr, d_br].map(|d| 1.0 / d);
    let total = 1.0 / (w_tl + w_bl + w_tr + w_br);
    [w_tl * total, w_bl * total, w_tr * total, w_br * total]
}

/// ISO 21496-1's reconstruction, per channel: `(base + base_offset) × gain − alternate_offset`.
pub fn apply(base: [f32; 3], gain: [f32; 3], p: &GainMapParams) -> [f32; 3] {
    std::array::from_fn(|c| {
        (base[c] + p.channels[c].base_offset) * gain[c] - p.channels[c].alternate_offset
    })
}

/// One color channel's gain-map parameters. Gains and headrooms are log2, gamma and offsets
/// linear, as ISO 21496-1 writes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelParams {
    /// log2 of the gain a map value of 0 stands for.
    pub min: f32,
    /// log2 of the gain a map value of 1 stands for.
    pub max: f32,
    /// The map's encoding gamma: a map value `v` is `pow(v, 1 / gamma)` of the way from `min`
    /// to `max`.
    pub gamma: f32,
    /// Added to the base rendition before the gain.
    pub base_offset: f32,
    /// Subtracted from the result after the gain.
    pub alternate_offset: f32,
}

impl Default for ChannelParams {
    /// ISO 21496-1's (and Ultra HDR's) defaults, where a field is optional.
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 0.0,
            gamma: 1.0,
            base_offset: 1.0 / 64.0,
            alternate_offset: 1.0 / 64.0,
        }
    }
}

/// Where a file's gain-map parameters came from. The first one a file carries wins, in this
/// order (`docs/specs/hdr-images.md`, section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GainMapSource {
    /// An ISO 21496-1 APP2 segment on the gain-map image.
    Iso21496,
    /// Ultra HDR v1's `hdrgm:` XMP on the gain-map image.
    UltraHdrXmp,
    /// Apple's maker-note headroom, with the map tagged as Apple's HDR gain map.
    Apple,
}

/// A gain map's parameters, whatever metadata they were read from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainMapParams {
    /// R, G, B. A single-channel map repeats the same parameters three times.
    pub channels: [ChannelParams; 3],
    /// log2 of the base rendition's headroom: 0 for an SDR base.
    pub base_headroom: f32,
    /// log2 of the alternate rendition's headroom.
    pub alternate_headroom: f32,
    /// Apply the gain in the base image's color space (`true`) or the alternate's.
    pub use_base_color_space: bool,
}

impl GainMapParams {
    /// Whether every number is one the shader can use: finite, and a positive gamma. Metadata
    /// that fails this is treated as no gain map at all.
    pub fn is_usable(&self) -> bool {
        self.base_headroom.is_finite()
            && self.alternate_headroom.is_finite()
            && self.channels.iter().all(|c| {
                [c.min, c.max, c.gamma, c.base_offset, c.alternate_offset]
                    .iter()
                    .all(|v| v.is_finite())
                    && c.gamma > 0.0
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::hdr::to_upstream;
    use ultrahdr_core::gainmap::apply::{GainMapLut, calculate_weight};

    fn params(base_headroom: f32, alternate_headroom: f32) -> GainMapParams {
        GainMapParams {
            channels: [ChannelParams {
                max: alternate_headroom,
                ..ChannelParams::default()
            }; 3],
            base_headroom,
            alternate_headroom,
            use_base_color_space: true,
        }
    }

    const HEADROOMS: [f32; 6] = [1.0, 1.5, 2.0, 4.0, 8.0, 16.0];

    #[test]
    fn weight_matches_upstream_for_an_sdr_base() {
        for (base, alternate) in [(0.0, 2.0), (0.5, 3.0), (0.0, 0.8), (1.0, 1.0), (0.0, 0.0)] {
            let p = params(base, alternate);
            for h in HEADROOMS {
                let want = calculate_weight(h, &to_upstream(&p));
                assert!(
                    (weight(h, &p) - want).abs() < 1e-6,
                    "base {base}, alternate {alternate}, headroom {h}: {} vs {want}",
                    weight(h, &p)
                );
            }
        }
    }

    /// libavif's direction: an HDR base needs the whole gain at headroom 1 (to reach SDR) and
    /// none at its own headroom.
    #[test]
    fn weight_runs_backwards_for_an_hdr_base() {
        let p = params(2.0, 0.0);
        assert_eq!(weight(1.0, &p), 1.0);
        assert_eq!(weight(2.0, &p), 0.5);
        assert_eq!(weight(4.0, &p), 0.0);
        assert_eq!(weight(16.0, &p), 0.0);
        let ws = HEADROOMS.map(|h| weight(h, &p));
        assert!(ws.windows(2).all(|w| w[1] <= w[0]), "{ws:?}");
    }

    /// The edge cases upstream's LUT handles: gamma 1, a non-unit gamma, `min == max`, a
    /// negative min, and the weights at both ends.
    #[test]
    fn gain_matches_upstreams_lut_for_every_code() {
        let mut variants = vec![params(0.0, 2.0)];
        let mut p = params(0.0, 1.767_799);
        p.channels[1].gamma = 0.608_887;
        p.channels[2].min = -0.5;
        variants.push(p);
        let mut flat = params(0.0, 1.0);
        flat.channels[0].min = 1.0;
        variants.push(flat);
        for p in variants {
            for w in [0.0, 0.3, 1.0] {
                let lut = GainMapLut::new(&to_upstream(&p), w);
                for code in 0..=255u8 {
                    for c in 0..3 {
                        let want = lut.lookup(code, c);
                        let got = gain(f32::from(code) / 255.0, c, &p, w);
                        assert!(
                            (got - want).abs() <= want * 1e-5,
                            "code {code} channel {c} weight {w}: {got} vs {want}"
                        );
                    }
                }
            }
        }
    }

    /// A deterministic, busy pattern: neighbouring texels differ a lot, which is where a sampler
    /// that isn't upstream's (centre-aligned bilinear on encoded values) would show.
    fn noise(i: u32) -> u8 {
        (i.wrapping_mul(2_654_435_761) >> 24) as u8
    }

    fn srgb_decode(byte: u8) -> f32 {
        let v = f32::from(byte) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// The reference against `ultrahdr_core::gainmap::apply_gainmap` over a whole image, at
    /// texel centres, for a single- and a three-channel map: the spec's bound is a mean relative
    /// error under 0.5%.
    #[test]
    fn the_reference_matches_apply_gainmap_on_a_busy_map() {
        use ultrahdr_core::{ColorPrimaries, GainMap, PixelFormat, TransferFunction, Unstoppable};

        // A power-of-two size keeps every texture coordinate exact; the iPhone's 776 wide over a
        // 388-wide map is a size where `(x + 0.5) / w × w − 0.5` rounds below `x`.
        for (width, height, map_w, map_h) in [(32u32, 24u32, 8u32, 6u32), (776, 6, 388, 3)] {
            let base: Vec<u8> = (0..width * height * 4)
                .map(|i| if i % 4 == 3 { 255 } else { noise(i) })
                .collect();
            for channels in [1u8, 3] {
                let data: Vec<u8> = (0..map_w * map_h * u32::from(channels))
                    .map(|i| noise(i + 7))
                    .collect();
                let rgba: Vec<u8> = data
                    .chunks(channels as usize)
                    .flat_map(|t| [t[0], t[t.len() / 2], t[t.len() - 1], 255])
                    .collect();
                let mut p = params(0.0, 2.0);
                if channels == 3 {
                    p.channels[1].gamma = 0.7;
                    p.channels[2].min = -0.5;
                    p.channels[2].max = 1.5;
                }
                for headroom in [1.5, 4.0, 16.0] {
                    let sdr = ultrahdr_core::pixel_buffer_from_vec(
                        base.clone(),
                        width,
                        height,
                        PixelFormat::Rgba8,
                        ColorPrimaries::DisplayP3,
                        TransferFunction::Srgb,
                    )
                    .expect("base buffer");
                    let map = GainMap {
                        width: map_w,
                        height: map_h,
                        channels,
                        data: data.clone(),
                    };
                    let out = ultrahdr_core::gainmap::apply_gainmap(
                        &sdr,
                        &map,
                        &to_upstream(&p),
                        headroom,
                        ultrahdr_core::HdrOutputFormat::LinearFloat,
                        Unstoppable,
                    )
                    .expect("upstream apply");
                    let slice = out.as_slice();
                    let stride = slice.stride();
                    let bytes = slice.as_strided_bytes();

                    let view = MapView {
                        width: map_w,
                        height: map_h,
                        rgba: &rgba,
                    };
                    let w = weight(headroom, &p);
                    let (mut sum, mut max, mut n) = (0.0f64, 0.0f32, 0u32);
                    for y in 0..height {
                        for x in 0..width {
                            let i = ((y * width + x) * 4) as usize;
                            let linear = [0, 1, 2].map(|c| srgb_decode(base[i + c]));
                            let uv = [
                                (x as f32 + 0.5) / width as f32,
                                (y as f32 + 0.5) / height as f32,
                            ];
                            let ours =
                                apply(linear, sample_gain(&view, [width, height], uv, &p, w), &p);
                            for (c, ours) in ours.into_iter().enumerate() {
                                let at = y as usize * stride + x as usize * 16 + c * 4;
                                let want =
                                    f32::from_le_bytes(bytes[at..at + 4].try_into().expect("f32"));
                                let error = (ours - want).abs() / want.abs().max(1e-3);
                                sum += f64::from(error);
                                max = max.max(error);
                                n += 1;
                            }
                        }
                    }
                    let mean = sum / f64::from(n);
                    assert!(
                        mean < 0.005 && max < 0.01,
                        "{width}x{height}, {channels}-channel map at headroom {headroom}: mean \
                     {mean:e}, max {max:e}"
                    );
                }
            }
        }
    }

    /// A P3 base needs no conversion at all, whoever wrote its profile: the iPhone's Display P3
    /// colorants differ from `moxcms`' in the last digits, and an almost-identity matrix leaks a
    /// little of each channel into the others, which shows as error on dark channels.
    #[test]
    fn a_p3_base_converts_through_the_exact_identity() {
        let iphone = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/hdr/iphone-air-gain-map-776.jpg"),
        )
        .expect("fixture");
        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(iphone.as_slice()));
        decoder.decode_headers().expect("headers");
        let apple_p3 = decoder
            .icc_profile()
            .expect("the iPhone base has a profile");
        for icc in [crate::color::display_p3_icc_bytes(), apple_p3.as_slice()] {
            let spaces = GainMapSpaces::new(icc, None, true);
            assert_eq!(spaces.p3_to_app, IDENTITY);
            if cfg!(target_os = "macos") {
                assert_eq!(spaces.app_to_display, IDENTITY);
                assert_eq!(spaces.p3_to_display, IDENTITY);
            }
        }
        // An sRGB base is a real conversion, and stays one.
        let srgb = GainMapSpaces::new(crate::color::srgb_icc_bytes(), None, true);
        assert_ne!(srgb.p3_to_app, IDENTITY);
    }

    /// A conversion a few 8-bit codes off the identity near black is a real one: 5e-4 of a
    /// saturated channel leaking into a dark one is about 1.5 sRGB codes there.
    #[test]
    fn a_conversion_a_few_codes_off_the_identity_is_kept() {
        let mut m = moxcms::Matrix3d {
            v: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        m.v[1][0] = 5e-4;
        assert_ne!(to_mat3(m), IDENTITY);
        m.v[1][0] = 2e-5;
        assert_eq!(to_mat3(m), IDENTITY);
    }

    #[test]
    fn a_pixel_on_a_map_texel_takes_that_texel_alone() {
        let rgba = [10u8, 10, 10, 255, 250, 250, 250, 255];
        let view = MapView {
            width: 2,
            height: 1,
            rgba: &rgba,
        };
        let p = params(0.0, 2.0);
        // Image pixel 0 sits exactly on map texel 0.
        let g = sample_gain(&view, [8, 1], [0.5 / 8.0, 0.5], &p, 1.0);
        assert_eq!(g, [gain(10.0 / 255.0, 0, &p, 1.0); 3]);
    }
}
