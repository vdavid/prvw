//! DNG `OpcodeList1` / `OpcodeList2` / `OpcodeList3` parsing and application.
//!
//! Adobe's DNG spec 1.6, chapter 6, defines an *opcode list*: a binary blob
//! carried on tags 51008 (`OpcodeList1`), 51009 (`OpcodeList2`), and 51022
//! (`OpcodeList3`). Each opcode is a pixel-level correction step that a
//! well-behaved DNG renderer applies at a specific pipeline slot:
//!
//! - `OpcodeList1` runs on the raw sensor data *before* black-level
//!   subtraction and linear rescale. CFA mosaic, still in sensor units.
//! - `OpcodeList2` runs *after* linearization to [0, 1] but *before*
//!   demosaic. Also operates on the CFA mosaic, so opcodes can target
//!   individual Bayer sub-planes. This is where iPhone ProRAW stores its
//!   per-Bayer-phase lens-shading `GainMap`s — one GainMap per phase with
//!   pitch 2×2 starting at (0,0), (0,1), (1,0), (1,1).
//! - `OpcodeList3` runs *after* demosaic and color-space conversion (in
//!   our pipeline: after `camera_to_linear_rec2020`). Typically carries
//!   `WarpRectilinear` for optical distortion correction.
//!
//! `rawler` 0.7.2 parses these tags as raw byte blobs on the virtual raw
//! IFD (`WellKnownIFD::VirtualDngRawTags`) but doesn't apply them. This
//! module fills that gap. `rawler` already applies `LinearizationTable`
//! (tag 50712) internally during raw decoding, so that step isn't our
//! responsibility.
//!
//! ## Byte layout (spec § 6.1)
//!
//! ```text
//! OpcodeList    := Count (u32 BE) Opcode*
//! Opcode        := OpcodeID (u32 BE)
//!                  Version  (u32 BE)
//!                  Flags    (u32 BE)   -- bit 0 = optional, bit 1 = preview-only
//!                  ByteCount (u32 BE)
//!                  Parameters (ByteCount bytes)
//! ```
//!
//! All integers are big-endian even on little-endian systems; this is a
//! deliberate deviation from the enclosing TIFF's native byte order. All
//! floats are IEEE 754 single-precision, also big-endian. Doubles are
//! IEEE 754 double-precision, big-endian.
//!
//! ## Implementation status
//!
//! Implemented:
//!
//! - **Opcode 1 — `WarpRectilinear`**. Per-plane lens-distortion correction
//!   (radial + tangential). Supports 1-plane (monochrome) or 3-plane (RGB)
//!   parameter sets. Bilinear source sampling; clamp-to-edge for out-of-
//!   bounds source coords. Wired into `OpcodeList3` for post-color
//!   application.
//! - **Opcode 4 — `FixBadPixelsConstant`**. Replace any pixel equal to a
//!   constant with the average of its 3×3 neighbors excluding other bad
//!   pixels.
//! - **Opcode 5 — `FixBadPixelsList`**. Explicit coordinate list of bad
//!   pixels and rectangles; each gets replaced with the average of its good
//!   neighbors.
//! - **Opcode 9 — `GainMap`**. Lens-shading / vignette correction. Bilinear
//!   interpolation over the gain grid, Bayer-aware for CFA data (pixel
//!   modified only when CFA color index matches the opcode's `plane`),
//!   plain RGB-plane scaling for LinearRaw / post-demosaic buffers.
//!
//! Stubbed (log + skip if optional, warn if mandatory):
//!
//! - **Opcode 2 — `WarpFisheye`**. Not seen on the files we support.
//! - **Opcode 3 — `FixVignetteRadial`**. Not seen on the files we support.
//! - **Opcode 6 — `TrimBounds`**. Rawler already handles active-area crop.
//! - **Opcode 7 — `MapPolynomial`** and **Opcode 8 — `MapTable`**. Per-plane
//!   pixel remap. Rawler's `LinearizationTable` path handles the common
//!   Nikon case already.
//! - **Opcodes 10–13** (delta / scale per row / column). Rare outside
//!   MapPolynomial-driven files.
//!
//! ## Design notes
//!
//! - **Byte parsing is std-only.** No `byteorder` or `nom` dep — all we need
//!   is `u32::from_be_bytes` and friends. Keeps compile times down.
//! - **Application functions operate on `&mut [f32]`.** `u16` raw data
//!   round-trips through `RawImageData::as_f32` / back to `u16` at the
//!   caller. Gain maps and warps both need sub-pixel precision anyway.
//! - **Opcode coordinates are in raw image pixels**, relative to the top-
//!   left of the raw frame (before any crop). `OpcodeList3` runs on the
//!   demosaiced + active-area-cropped buffer — coordinates are still in raw
//!   space, but for the iPhone fixture `active_area` starts at (0, 0) so
//!   no shifting is needed. Cameras with a nonzero active-area origin would
//!   need a shift (tracked as future work; no fixture in scope hits that
//!   edge).
//! - **Rayon parallelism.** Per-pixel gain multiply and per-output-pixel
//!   warp sample are both embarrassingly parallel by row; we use
//!   `par_chunks_mut` for both.

use std::fmt;

use rayon::prelude::*;

/// Maximum size of a single opcode parameter block, 64 MiB. Guard against a
/// malformed tag claiming a billion-byte block and causing us to allocate.
/// Real opcode blobs are well under 1 MiB.
const MAX_OPCODE_PARAM_BYTES: usize = 64 * 1024 * 1024;

/// Maximum number of opcodes in a single list. Real files have ~5; a few
/// hundred would already be suspicious. Guard against a corrupt count header.
const MAX_OPCODE_COUNT: usize = 256;

/// A parsed DNG opcode. The `raw` bytes of the parameter block are kept so
/// apply-time code can re-parse them per opcode kind without carrying dozens
/// of enum variants through the rest of the pipeline.
#[derive(Debug, Clone)]
pub struct Opcode {
    pub id: OpcodeId,
    /// DNG opcode-version field. We record it for debug logging and future
    /// version-aware parsers; none of today's applies branch on it.
    #[allow(dead_code)]
    pub version: u32,
    pub flags: OpcodeFlags,
    /// The raw big-endian parameter bytes for this opcode. Decoded on demand
    /// by `apply_*` functions below.
    pub params: Vec<u8>,
}

impl Opcode {
    pub fn is_optional(&self) -> bool {
        self.flags.contains(OpcodeFlags::OPTIONAL)
    }

    /// True for preview-only opcodes (flag bit 1). Currently unused — we
    /// apply every opcode we understand since our entire pipeline is a
    /// preview path. Kept for spec completeness.
    #[allow(dead_code)]
    pub fn is_preview_only(&self) -> bool {
        self.flags.contains(OpcodeFlags::PREVIEW_ONLY)
    }
}

/// Known and unknown opcode IDs. Unknown values round-trip through
/// `OpcodeId::Unknown(u32)` so we can log their numeric ID when deciding
/// whether to skip or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpcodeId {
    WarpRectilinear,
    WarpFisheye,
    FixVignetteRadial,
    FixBadPixelsConstant,
    FixBadPixelsList,
    TrimBounds,
    MapPolynomial,
    MapTable,
    GainMap,
    DeltaPerRow,
    DeltaPerColumn,
    ScalePerRow,
    ScalePerColumn,
    Unknown(u32),
}

impl OpcodeId {
    fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::WarpRectilinear,
            2 => Self::WarpFisheye,
            3 => Self::FixVignetteRadial,
            4 => Self::FixBadPixelsConstant,
            5 => Self::FixBadPixelsList,
            6 => Self::TrimBounds,
            7 => Self::MapPolynomial,
            8 => Self::MapTable,
            9 => Self::GainMap,
            10 => Self::DeltaPerRow,
            11 => Self::DeltaPerColumn,
            12 => Self::ScalePerRow,
            13 => Self::ScalePerColumn,
            other => Self::Unknown(other),
        }
    }

    #[allow(dead_code)]
    pub fn numeric(&self) -> u32 {
        match *self {
            Self::WarpRectilinear => 1,
            Self::WarpFisheye => 2,
            Self::FixVignetteRadial => 3,
            Self::FixBadPixelsConstant => 4,
            Self::FixBadPixelsList => 5,
            Self::TrimBounds => 6,
            Self::MapPolynomial => 7,
            Self::MapTable => 8,
            Self::GainMap => 9,
            Self::DeltaPerRow => 10,
            Self::DeltaPerColumn => 11,
            Self::ScalePerRow => 12,
            Self::ScalePerColumn => 13,
            Self::Unknown(v) => v,
        }
    }
}

impl fmt::Display for OpcodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WarpRectilinear => f.write_str("WarpRectilinear"),
            Self::WarpFisheye => f.write_str("WarpFisheye"),
            Self::FixVignetteRadial => f.write_str("FixVignetteRadial"),
            Self::FixBadPixelsConstant => f.write_str("FixBadPixelsConstant"),
            Self::FixBadPixelsList => f.write_str("FixBadPixelsList"),
            Self::TrimBounds => f.write_str("TrimBounds"),
            Self::MapPolynomial => f.write_str("MapPolynomial"),
            Self::MapTable => f.write_str("MapTable"),
            Self::GainMap => f.write_str("GainMap"),
            Self::DeltaPerRow => f.write_str("DeltaPerRow"),
            Self::DeltaPerColumn => f.write_str("DeltaPerColumn"),
            Self::ScalePerRow => f.write_str("ScalePerRow"),
            Self::ScalePerColumn => f.write_str("ScalePerColumn"),
            Self::Unknown(v) => write!(f, "Unknown({v})"),
        }
    }
}

/// Opcode flag bits per DNG spec § 6.1. Only two bits are defined; everything
/// else is reserved and we ignore it. We roll our own instead of pulling in
/// the `bitflags` crate for such a small flag set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpcodeFlags(pub u32);

#[allow(dead_code)]
impl OpcodeFlags {
    pub const OPTIONAL: Self = Self(1 << 0);
    pub const PREVIEW_ONLY: Self = Self(1 << 1);

    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits)
    }
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Parse an `OpcodeList` blob into a `Vec<Opcode>`. Returns an empty vec for
/// an empty input (which is how a valid-but-no-ops list is encoded).
///
/// Errors on malformed input (truncated header, truncated parameter block,
/// unreasonable counts).
pub fn parse_opcode_list(bytes: &[u8]) -> Result<Vec<Opcode>, OpcodeParseError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() < 4 {
        return Err(OpcodeParseError::Truncated);
    }
    let count = read_u32_be(bytes, 0)? as usize;
    if count > MAX_OPCODE_COUNT {
        return Err(OpcodeParseError::TooManyOpcodes(count));
    }

    let mut out = Vec::with_capacity(count);
    let mut cursor = 4;
    for _ in 0..count {
        if cursor + 16 > bytes.len() {
            return Err(OpcodeParseError::Truncated);
        }
        let id_raw = read_u32_be(bytes, cursor)?;
        let version = read_u32_be(bytes, cursor + 4)?;
        let flags_bits = read_u32_be(bytes, cursor + 8)?;
        let param_len = read_u32_be(bytes, cursor + 12)? as usize;
        cursor += 16;

        if param_len > MAX_OPCODE_PARAM_BYTES {
            return Err(OpcodeParseError::ParamBlockTooLarge(param_len));
        }
        if cursor + param_len > bytes.len() {
            return Err(OpcodeParseError::Truncated);
        }

        let params = bytes[cursor..cursor + param_len].to_vec();
        cursor += param_len;

        out.push(Opcode {
            id: OpcodeId::from_u32(id_raw),
            version,
            flags: OpcodeFlags::from_bits_truncate(flags_bits),
            params,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpcodeParseError {
    Truncated,
    TooManyOpcodes(usize),
    ParamBlockTooLarge(usize),
}

impl fmt::Display for OpcodeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("opcode list truncated"),
            Self::TooManyOpcodes(n) => {
                write!(f, "opcode list claims {n} opcodes (max {MAX_OPCODE_COUNT})")
            }
            Self::ParamBlockTooLarge(n) => write!(
                f,
                "opcode parameter block is {n} bytes (max {MAX_OPCODE_PARAM_BYTES})"
            ),
        }
    }
}

impl std::error::Error for OpcodeParseError {}

// ----- GainMap (opcode 9) --------------------------------------------------

/// Parsed `GainMap` parameters. See DNG spec § 6.1.2.
#[derive(Debug, Clone)]
pub struct GainMap {
    /// Area of the raw image this map covers (top, left, bottom, right).
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    /// Step through the rectangle in `row_pitch` / `col_pitch` pixels.
    pub row_pitch: u32,
    pub col_pitch: u32,
    /// Index of the *first* plane this gain applies to (0-based) and the
    /// number of consecutive planes it covers.
    pub plane: u32,
    /// Number of *consecutive* raw planes this opcode applies to, starting
    /// at `plane`. Our applier only scales the single plane at index
    /// `plane`; the field is preserved for spec completeness.
    #[allow(dead_code)]
    pub planes: u32,
    /// Dimensions of the gain grid in sample points (vertical × horizontal).
    pub map_points_v: u32,
    pub map_points_h: u32,
    /// Spacing between map samples in *normalised* image coordinates, i.e.
    /// `x_img = (map_origin_h + col * map_spacing_h) * (right - left)`. Per
    /// the spec these are in the range [0, 1].
    pub map_spacing_v: f64,
    pub map_spacing_h: f64,
    pub map_origin_v: f64,
    pub map_origin_h: f64,
    /// Number of planes described by `gains`. Usually 1; for OpcodeList1 a
    /// separate GainMap is emitted for each Bayer sub-plane.
    pub map_planes: u32,
    /// Flat `map_points_v * map_points_h * map_planes` f32 grid, row-major.
    pub gains: Vec<f32>,
}

pub fn parse_gain_map(params: &[u8]) -> Result<GainMap, OpcodeParseError> {
    // Fixed header per DNG spec 1.6 § 6.1.2 (matches Adobe DNG SDK):
    //   Top, Left, Bottom, Right (4 × u32)
    //   Plane, Planes, RowPitch, ColPitch (4 × u32)
    //   MapPointsV, MapPointsH (2 × u32)
    //   MapSpacingV, MapSpacingH (2 × f64)
    //   MapOriginV, MapOriginH (2 × f64)
    //   MapPlanes (u32)
    // Total: 16 + 16 + 8 + 16 + 16 + 4 = 76 bytes.
    if params.len() < 76 {
        return Err(OpcodeParseError::Truncated);
    }
    let top = read_u32_be(params, 0)?;
    let left = read_u32_be(params, 4)?;
    let bottom = read_u32_be(params, 8)?;
    let right = read_u32_be(params, 12)?;
    let plane = read_u32_be(params, 16)?;
    let planes = read_u32_be(params, 20)?.max(1);
    let row_pitch = read_u32_be(params, 24)?.max(1);
    let col_pitch = read_u32_be(params, 28)?.max(1);
    let map_points_v = read_u32_be(params, 32)?;
    let map_points_h = read_u32_be(params, 36)?;
    let map_spacing_v = read_f64_be(params, 40)?;
    let map_spacing_h = read_f64_be(params, 48)?;
    let map_origin_v = read_f64_be(params, 56)?;
    let map_origin_h = read_f64_be(params, 64)?;
    let map_planes = read_u32_be(params, 72)?.max(1);

    let gain_count = (map_points_v as usize)
        .checked_mul(map_points_h as usize)
        .and_then(|n| n.checked_mul(map_planes as usize))
        .ok_or(OpcodeParseError::ParamBlockTooLarge(usize::MAX))?;
    let gains_bytes = gain_count
        .checked_mul(4)
        .ok_or(OpcodeParseError::ParamBlockTooLarge(usize::MAX))?;
    if params.len() < 76 + gains_bytes {
        return Err(OpcodeParseError::Truncated);
    }
    let mut gains = Vec::with_capacity(gain_count);
    for i in 0..gain_count {
        let off = 76 + i * 4;
        gains.push(read_f32_be(params, off)?);
    }
    Ok(GainMap {
        top,
        left,
        bottom,
        right,
        row_pitch,
        col_pitch,
        plane,
        planes,
        map_points_v,
        map_points_h,
        map_spacing_v,
        map_spacing_h,
        map_origin_v,
        map_origin_h,
        map_planes,
        gains,
    })
}

impl GainMap {
    /// Sample the gain grid at `(v, h)` in the normalised rect-relative
    /// coordinate space `[0, 1]`. Uses bilinear interpolation for sub-sample
    /// positions; clamps to edge outside the grid.
    fn sample(&self, v: f64, h: f64, plane_idx: usize) -> f32 {
        let points_v = self.map_points_v.max(1) as f64;
        let points_h = self.map_points_h.max(1) as f64;
        // Grid coordinates in sample-index space, with origin/spacing.
        let fy = ((v - self.map_origin_v) / self.map_spacing_v.max(f64::EPSILON))
            .clamp(0.0, points_v - 1.0);
        let fx = ((h - self.map_origin_h) / self.map_spacing_h.max(f64::EPSILON))
            .clamp(0.0, points_h - 1.0);
        let y0 = fy.floor() as usize;
        let x0 = fx.floor() as usize;
        let y1 = (y0 + 1).min(self.map_points_v.saturating_sub(1) as usize);
        let x1 = (x0 + 1).min(self.map_points_h.saturating_sub(1) as usize);
        let ty = (fy - y0 as f64) as f32;
        let tx = (fx - x0 as f64) as f32;

        let stride = self.map_points_h as usize * self.map_planes as usize;
        let plane_stride = self.map_planes as usize;
        let idx = |y: usize, x: usize| y * stride + x * plane_stride + plane_idx;

        let g00 = self.gains[idx(y0, x0)];
        let g01 = self.gains[idx(y0, x1)];
        let g10 = self.gains[idx(y1, x0)];
        let g11 = self.gains[idx(y1, x1)];

        let g0 = g00 * (1.0 - tx) + g01 * tx;
        let g1 = g10 * (1.0 - tx) + g11 * tx;
        g0 * (1.0 - ty) + g1 * ty
    }
}

/// Apply a `GainMap` opcode on a CFA (Bayer) buffer. `data` is a single-plane
/// mosaic, `cfa_color_at(y, x)` returns the CFA color index at that pixel.
/// The opcode's `plane` is the CFA color index it applies to; only pixels of
/// that plane are scaled.
pub fn apply_gain_map_cfa(
    data: &mut [f32],
    width: u32,
    height: u32,
    map: &GainMap,
    cfa_color_at: impl Fn(u32, u32) -> u32 + Sync,
) {
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h {
        return;
    }
    let rect_h = map.right.saturating_sub(map.left).max(1) as f64;
    let rect_v = map.bottom.saturating_sub(map.top).max(1) as f64;
    let target_plane = map.plane;

    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y_u = y as u32;
        if y_u < map.top || y_u >= map.bottom {
            return;
        }
        // Subsample step in the opcode's "row_pitch" / "col_pitch" sense: per
        // DNG spec only rows/cols at multiples of the pitch within the rect
        // are affected. For the overwhelmingly common case of pitch = 1 this
        // is a no-op; for pitch > 1 it skips in-between rows.
        if !(y_u - map.top).is_multiple_of(map.row_pitch) {
            return;
        }
        let v_norm = (y_u - map.top) as f64 / rect_v;
        for (x, pixel) in row.iter_mut().enumerate() {
            let x_u = x as u32;
            if x_u < map.left || x_u >= map.right {
                continue;
            }
            if !(x_u - map.left).is_multiple_of(map.col_pitch) {
                continue;
            }
            if cfa_color_at(y_u, x_u) != target_plane {
                continue;
            }
            let h_norm = (x_u - map.left) as f64 / rect_h;
            // A per-plane gain map stores exactly one plane; plane_idx 0.
            let gain = map.sample(v_norm, h_norm, 0);
            *pixel *= gain;
        }
    });
}

/// Apply a `GainMap` on a packed RGB (3-plane) float buffer (post-demosaic).
/// `plane_offset` adjusts the opcode's plane index (e.g. OpcodeList2 on a
/// 3-channel image uses 0=R, 1=G, 2=B directly).
pub fn apply_gain_map_rgb(data: &mut [f32], width: u32, height: u32, map: &GainMap) {
    if map.plane >= 3 {
        log::debug!(
            "GainMap plane {} is outside RGB range; skipping (possibly a CFA-only map in OpcodeList2)",
            map.plane
        );
        return;
    }
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h * 3 {
        return;
    }
    let rect_h = map.right.saturating_sub(map.left).max(1) as f64;
    let rect_v = map.bottom.saturating_sub(map.top).max(1) as f64;
    let plane = map.plane as usize;

    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let y_u = y as u32;
        if y_u < map.top || y_u >= map.bottom {
            return;
        }
        if !(y_u - map.top).is_multiple_of(map.row_pitch) {
            return;
        }
        let v_norm = (y_u - map.top) as f64 / rect_v;
        for (x, chunk) in row.chunks_exact_mut(3).enumerate() {
            let x_u = x as u32;
            if x_u < map.left || x_u >= map.right {
                continue;
            }
            if !(x_u - map.left).is_multiple_of(map.col_pitch) {
                continue;
            }
            let h_norm = (x_u - map.left) as f64 / rect_h;
            let gain = map.sample(v_norm, h_norm, 0);
            chunk[plane] *= gain;
        }
    });
}

// ----- WarpRectilinear (opcode 1) ------------------------------------------

/// Per-plane `WarpRectilinear` coefficients (DNG spec § 6.1.1). Each plane
/// has 6 radial + tangential coefficients and a 2-component optical center.
#[derive(Debug, Clone, Copy)]
pub struct WarpPlane {
    pub kr0: f64,
    pub kr1: f64,
    pub kr2: f64,
    pub kr3: f64,
    pub kt0: f64,
    pub kt1: f64,
    pub cx: f64,
    pub cy: f64,
}

#[derive(Debug, Clone)]
pub struct WarpRectilinear {
    pub planes: Vec<WarpPlane>,
}

pub fn parse_warp_rectilinear(params: &[u8]) -> Result<WarpRectilinear, OpcodeParseError> {
    if params.len() < 4 {
        return Err(OpcodeParseError::Truncated);
    }
    let plane_count = read_u32_be(params, 0)? as usize;
    if plane_count == 0 || plane_count > 4 {
        // Spec allows 1 or 3; four leaves room for future 4-plane sensors.
        return Err(OpcodeParseError::ParamBlockTooLarge(plane_count));
    }
    // Per plane: 6 radial+tangential coefficients (`kr0..kt1`) followed by 2
    // doubles for the optical center `(cx, cy)` (normalised, 0..1 per axis).
    // That's 8 doubles × plane_count = 64 × plane_count bytes after the
    // 4-byte plane-count header.
    let needed = 4 + plane_count * 8 * 8;
    if params.len() < needed {
        return Err(OpcodeParseError::Truncated);
    }
    let mut planes = Vec::with_capacity(plane_count);
    let mut cursor = 4;
    for _ in 0..plane_count {
        let kr0 = read_f64_be(params, cursor)?;
        let kr1 = read_f64_be(params, cursor + 8)?;
        let kr2 = read_f64_be(params, cursor + 16)?;
        let kr3 = read_f64_be(params, cursor + 24)?;
        let kt0 = read_f64_be(params, cursor + 32)?;
        let kt1 = read_f64_be(params, cursor + 40)?;
        let cx = read_f64_be(params, cursor + 48)?;
        let cy = read_f64_be(params, cursor + 56)?;
        planes.push(WarpPlane {
            kr0,
            kr1,
            kr2,
            kr3,
            kt0,
            kt1,
            cx,
            cy,
        });
        cursor += 64;
    }
    Ok(WarpRectilinear { planes })
}

/// Apply `WarpRectilinear` to a packed RGB float buffer. For each output
/// pixel and each plane, compute the source position, bilinearly sample the
/// input, and write the result. Allocates a copy of the source buffer once
/// and writes back into the caller's slice.
pub fn apply_warp_rectilinear_rgb(
    data: &mut [f32],
    width: u32,
    height: u32,
    warp: &WarpRectilinear,
) {
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h * 3 || w < 2 || h < 2 {
        return;
    }
    if warp.planes.is_empty() {
        return;
    }
    let source = data.to_vec();

    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, chunk) in row.chunks_exact_mut(3).enumerate() {
            for (plane_idx, slot) in chunk.iter_mut().enumerate().take(3) {
                // If the warp ships one plane, reuse it for all channels;
                // otherwise pick the plane matching the channel index.
                let plane = if warp.planes.len() == 1 {
                    warp.planes[0]
                } else {
                    warp.planes[plane_idx.min(warp.planes.len() - 1)]
                };
                let (sx, sy) = warp_source_coord(x as f64, y as f64, w, h, &plane);
                *slot = sample_bilinear_rgb(&source, w, h, sx, sy, plane_idx);
            }
        }
    });
}

/// Compute the source pixel position for an output (x, y) under the given
/// warp plane. The DNG spec defines a normalised coordinate system centered
/// on the optical center `(cx, cy)`, scaled so the half-diagonal has length
/// 1. Both `cx` and `cy` are given as fractions of the image dimensions.
fn warp_source_coord(x: f64, y: f64, w: usize, h: usize, plane: &WarpPlane) -> (f64, f64) {
    let cx_pix = plane.cx * (w as f64 - 1.0);
    let cy_pix = plane.cy * (h as f64 - 1.0);
    // Half-diagonal: distance from optical center to the farthest corner.
    // Using the image center's half-diagonal keeps scaling stable even when
    // the optical center sits off-axis.
    let half_w = (w as f64 - 1.0) * 0.5;
    let half_h = (h as f64 - 1.0) * 0.5;
    let norm = (half_w * half_w + half_h * half_h).sqrt().max(1.0);

    let dx = (x - cx_pix) / norm;
    let dy = (y - cy_pix) / norm;
    let r2 = dx * dx + dy * dy;
    let r4 = r2 * r2;
    let r6 = r4 * r2;
    let radial = plane.kr0 + plane.kr1 * r2 + plane.kr2 * r4 + plane.kr3 * r6;
    let sx_rel = dx * radial + 2.0 * plane.kt0 * dx * dy + plane.kt1 * (r2 + 2.0 * dx * dx);
    let sy_rel = dy * radial + plane.kt0 * (r2 + 2.0 * dy * dy) + 2.0 * plane.kt1 * dx * dy;
    (sx_rel * norm + cx_pix, sy_rel * norm + cy_pix)
}

/// Bilinearly sample channel `plane` from an interleaved RGB f32 buffer.
/// Clamps source coords to the image edge (no wrap, no black border).
fn sample_bilinear_rgb(src: &[f32], w: usize, h: usize, sx: f64, sy: f64, plane: usize) -> f32 {
    if !sx.is_finite() || !sy.is_finite() {
        return 0.0;
    }
    let max_x = (w as f64 - 1.0).max(0.0);
    let max_y = (h as f64 - 1.0).max(0.0);
    let x = sx.clamp(0.0, max_x);
    let y = sy.clamp(0.0, max_y);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (x - x0 as f64) as f32;
    let ty = (y - y0 as f64) as f32;
    let at = |yy: usize, xx: usize| src[(yy * w + xx) * 3 + plane];
    let a = at(y0, x0) * (1.0 - tx) + at(y0, x1) * tx;
    let b = at(y1, x0) * (1.0 - tx) + at(y1, x1) * tx;
    a * (1.0 - ty) + b * ty
}

// ----- Bad-pixel fixes (opcodes 4, 5) --------------------------------------

#[derive(Debug, Clone)]
pub struct FixBadPixelsConstant {
    pub constant: u32,
    /// CFA phase the opcode was authored against. We don't branch on it;
    /// the apply treats every "bad" pixel identically regardless of
    /// position. Kept for spec completeness.
    #[allow(dead_code)]
    pub bayer_phase: u32,
}

pub fn parse_fix_bad_pixels_constant(
    params: &[u8],
) -> Result<FixBadPixelsConstant, OpcodeParseError> {
    if params.len() < 8 {
        return Err(OpcodeParseError::Truncated);
    }
    Ok(FixBadPixelsConstant {
        constant: read_u32_be(params, 0)?,
        bayer_phase: read_u32_be(params, 4)?,
    })
}

/// Replace any pixel whose value equals `constant` with the arithmetic mean
/// of its eight nearest neighbors that aren't also `constant`. `constant`
/// here is `raw_constant / 65535.0` since the caller hands us float-scaled
/// raw data. For DNGs where `RawImageData` is `Integer`, rawler's `as_f32()`
/// divides by `u16::MAX`.
pub fn apply_fix_bad_pixels_constant(
    data: &mut [f32],
    width: u32,
    height: u32,
    op: &FixBadPixelsConstant,
) {
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h || w < 3 || h < 3 {
        return;
    }
    let target = (op.constant as f32) / (u16::MAX as f32);
    // Small tolerance in case rescale has shifted the value by a hair.
    let eps = 1.0 / (u16::MAX as f32);
    let src = data.to_vec();
    for y in 0..h {
        for x in 0..w {
            let v = src[y * w + x];
            if (v - target).abs() > eps {
                continue;
            }
            let mut sum = 0.0_f32;
            let mut count = 0_u32;
            for dy in -1..=1_i32 {
                for dx in -1..=1_i32 {
                    if dy == 0 && dx == 0 {
                        continue;
                    }
                    let ny = y as i32 + dy;
                    let nx = x as i32 + dx;
                    if ny < 0 || ny >= h as i32 || nx < 0 || nx >= w as i32 {
                        continue;
                    }
                    let nv = src[ny as usize * w + nx as usize];
                    if (nv - target).abs() <= eps {
                        continue;
                    }
                    sum += nv;
                    count += 1;
                }
            }
            if count > 0 {
                data[y * w + x] = sum / count as f32;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FixBadPixelsList {
    /// CFA phase the opcode was authored against. Kept for spec
    /// completeness; our apply interpolates from every immediate neighbor
    /// regardless of phase.
    #[allow(dead_code)]
    pub bayer_phase: u32,
    pub bad_points: Vec<(u32, u32)>,
    pub bad_rects: Vec<(u32, u32, u32, u32)>, // top, left, bottom, right
}

pub fn parse_fix_bad_pixels_list(params: &[u8]) -> Result<FixBadPixelsList, OpcodeParseError> {
    if params.len() < 12 {
        return Err(OpcodeParseError::Truncated);
    }
    let bayer_phase = read_u32_be(params, 0)?;
    let points_count = read_u32_be(params, 4)? as usize;
    let rects_count = read_u32_be(params, 8)? as usize;
    let needed = 12 + points_count * 8 + rects_count * 16;
    if params.len() < needed {
        return Err(OpcodeParseError::Truncated);
    }
    let mut bad_points = Vec::with_capacity(points_count);
    let mut cursor = 12;
    for _ in 0..points_count {
        bad_points.push((
            read_u32_be(params, cursor)?,
            read_u32_be(params, cursor + 4)?,
        ));
        cursor += 8;
    }
    let mut bad_rects = Vec::with_capacity(rects_count);
    for _ in 0..rects_count {
        bad_rects.push((
            read_u32_be(params, cursor)?,
            read_u32_be(params, cursor + 4)?,
            read_u32_be(params, cursor + 8)?,
            read_u32_be(params, cursor + 12)?,
        ));
        cursor += 16;
    }
    Ok(FixBadPixelsList {
        bayer_phase,
        bad_points,
        bad_rects,
    })
}

/// Replace each listed bad pixel with the arithmetic mean of its 3×3
/// neighbors (clamped to image borders). Skips the pixel itself.
pub fn apply_fix_bad_pixels_list(data: &mut [f32], width: u32, height: u32, op: &FixBadPixelsList) {
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h {
        return;
    }
    let src = data.to_vec();
    let repair = |data: &mut [f32], py: u32, px: u32| {
        if py >= height || px >= width {
            return;
        }
        let mut sum = 0.0_f32;
        let mut count = 0_u32;
        for dy in -1..=1_i32 {
            for dx in -1..=1_i32 {
                if dy == 0 && dx == 0 {
                    continue;
                }
                let ny = py as i32 + dy;
                let nx = px as i32 + dx;
                if ny < 0 || ny >= h as i32 || nx < 0 || nx >= w as i32 {
                    continue;
                }
                sum += src[ny as usize * w + nx as usize];
                count += 1;
            }
        }
        if count > 0 {
            data[py as usize * w + px as usize] = sum / count as f32;
        }
    };
    for &(y, x) in &op.bad_points {
        repair(data, y, x);
    }
    for &(top, left, bottom, right) in &op.bad_rects {
        for y in top..bottom.min(height) {
            for x in left..right.min(width) {
                repair(data, y, x);
            }
        }
    }
}

// ----- byte helpers --------------------------------------------------------

fn read_u32_be(buf: &[u8], off: usize) -> Result<u32, OpcodeParseError> {
    buf.get(off..off + 4)
        .ok_or(OpcodeParseError::Truncated)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_f32_be(buf: &[u8], off: usize) -> Result<f32, OpcodeParseError> {
    buf.get(off..off + 4)
        .ok_or(OpcodeParseError::Truncated)
        .map(|s| f32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_f64_be(buf: &[u8], off: usize) -> Result<f64, OpcodeParseError> {
    buf.get(off..off + 8)
        .ok_or(OpcodeParseError::Truncated)
        .map(|s| f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an opcode-list blob: count + repeated (id, ver, flags, len,
    /// params). All big-endian.
    fn build_list(entries: &[(u32, u32, u32, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (id, ver, flags, params) in entries {
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&ver.to_be_bytes());
            out.extend_from_slice(&flags.to_be_bytes());
            out.extend_from_slice(&(params.len() as u32).to_be_bytes());
            out.extend_from_slice(params);
        }
        out
    }

    #[test]
    fn parse_empty_blob() {
        assert!(parse_opcode_list(&[]).unwrap().is_empty());
    }

    #[test]
    fn parse_zero_count_blob() {
        let buf = 0u32.to_be_bytes();
        assert!(parse_opcode_list(&buf).unwrap().is_empty());
    }

    #[test]
    fn parse_truncated_header() {
        // 2 bytes, not enough for the 4-byte count header
        assert!(matches!(
            parse_opcode_list(&[0x00, 0x01]),
            Err(OpcodeParseError::Truncated)
        ));
    }

    #[test]
    fn parse_truncated_opcode() {
        // Claims 1 opcode but cuts off mid-header.
        let mut buf = 1u32.to_be_bytes().to_vec();
        buf.extend_from_slice(&[0; 8]); // only 8 of the 16 header bytes
        assert!(matches!(
            parse_opcode_list(&buf),
            Err(OpcodeParseError::Truncated)
        ));
    }

    #[test]
    fn parse_two_opcodes_with_flags() {
        let blob = build_list(&[(9, 1, 0x1, &[0xAA]), (1, 2, 0x2, &[0xBB, 0xCC])]);
        let parsed = parse_opcode_list(&blob).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, OpcodeId::GainMap);
        assert_eq!(parsed[0].version, 1);
        assert!(parsed[0].is_optional());
        assert!(!parsed[0].is_preview_only());
        assert_eq!(parsed[0].params, vec![0xAA]);
        assert_eq!(parsed[1].id, OpcodeId::WarpRectilinear);
        assert!(!parsed[1].is_optional());
        assert!(parsed[1].is_preview_only());
    }

    #[test]
    fn parse_unknown_opcode_round_trips_numeric() {
        let blob = build_list(&[(42, 1, 0, &[])]);
        let parsed = parse_opcode_list(&blob).unwrap();
        assert_eq!(parsed[0].id, OpcodeId::Unknown(42));
        assert_eq!(parsed[0].id.numeric(), 42);
    }

    #[test]
    fn parse_refuses_insane_count() {
        // Count = 10_000 far above MAX_OPCODE_COUNT
        let mut buf = (10_000u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&[0; 16]);
        assert!(matches!(
            parse_opcode_list(&buf),
            Err(OpcodeParseError::TooManyOpcodes(_))
        ));
    }

    // ---------------------- GainMap tests ---------------------------------

    /// Build a GainMap parameter blob with `map_planes = 1` and gains in
    /// row-major order. Field order matches `parse_gain_map`.
    #[allow(clippy::too_many_arguments)]
    fn build_gain_map_params(
        top: u32,
        left: u32,
        bottom: u32,
        right: u32,
        row_pitch: u32,
        col_pitch: u32,
        plane: u32,
        planes: u32,
        points_v: u32,
        points_h: u32,
        spacing_v: f64,
        spacing_h: f64,
        origin_v: f64,
        origin_h: f64,
        gains: &[f32],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&top.to_be_bytes());
        out.extend_from_slice(&left.to_be_bytes());
        out.extend_from_slice(&bottom.to_be_bytes());
        out.extend_from_slice(&right.to_be_bytes());
        // plane, planes, row_pitch, col_pitch (per DNG SDK ordering)
        out.extend_from_slice(&plane.to_be_bytes());
        out.extend_from_slice(&planes.to_be_bytes());
        out.extend_from_slice(&row_pitch.to_be_bytes());
        out.extend_from_slice(&col_pitch.to_be_bytes());
        out.extend_from_slice(&points_v.to_be_bytes());
        out.extend_from_slice(&points_h.to_be_bytes());
        out.extend_from_slice(&spacing_v.to_be_bytes());
        out.extend_from_slice(&spacing_h.to_be_bytes());
        out.extend_from_slice(&origin_v.to_be_bytes());
        out.extend_from_slice(&origin_h.to_be_bytes());
        out.extend_from_slice(&1_u32.to_be_bytes()); // map_planes = 1
        for g in gains {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    #[test]
    fn gain_map_identity_leaves_data_unchanged() {
        // 2x2 gain grid, all 1.0, over a 4x4 image.
        let params = build_gain_map_params(
            0,
            0,
            4,
            4,
            1,
            1,
            0,
            1,
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            &[1.0, 1.0, 1.0, 1.0],
        );
        let map = parse_gain_map(&params).unwrap();
        let mut data = vec![0.5_f32; 16];
        apply_gain_map_cfa(&mut data, 4, 4, &map, |_y, _x| 0);
        for v in &data {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn gain_map_scales_corner_pixels() {
        // 2x2 gain grid over a 4x4 image with corner gains explicit.
        let params = build_gain_map_params(
            0,
            0,
            4,
            4,
            1,
            1,
            0,
            1,
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            &[2.0, 2.0, 2.0, 2.0],
        );
        let map = parse_gain_map(&params).unwrap();
        let mut data = vec![1.0_f32; 16];
        apply_gain_map_cfa(&mut data, 4, 4, &map, |_y, _x| 0);
        // Every pixel multiplied by 2.0
        for v in &data {
            assert!((v - 2.0).abs() < 1e-6);
        }
    }

    #[test]
    fn gain_map_is_bayer_aware() {
        // Applies plane=0 (R) to an RGGB pattern. Only the R pixels should be scaled.
        let params = build_gain_map_params(
            0,
            0,
            4,
            4,
            1,
            1,
            0,
            1,
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            &[3.0, 3.0, 3.0, 3.0],
        );
        let map = parse_gain_map(&params).unwrap();
        let mut data = vec![1.0_f32; 16];
        apply_gain_map_cfa(&mut data, 4, 4, &map, |y, x| {
            // RGGB: plane 0 = R at (0,0), (0,2), (2,0), (2,2) and so on
            match (y % 2, x % 2) {
                (0, 0) => 0, // R
                (0, 1) => 1, // G
                (1, 0) => 1, // G
                (1, 1) => 2, // B
                _ => unreachable!(),
            }
        });
        for y in 0..4 {
            for x in 0..4 {
                let v = data[y * 4 + x];
                let is_red = y % 2 == 0 && x % 2 == 0;
                let want = if is_red { 3.0 } else { 1.0 };
                assert!((v - want).abs() < 1e-6, "y={y} x={x} v={v} want={want}");
            }
        }
    }

    #[test]
    fn gain_map_on_rgb_only_touches_target_plane() {
        // Same 2x2 all-3.0 gain map but on a 3-channel RGB buffer; plane=1 (G).
        let params = build_gain_map_params(
            0,
            0,
            4,
            4,
            1,
            1,
            1,
            1,
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            &[3.0, 3.0, 3.0, 3.0],
        );
        let map = parse_gain_map(&params).unwrap();
        let mut data = vec![1.0_f32; 4 * 4 * 3];
        apply_gain_map_rgb(&mut data, 4, 4, &map);
        for chunk in data.chunks_exact(3) {
            assert!((chunk[0] - 1.0).abs() < 1e-6);
            assert!((chunk[1] - 3.0).abs() < 1e-6);
            assert!((chunk[2] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn gain_map_bilinear_interpolates_between_corners() {
        // 2×2 grid, corners at 1.0, 2.0, 2.0, 3.0. Image 2×2 with rect
        // covering the whole image. Each map point lands on a pixel corner
        // exactly, so we verify the unambiguous corner gains.
        let params = build_gain_map_params(
            0,
            0,
            2,
            2,
            1,
            1,
            0,
            1,
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            &[1.0, 2.0, 2.0, 3.0],
        );
        let map = parse_gain_map(&params).unwrap();
        let mut data = vec![1.0_f32; 4];
        apply_gain_map_cfa(&mut data, 2, 2, &map, |_y, _x| 0);
        // h_norm = (x - left) / (right - left); x = 0 -> 0, x = 1 -> 0.5.
        // (Right edge x = 1 is the *last pixel*, not "one past"; rect is
        // half-open [left, right) in our apply but the normalisation uses
        // the full width.)
        // Expected:
        //   (0, 0): sample at (0, 0) = 1.0
        //   (0, 1): sample at (0, 0.5) = (1.0 + 2.0)/2 = 1.5
        //   (1, 0): sample at (0.5, 0) = (1.0 + 2.0)/2 = 1.5
        //   (1, 1): sample at (0.5, 0.5) = (1+2+2+3)/4 = 2.0
        assert!((data[0] - 1.0).abs() < 1e-5, "got {}", data[0]);
        assert!((data[1] - 1.5).abs() < 1e-5, "got {}", data[1]);
        assert!((data[2] - 1.5).abs() < 1e-5, "got {}", data[2]);
        assert!((data[3] - 2.0).abs() < 1e-5, "got {}", data[3]);
    }

    // ---------------------- WarpRectilinear tests -------------------------

    fn build_warp_params_single(plane: WarpPlane) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1_u32.to_be_bytes());
        out.extend_from_slice(&plane.kr0.to_be_bytes());
        out.extend_from_slice(&plane.kr1.to_be_bytes());
        out.extend_from_slice(&plane.kr2.to_be_bytes());
        out.extend_from_slice(&plane.kr3.to_be_bytes());
        out.extend_from_slice(&plane.kt0.to_be_bytes());
        out.extend_from_slice(&plane.kt1.to_be_bytes());
        out.extend_from_slice(&plane.cx.to_be_bytes());
        out.extend_from_slice(&plane.cy.to_be_bytes());
        out
    }

    #[test]
    fn warp_rectilinear_identity_is_noop() {
        let params = build_warp_params_single(WarpPlane {
            kr0: 1.0,
            kr1: 0.0,
            kr2: 0.0,
            kr3: 0.0,
            kt0: 0.0,
            kt1: 0.0,
            cx: 0.5,
            cy: 0.5,
        });
        let warp = parse_warp_rectilinear(&params).unwrap();
        assert_eq!(warp.planes.len(), 1);
        // Fill a 16x16 buffer with a horizontal gradient; identity warp should
        // leave it (up to bilinear rounding) identical.
        let w = 16;
        let h = 16;
        let mut data = vec![0.0_f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = (x as f32) / (w as f32);
                let idx = (y * w + x) * 3;
                data[idx] = v;
                data[idx + 1] = v;
                data[idx + 2] = v;
            }
        }
        let before = data.clone();
        apply_warp_rectilinear_rgb(&mut data, w as u32, h as u32, &warp);
        for (i, (a, b)) in before.iter().zip(data.iter()).enumerate() {
            assert!((a - b).abs() < 1e-4, "pixel {i}: before {a} after {b}");
        }
    }

    // ---------------------- Bad pixel tests -------------------------------

    #[test]
    fn fix_bad_pixels_list_replaces_listed_coord() {
        let op = FixBadPixelsList {
            bayer_phase: 0,
            bad_points: vec![(1, 1)],
            bad_rects: vec![],
        };
        // 3x3 image with center at 0.0, everything else at 1.0. After fix
        // the center should average to 1.0 (all 8 neighbors are 1.0).
        let mut data = vec![1.0_f32; 9];
        data[4] = 0.0;
        apply_fix_bad_pixels_list(&mut data, 3, 3, &op);
        assert!((data[4] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fix_bad_pixels_constant_averages_neighbors() {
        let op = FixBadPixelsConstant {
            constant: 0, // zero-valued pixels are "bad"
            bayer_phase: 0,
        };
        // 3x3 image, center at 0.0, neighbors all at 0.5 (non-zero).
        let mut data = vec![0.5_f32; 9];
        data[4] = 0.0;
        apply_fix_bad_pixels_constant(&mut data, 3, 3, &op);
        assert!((data[4] - 0.5).abs() < 1e-5, "got {}", data[4]);
    }

    #[test]
    fn fix_bad_pixels_list_handles_empty_list() {
        let op = FixBadPixelsList {
            bayer_phase: 0,
            bad_points: vec![],
            bad_rects: vec![],
        };
        let original = vec![0.1, 0.2, 0.3, 0.4];
        let mut data = original.clone();
        apply_fix_bad_pixels_list(&mut data, 2, 2, &op);
        assert_eq!(data, original);
    }
}
