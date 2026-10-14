# HDR images: gain maps, PQ/HLG, float, and 16-bit

Tracking issue: https://github.com/vdavid/prvw/issues/5

Goal: show every HDR still Prvw can open the way the file intends, on every display, and stop losing precision on 16-bit
SDR files. Today only camera RAW reaches the EDR/HDR surface. Everything else is decoded to 8-bit, color-managed into
the display profile, and anything above SDR white is gone before the GPU sees it.

This spec covers four inputs, in this order of real-world impact:

1. **Gain-map JPEGs**: Ultra HDR (Pixel, Samsung, most Android since 14), ISO 21496-1 (Android 15+, Lightroom, newer
   iPhone exports), and Apple's older maker-note flavor. Normal SDR JPEG plus a hidden "gain map" image saying how much
   brighter each area gets on an HDR display.
2. **PQ and HLG PNGs**: PNG third edition's `cICP` chunk labels a 16-bit PNG as BT.2100 PQ or HLG. Today we read the PQ
   code values as sRGB, so these look grey and flat, which is worse than clipping.
3. **Float TIFFs**: linear-light `f32` samples, where values above 1.0 are highlights. Today `to_rgba8()` clips them.
4. **16-bit SDR PNGs and TIFFs**: no HDR, but `to_rgba8()` truncates to 8 bits _before_ the ICC transform, so the
   transform's own rounding stacks on top and smooth gradients band.

AVIF is out of scope here, but the design is built so it plugs straight in later (see "How AVIF plugs in").

## Decisions already made (David, 2026-10-13)

- **Gain maps and HDR tone mapping run in the fragment shader**, with the display's live headroom as a uniform. The CPU
  decodes and color-manages the SDR base once, and the GPU does the per-frame, headroom-dependent part.
  - **Why:** macOS changes the EDR headroom continuously with screen brightness. A CPU-side gain map would mean a full
    re-decode on every brightness change (today `App::refresh_edr_headroom` clears the whole image cache whenever the
    headroom moves). In the shader, a headroom change is one uniform write. It also keeps gain-map images at 8 bits per
    channel plus a small map, so the preload cache holds as many of them as SDR images. Skia (Chrome, Android) does it
    the same way.
  - **Accepted costs:** a second HDR mechanism next to RAW's CPU path (RAW stays on the CPU, because its HDR comes out
    of the develop, not a map), a GPU parity test harness, and care around color spaces (spelled out below).
- **Crates:** `ultrahdr-core` for the gain-map math and metadata, never `ultrahdr-rs` (see "Dependencies").
- **Order:** this whole spec lands before AVIF.

## Decisions made with the spec (David, 2026-10-13)

David picked 1(a) and 2(a). The alternatives stay listed so the reasoning survives.

1. **Where the HDR switch lives: (a), move it to Settings → Color.** Today it's Settings → RAW → "HDR / EDR output"
   (`raw.hdr_output`, `RawPipelineFlags::hdr_output`). After this spec it governs every format.
   - (a) **Chosen:** move it to Settings → Color as "HDR output", key `color.hdr_output`, migrating the old key on load.
     Rename internals to match (principle 4 in `AGENTS.md`), update the parity registry (`parity/setting_keys.rs`) and
     the Windows settings model (`settings/windows/model.rs`).
   - (b) Keep it in the RAW panel and widen its meaning. Cheaper, but the label then lies.
2. **How far 16-bit SDR precision goes (M1): (a), keep the 8-bit surface for now.**
   - (a) **Chosen:** keep the 8-bit SDR surface. The 16-bit buffer removes the double rounding (decode then transform),
     which is where the visible banding comes from. Output still quantizes to 8 bits once, like any SDR image.
   - (b) Later, if a gradient test on the XDR panel still shows banding: switch the surface to `Rgba16Float` while a
     16-bit image is on screen, so more than 8 bits reach the compositor. That needs the shader to output
     display-encoded values on a float surface whose layer colorspace is the display ICC, which is new surface plumbing.
     Not in this spec.

## Background: how pixels reach the screen today

Read these before writing code: `apps/desktop/src/render/CLAUDE.md`, `apps/desktop/src/decoding/CLAUDE.md` (especially
"HDR / EDR flow"), `apps/desktop/src/color/CLAUDE.md`, and `docs/notes/raw-support-phase5.md`.

- **SDR path (every non-RAW image, and RAW with HDR off):** decode to RGBA8 → `color::transform_icc` from the embedded
  profile (sRGB if none) into the display's ICC profile → `PixelBuffer::Rgba8` → texture `Rgba8UnormSrgb` → surface
  `Bgra8UnormSrgb`. The texture's sRGB decode and the surface's sRGB encode cancel, so the display-encoded bytes reach
  the compositor unchanged, and the `CAMetalLayer` colorspace is the display ICC.
- **RAW HDR path:** `raw::decode` decides `hdr_active = flags.hdr_output && edr_headroom > 1.0` (it uses the headroom
  only as that gate; verified at `decoding/raw.rs:223`). Output is linear light in the platform's HDR display space
  (`color::profiles::HdrDisplaySpace::for_host()`: linear Display P3 on macOS, linear BT.709/scRGB on Windows) →
  `PixelBuffer::Rgba16F` → texture `Rgba16Float` → surface `Rgba16Float` with `kCGColorSpaceExtendedLinearDisplayP3`
  (macOS) or `ExtendedSrgbLinear` (Windows). Values above 1.0 are brighter than SDR white.
- **Surface switching:** `App::want_edr_surface()` =
  `raw_flags.hdr_output && edr_headroom > 1.0 && current_image_is_hdr`, applied by `App::apply_edr_surface_state()` →
  `Renderer::reconfigure_surface_format()` (+ `display_profile::set_layer_edr_state` on macOS). `current_image_is_hdr`
  comes from `PixelBuffer::is_hdr()`, which today means "is `Rgba16F`".
- **Headroom:** `Renderer::display_hdr_headroom()` (wgpu `Surface::display_hdr_info`, main-thread only on Metal).
  Refreshed only in `App::refresh_edr_headroom()`, which runs on `DisplayChanged` and on a window move to another
  monitor. Nothing notices a brightness change today.
- **Shader:** `render/shader.wgsl` is one texture sample plus a checkerboard for alpha and a `fade` factor for the
  slideshow crossfade. Bind group 0 = transform uniform, image texture, sampler. The crossfade keeps the outgoing
  image's bind group alive in `CrossfadeState`.
- **Other consumers of `DecodedImage::pixels`:** `histogram/compute.rs`, `platform/windows/ui_common.rs` (copy and print
  re-decode with headroom `1.0` and require `Rgba8`), `previews/generator.rs`, `qa/window_capture.rs`,
  `app/executor.rs`, `navigation/preloader.rs` (`image_memory_cost`, `set_hdr_mode` budget switch). Every one must
  handle the new shapes. Matches on `PixelBuffer` must stay exhaustive (no `_ =>` arms), so the compiler finds them for
  you.

## Design

### 1. Say what the pixels mean, not only how they're stored

`PixelBuffer::is_hdr()` conflates storage with meaning, and M1 breaks it (a 16-bit SDR image stored as `Rgba16F` is not
HDR). Split the two:

```rust
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: PixelBuffer,        // storage: Rgba8 | Rgba16F (unchanged variants)
    pub encoding: PixelEncoding,    // meaning, new
    pub exif: Option<Box<ExifMetadata>>,
    pub stamp: Option<FileStamp>,
}

pub enum PixelEncoding {
    /// Display-encoded SDR: values are in the display's ICC profile. For `Rgba8`, the bytes as-is.
    /// For `Rgba16F`, the display-encoded value passed through the sRGB EOTF, so the texture's
    /// `Rgba16Float` sample plus the sRGB surface encode lands on the same display-encoded value
    /// an `Rgba8UnormSrgb` texture would (M1).
    DisplaySdr,
    /// Linear light in `HdrDisplaySpace::for_host()`, 1.0 = SDR white. RAW HDR today.
    LinearHdrDisplay,
    /// SDR base plus a gain map, applied in the shader (M3).
    GainMapped(Box<GainMapPayload>),
    /// Linear light relative to SDR white in some source primaries, tone-mapped in the shader (M4).
    SceneLinear(SceneLinearParams),
}
```

- Constructors enforce the legal pairs: `DisplaySdr` with either storage, `LinearHdrDisplay` and `SceneLinear` with
  `Rgba16F`, `GainMapped` with `Rgba8`. Keep `from_rgba8` (it means `DisplaySdr`) so the existing call sites don't
  churn.
- `is_hdr()` moves to `PixelEncoding` and means "needs the EDR surface": everything except `DisplaySdr`.
- `image_memory_cost` adds the gain map's bytes.
- Name check (principle 4): there's no UI label for these yet. If M5 renames the setting to "HDR output", keep the
  code's vocabulary aligned with it.

### 2. One shader, four modes, two uniform groups

Extend `shader.wgsl` and `Renderer` so every image draws through one pipeline:

- **Group 0 (per image, owned by the image's bind group, so the crossfade keeps working):** transform uniform (as
  today), image texture, sampler, **gain-map texture** (binding 3; a 1×1 placeholder texture when unused), and an
  **image params uniform** (binding 4) holding `mode` plus that mode's parameters.
- **Group 1 (per frame, one buffer for the whole renderer):** `headroom_now` (the display's current headroom, ≥ 1.0) and
  whatever else is display-dependent. A headroom change writes this one buffer and requests a redraw. Nothing per image
  is rewritten.
- **Modes:**
  - `0` passthrough for `DisplaySdr` (both storages). Today's behavior.
  - `1` passthrough for `LinearHdrDisplay`. Today's RAW HDR behavior.
  - `2` gain map (section 4).
  - `3` scene-linear tone map (section 5).
- The checkerboard alpha composite and the crossfade `fade` stay as they are for every mode, applied after the mode's
  color math.
- Keep a Rust reference implementation of modes 2 and 3 next to the color code (`color/hdr/` is a reasonable home:
  `gain_map.rs`, `tone_map.rs`, `transfer.rs`). It's the spec for the WGSL, it's what unit tests exercise, and the CPU
  SDR fallback for PQ/HLG (section 5) runs it for real. The GPU parity test (section 8) keeps the two in step.

### 3. Headroom: decide HDR once per display, track brightness live

Two different numbers, and conflating them would re-decode on every brightness tweak:

- **Potential headroom** answers "can this display show HDR at all?". It's static per display and display mode. It
  decides `hdr_active` for decoding and `want_edr_surface()`. Crossing this boundary (window onto an SDR monitor,
  Windows HDR mode toggled, the setting flipped) still clears the cache and re-decodes, as today.
- **Current headroom** answers "how much brighter than SDR white right now?". On macOS it moves with brightness. It
  feeds only the group 1 uniform. Changing it never clears the cache and never re-decodes.

Work for M2:

1. **Find out what `Renderer::display_hdr_headroom()` returns today** (wgpu's `tone_map_headroom()`) on macOS: current
   or potential? On macOS, `NSScreen.maximumExtendedDynamicRangeColorComponentValue` is current and can read 1.0 until a
   layer actually requests EDR; `maximumPotentialExtendedDynamicRangeColorComponentValue` is the static ceiling. If wgpu
   only exposes one, read the other straight from `NSScreen` next to the existing macOS display code. On Windows,
   potential is "this output is in HDR mode" (DXGI color space); current is today's panel peak over SDR white level.
   Write the findings into "Findings" at the bottom of this spec.
2. **Split the API:** `display_hdr_capable() -> bool` (potential > 1.0) and `display_hdr_headroom_now() -> f32`.
3. **Stop clearing the cache on a magnitude-only change.** `App::refresh_edr_headroom()` keeps its re-decode only for
   the capable/not-capable flip. Safe for RAW: its decode reads the headroom only as the `> 1.0` gate.
4. **Poll the current headroom** while `want_edr_surface()` is true and the window is visible: every 500 ms, through the
   existing `ControlFlow::WaitUntil` deadline machinery in `app.rs` (the slideshow timer uses it), on the main thread.
   If it moved by more than 1%, write group 1 and request a redraw. No polling at all while an SDR image is shown
   (principle 2: no idle cost). Neither platform sends a notification for a brightness or SDR-white-level change that we
   could use instead; if you find one, prefer it and note it in "Findings".

### 4. Gain-map JPEGs (mode 2)

**Decode, when `hdr_active` is false (SDR display, HDR off, Linux):** exactly today's path. Don't look for the map,
don't decode it. Zero cost over today. This is also the path the clipboard and print re-decodes take (they pass headroom
`1.0`).

**Decode, when `hdr_active` is true:**

1. `zune-jpeg` decodes the primary image as today. After `decode_headers`, its `info()` exposes
   `multi_picture_information` (the MPF APP2 payload) and `multi_picture_information_offset`, and `xmp()` returns the
   primary's XMP.
2. **Find and parse the metadata**, first match wins:
   - ISO 21496-1: an APP2 segment with the `urn:iso:std:iso:ts:21496:-1` identifier, in the primary or the gain-map
     image. Parse with `ultrahdr_core::parse_iso21496_fmt`.
   - Ultra HDR v1 XMP: the `hdrgm:` namespace (`hdrgm:Version`, `GainMapMin`, `GainMapMax`, `Gamma`, `OffsetSDR`,
     `OffsetHDR`, `HDRCapacityMin`, `HDRCapacityMax`, `BaseRenditionIsHDR`), carried in the gain-map image's XMP, with
     the primary's XMP pointing at it through a `GContainer` directory. **Check whether `ultrahdr-core` (or `zencodec`,
     MIT/Apache) still parses XMP**: its README says `ultrahdr_core::metadata` is moving into `zenjpeg::container`,
     which is AGPL and off limits. If no permissively licensed parser is left, write our own: the schema is a dozen
     attributes, each either a scalar or a per-channel `rdf:Seq`. Values are log2 for min/max and capacity.
   - Apple: `ultrahdr_core::parse_exif_for_apple_hdr` on the primary's EXIF, then `ultrahdr_core::from_apple_headroom` →
     `GainMapMetadata`. The map is the MPF image whose XMP carries the Apple auxiliary type
     `urn:com:apple:photo:2020:aux:hdrgainmap`.
   - None of the above: it's a plain SDR JPEG. Take today's path.
3. **Find the gain-map image** through MPF: a small TIFF IFD listing each image's size and offset. Offsets are relative
   to the start of the MPF TIFF header (the byte right after `MPF\0` in the APP2 segment), except the first image's,
   which is 0. Write a small parser with tests if no permissive crate has one (the `MpfParse` error in `ultrahdr-core`
   suggests it might). Bounds-check every offset against the file length: these are attacker-controlled bytes.
4. **Decode the gain map** with `zune-jpeg` into a single-channel or RGB buffer, then store it as RGBA8 (replicate a
   single channel). It's usually a quarter of the base's width and height, so this costs about 1/16 of the base's
   memory.
   - ❌ **Never color-manage the gain map.** It's data, not color. Its ICC profile, if any, describes something else.
   - ❌ **The map must follow the base's EXIF orientation.** `apply_orientation` rotates the base on the CPU; rotate the
     map the same way, or the gain lands on the wrong pixels. A test with an orientation-6 fixture anchors this.
5. **Color-manage the base into the working space:** `moxcms` from the base's ICC profile (sRGB if none) into **Display
   P3** (`ColorProfile::new_display_p3()`), 8-bit, stored `Rgba8`. Display P3 uses the sRGB transfer curve, so the
   `Rgba8UnormSrgb` texture hands the shader linear Display P3 for free. P3 is a superset of sRGB and is what iPhones
   shoot, so real gain-map bases don't clip. A base wider than P3 would clip; log it at `debug`.
   `transform_is_negligible` already makes a P3-tagged base free.
6. **Build `GainMapPayload`:** the map (RGBA8, its own width and height), the parsed `GainMapMetadata`, and the 3×3
   matrices from linear Display P3 to the gain application space and from there to `HdrDisplaySpace::for_host()`.
   - The application space is the base's primaries (taken from the base ICC's colorants when it's a matrix-shaper
     profile), unless ISO metadata says to use the alternate's. Fallback for a LUT-based base profile: Display P3, and a
     `debug` log.
   - Why bother when most maps are single-channel: a single-channel gain scales R, G, and B by the same factor, which
     commutes with any linear matrix, and equal per-channel offsets commute with any white-preserving matrix. So for the
     common case the space doesn't matter. For three-channel maps and unequal offsets it does, and two 3×3 multiplies
     per pixel cost nothing on a GPU.

**Shader, mode 2,** per fragment:

```
sdr      = textureSample(base)                   // linear Display P3 (sRGB texture decode)
x        = M_p3_to_app * sdr
g        = textureSample(gain_map)               // Rgba8Unorm, NOT Srgb: raw map values
w        = weight(headroom_now, metadata)        // see below
for each channel c:
  log_recovery = pow(g[c], 1 / gamma[c])
  log_boost    = mix(log2_min[c], log2_max[c], log_recovery)
  y[c]         = (x[c] + offset_sdr[c]) * exp2(log_boost * w) - offset_hdr[c]
out      = M_app_to_display * y                  // linear HdrDisplaySpace, may exceed 1.0
```

- **Weight:** `w = clamp((log2(headroom_now) - log2_capacity_min) / (log2_capacity_max - log2_capacity_min), 0, 1)`, and
  when the base rendition is the HDR one (ISO 21496-1 allows it), the gain runs the other way. Port the logic of
  `ultrahdr_core::gainmap::calculate_weight` rather than re-deriving it, and unit-test the Rust reference against it
  across headrooms 1.0, 1.5, 2, 4, 8, and 16, for both directions.
- **Port the per-pixel formula from `ultrahdr-core`'s kernel** (`gainmap::apply_gain_row_scalar` and friends), not from
  this sketch, so the edge cases (gamma 1.0, zero offsets, `min == max`) match the reference implementation exactly.
- **Sampling:** the map texture uses the same bilinear sampler as the base, at the same normalized coordinates. Linear
  filtering of the encoded map values is what libultrahdr does too.
- **At `w == 0`** the output is linear Display P3 of the base, which the EDR compositor color-manages correctly. That's
  the "HDR display, but headroom happens to be 1.0 right now" case, and it should look like the SDR rendition.

### 5. Scene-linear HDR: PQ/HLG PNGs and float TIFFs (mode 3)

**PNG decode.** Decode PNGs through the `png` crate directly (it's already in the tree, 0.18.1, as `image`'s backend):
`image` 0.25.10 doesn't pass the `cICP` chunk on, and `png` parses it into `Info::coding_independent_code_points`
(`color_primaries`, `transfer_function`, `matrix_coefficients`, `is_video_full_range_image`), alongside
`mastering_display_color_volume` and `content_light_level`.

- Per PNG third edition, `cICP` takes precedence over `iCCP`, `sRGB`, and `gAMA`.
- `transfer_function` 16 (PQ) or 18 (HLG) → scene-linear path. `matrix_coefficients` must be 0 (RGB); anything else is
  invalid for PNG, so log it and fall back to the SDR path.
- Any other `cICP` (for example, 1 or 13 with primaries 12 = Display P3): SDR, but honor it. Build the source profile
  with `moxcms::ColorProfile::new_from_cicp` and take the normal SDR path. That fixes SDR `cICP` PNGs for free.
- Narrow-range PNGs (`is_video_full_range_image == false`) are vanishingly rare. Scale them to full range, and test it.
- Keep `image` for everything else (APNG frames, palettes, and so on). Only the `cICP` probe and the 16-bit HDR decode
  go through `png`.

**Converting to linear light relative to SDR white (1.0 = 203 nits, BT.2408 reference white):**

- **PQ:** SMPTE ST 2084 EOTF → absolute nits → divide by 203.
- **HLG:** BT.2100 inverse OETF → scene light, then the OOTF with system gamma 1.2 for a 1,000-nit nominal peak →
  display nits → divide by 203. HLG's 75% signal is reference white, so it lands at about 1.0.
- Reference values for tests: PQ code 0.5806 ≈ 203 nits, 0.7518 ≈ 1,000 nits, 1.0 = 10,000 nits.

**Float TIFF decode:** `image`'s TIFF decoder yields `Rgb32F` / `Rgba32F`. Treat the samples as linear with 1.0 = SDR
white. Primaries come from the ICC profile's colorants if it's a matrix-shaper profile, and BT.709 otherwise. If the
measured peak is ≤ 1.0, the file is SDR in a float container: send it down the 16-bit SDR path (M1) rather than mode 3.

**Measure the content peak during decode:** a 1,024-bin log2 luminance histogram over the decoded pixels, taking the
99.9th percentile as `content_peak`. One parallel pass, a few milliseconds. Prefer it over `cLLI`'s MaxCLL, which is
often missing or wrong; log both at `debug`.

**Store:** linear light in the source primaries, `Rgba16F`,
`PixelEncoding::SceneLinear { source_to_display: Mat3, content_peak: f32, luminance_weights: [f32; 3] }`. The 3×3 goes
from source primaries to `HdrDisplaySpace::for_host()`, both D65 (Bradford-adapt if a source white isn't D65).

**Shader, mode 3:** `rgb = source_to_display * sample`, then tone-map luminance from `content_peak` down to
`headroom_now`, then scale RGB by `Y_out / Y_in` (hue-preserving, like the RAW tone curve).

- **Curve:** the BT.2390 EETF (the Hermite-spline knee that BT.2408 Annex 5 uses), applied to luminance in the PQ
  domain, with source max = `content_peak × 203` nits, target max = `headroom_now × 203` nits, and black lift off. When
  `content_peak <= headroom_now` it's the identity, so nothing is touched that fits.
- Unit-test the Rust reference for: identity below the knee, monotonic, continuous at the knee, maps `content_peak`
  exactly to `headroom_now`, identity when the content fits.

**SDR fallback (`hdr_active` false):** run the same Rust tone map on the CPU with a target headroom of 1.0, convert from
linear source primaries into the display ICC with `moxcms` (f32 transform, then quantize), and store `DisplaySdr` RGBA8.
That gives SDR displays, Linux, copy, and print a sensible rendition rather than the grey mess of today. `gainforge`
(BSD-3/Apache, by the `moxcms` author) has a ready-made BT.2408 tone mapper on top of `moxcms`; evaluate it for this CPU
path before writing our own, but the WGSL still needs our own port, so the Rust reference has to exist either way.

### 6. 16-bit SDR PNGs and TIFFs (M1, independent of the rest)

- In `decoding/generic.rs`: when the decoded `DynamicImage` has 16 bits per channel, use `to_rgba16()` instead of
  `to_rgba8()`, and run the ICC transform with `moxcms`'s 16-bit executor (`create_transform_16bit` /
  `create_in_place_transform_16bit`; extend `color/transform.rs` with a 16-bit sibling of `transform_icc`, keeping the
  byte-equality and `transform_is_negligible` skips).
- Store: apply the sRGB EOTF to each display-encoded value, quantize to `f16`, `Rgba16F` storage,
  `PixelEncoding::DisplaySdr`. The existing passthrough shader and `Bgra8UnormSrgb` surface then produce the right
  bytes. f16 keeps about 11 significant bits, densest near black, which is where the EOTF puts the most values.
- Copy/print (`platform/windows/ui_common.rs`, and the macOS equivalent) expect `Rgba8`. Give `load_image` a way to ask
  for 8-bit output (a parameter, or a small `DecodeTarget` struct if the argument list gets worse; it's already
  `#[allow(clippy::too_many_arguments)]`), and use it from those callers.
- The histogram's `Rgba16F` branch clamps at 1.0, which is right for `DisplaySdr` f16. It must read the linearized
  values back through the sRGB OETF, though, or the histogram skews dark. Test it.
- 8-bit PNGs and every other format must not change at all: same bytes, same speed.

### 7. Everything else that touches pixels

- **Histogram:** `GainMapped` → the SDR base's histogram (state it in `histogram/CLAUDE.md`). `SceneLinear` → the
  existing clamp-at-1.0 `Rgba16F` histogram of the untone-mapped values is acceptable for now; note it as a known
  simplification.
- **Screenshot readback (`Renderer::capture_screenshot`):** renders through an SDR offscreen target clamped at display
  white. Keep that, and make modes 2 and 3 render with `headroom_now = 1.0` there, so screenshots show the SDR rendition
  instead of clipped HDR. The RAW HDR path has the same issue today; leave it alone unless the fix is free.
- **Slideshow crossfade:** read how a crossfade between an HDR RAW and an SDR image behaves today (which surface format
  is active during the fade?). Write the answer into "Findings". The rule this spec needs: while either image is HDR,
  the surface is in EDR mode, and a `DisplaySdr` image drawn on the EDR surface gets converted to linear display space
  in the shader (it's display-ICC-encoded, so that's exact only when the display profile is Display P3, which is the
  MacBook case). If today's behavior differs, document it and keep it rather than widening the scope.
- **Preview placeholders, browse-grid thumbnails:** come from QuickLook / the shell cache and stay SDR. No change.
- **Preload cache:** `set_hdr_mode` doubles the budget in HDR mode. Gain-map images now cost about what SDR images do,
  and `image_memory_cost` counts real bytes per image, so keep the mechanism and just make sure the map's bytes are
  counted.

### 8. Testing

TDD per the house rule: write each test first and see it fail for the right reason.

- **Unit, any host (`cargo test`):**
  - MPF parser: valid, truncated, offsets past the end of the file, zero images.
  - XMP `hdrgm` parser (if ours): scalar and per-channel forms, missing optional fields with their spec defaults, and a
    `GContainer` directory.
  - ISO 21496-1 and Apple parsing through `ultrahdr-core`, on fixture bytes.
  - `cICP` → path selection, PQ/HLG EOTF reference values, the narrow-range case.
  - Tone-map curve properties (section 5) and the weight formula against `calculate_weight` (section 4).
  - Rust gain-map reference against `ultrahdr_core::gainmap::apply_gainmap` on a fixture, mean relative error < 0.5%.
  - Gain map rotated with the base for every EXIF orientation 1–8.
  - 16-bit path: a 16-bit gradient through a non-identity transform keeps more than 256 distinct output levels; the same
    image as 8-bit is byte-identical to today.
  - `PixelEncoding` constructors reject illegal storage pairs.
- **GPU parity test:** a headless wgpu device renders a small strip of known inputs through the real image pipeline into
  an `Rgba16Float` offscreen target at a forced headroom, reads it back, and compares with the Rust reference (relative
  error < 1%, absolute < 1e-3 near black). Factor what's needed out of `Renderer` so this doesn't need a window. macOS
  and Windows CI have a GPU adapter and must run it. If Linux CI has none, gate it off there with an explicit `cfg` and
  a comment, never a silent skip on a missing adapter.
- **E2E (`tests/e2e_shared.rs`, through the QA server):**
  - Add to `/state`: the current image's `PixelEncoding` kind, `hdr_capable`, and `headroom_now`.
  - Add a QA override for the current headroom (an endpoint or a `PRVW_TEST_HEADROOM` env var), since CI displays have
    none.
  - Open each fixture and assert the encoding kind and that the surface mode follows `want_edr_surface()`.
  - **The no-re-decode promise:** with a gain-map image open, change the overridden current headroom three times and
    assert `full_decodes` didn't move. Then flip HDR capability and assert it did.
- **Fixtures (`apps/desktop/tests/fixtures/hdr/`), small and license-clean:** generate them with a dev example
  (`cargo run --example make-hdr-fixtures`, using `image`'s JPEG and `png`'s encoders, both already dependencies) and
  commit the output: Ultra HDR XMP JPEG 64×48 (single-channel map), ISO 21496-1 JPEG 64×48 (three-channel map), the
  Ultra HDR one again with EXIF orientation 6, PQ BT.2020 16-bit PNG, HLG 16-bit PNG, linear float TIFF with a peak of
  4.0, 16-bit Display P3 gradient PNG.
  - Self-made fixtures risk symmetric bugs (the writer and the parser sharing a misunderstanding), so also validate
    against real files locally, without committing them: David's Pixel photos
    `~/Downloads/takeout-unique-samples/photo-27 [Photos from 2025] PXL_20250219_153250675.jpg` and
    `photo-28 [...] PXL_20251008_104424078.jpg` (both Ultra HDR v1 XMP with MPF, verified 2026-10-13; these are personal
    photos, ❌ never commit them).
  - **iPhone samples** (original camera JPEGs on Wikimedia Commons, which keeps uploads byte for byte; gain maps
    confirmed with ImageIO's `CGImageSourceCopyAuxiliaryDataInfoAtIndex` on 2026-10-13). Download them locally for the
    real-file check. Only the iPhone Air one is committed (David agreed, 2026-10-13); don't commit the others.
    - Apple flavor only (maker-note headroom, `urn:com:apple:photo:2020:aux:hdrgainmap` map at half resolution, no ISO
      metadata), iPhone 14 Plus, 4032×3024, 3.0 MB, CC BY-SA 4.0:
      https://commons.wikimedia.org/wiki/File:About_iPhone_15_Pro_Max_Natural_Titanium.jpg (file:
      https://upload.wikimedia.org/wikipedia/commons/2/23/About_iPhone_15_Pro_Max_Natural_Titanium.jpg)
    - Apple flavor only, a second one from the same phone, CC BY-SA 4.0:
      https://commons.wikimedia.org/wiki/File:Back_view_of_iPhone_14_Pro_Max_Deep_Purple.jpg (file:
      https://upload.wikimedia.org/wikipedia/commons/c/cb/Back_view_of_iPhone_14_Pro_Max_Deep_Purple.jpg)
    - Apple and ISO 21496-1 maps both present (iOS 18+ writes both), iPhone 16e, 4284×5712 portrait, 3.9 MB, CC BY 4.0:
      https://commons.wikimedia.org/wiki/File:IPhone_15_pro_model.jpg (file:
      https://upload.wikimedia.org/wikipedia/commons/e/e3/IPhone_15_pro_model.jpg)
    - Apple and ISO 21496-1, iPhone Air, 776×776, 306 KB, CC BY-SA 4.0. **Committed** as
      `apps/desktop/tests/fixtures/hdr/iphone-air-gain-map-776.jpg`, with its attribution in `licenses.md` next to it.
      Use it in the M3 tests: it's the one real-world gain-map file CI can see. Source:
      https://commons.wikimedia.org/wiki/File:IPhone_16_Pro_Super_Retina_XDR_OLED_Display_(100X_Mag.).jpg
    - The file names name the phone in the picture, not the camera: trust the EXIF `Model` above.
    - Both flavors in one file is the case to get right: prefer the ISO 21496-1 metadata when present (section 4's
      order), and check the result against the Apple-only reading of the same file.
  - libultrahdr's repo has small test images under an Apache-2.0 license. If one is small enough (< 100 KB), committing
    it with an entry in `apps/desktop/tests/fixtures/hdr/licenses.md` is a good second, independent source.
  - Optional macOS oracle: ImageIO decodes gain-map JPEGs to HDR (`kCGImageSourceDecodeRequest` =
    `kCGImageSourceDecodeToHDR`, macOS 15+). A throwaway Swift script reading a few pixel values gives an independent
    reference for the real-file check. Don't add it to the repo.

### 9. Performance budget

Measure release builds on the M3 Max, log stage timings at `debug` the way `raw.rs` does, and write the numbers into
"Findings".

- **SDR images: no regression.** An 8-bit JPEG, PNG, or TIFF decodes within noise of today (±3%). The gain-map probe is
  a scan of APP segments `zune-jpeg` has already parsed; the `cICP` probe reads PNG header chunks before `IDAT`.
- **Gain-map JPEG, 12 MP, HDR display:** at most 1.3× today's decode of the same file (the map decode plus metadata). No
  per-pixel CPU work beyond what SDR does today. Memory within 10% of the SDR decode.
- **Gain-map JPEG on an SDR display:** identical to today (the map is never read).
- **PQ/HLG PNG, 12 MP:** the 16-bit PNG decode dominates; our added CPU work (EOTF, peak histogram, f16 pack) under 25
  ms in total.
- **Headroom change:** under one frame, no decode, cache untouched.
- **GPU cost:** two extra texture samples, two 3×3 multiplies, and a few `exp2`/`pow` per fragment, and only when
  rendering, which is on demand. Not worth measuring unless a frame visibly drops.

## Milestones

One commit per meaningful step, each green on `./scripts/check.sh` plus
`./scripts/check.sh --check windows-cross --check linux-cross` (the Windows and Linux builds must compile at every step;
Linux simply never activates HDR).

- **M0: Groundwork.**
  - Add `ultrahdr-core` (see "Dependencies").
  - Make sure a license check catches copyleft crates. imazen publishes several useful crates as
    `AGPL-3.0-only OR LicenseRef-Imazen-Commercial`, and one default feature is enough to pull them in. The repo's
    `cargo-deny` check (`deny.toml`) does this (see "Findings").
  - Investigate and fill in "Findings": headroom (potential vs current, per platform), crossfade surface behavior,
    whether `ultrahdr-core` or `zencodec` still parse XMP and MPF, whether `png` 0.18.1's encoder writes `cICP`.
- **M1: 16-bit SDR PNG/TIFF** (section 6). Introduces `PixelEncoding` with only `DisplaySdr` and `LinearHdrDisplay`, so
  `is_hdr()` stops meaning "is f16".
- **M2: Renderer plumbing, no visible change.** Group 1 uniform, image params uniform, gain-map texture slot, shader
  modes 0 and 1, the GPU parity harness, the potential/current headroom split, `refresh_edr_headroom` no longer clearing
  the cache on a magnitude-only change, headroom polling, and the `/state` fields plus the QA headroom override.
- **M3: Gain-map JPEGs** (section 4), Ultra HDR XMP first, then ISO 21496-1, then Apple.
- **M4: PQ/HLG PNGs and float TIFFs** (section 5), with the CPU SDR fallback.
- **M5: The setting** (decision 1(a): move it to Settings → Color as "HDR output"), docs, and the hand-off to David for
  QA.

## Dependencies

Verify versions on crates.io at implementation time and pin one at least three days old (user rule). As of 2026-10-13:

- **`ultrahdr-core` 0.6.0** (Apache-2.0, `imazen/ultrahdr`, actively maintained, pre-1.0 with renames between minors):
  `ultrahdr-core = { version = "=0.6.0", default-features = false, features = ["std"] }`.
  - ❌ **Never enable its `tonemap` or `resize` features**: they pull in `zentone` and `zenresize`, both AGPL. Its
    `default` includes `tonemap`, hence `default-features = false`.
  - Its required dependencies (`zencodec`, `zenpixels`, `linear-srgb`, `enough`, `whereat`, `bytemuck`, `thiserror`) are
    MIT/Apache. `simd` (`archmage`, `magetypes`) and `f16` (`half`) are MIT/Apache too, and fine to enable if useful.
  - Pin exactly (`=`) and wrap it behind our own small module, so a 0.x rename touches one file.
- ❌ **`ultrahdr-rs`**: bundles `zenjpeg` (AGPL) and turns on `ultrahdr-core`'s AGPL defaults.
- **`gainforge`** (BSD-3/Apache): optional, only if it earns its place in the CPU SDR fallback for PQ/HLG.
- No new decoder crates: `zune-jpeg`, `png`, `image`, `moxcms`, and `half` are already in the tree.

## How AVIF plugs in later

The AVIF spec (to be written after this lands) gets nearly everything for free:

- SDR AVIF: `avif-decode` → our own `colr` box reader (ICC or CICP; neither crate exposes it, verified 2026-10-13) →
  `moxcms` (`new_from_cicp` for CICP) → `DisplaySdr`.
- PQ/HLG AVIF → `SceneLinear` (mode 3), unchanged.
- Gain-map AVIF (`tmap` derived item) → `GainMapped` (mode 2), with `ultrahdr-core`'s ISO 21496-1 parser.

Background research and benchmarks from the AVIF discussion: `avif-decode` 3.0.0 decodes a 12 MP AVIF in about 115 ms on
the M3 Max with `rav1d`'s assembly off (its aarch64 assembly fails to build: `memorysafety/rav1d#1449`).

## Docs to update as you go

- `apps/desktop/src/decoding/CLAUDE.md`: the new format paths, `PixelEncoding`, and the gotchas from section 4 (map is
  not color-managed, map follows orientation).
- `apps/desktop/src/render/CLAUDE.md`: the two uniform groups, the modes, the parity harness.
- `apps/desktop/src/color/CLAUDE.md`: the `color/hdr/` module, potential vs current headroom, the working-space choice
  for gain-map bases.
- `apps/desktop/src/histogram/CLAUDE.md`, `apps/desktop/src/navigation/CLAUDE.md` (cache), `docs/architecture.md`.
- Settings and parity docs in M5.

## Manual QA for David

On the MacBook Pro's XDR display, brightness well below full (the headroom is what's left above the current brightness),
Preview open side by side. Run a release build from the worktree: `cd apps/desktop` then
`cargo run --release -- <file or folder>`. Settings → Color → "HDR output" should be on (it is by default, and your
existing RAW setting carries over).

**Files.**

- Pixel (Ultra HDR): `~/Downloads/takeout-unique-samples/photo-27 [Photos from 2025] PXL_20250219_153250675.jpg` and
  `photo-28 [Photos from 2025] PXL_20251008_104424078.jpg` in the same folder.
- iPhone, both flavors in one file: `apps/desktop/tests/fixtures/hdr/iphone-air-gain-map-776.jpg` (in the repo).
- iPhone, Apple's older flavor only: download the two iPhone 14 Plus files listed in section 8 ("About iPhone 15 Pro
  Max…" and "Back view of iPhone 14 Pro Max…", Wikimedia Commons). Don't commit them.
- PQ, HLG, and float: `apps/desktop/tests/fixtures/hdr/pq-bt2020-32x16.png`, `hlg-bt2020-32x16.png`,
  `float-linear-peak-4-32x16.tif`, and `float-linear-sdr-32x16.tif` (all in the repo, 32×16, so zoom in). They come from
  `cd apps/desktop && cargo run --example make-hdr-fixtures`, which rewrites every fixture in that folder; only run it
  if you want fresh ones (it also rewrites `iso-21496-64x48.jpg`'s timestamp, so discard that change). We have no real
  PQ or HLG file, so any you find is worth a look too.
- Any RAW of yours.

**Steps.**

1. **Gain maps against Preview.** Open the two Pixel photos, then the iPhone files. Highlights (sky, lamps, reflections)
   should glow like in Preview, and everything else should match it. The Apple-only iPhone 14 Plus files are the ones
   measured against ImageIO (0.5% off), so they should be the closest match.
2. **Brightness.** With a gain-map photo open, drag the brightness slider down and up. The highlights adapt within about
   half a second, with no flash, no "Loading…", and no re-decode (`RUST_LOG=prvw::app=debug,prvw::decoding=info` shows
   `Display headroom now: …` lines and no new `Decoded …` line).
3. **An SDR monitor.** Move the window to an external monitor with HDR off. The photo shows its normal SDR version,
   identical to the release build. Back on the MacBook, it glows again.
4. **The switch.** Turn Settings → Color → "HDR output" off. Every image renders SDR, identical to the release build
   (gain-map JPEGs as their plain JPEG, RAW as its SDR develop), except a RAW's saturated highlights: those now keep
   their color where the release build washed them pale (Findings, the RAW SDR decision). Colored lights show it best.
   On again, the glow comes back. On the external SDR monitor, toggling it changes nothing and decodes nothing.
5. **PQ and HLG.** Open the PQ and HLG PNGs. They look like a grey ramp and four colored patches, not a flat grey wash,
   and the bright end of the ramp glows. Preview may not read `cICP` on your macOS; if it shows these grey and flat,
   that's the bug Prvw used to have. Then the float TIFFs: the peak-4 one glows, the SDR one doesn't.
6. **The SDR renditions.** Repeat step 5 with HDR output off: diffuse white should sit close to full white (about 243 of
   255), not dim, and the highlights packed into the top. HLG here is the one to judge (open item 4 below).
7. **Navigation.** Step through a folder mixing SDR JPEGs, gain-map JPEGs, and RAWs: it should feel as instant as the
   release build.
8. **RAW at launch.** At full brightness, launch Prvw on a RAW (from the command line, so it's the first image). It
   should open with its highlights glowing straight away, not after a display change (`RUST_LOG=info` logs
   `Display HDR capable: false -> true` before the first `Decoded …` line). Open item 1.

**Open items to decide or check on real hardware.**

1. **The launch-time RAW gating bug from M0 should be gone.** Gating moved from the current headroom (which can read 1.0
   at launch) to the potential one, which an MBP reports as 16 whatever the brightness. Step 8 confirms it on hardware.
2. **Windows HDR has never run on a real HDR display.** The scRGB surface, the SDR-white scale (`sdr_white_nits / 80`),
   and the headroom poll are unit- and GPU-tested only. Known gap: the overlay pills and text draw through their own
   pipeline and stay unscaled on a Windows HDR surface, so they look dim next to the image there.
3. **16-bit TIFFs decode about 30% slower** (24 → 31.5 ms for 12 MP, measured under load; PNG is +7%). Almost all of it
   is `moxcms`' 16-bit transform. Worth an idle-machine rerun (`sixteen_bit_decode_perf`, `#[ignore]`) before deciding
   whether it matters.
4. **HLG's SDR rendition** uses the fixed 1,000-nit OOTF plus the tone map, so it matches the HDR one at headroom 1. If
   it looks off in step 6, the alternative is in Findings (review L2).
5. **Crossfades between an HDR and an SDR image** show the outgoing image on the wrong surface for one fade (Findings,
   M2 and M4). Watch a slideshow mixing the two and say whether it's noticeable.
6. **The settings copy** for "HDR output" is a draft: `apps/desktop/src/color/settings_panel.rs` (macOS),
   `apps/desktop/src/settings/windows/model.rs` (Windows, Color tab and the RAW tab's HDR gain line), and the RAW
   panel's HDR gain line in `apps/desktop/src/settings/panels/raw.rs`.

## Findings

Filled in by the implementing agent during M0 and later. Date each entry.

- **2026-10-13, headroom on macOS: today's number is the _current_ headroom, and wgpu already has the potential one.**
  wgpu 30's `DisplayHdrInfo::headroom` carries all three `NSScreen` values (`current` =
  `maximumExtendedDynamicRangeColorComponentValue`, `potential` = `maximumPotential...`, `reference` =
  `maximumReference...`, read in `wgpu-hal` `metal/surface.rs`), and `tone_map_headroom()` returns `current`. So
  `Renderer::display_hdr_headroom` and `App::edr_headroom` are current headroom. A scratch Swift probe on the MBP read
  current 1.0, potential 16.0, reference 0.0 (AppKit's "unknown"). With a window whose `CAMetalLayer` asked for EDR and
  drew 4.0, current still read 1.0, but the run was unattended and the display may have been asleep. An E2E launch an
  hour later logged `Display EDR headroom: 1.20` with no EDR content of ours, so an awake screen at high brightness
  reports a small current headroom, not a flat 1.0.
  - **Changes M2:** no `NSScreen` code of our own. `display_hdr_capable` reads `info.headroom.potential > 1.0` on macOS,
    and `display_hdr_headroom_now` keeps `tone_map_headroom()`. Don't use wgpu's `coarse.high_dynamic_range` on macOS:
    wgpu derives it from `current > 1.0`.
  - **Possible existing bug, worth checking in QA:** today's `edr_headroom > 1.0` gate reads current. If current ever
    reads exactly 1.0 at launch (full brightness, or Apple holding it at 1.0 until EDR content is on screen, which the
    probe above couldn't confirm or rule out), RAW decodes SDR, the surface never asks for EDR, and nothing re-reads the
    headroom until a display change. Gating on potential (M2) removes that loop either way.
- **2026-10-13, headroom on Windows.** wgpu reads `IDXGIOutput6::GetDesc1` plus `DISPLAYCONFIG_SDR_WHITE_LEVEL` on every
  call (`wgpu-hal` `auxil/dxgi/hdr.rs`; the factory is cached, the monitor lookup and adapter/output walk are not).
  `coarse.high_dynamic_range` is "the output's colour space is G2084 or G10", which is HDR mode on: that's potential.
  `tone_map_headroom()` is 1.0 in SDR mode, else `max_nits / sdr_white_nits`, which moves with the SDR brightness
  slider: that's current.
  - **Changes M2:** `display_hdr_capable` on Windows reads `coarse.high_dynamic_range == Some(true)`, not "headroom >
    1.0". A panel whose EDID peak sits at or below its SDR white would otherwise read as not capable, because
    `display_hdr_headroom` filters values under 1.0. Polling every 500 ms is fine; per frame it wouldn't be.
- **2026-10-13, what the app polls today: nothing.** `query_edr_headroom` runs at init (`app.rs` `resumed`) and from
  `refresh_edr_headroom`, which only `DisplayChanged` and a move onto another monitor reach. No brightness or SDR-white
  notification turned up in wgpu or the platform code; M2's poll stands.
- **2026-10-13, crossfade surface: the surface follows the _incoming_ image for the whole fade.**
  `App::display_from_cache` calls `prepare_display`, which sets `current_image_is_hdr` and runs
  `apply_edr_surface_state`, before `begin_crossfade`. Both bind groups share one layout, so the reconfigure doesn't
  break the fade. What it shows:
  - SDR → HDR: the outgoing SDR texture (display-ICC bytes, sRGB-decoded) lands on the linear extended Display P3
    surface as if it were linear P3. Close only when the display profile is near P3 (the MacBook case).
  - HDR → SDR: the outgoing f16 texture lands on `Bgra8UnormSrgb` with the display ICC as colourspace: highlights clip
    at 1.0 and linear P3 is read as display-encoded, for the length of one fade.
  - This differs from section 7's rule ("EDR while either image is HDR"). Per section 7, keep it and don't widen M2.
- **2026-10-13, `ultrahdr-core` 0.6.0 and `zencodec` 0.1.26 parse neither `hdrgm` XMP nor MPF.** The crate docs say both
  moved to `zenjpeg::container`, and `zenjpeg` (0.8.4) is `AGPL-3.0-only OR LicenseRef-Imazen-Commercial`.
  `Error::XmpParse` and `Error::MpfParse` are leftovers. What's left and permissive: `parse_iso21496_fmt` (from
  `zencodec`, re-exported), the Apple trio (`parse_exif_for_apple_hdr`, `parse_apple_makernote`, `from_apple_headroom`),
  and `zencodec::Metadata::xmp` as raw bytes only.
  - **Changes M3:** write our own `hdrgm` XMP parser and MPF parser, the spec's fallback. Both, not maybe.
- **2026-10-13, the `ultrahdr-core` kernel, for the M3 port.** Three things the sketch in section 4 gets wrong or leaves
  out:
  - **The per-pixel math** lives in `GainMapLut::new` (gain =
    `exp((log_min + pow(v, 1/gamma) × (log_max − log_min)) × weight)`, clamped to [2⁻³⁰, 2³⁰], non-finite → 1.0) and
    `apply_simd::apply_gain_row_presampled` (`out = (sdr + base_offset) × gain − alternate_offset`). There's an
    `apply_gain_row_scalar`, but it's a single-channel shortcut without offsets; port the two above.
  - **The map isn't filtered bilinearly on its encoded values.** It's Shepard's inverse-distance weighting over the four
    neighbouring texels, applied to the _linear gains_ (after the LUT), with top-left-aligned texels (`x / scale`, no
    half-texel offset). Hardware bilinear on encoded values, centre-aligned, differs at every texel edge, so the "mean
    relative error < 0.5% against `apply_gainmap`" test would fail on a busy map. Either fetch four texels with
    `textureLoad` and weight the gains (exact, four loads), or keep the hardware sampler, make the Rust reference mirror
    it, and compare against `apply_gainmap` only on smooth regions with a looser bound. The second is what GPU viewers
    usually do; decide in M3.
  - **`calculate_weight` returns 1.0 whenever the alternate headroom is at or below the base's**, so an HDR-base map
    (allowed by ISO 21496-1) always gets the full gain and shows the SDR rendition, and `backward_direction` is never
    read. Port it for the SDR-base direction only, and take libavif's `clamp((log2 H − base) / (alt − base), 0, 1)` for
    the HDR base (1.0 at H = 1, 0.0 at H = 2^base). Test against `calculate_weight` only for an SDR base. Tests in
    `color/hdr/mod.rs` pin the upstream behaviour, so an upgrade that changes it fails loudly.
- **2026-10-13, `png` 0.18.1's encoder doesn't write `cICP`.** The decoder fills `Info::coding_independent_code_points`,
  but `write_header` ignores that field (and the mastering/content-light ones). Writing it by hand works:
  `writer.write_chunk(png::chunk::cICP, &[primaries, transfer, matrix, full_range])` right after `write_header()`
  round-trips (verified with a scratch crate: decoded `Some(9, 16, 0, true)`).
  - **Changes M4:** the fixture generator writes `cICP` (and `cLLI`, if a fixture wants it) through `write_chunk`. `png`
    is only a transitive dependency today, so M4 adds it as a direct one at the same 0.18.1 (the decode path needs it
    anyway); the fixtures example then gets it for free.
- **2026-10-14, M1 banding, measured.** A 4,096-wide 16-bit red ramp, sRGB → Display P3, compared with moxcms' f32
  transform of the same samples: today's path (8 bits, then the transform) is up to 1.55 steps off and 48% of samples
  end a step away from single rounding. The 16-bit path is under 0.1 steps and 1.7% (moxcms' 16-bit transform sits a few
  hundredths of a step off its f32 one, which tips samples on a rounding boundary). Anchors in `decoding/generic.rs`.
- **2026-10-14, M1 decode cost.** Release build, M3 Max, 12 MP (4000×3000) RGB16 with a smooth gradient plus noise,
  median of seven; `OutputDepth::Rgba8` is exactly today's path, so one binary compares both (`sixteen_bit_decode_perf`,
  `#[ignore]`). Quietest run:
  - PNG, Display P3 target: today 81.1 ms, 16-bit 87.0 ms (+7%). sRGB target (transform skipped): 63.5 → 64.1 ms.
  - TIFF (uncompressed), Display P3 target: today 24.2 ms, 16-bit 31.5 ms (+30%, +7 ms). sRGB target: 7.3 → 7.7 ms.
  - Stage split of the 16-bit path: `into_rgba16` 2.2 ms, the 16-bit ICC transform 23 ms, the EOTF into f16 1.7 ms
    (rayon). The added cost is almost all moxcms' 16-bit transform being slower than its 8-bit one.
  - Two reruns ran at a load average of ~50 from other sessions and scattered from −9% to +85%, so take the quietest run
    as the number and rerun on an idle machine before quoting it anywhere. Section 9's ±3% is for 8-bit sources, whose
    code path is unchanged (one `matches!` added); `an_8_bit_png_is_byte_identical_to_todays_path` pins the bytes.
  - Memory: a 16-bit image now costs 8 B/px in the cache and on the GPU, double today's. `image_memory_cost` already
    counts real bytes, so the budget holds; fewer 16-bit images fit in it.
- **2026-10-14, M1 deviations from the design.**
  - No fallible `DecodedImage::new`. One constructor per legal pair (`from_rgba8`, `from_display_sdr_rgba16f`,
    `from_linear_hdr_rgba16f`, the last renamed from `from_rgba16f`) makes illegal pairs unbuildable, and `finalize`
    `debug_assert!`s `PixelEncoding::accepts`, since the fields stay public. A fallible constructor nothing called would
    have been dead code. M3 and M4 can add one if `GainMapped` wants it.
  - `load_image` gained an `OutputDepth` parameter (`Native` / `Rgba8`). `Rgba8` also pins the headroom to 1.0, so a
    byte consumer can't get HDR whatever it passes. Besides Windows copy and print, `previews/generator.rs` needed it
    too: it requires `Rgba8` and would have dropped every 16-bit preview.
  - `histogram::compute::compute` takes the encoding next to the buffer.
  - The gradient fixtures are generated in-test (`image`'s PNG and TIFF encoders), not by `make-hdr-fixtures`: unit
    tests want a 4,096-wide ramp, which isn't worth committing. M3 starts the example.
  - Float TIFFs stay on the 8-bit path; M4 owns them.
- **For M2.** `PixelEncoding` has `is_hdr()` and `accepts()`, both exhaustive matches, so the new variants surface every
  site. The renderer still picks the texture format from storage only; mode 0 covers `DisplaySdr` in both storages with
  no shader change. A `DisplaySdr` f16 image on the EDR surface (only reachable mid-crossfade) shows linearized display
  values as linear P3, like an 8-bit SDR one does today.
- **2026-10-14, license check (M0): `cargo-deny` covers it, so there's no HDR-specific guard.** `Cargo.lock` lists the
  AGPL `zentone`, because `ultrahdr-core`'s `std` feature names it weakly (`zentone?/std`), though cargo never builds
  it. `cargo-deny` resolves features the way the build does: it passes with `features = ["std"]`, and with `"tonemap"`
  added it fails on `zentone`'s `AGPL-3.0-only OR LicenseRef-Imazen-Commercial` (both verified 2026-10-14).
- **2026-10-14, M2: what the two headrooms read in practice.** Debug build on the MBP's XDR panel, a RAW on screen with
  the EDR layer on, polled every 500 ms for eight seconds: `potential` 16.0 and `current` 1.2 on every read, `reference`
  `None`, and wgpu's `coarse.high_dynamic_range` `Some(true)` (derived from `current > 1.0`). An SDR JPEG made one read
  at launch and none after, so the poll costs nothing while no HDR image is up. The session ran unattended, so nobody
  moved the brightness slider; David's QA step 2 is the first real test of the live update.
- **2026-10-14, M2 deviations from the design.**
  - One query, not two: `Renderer::display_hdr() -> DisplayHdr { capable, headroom_now }`, because on Windows every
    `display_hdr_info` call walks the adapter's outputs, and the two numbers should come from the same read. The
    per-platform reading is the pure `DisplayHdr::from_info`, tested from any host.
  - Decoders take `hdr_display: bool` (the capability) in place of `edr_headroom: f32`. The magnitude was only ever a
    `> 1.0` gate, and Windows' capability isn't a number at all.
  - The QA override is `POST /display-hdr` (`{"capable", "headroom_now"}`, both optional), not an env var: the E2E test
    has to change the headroom three times in one run. `/state` also reports `want_edr_surface`, `edr_surface`, and
    `hdr_surface_supported`, which is what "the surface follows `want_edr_surface()`" needs to be asserted on a GPU that
    can't present EDR.
  - `EncodingKind` is the payload-free tag of `PixelEncoding` (identical until M3), and `App` tracks it for the image on
    screen in place of `current_image_is_hdr`. `PixelEncoding::kind()`, `EncodingKind::is_hdr()`, `shader_mode()`, and
    `name()` are exhaustive matches.
  - `ImageParams` holds only `mode` for now; M3 appends its fields. The Rust reference for modes 0 and 1 is
    `color::hdr::fragment::shade`, where M3's mode 2 and M4's mode 3 go too.
  - Fixed on the way: a display change or a monitor move that flipped HDR without changing the ICC profile cleared the
    cache but left the old decode on screen. It now re-decodes the image on screen (`App::redisplay_current`), and
    `apply_icc_settings` reports whether it already did, so nothing decodes twice. The launch loop from the 2026-10-13
    finding ("current reads 1.0, RAW decodes SDR") is gone with the gate on potential.
- **2026-10-14, M2 for M3: the layout mode 2 plugs into.**
  - Group 0: 0 transform (`TransformUniform`, vertex + fragment), 1 image texture, 2 the shared linear sampler, 3 gain
    map (`texture_2d<f32>`, filterable; today a shared 1×1 `Rgba8Unorm` placeholder), 4 `ImageParams` uniform
    (`{ mode: u32, _pad: [u32; 3] }`, one 16-byte buffer per image, created in `ImagePipeline::upload`). Group 1: 0
    `FrameUniform { headroom_now: f32, _pad: [f32; 3] }`, one buffer, written by `Renderer::set_headroom_now`.
  - Mode 2: add `ShaderMode::GainMap = 2` and its `shade` branch in `color/hdr/fragment.rs`, a `case 2u` in
    `shader.wgsl`'s `shade` (it already receives `uv`), and the gain-map fields at the end of both `ImageParams` structs
    (vec4-aligned: per-channel log2 min/max, gamma, offsets, capacity; two 3×3 matrices as three `vec4`s each). Give
    `GpuImage` an `Option<wgpu::Texture>` for the map, created in `upload` from the `GainMapped` payload and destroyed
    in `GpuImage::destroy`, and bind it at 3 in `ImagePipeline::bind` in place of the placeholder. The crossfade carries
    the whole `GpuImage`, so the outgoing image keeps its map and mode with no further change.
  - A parity case: build a one-row `DecodedImage` (`strip_rgba8` / `strip_f16` in `image_pipeline.rs`'s `gpu_parity`, or
    a new helper that also builds the map), and call `assert_matches_reference(&image, &[1.0, 2.0, 8.0])`. The harness
    computes the texture sample per storage and runs `shade`; a mode that samples the map needs the reference to sample
    it the same way (the Shepard-vs-bilinear question from the M0 kernel finding). The tests are verified to catch a
    clip: a mode-1 shader clamped at 1.0 fails with `GPU 1 vs reference 2`.
  - Screenshots already bind a group 1 pinned at headroom 1.0, so mode 2 screenshots as its SDR rendition for free.
- **2026-10-14, M3: Apple's "ISO 21496-1" gain map is XMP, not an APP2 segment.** The iPhone Air fixture has no ISO APP2
  at all. Its one 388×388 single-channel map, appended through MPF and tagged `urn:com:apple:photo:2020:aux:hdrgainmap`,
  carries ISO 21496-1's fields as XMP in Adobe's `hdrgm` namespace, bound to the prefix `HDRToneMap`, with ISO's names
  (`BaseHeadroom`, `AlternateHeadroom`, not Ultra HDR's `HDRCapacityMin/Max`) and `0`/`1` booleans. That's what ImageIO
  reports as the ISO gain map. Our XMP parser reads both dialects and labels this one `Iso21496`, so the precedence is:
  an ISO APP2 on the map, then this XMP form, then full ISO parameters on the primary, then Ultra HDR XMP, then Apple's
  maker note. On this file the ISO reading says 1.77 stops and the maker note's fitted formula 2.08; the test checks
  they agree within half a stop.
  - The Pixel photos (checked locally, not committed) are textbook Ultra HDR: MPF plus `GContainer`, primary XMP with
    `hdrgm:Version` only, map XMP with the parameters as attributes, single-channel maps at 816×614 for an 8160×6144
    base (a non-integer 10.006 ratio vertically) and 768×1020 for 3072×4080. Both found and decoded.
- **2026-10-14, M3: map sampling, decided: exact Shepard with four `textureLoad`s.** Plain bilinear on linear gains (let
  alone hardware bilinear on encoded values) misses `ultrahdr_core::gainmap::apply_gainmap` by 2.5% mean and 82% max on
  a busy map, five times the spec's 0.5% bound, and the GPU parity test catches the hardware-sampler version at a texel
  edge (4% off). The exact port costs four loads and a dozen `pow`/`exp2` per fragment, on a viewer that renders on
  demand. Against upstream, the port is 3e-5 mean and 5e-4 max on a busy synthetic map, and 6e-4 mean on the real iPhone
  file (its dark, noisy channels sit near the 1e-3 floor of the relative-error measure).
  - **Shepard's weighting jumps at a cell edge**, so the coordinate has to land on the right side of it. At widths that
    aren't powers of two, `(x + 0.5) / w × w − 0.5` rounds just under `x`, and a GPU's division isn't correctly rounded;
    together they put 23% errors on the iPhone file. A fragment within 1/64 px of a pixel centre now counts as that
    pixel, with its map cell from integer arithmetic, exactly upstream's `x × map / image`. Power-of-two test sizes hid
    this; the busy-map and parity tests now include a 776-wide case.
  - **A near-identity matrix isn't harmless.** Apple's Display P3 profile and `moxcms`' differ in their colorants' last
    digits (even P3 to P3 through `moxcms` comes out 1e-5 off the identity), and the leak between channels showed as
    error on dark channels, one going negative. `GainMapSpaces` snaps any conversion within 1e-3 of the identity to the
    exact identity, so P3 bases (iPhone, Pixel) go through mode 2 with no matrix at all on macOS.
- **2026-10-14, M3 cost, release, M3 Max** (`gain_map_decode_perf`, `#[ignore]`, median of seven, load average 5–8):
  - 12.5 MP (Pixel, 3072×4080): SDR 33.8 ms, gain map 34.8 ms (1.03×) with a P3 display target; 50.5 against 34.0 ms
    (0.67×) with an sRGB one, since the gain-map path's P3 transform is free for a P3 base and the display's isn't.
    Memory 50.1 MB against 53.3 MB (+6.2%, the map's sixteenth).
  - 50 MP (Pixel, 8160×6144): 88.6 against 89.2 ms (1.01×) with P3, 156.8 against 88.8 ms (0.57×) with sRGB; memory
    +1.0% (that map is a hundredth).
  - An SDR display, HDR off, and the byte consumers take the old path: the map is never looked for, and
    `a_gain_map_jpeg_without_hdr_is_its_plain_primary` pins the bytes.
- **2026-10-14, M3 deviations from the design.**
  - **The ISO parser is upstream's, the rest is ours.** `ultrahdr-core` wraps only ISO 21496-1 APP2 and the Apple maker
    note (`color::hdr`); MPF and `hdrgm` XMP are our own (`decoding::gain_map`), as the M0 finding said. Both of ours
    are fuzzed with truncations and byte flips and never panic. With no MPF, the `GContainer` directory's lengths locate
    the map from the end of the file.
  - **An XMP gain map with an HDR base rendition is refused** (shown as SDR), as libultrahdr refuses it: Ultra HDR
    doesn't pin down the gain's direction for it. ISO APP2 metadata with an HDR base is accepted, with libavif's weight.
  - **At a weight of exactly 0 the base is shown untouched**, where upstream would still add and subtract the two
    offsets (which needn't cancel, and on the iPhone don't matter). ISO 21496-1 says to show the base at or below its
    headroom, and it's what makes headroom 1 the same bits as mode 0 on macOS.
  - **A single-channel map applies channel 0's parameters to all three**, as upstream does; the replicated RGBA texels
    would otherwise each read their own.
  - **Apple's maker-note flavor keeps upstream's 1/64 default offsets** from `from_apple_headroom`. Apple's own formula
    has none; it's the last-resort flavor (any iOS 18 file carries the ISO one), so it stays upstream's call.
  - **The base's working space is Display P3, the gain's application space is the base ICC's primaries** (or the
    alternate's, from the map's ICC, when ISO says so; P3 for a LUT profile), and three matrices go to the shader: P3 →
    application space, application space → `HdrDisplaySpace::for_host()`, and P3 → display space for the w = 0 case.
    `ImageParams` is 256 bytes, its size pinned at compile time.
  - **The E2E test** drives the Ultra HDR fixture through `POST /display-hdr`: capable → `gain_mapped` and the EDR
    surface wanted; three headroom changes → `full_decodes` unchanged; capability off → decoded again, as `display_sdr`.
- **2026-10-14, M4: `moxcms` believes a profile's CICP tag over its curves.** The float path built its linear source
  profile as `new_srgb()` with linear TRCs, and an f32 transform from it into sRGB came out as a no-op (0.18 in, 0.18
  out, where 0.461 was due): `new_srgb()` carries sRGB's CICP tag, and `moxcms` took the tag's word. Clearing `cicp`
  fixes it. Two more `moxcms` traps on the way: `ColorProfile::new_from_cicp` silently returns an empty profile for
  primaries or transfers it can't build, so check `ColorPrimaries::try_from` and `ToneReprCurve::try_from` first; and
  `ColorProfile::encode` stamps the current time, so `make-hdr-fixtures` rewrites `iso-21496-64x48.jpg` on every run
  (header bytes only; the committed one stays). All three are in `color/CLAUDE.md`.
- **2026-10-14, M4: luminance weights need the source's white, not D50.** ICC colorants (and so every `moxcms` profile)
  are Bradford-adapted to D50, and their Y row puts BT.2020 red at 0.279 and blue at 0.046, against BT.2100's 0.2627 and
  0.0593. HLG's OOTF and the tone map both read luminance, so `decoding::scene_linear` undoes the adaptation back to D65
  with `moxcms::adaption_matrix_d`, and a test pins BT.2100's weights to 1e-3. Every HDR source we take is D65; a
  DCI-white source would be a few tenths of a percent off.
- **2026-10-14, M4 cost, release, M3 Max** (`pq_decode_perf`, `#[ignore]`, median of seven, load average 5–10), 12 MP
  RGB16 with a smooth gradient plus noise:
  - Added work on the HDR path (light through a 64K lookup, the peak histogram, the half-float pack, one parallel pass):
    **PQ 2.9 ms, HLG 8.1 ms** (HLG's OOTF is a `powf` per pixel), against the 25 ms budget. `into_rgba16` of the RGB16
    decode is another 2.0 ms, the same widening M1's 16-bit path does.
  - The `cICP` probe every PNG now pays: 1.3 µs. 8-bit PNGs are otherwise untouched
    (`an_8_bit_png_is_byte_identical_to_todays_path`).
  - The CPU SDR fallback: 27 ms on top (tone map 21, `moxcms` f32 3.8, quantize 2.7). It measured 115 ms at first: the
    EETF set itself up per pixel and round-tripped every pixel through PQ, and the transform and quantize ran on one
    thread. `ToneCurve` now sets up once and returns pixels under the knee untouched, and `transform_f32_with_profile`
    runs in rayon chunks (RAW's SDR conversion gets that too). This gradient puts most pixels above the knee; a photo
    spends less in the tone map.
  - Whole decodes of the same file, 15 ms with HDR and 50 ms through the fallback, flatter the PNG inflate: the
    synthetic gradient compresses far better than a photo, so don't quote those as a real file's.
- **2026-10-14, M4 crossfade: scene-linear follows M2's rule.** The surface follows the incoming image, so an outgoing
  `SceneLinear` image fading out on an SDR surface shows tone-mapped linear light read as display-encoded (dark and
  clipped) for one fade, like RAW HDR does; an incoming one fades in on the EDR surface over an SDR image. Unchanged,
  per section 7.
- **2026-10-14, M4 deviations from the design.**
  - **`png` reads the header chunks only; `image` still decodes the pixels**, PQ and HLG included. The spec routed the
    16-bit HDR decode through `png` too, but `image` already expands palettes, grey, and alpha into RGBA16, and the only
    thing it loses is `cICP`, which the probe reads first.
  - **The content peak keeps each histogram bin's maximum** and returns the brightest pixel in the 99.9th-percentile
    bin, never a bin's upper edge: an all-1.0 float TIFF must peak at 1.0, not 1.03, or it tips into HDR. The bins come
    off the f32 bit pattern (exponent plus five mantissa bits), so no `log2` per pixel.
  - **EETF edges:** the 0.1% of pixels above the content peak clamp onto the target with it; the knee never goes below 0
    and the spline's output never above `maxLum` (both only matter past about 10⁸ nits, where the spline would turn
    back); black-lift normalization drops PQ(0), which is 7e-7.
  - **An SDR float TIFF converts in f32**, then stores M1's shape (display-encoded 16-bit through the sRGB EOTF into
    half-floats). Running M1's 16-bit transform would have quantized linear light to 16 bits before the transform. "SDR"
    is a peak at or under 1 + 1/1024, so white through weights that sum to 1.0000001 still counts.
  - **PQ and HLG stay `SceneLinear` with HDR active whatever their peak**; only float TIFFs take the SDR shortcut, as
    section 5 says. Unusable primaries (reserved, unspecified) on a PQ or HLG PNG are taken as BT.2020.
  - **A narrow-range SDR `cICP` PNG** is scaled to full range as RGBA16, so an 8-bit one then takes the 16-bit SDR path.
    Rare, and one rounding.
  - **`gainforge` isn't used.** The WGSL needed our own BT.2390 port anyway, and one curve on both sides keeps the SDR
    and HDR renditions in step. One fewer dependency.
  - **The curve bounds luminance, not channels**, as section 5 specifies: a saturated highlight keeps one channel above
    the target, which the display (or `moxcms`, in the fallback) clips. The parity test's orange shows it.
  - **The WGSL can't name a variable `target`** (a reserved word); naga's error says so, the parity test is how it
    surfaced.
  - **Not checked against a real PQ or HLG file**: none was at hand, so the fixtures are ours and the curves are pinned
    against BT.2100's and BT.2408's published reference values instead. Spec QA step 5 is the first real check.
- **2026-10-14, review fix: an HDR image on a surface that stayed SDR drew at the live headroom.** Decoding gates on
  `capable`, but the surface also needs `hdr_surface_supported`; where that's false (macOS refusing EDR, the Windows
  DX12 to Vulkan fallback), a `GainMapped` image drew on `Bgra8UnormSrgb` with `headroom_now > 1`, and its gain clipped
  at 1.0 (a `SceneLinear` one would have tone-mapped to the headroom and clipped too). The render pass now binds group 1
  through `ImagePipeline::frame_bind_group_for(edr_surface)`, which hands any non-EDR surface the headroom-1 group the
  screenshot already used: the gain map's SDR base, and mode 3's tone map onto SDR white. The GPU test renders both at
  headroom 8 onto a non-EDR target and gets the headroom-1 bits exactly.
- **2026-10-14, review fix: Windows' scRGB surface put SDR white at 80 nits.** On a DXGI `Rgba16Float` swapchain in
  `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`, 1.0 is 80 nits, and Windows composites SDR at the user's SDR content
  brightness, so modes 1 and 2 (and 3) showed SDR white about 3× too dark next to a 250-nit desktop. wgpu does expose
  the number: `DisplayHdrInfo::luminance.sdr_white_nits`, from `DISPLAYCONFIG_SDR_WHITE_LEVEL`, read on every
  `display_hdr_info` call. `DisplayHdr` now carries `sdr_white_scale` (`sdr_white_nits / 80` on an scRGB host with HDR
  mode on, 1.0 on macOS and whenever the number is missing), `DisplayHdr::from_info` takes the `HdrDisplaySpace` so both
  platforms test from any host, the 500 ms poll and `FrameUniform` carry it, and the shader multiplies its final colour
  by it. The headroom is already peak over SDR white on Windows, so the gain and the tone map are unchanged.
  - **Applied to every mode on the EDR surface, not only 1–3**: the checkerboard and a `DisplaySdr` image mid-crossfade
    are in SDR-white units too, and leaving them out would drop them to 80 nits beside the HDR image. An SDR surface
    binds the headroom-1 group, whose scale is 1.0, so nothing SDR changes.
  - The overlay pills and text draw through their own pipeline and stay unscaled on an EDR surface; that's dim on
    Windows HDR too, and outside this fix.
  - Verified by unit tests on `from_info` and a GPU test (each HDR mode at 3.125 is its unscaled render times 3.125);
    nobody has looked at a real Windows HDR display with it yet.
- **2026-10-14, review fix: Apple's maker-note flavor had upstream's headroom and curve, both wrong.**
  - **Headroom.** `ultrahdr-core`'s last branch is `-0.44 × maker48 + 2.86`; Apple's published one ("Applying Apple HDR
    effect to your photos", developer.apple.com, fetched 2026-10-14) is `-0.303 × maker48 + 2.303`. On the iPhone Air
    fixture (maker33 1.01, maker48 1.766) Apple's gives 1.7678 stops, exactly the file's ISO `AlternateHeadroom`
    (1.767799); upstream's gave 2.08, about 24% too bright at the peak. Confirmed with ImageIO: an Apple-only copy of
    the fixture (its ISO XMP namespace and `HDRGainMapHeadroom` renamed in a scratch copy, so ImageIO reports no ISO
    gain map) fits best at 1.77 stops in a sweep from 1.4 to 1.95. `color::hdr::apple_headroom_stops` is Apple's
    formula; only the tag reader is upstream's.
  - **Curve.** Apple reconstructs `hdr = sdr × (1 + (headroom − 1) × g)`, linear, no offsets; upstream mapped it onto
    ISO's log2 curve with 1/64 offsets. `GainCurve::Apple` (a flag in `ImageParams`, Rust reference and WGSL both, a GPU
    parity case) is Apple's formula.
  - **The map's transfer is gamma 2.2, not Rec.709.** Apple's article says to linearize the map with the Rec.709
    transfer. A Swift probe applied candidate curves to ImageIO's own SDR decode and aux map of an Apple-only iPhone 14
    Plus photo (the Wikimedia "About iPhone 15 Pro Max" file, kept outside the repo) and compared with ImageIO's HDR
    decode (`kCGImageSourceDecodeToHDR`): mean relative error 0.5% for gamma 2.2, 2.4% for the sRGB curve, 3.2% for
    gamma 2.4, 5.5% for Skia's `pow(v, 1.961)`, 13% for Rec.709's inverse OETF, 22% for upstream's log2 curve. Gamma 2.2
    is macOS' default for an untagged grey image; we match ImageIO, since Preview is the QA reference.
  - **Weight at a partial headroom: log2, like every other flavor**, which is Skia's choice
    (`exp(log(1 + (H − 1)·g) × W)` in `SkGainmapShader`): the full gain `(1 + (H − 1)·g)` raised to the weight. The
    brightest texel then lands on the display's headroom, and the shape matches ISO's interpolation between renditions.
    Weighting the headroom alone (`1 + (H^w − 1)·g`) was the alternative; it keeps midtones brighter at low headroom,
    and nothing we have says Apple does it.
  - Both of these files have maker48 0 (3.0 stops on either formula), so they check the curve, not the constants; the
    fixture's two flavors check the constants. Two consequences for the tests:
    `the_apple_fixture_reads_the_maker_note_headroom` now pins 2.1515 (was 2.64), and the iPhone fixture's two readings
    agree to 2e-3 stops (was half a stop).
- **2026-10-14, review finding L1, not taken: the identity snap stays at 1e-3.** The review assumed two Display P3
  profiles differ by about 1e-5; that's `moxcms` against itself. The iPhone's Apple-written P3 profile is 3.5e-4 off
  `moxcms`', so 1e-4 brought the leak back (two iPhone tests failed). The comment on `IDENTITY_TOLERANCE` now states the
  real numbers and the worst case 1e-3 lets through (about three sRGB codes near black).
- **2026-10-14, M4 review fix M1: the SDR fallback clipped in BT.2020, before the matrix.** `moxcms`' in-place f32
  transform runs each input channel through its linearization table, which clamps to [0, 1] first, so a saturated
  BT.2020 orange the tone map left at R ≈ 2.6 came out salmon (`[255, 164, 102]` against the true `[255, 114, 91]` in
  sRGB), and wide-gamut negatives were cut before conversion too. `color::transform_linear_f32_to_display` now does the
  primaries in Rust (`source.transform_matrix(&target)`, unclipped), clips each channel in the target's primaries, and
  leaves `moxcms` only the target's curves (a linear copy of the target, CICP tag dropped). Non-matrix-shaper targets
  keep the old call. The scene-linear fallback and the SDR float TIFF path use it;
  `the_sdr_fallback_clips_in_the_displays_primaries` checks a saturated orange and an out-of-sRGB green against an
  independent reference. The comment on `transform_f32_with_profile` no longer claims the f32 path avoids that clip.
  - **RAW has the same clip and keeps it, for David to decide.** Its SDR conversion (`raw.rs`, linear Rec.2020 →
    display) still calls `transform_f32_with_profile`. Measured by switching it to the new function (temporarily, not
    committed): the golden `synthetic-bayer-128.dng` changes in 481 of 16,384 pixels into sRGB (mean ΔE 0.060, p95 0,
    max 3.96, which would fail the golden test's max-3.0 bound) and 409 into Display P3 (mean 0.021, max 1.86).
    Saturated Rec.2020 highlights into sRGB, today vs clip-after: `[1.2, 0.5, 0.2]` → `[255, 177, 110]` vs
    `[255, 173, 109]`; `[2.0, 0.6, 0.1]` → `[255, 196, 51]` vs `[255, 175, 33]`; `[1.5, 1.4, 0.2]` → `[255, 255, 91]` vs
    `[255, 255, 67]`; `[0.9, 0.9, 1.3]` → `[243, 243, 255]` vs `[240, 243, 255]`; in-gamut and negative-only cases
    identical. So the change only touches highlights past white, which RAW's highlight recovery already desaturates;
    clip-after keeps them more saturated (closer to the RAW HDR path), today's look is paler. Switching is one call in
    `raw.rs` plus a regenerated golden. David took it the same day (the RAW SDR decision below).
- **2026-10-14, M4 review fix M2: float TIFFs decide SDR vs HDR by their brightest pixel.** The 99.9th percentile is the
  right source peak for the tone map, and the wrong test for "is anything above SDR white": a render whose lamps or sun
  glints cover under 0.1% of the pixels, or a star field, came out SDR and clipped at white even on an XDR panel.
  `PeakHistogram::max` (the last occupied bin's maximum, so it costs nothing extra) now decides, and the percentile
  still sets `content_peak`. The review's suggested floor (content peak at least 1.0 whenever the max passes 1) is left
  out: the target is never below 1.0, and content at or under the target is the identity either way. Anchors:
  `a_float_tiff_with_sparse_highlights_is_still_hdr` (two stars at 20× in 4,000 pixels) and `max` in
  `the_peak_ignores_stray_hot_pixels_but_not_highlights`.
- **2026-10-14, review fix M2: a turned gain map sat off by up to 3 pixels on every reversed axis.** `finalize` turned
  the map with the base, but upstream aligns map texel 0 with the _stored_ pixel 0, so after a flip or a rotation an
  axis that runs backwards needs `X × m / W − (1 − m / W)`, and the shader used `X × m / W`: half a texel (1 image
  pixel) for a half-res map, three quarters (3 pixels) for Apple's quarter-res. Of the two fixes the review offered, the
  map now stays in the file's frame and the sampler goes back through the turn (`MapTurn`: swap the axes, reverse x, y,
  or both, in `ImageParams::map_turn`), not a per-axis offset: upstream clamps the far neighbour at the stored frame's
  last texel, and only sampling in that frame reproduces it at the edge. It also skips turning the map on the CPU. A
  busy-map test compares all eight orientations with `apply_gainmap` on the unturned buffer (orientation 2 was 27% off
  on it before), and a GPU parity case runs the eight turns. The real Apple-only iPhone 14 Plus photo, orientation 6
  with a half-res map, matches ImageIO's HDR decode through our whole decode to 0.54% mean relative error at full
  headroom (a second file from the same phone, 0.67%), which is also review finding L9's real-file check.
- **2026-10-14, review lows, decided.**
  - **L2, fixed:** `want_edr_surface` needs image mode, and every mode switch re-applies the surface state, so browse
    drops the EDR surface and the headroom poll; `WindowEvent::Occluded` stops the poll for a covered window. The review
    suggested also clearing the image's encoding on entering browse; the gate does the same job and keeps what image
    mode returns to.
  - **L3, kept and documented:** a gain-map base is color-managed into Display P3 even with ICC off. The HDR surface's
    color space defines its values, so "unmanaged" has no meaning there, and RAW and scene-linear HDR convert regardless
    too. ICC off governs the SDR path (`decoding/CLAUDE.md`).
  - **L4, kept and documented:** the histogram of a `GainMapped` image bins P3 bytes, and an HDR screenshot is encoded
    in the HDR display space. Both match the display on a P3 panel, where gain maps show at all (`histogram/CLAUDE.md`,
    `render/CLAUDE.md`).
  - **L5, fixed:** an MPF index listing an image past the primary counts as a gain-map signal, so an iPhone file whose
    maker note was stripped still finds its ISO map. Costs a header read per appended image on HDR decodes.
  - **L6, fixed:** full ISO parameters on the primary go to an appended image with its own ISO segment, else the first
    one not tagged (`apdi:AuxiliaryImageType`) as a depth map or matte, else no map at all.
  - **L7, documented:** an HDR base in an 8-bit JPEG is taken as SDR-white-relative; no writer we know does it.
  - **L8, fixed:** the `GContainer` scan reads at most 16 items and looks at most 64 names.
  - **L9:** covered by the real Apple-only files in the H1 and M2 entries. A real Pixel file stays local (David's
    photos, never committed), so CI's only real-world gain-map input is still the iPhone Air fixture.
- **2026-10-14, M4 review fix M3: diffuse white on an SDR target, decided.** BT.2390 with the target at SDR white put
  203-nit white at 0.90 (400-nit master), 0.78 (1,000), 0.63 (4,000), and 0.55 (10,000) of SDR white: 0.15 to 0.87 stop
  dim, with midtones compressed from 24 nits up for the brightest masters. That's structural, not a tuning slip:
  BT.2390's knee `KS = 1.5 × maxLum − 0.5` is exactly the highest its slope-1 PQ-domain Hermite allows without its slope
  going negative near the peak, so no other knee fixes it inside BT.2390.
  - **Chosen:** keep BT.2390 wherever its knee sits at or above `WHITE_ANCHOR` (0.8) × the target, and below that use a
    white-anchored shoulder: the identity up to 0.8 × target, then Reinhard's `x / (1 + a·x)` on luminance above it,
    with `a = 1/(target − knee) − 1/(peak − knee)`, so its slope is 1 at the knee and the content peak lands exactly on
    the target. Diffuse white now sits at 0.900–0.903 of SDR white for 1,000- to 10,000-nit masters (about −0.15 stop,
    243 of 255 in sRGB), and nothing under 0.8 × SDR white moves. The cost is highlights packed into the top tenth on an
    SDR target, which is what SDR has room for.
  - **Why this over the alternatives:** BT.2446 Method C is an HDR-to-SDR conversion for broadcast, with its own
    crosstalk and a different look from what the GPU shows; a target-only switch would make the SDR fallback disagree
    with the same file at headroom 1 on an HDR display. The shader makes the same choice per fragment (one extra PQ
    decode), so the CPU fallback and the GPU at a low headroom (an MBP at full brightness reads about 1.2) still agree,
    and the parity test covers headrooms 1 and 1.5, where the shoulder runs.
  - **Tests:** `diffuse_white_stays_near_sdr_white_on_an_sdr_target_at_any_peak` (1,000 to 10,000 nits),
    `the_curve_is_monotonic_and_smooth_across_peaks_and_headrooms` (four masters by six headrooms, slope 1 at the knee),
    `bt2390_runs_where_its_knee_leaves_white_alone`. All three failed first with the anchor set to 0, which is pure
    BT.2390.
- **2026-10-14, M4 review lows, fixed.**
  - **L1, 16 bits on an SDR display:** the CPU SDR rendition was RGBA8 even for the viewer, so a 16-bit PQ/HLG PNG or an
    HDR float TIFF rounded to 8 bits right after a strong tone curve. The viewer now gets M1's 16-bit `DisplaySdr`
    half-floats; only the byte consumers get RGBA8. The SDR float TIFF path folded into the same function.
  - **L3, a LUT-only display profile:** `moxcms`' in-place f32 transform refuses a non-matrix-shaper target, and the
    rendition went out still linear. `transform_linear_f32_to_display` sends such targets through `moxcms`' copying f32
    transform, which runs the LUTs. **Still open for RAW:** `transform_f32_with_profile` has the same refusal, so a RAW
    on a LUT-only calibrated display shows linear (dark) in SDR; not changed here, since RAW's path stays as it is until
    David decides on M1. Fixed with that decision (the RAW SDR decision below).
  - **L4, float TIFF alpha:** NaN alpha reads as 1 (opaque), alpha clamps to [0, 1], and a non-matrix-shaper ICC on a
    float TIFF logs that it's taken as linear BT.709.
  - **L5, luminance weights in the source's white:** a float TIFF's weights now undo the D50 adaptation back to its own
    white (`icc_white`: `chad⁻¹ · D50` for ICC v4, else the white-point tag), so ProPhoto keeps its D50 weights. Found
    on the way: `moxcms` writes Bradford's cone matrix as every profile's `chad`, which isn't an adaptation, so a `chad`
    whose white doesn't have Y = 1 is ignored.
- **2026-10-14, M4 review L2, decided: HLG keeps the fixed 1,000-nit OOTF, then the tone map.** BT.2100's HLG is
  display-referred, and a display at `Lw` nits would use system gamma `1.2 + 0.42 × log10(Lw / 1000)` and need no tone
  map. Prvw renders HLG at its nominal 1,000-nit display and tone-maps from there, for three reasons:
  1. One light for both renditions: the CPU SDR rendition and the GPU at headroom 1 stay the same image, which a
     per-target gamma would break (the GPU's target moves with brightness every frame, and its OOTF would have to move
     with it in the shader).
  2. With M3's white-anchored shoulder, HLG's reference white (75% signal) lands at 0.90 of SDR white on an SDR target,
     so the dimming that made a display-adaptive gamma attractive is gone.
  3. Every HLG file is mastered for the 1,000-nit reference display the 1.2 gamma belongs to; rendering it there first
     is the faithful starting point.
  - **The alternative, if QA says HLG looks off on SDR:** on the CPU fallback only, render with the display-adaptive
    gamma at `Lw` = 203 nits (BT.2100's extended formula gives about 0.95) and skip the tone map. It would match a
    native HLG SDR display more closely, at the cost of the SDR rendition no longer matching headroom 1 on an HDR one.
- **2026-10-14, M5: the setting, as built.**
  - **Key and shape.** `Settings::color.hdr_output` (a `ColorSettings` group under `color` in the JSON, default on),
    `SettingKey::HdrOutput` on the Color panel, `CommandKey::HdrOutput`, `AppCommand::SetHdrOutput`, and
    `color::State::hdr_output`. `RawPipelineFlags::hdr_output` is gone; the RAW panel keeps only "HDR brightness gain"
    under "Output", and on Windows that trackbar is no longer indented, since nothing sits above it.
  - **Migration.** `Settings::parse` copies a persisted `raw.hdr_output` into `color.hdr_output` unless the new key is
    already there, and the next save drops the old one (serde ignores unknown fields, so without the copy an old `false`
    would silently turn back on). Pinned by `the_old_raw_hdr_output_key_carries_over`.
  - **One gate.** Decoders take `hdr_active` (the setting AND the display's capability, `App::hdr_active`) in place of
    M2's `hdr_display` plus the RAW flag; `App::sync_hdr_active` is the only place that reacts to it flipping
    (preloader, cache budget, cache flush), shared by display changes and the setting. Toggling the setting on an SDR
    display decodes nothing.
  - **QA.** `/state` reports `hdr_output`, `POST /hdr-output` and the `hdr_output` MCP tool set it, and
    `hdr_output_re_decodes_only_when_it_changes_what_shows` checks both directions on an HDR display and no re-decode on
    an SDR one (verified to fail when the setting always re-decodes).
  - **Not added:** a menu item. The other color settings have one, but HDR output is a set-and-forget preference.
- **2026-10-14, David decided: RAW's SDR conversion clips in the display's primaries too.** The open question from M4
  review fix M1 and L3. `raw::rec2020_to_sdr_display` now calls `color::transform_linear_f32_to_display`, so RAW's SDR
  develop converts the primaries in f32 first and clips after, like the scene-linear fallback; the HDR path is
  unchanged. Outcome:
  - **Saturated highlights keep their hue.** Rec.2020 `[2.0, 0.6, 0.1]` into sRGB is `[255, 175, 33]`, where it was
    `[255, 196, 51]` (`sdr_conversion_clips_in_the_displays_primaries`). The SDR tone curve's peak of 1.0 caps
    luminance, not channels, so a saturated channel still reaches the conversion above 1.0.
  - **On a real photo it's more than the golden shows.** A 24 MP Sony A7 III ARW into sRGB (old path emulated as a clamp
    to [0, 1] before the same conversion, temporary instrumentation): 8.7% of its pixels reach the conversion with a
    channel above 1.0 (up to 3.68) and 4.1% change on screen: 865,000 by up to four codes, 104,000 by 5–10, 15,000 by
    11–20, and 3,000 (0.012%) by more, up to 107. The big ones are saturated blue lights (Rec.2020 blue 2–3): pale sky
    blue before, a saturated blue-cyan now, the way the HDR path already shows them (`[0.22, 0.33, 3.06]` was
    `[90, 157, 255]`, now `[0, 153, 255]`). Worth a look on a RAW with colored lights.
  - **A LUT-only display profile runs its LUTs** instead of leaving the RAW linear and dark
    (`sdr_conversion_runs_a_lut_only_display_profile`). Values still clip in Rec.2020 there, as for every LUT target.
  - **The golden moved, as measured in M1.** Old code vs new on `synthetic-bayer-128.dng` into sRGB: 481 of 16,384
    pixels, up to 8 codes (114 by one). None of its pixels reach the conversion above 1.0; the 378 with a negative
    Rec.2020 channel (down to −0.124, deep reds and magentas, outside Rec.2020) are what moves, and clarity and
    sharpening spread it to their neighbors (419 changed with both off). The old checked-in golden read mean ΔE 0.077,
    p95 0.36, max 3.96 against the new output, past the test's 3.0 bound, so it was regenerated. It was already 777
    pixels (up to two codes) off the old code, from drift nobody had regenerated for.
  - **Cost: about 2 ms on 24 MP.** Release, M3 Max under load (`raw_sdr_decode_perf`, `#[ignore]`): the conversion alone
    went from 7.2–8.6 ms to 9.4–10.7 ms (the extra matrix pass), and a whole SDR decode of a 24 MP Sony A7 III ARW
    measured 475–586 ms both before and after, so the difference is under the noise.
