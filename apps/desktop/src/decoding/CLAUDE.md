# Decoding

Decode image files (to RGBA8, or half-float for 16-bit sources and HDR), extract the embedded ICC profile, apply EXIF
orientation, and hand a `DecodedImage` off to the renderer.

| File               | Purpose                                                                                                                                                            |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `mod.rs`           | Public API: `DecodedImage`, `PixelBuffer` (storage), `PixelEncoding` (meaning), `OutputDepth`, `load_image` (always-cancellable), `is_supported_extension`         |
| `read_progress.rs` | `ReadProgress`: the live byte counter of one file read, plus `READ_CHUNK_BYTES` and the `PRVW_READ_DELAY_MS` test hook                                             |
| `dispatch.rs`      | `Backend` enum + extension-to-backend mapping                                                                                                                      |
| `jpeg.rs`          | Fast JPEG path via `zune-jpeg` (SIMD); keeps a gain-map JPEG's map when HDR is active                                                                              |
| `gain_map/`        | Finding a JPEG's gain map: `mpf.rs` (MPF index), `xmp.rs` (`hdrgm`, `GContainer`, Apple's aux tag), `mod.rs` (`locate`, metadata precedence, `GainMapPayload`)     |
| `raw.rs`           | Camera RAW via `rawler` (DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW)                                                                                         |
| `raw_preview.rs`   | Fast embedded-JPEG preview extraction (`decode_raw_preview`): no develop, downscaled + oriented soft placeholder shown on RAW cache-miss while the develop runs    |
| `dng_opcodes.rs`   | DNG `OpcodeList1/2/3` parsing + application (Phase 3.0)                                                                                                            |
| `generic.rs`       | Fallback path via the `image` crate (PNG, GIF, WebP, BMP, TIFF), including the 16-bit SDR path and the `cICP` probe                                                |
| `scene_linear.rs`  | PQ and HLG PNGs and float TIFFs: linear light, its content peak, then `SceneLinear` or the CPU's tone-mapped SDR rendition                                         |
| `orientation.rs`   | EXIF orientation parsing and in-place pixel-buffer rotation                                                                                                        |
| `exif_metadata.rs` | Curated EXIF subset for the EXIF info overlay (camera, lens, exposure, time, GPS). nom-exif on JPEG/Generic/RAW outer bytes; rawler `RawMetadata` fallback for RAW |

## Key patterns

- **Backend dispatch is extension-based.** `dispatch::pick_backend(ext)` picks the decoder; `is_supported_extension` is
  the gate the directory scanner uses. Adding a format means: teach `dispatch` about its extensions, add a backend
  module, and match it in `mod::decode_with`.
- **One exception: a RAW extension on a JPEG or PNG.** `load_image` calls `dispatch::pick_backend_for(ext, bytes)`,
  which sends a RAW-extension file whose first bytes are a JPEG or PNG signature to that decoder. A Sony JPEG saved as
  `.arw` made rawler fail with "No decoder found, model ''". TIFF signatures don't count, because most RAWs are TIFF
  containers. The RAW quick preview and `previews::metadata` still go by extension and simply find nothing for such a
  file. `a_jpeg_or_png_wearing_a_raw_extension_decodes_by_its_content` is the anchor.
- `failed_decodes()` counts every `load_image` failure that isn't a cancel, for `/state`.
- **Cancellation.** `load_image` takes an `AtomicBool`, checked between each RAW pipeline stage and at every 256 KB
  file-read chunk. Returns `Err("cancelled")` when the flag flips. Worst-case cancel latency inside RAW decode is one
  stage (≈ 80 ms on the `lens` stage; see the stage budget table). JPEG and generic decodes are a single opaque library
  call we can't checkpoint, so they run on an abandonable thread instead (see below) — cancel frees the caller within
  ~10 ms regardless of how far the decode has progressed.
- **Abandonable file read + decode.** Two helpers share the same shape: run the blocking work on a detached
  `std::thread` and poll the result through a `mpsc::sync_channel` with 10 ms timeouts; when cancellation fires, the
  caller drops the receiver and returns `Err("cancelled")` immediately, while the worker thread finishes on its own and
  discards its result when the send fails.
  - `read_file_cancellable` covers the file read. Critical for slow / wedged network shares: `std::fs::File::read` has
    no timeout, so an in-thread flag check does nothing until the kernel unblocks the syscall. The caller is never
    blocked. It also fills in an optional `ReadProgress` handle (file length from metadata, then a byte count per
    `READ_CHUNK_BYTES` chunk), which is what the "Loading…" overlay's read bar draws — see `navigation/CLAUDE.md`.
    `PRVW_READ_DELAY_MS` pauses it before every chunk so tests can watch the bar climb. The same metadata call, made on
    the open handle before the first read, becomes `DecodedImage::stamp`, which live sync compares against the file
    later to tell a re-save from a tag write (`crate::file_stamp`). Before the read, never after: a racing write then
    leaves the stamp older than the pixels, which costs a re-decode rather than hiding a change.
  - `full_decodes()` counts every `load_image` success; `/state` reports it for tests.
  - `run_decode_cancellable` wraps the JPEG and generic decodes (RAW self-cancels between stages, so it stays inline).
    Trade-off: an abandoned decode burns CPU to completion (bounded; only on cancellation of a large in-flight decode),
    in exchange for the serial preload worker never blocking on a huge image the user has already navigated past — for
    example an 80 MP JPEG, which would otherwise stall the next image's decode by hundreds of ms. Like the preloader's
    worker, it's a plain OS thread (not a rayon worker), so internal `par_iter` falls back to the global pool instead of
    collapsing onto a single-thread pool.
    - **Salvage.** The abandoned decode isn't pure waste: it recovers a value via `SendError` (the caller dropped the
      receiver, so `send` hands the result back) and, if the caller passed a `SalvageSink`, calls it with the finished
      image on the decode thread. The sink is preloader-agnostic — the decode layer just hands the image back; the
      receiver decides whether to keep it. The preloader gates on the cache window (see `navigation/CLAUDE.md`), turning
      would-be waste into a speculative prefetch when the image is still wanted. Best-effort: a tiny race (decode
      finishes the same instant cancel fires, so `send` succeeds into the buffer before the caller drops `rx`) drops one
      salvageable image rather than salvaging it. Harmless.
- **EXIF rides on the `DecodedImage`, and the attach is portable.** `decode_with` parses the outer file bytes with
  `parse_exif_metadata` (RAW additionally falls back to rawler's `RawMetadata` via `parse_raw_exif`) and stores the
  result in `DecodedImage::exif`; the overlay reads it back out of the image cache. Nothing between the file bytes and
  that field may be `#[cfg]`-fenced to one platform, and `load_image_attaches_exif_to_the_decoded_jpeg` /
  `load_image_leaves_exif_none_for_a_png` in `mod.rs` are the anchors that say so from every host.
- **ICC profile first, pixels second.** See the gotcha below.
- **Storage and meaning are separate fields.** `PixelBuffer` says how the pixels are stored (`Rgba8` or `Rgba16F`),
  `PixelEncoding` what they mean: `DisplaySdr` (display-ICC-encoded SDR, either storage), `LinearHdrDisplay` (linear
  light in `HdrDisplaySpace::for_host()`, `Rgba16F` only, RAW HDR), `GainMapped` (a Display P3 SDR base plus its gain
  map, `Rgba8` only; see below), or `SceneLinear` (linear light in the source's own primaries, `Rgba16F` only; PQ, HLG,
  and HDR float TIFFs, see below). `EncodingKind::is_hdr()` is the "needs the EDR surface" question, so never infer HDR
  from storage. Each legal pair has its own constructor (`from_rgba8`, `from_display_sdr_rgba16f`,
  `from_linear_hdr_rgba16f`, `from_gain_mapped`, `from_scene_linear_rgba16f`), and `finalize` re-checks
  `PixelEncoding::accepts` in debug builds. `EncodingKind` is the payload-free tag the app keeps for the image on
  screen.
- **`OutputDepth::Rgba8` is for consumers that can only carry bytes.** The Windows clipboard and print
  (`platform/windows/ui_common.rs`) and the preview generator pass it. It forces `hdr_active` off (so RAW develops SDR)
  and keeps 16-bit sources on the 8-bit path, so those callers get exactly what they got before 16-bit decoding existed.
  The viewer passes `OutputDepth::Native`. macOS copy and print re-read through ImageIO and never call `load_image`.

## 16-bit SDR PNGs and TIFFs

`generic.rs` sends `Luma16` / `LumaA16` / `Rgb16` / `Rgba16` decodes (at `OutputDepth::Native`) through `into_rgba16()`
→ `color::transform_icc_16bit` → `color::display_sdr::rgba16_to_display_sdr_f16_inplace`, stored as `Rgba16F` tagged
`DisplaySdr`. The renderer draws it on the normal SDR surface, which rounds to 8 bits exactly once.

**Why:** `to_rgba8()` before an 8-bit transform rounds twice, and on a smooth gradient through a real conversion (sRGB →
Display P3) 48% of samples land a step off the single-rounding answer, up to 1.5 steps; those off-by-ones are the bands.
The 16-bit path is under 0.1 steps everywhere. `a_16_bit_png_gradient_does_not_band_through_a_real_transform` is the
anchor. Cost and numbers: `docs/specs/hdr-images.md` Findings, 2026-10-14. Float TIFFs take the scene-linear path
(below), since they may be HDR. 8-bit sources are byte-identical to before
(`an_8_bit_png_is_byte_identical_to_todays_path`). `RUST_LOG=prvw::decoding::generic=debug` logs the stage split
(`to_rgba16`, `icc`, `to_f16`).

## Gain-map JPEGs

A gain-map JPEG is a normal SDR JPEG with a second, smaller JPEG appended: the map, saying how much brighter each area
gets on an HDR display. `jpeg::decode` keeps it only when `hdr_active` (see "HDR / EDR flow" below); otherwise the map
is never looked for, and the file decodes byte for byte like its primary alone
(`a_gain_map_jpeg_without_hdr_is_its_plain_primary`). `OutputDepth::Rgba8` forces `hdr_active` off, so the clipboard,
print, and previews get the SDR JPEG.

With HDR active:

1. `gain_map::locate` reads the headers zune already parsed: the primary's XMP, EXIF, MPF payload, and ISO APP2
   segments. No signal (ISO segment, `hdrgm` namespace, Apple maker-note headroom, or MPF listing an image past the
   primary) means a plain JPEG and today's path. MPF counts because exporters and privacy tools strip maker notes and
   keep MPF, and the iPhone's primary has no other signal (`an_mpf_listing_alone_is_enough_to_look_for_a_map`); the cost
   is a header read per appended image, on HDR decodes only, including a camera JPEG's MPF preview.
2. The candidates are the MPF entries other than the primary (`mpf.rs`, every offset bounds-checked), or, with no MPF,
   the `GContainer` directory's gain-map item, located from the end of the file by its listed length. Each candidate's
   headers are read (no pixels) for its ISO APP2, its XMP, and its ICC profile.
3. Metadata precedence: ISO 21496-1 (an APP2 on the map, then Apple's XMP form of the same fields, then full parameters
   on the primary, applied to the appended image with an ISO segment, else the first one not tagged as a depth map or
   matte, else none), Ultra HDR XMP, then Apple's maker note with a map tagged
   `urn:com:apple:photo:2020:aux:hdrgainmap`. Every flavor lands in one `color::hdr::gain_map::GainMapParams`. Apple's
   carries its own curve (`GainCurve::Apple`) and Apple's headroom formula (`color/CLAUDE.md`).
4. The map decodes to RGBA8 (a grayscale map replicated into R, G, B), the base is color-managed into **Display P3**,
   and `GainMapSpaces` works out the three matrices the shader's mode 2 needs. A map larger than its base on either side
   is refused. Anything that fails falls back to the SDR path.

The result is `PixelEncoding::GainMapped(Box<GainMapPayload>)`; the shader applies the map per frame at the display's
current headroom (`render/CLAUDE.md`), so a brightness change never re-decodes.

- **Decision: Display P3 is the base's working space.** It uses the sRGB transfer curve, so the `Rgba8UnormSrgb` texture
  hands the shader linear P3 for free, it's a superset of sRGB and what iPhones shoot, and `transform_is_negligible`
  makes a P3-tagged base (iPhone, Pixel) free. A base wider than P3 clips; `GainMapSpaces::new` logs it at `debug`.
- **Decision: our own MPF and `hdrgm` parsers.** `ultrahdr-core` 0.6.0 dropped both, and the crate that has them
  (`zenjpeg`) is AGPL. The XMP scanner resolves whatever prefix the packet binds the namespace to; the iPhone binds it
  to `HDRToneMap`. Truncated and corrupted inputs are tested to never panic.
- **Gotcha: Apple's "ISO 21496-1" metadata is XMP, not an APP2 segment.** iOS 18 writes ISO's fields (`BaseHeadroom`,
  `AlternateHeadroom`) in Adobe's `hdrgm` namespace on the map image, with `0`/`1` booleans, and ImageIO reports that as
  the ISO gain map. The iPhone fixture has no ISO APP2 at all. `xmp::parse_hdrgm` reads both dialects and says which.
- **Gotcha: an XMP gain map with an HDR base is refused**, as libultrahdr refuses it: Ultra HDR doesn't pin down the
  gain's direction for it. ISO 21496-1 APP2 metadata with an HDR base is accepted, and its weight runs backwards.
  **Assumption:** an 8-bit sRGB base is SDR-white-relative (1.0 is SDR white), so at a weight of 0 an HDR base shows at
  SDR brightness. A writer that means "base white sits at `2^base_headroom`" would show that much dimmer. No writer we
  know puts an HDR base in an 8-bit JPEG; revisit with a real file.
- **Decision: a gain-map base is color-managed even with ICC off.** Settings' ICC switch empties the SDR path's target
  (`App::effective_display_icc`), so SDR bytes go to the screen as the file has them. An HDR surface has no such option:
  its layer's color space (linear Display P3 or scRGB) says what every value means, so an unconverted sRGB base would
  show oversaturated. RAW HDR and scene-linear HDR convert regardless too; ICC off governs the SDR path.
- **Gotcha: the map is never color-managed.** It's data, not color; its ICC profile, if any, describes the alternate
  rendition's color space (used only when ISO says to apply the gain there).
- **Decision: the map stays in the file's frame; the sampler follows the turn.** `finalize` turns the base by its EXIF
  orientation and records the turn on the map (`GainMapPayload::turn`, `color::hdr::gain_map::MapTurn`); mode 2 maps
  each shown pixel back to the stored pixel it came from before sampling. **Why:** upstream aligns map texel 0 with
  stored pixel 0, so a map turned with the base sits `1 − map / image` texels off along every axis the turn reverses (1
  image pixel for half-res maps, 3 for Apple's quarter-res), and the edge clamp lands on the wrong side. Portrait iPhone
  shots are orientation 6, the common case. `a_turned_gain_map_matches_apply_gainmap_on_the_unturned_file` checks every
  orientation against upstream on the unturned buffer; a real orientation-6 Apple file matches ImageIO to 0.5%.
- **Gotcha: a single-channel map applies channel 0's parameters to all three**, as `ultrahdr-core` does. The RGBA
  replicas would otherwise each read their own channel's, which differ for a single-channel map with per-channel XMP.
- `RUST_LOG=prvw::decoding::jpeg=debug` logs the flavor, the map's size, and the stage split (`base`, `locate`, `map`,
  `icc`).

Cost (release, M3 Max, `gain_map_decode_perf`): a 12.5 MP Pixel Ultra HDR photo decodes in 1.03× its SDR time with a P3
display target and 0.67× with an sRGB one (the base's P3 transform is free where the display's isn't); memory is +6.2%,
the map's sixteenth. Numbers in `docs/specs/hdr-images.md` Findings, 2026-10-14.

## PQ and HLG PNGs, and float TIFFs

Their samples are light, not display values, so `scene_linear.rs` turns them into linear light relative to SDR white
(1.0 = 203 nits, BT.2408's reference white) in their own primaries, measures the content peak (the 99.9th percentile of
luminance), and then picks a shape:

- **HDR active** (`hdr_active`, at `OutputDepth::Native`; see "HDR / EDR flow" below): `SceneLinear` half-floats, which
  shader mode 3 tone-maps per frame at the current headroom (`render/CLAUDE.md`).
- **Otherwise**: the same tone curve on the CPU with SDR white as the target (`color::hdr::tone_map::ToneCurve`), then
  the primaries changed in f32 and clipped in the display's own primaries, with `moxcms` applying only the display's
  curves (`color::transform_linear_f32_to_display`). The viewer gets M1's 16-bit `DisplaySdr` half-floats (a strong tone
  curve then an 8-bit rounding is where skies band); the byte consumers get RGBA8. SDR displays, Linux, and HDR off all
  take this.
- **A float TIFF whose brightest pixel is at or below SDR white** is SDR in a float container: the same rendition with
  nothing to tone-map, on any display, so it never asks for the EDR surface. The brightest pixel decides, not the
  content peak: a render's lamps on 0.05% of the pixels are still HDR (`PeakHistogram::max`).

How each source gets to light:

- **PNG:** `generic::decode` calls `scene_linear::probe_png` first, which reads the header chunks through the `png`
  crate (`image` 0.25 doesn't pass `cICP` on; 1.3 µs per PNG). `cICP` outranks `iCCP` (PNG third edition). Transfer 16
  (PQ) or 18 (HLG) takes this path; any other transfer is SDR with the profile `moxcms::ColorProfile::new_from_cicp`
  builds, which then replaces the file's own on the normal SDR path. Matrix coefficients other than 0 mean a broken
  chunk (PNG is RGB only) and are ignored, as are code points `moxcms` can't build a profile from. `image` still decodes
  the pixels. Narrow-range files are scaled to full range first, in the source's own bit depth.
- **PQ:** SMPTE ST 2084's EOTF to nits, over 203. **HLG:** BT.2100's inverse OETF, then its OOTF at the nominal 1,000
  nits (system gamma 1.2), over 203; HLG's 75% signal is reference white. Fixed at 1,000 nits rather than adapted to the
  display, so the SDR rendition and the GPU at headroom 1 stay one image (decided in the spec's Findings, review L2).
  Both run through a 64K lookup table per transfer (built once), and HLG adds a `powf` per pixel for the OOTF.
- **Float TIFF** (`Rgb32F` / `Rgba32F` from `image`): already linear, 1.0 = SDR white. The ICC profile's colorants name
  the primaries when it's a matrix-shaper profile, BT.709 otherwise (logged at `debug`); its curves are ignored. A NaN
  color becomes 0, a NaN alpha 1 (the pixel stays rather than vanishing), alpha clamps to [0, 1], and color past the
  half-float range clamps, since a renderer's stray value would otherwise reach the texture
  (`float_tiff_alpha_is_sanitized`).

`cLLI` and `mDCV` are logged at `debug`, never trusted: the measured peak decides.
`RUST_LOG=prvw::decoding::scene_linear=debug` logs the stage split for both shapes.

- **Decision: the histogram's 1,024 bins come off the f32 bit pattern** (exponent plus five mantissa bits, 1/32 stop),
  so a pixel costs a shift and no `log2`, and each bin keeps its own maximum so a flat 1.0 image peaks at exactly 1.0. A
  bin's upper edge would tip an SDR float TIFF into HDR.
- **Gotcha: float samples are linear.** Never let them reach `to_rgba8()` and the 8-bit path: that reads them as sRGB
  codes, which shows the file far too dark and clips every highlight
  (`a_float_tiff_within_sdr_white_takes_the_16_bit_sdr_path`).
- **Gotcha: a profile whose curves are swapped must lose its CICP tag** (`color/CLAUDE.md`); the linear source profile
  drops it.

Cost (release, M3 Max, `pq_decode_perf`, 12 MP): the added work on the HDR path (light, peak, half-float pack) is 2.9 ms
for PQ and 8.1 ms for HLG, against a 25 ms budget; the CPU SDR fallback is 27 ms on top of that. Numbers in
`docs/specs/hdr-images.md` Findings, 2026-10-14.

## Gotchas

- **ICC extraction ordering (`generic.rs`).** `ImageReader::into_decoder()` returns `impl ImageDecoder`. `icc_profile()`
  takes `&mut self`, and `DynamicImage::from_decoder()` consumes the decoder. So call `icc_profile()` first, then
  `from_decoder()`. Reversing won't compile.
- **`zune-jpeg` in debug builds is unusably slow.** The **workspace-root** `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3` to fix this — it must be at the root, not `apps/desktop/Cargo.toml`,
  because Cargo ignores `[profile.*]` in member manifests (it warns and drops them). Without it, cold startup on a 20 MP
  photo takes seconds. The same override is set for **`moxcms`** (the per-pixel ICC color transform) for the same reason
  (~1s on a 24 MP photo in unoptimized debug). Both optimize only the dependency; our own crate stays opt-0 and
  debuggable.
- **JPEG decodes straight to RGBA.** `jpeg.rs` sets `jpeg_set_out_colorspace(ColorSpace::RGBA)` so zune emits RGBA in
  one SIMD color-convert pass — no separate RGB→RGBA repack (which doubled peak memory with a second framebuffer) and no
  3-channel assumption (zune handles CMYK / grayscale → RGBA). The generic backend already delegates the same way via
  `image`'s `to_rgba8()` (and `into_rgba16()` for 16-bit sources).
- **Unknown EXIF orientation values get logged and ignored.** The spec defines 1–8; cameras occasionally write garbage.
  We pass the buffer through unchanged rather than guess.
- **`nom-exif` decodes date/time tags into typed variants, never `Text`.** `DateTimeOriginal` / `CreateDate` /
  `ModifyDate` come back as `EntryValue::Time` (with offset) or `EntryValue::NaiveDateTime` (without), so the generic
  `text()` extractor returns `None` for them. Use `date_text()` in `exif_metadata.rs`, which handles both typed variants
  (plus a `Text` fallback). Matching only `Text` silently drops the date on every JPEG/HEIC/WebP — caught by the
  mandatory date assertion in `metadata_round_trip_via_little_exif`.
- **RAW orientation lives on the decoder's metadata, not `RawImage`.** Rawler hard-codes `RawImage.orientation` to
  `Normal`; the real EXIF value is on `decoder.raw_metadata(...).exif.orientation`. `raw.rs` reads it there and hands it
  back to the dispatcher so `apply_orientation` can rotate the developed buffer. Because of this, the RAW backend is the
  only one that supplies its own orientation instead of going through the shared `parse_exif_orientation` over the outer
  file bytes.
- **Fujifilm X-Trans demosaic is bilinear only.** Rawler ships a simple X-Trans bilinear demosaic, not Markesteijn.
  Usable in a viewer but less detailed than what dedicated RAW tools produce.
- **Rawler applies `LinearizationTable` (tag 50712) itself.** Look in `rawler-0.7.2/src/decoders/mod.rs::641`: the
  generic raw path dither-interpolates every raw pixel through the table when the tag is present, so we don't need a
  second pass in `dng_opcodes.rs`. The Phase 3.0 investigation (see `docs/notes/raw-support-phase3.md`) confirmed this.
- **DNG opcode coordinates are raw-image-absolute.** For CFA opcodes (OpcodeList1 and 2) that's fine because we apply
  them before active-area crop. For `OpcodeList3` on the demosaiced+cropped buffer we currently ignore the active- area
  origin; every fixture we test starts the active area at (0, 0), so no shift is needed today. Cameras with a nonzero
  origin would miscrop post-color opcodes. Tracked as Phase 3.x future work.

## RAW pipeline (Phase 2.5a + Phase 3.0)

`raw.rs` bypasses rawler's default `Calibrate`/`CropDefault`/`SRgb` stages so we can keep the intermediate wide-gamut:

1. Rawler's `raw_image` extracts the mosaic and metadata. 1a. **Phase 3.0: DNG `OpcodeList1` applied**
   (`dng_opcodes.rs`). Pre- linearization gain maps and bad-pixel fixes on the CFA mosaic. Silent no-op for non-DNG
   files and for DNGs without the tag. 1b. `raw.apply_scaling()`: rawler's black-level subtract + [0, 1] linear rescale,
   split out so we can slip the next step in between. 1c. **Phase 3.0: DNG `OpcodeList2` applied**. Post-linearization,
   pre- demosaic CFA-level gain maps. This is where iPhone ProRAW stashes its per-Bayer-phase lens-shading correction (4
   `GainMap`s with pitch 2×2). 1d. Rawler's remaining develop steps: `Demosaic → CropActiveArea`, producing a 3-channel
   float buffer in camera RGB.
2. `raw.rs::camera_to_linear_rec2020` applies white balance and `cam → linear Rec.2020` (via the camera's D65 matrix
   composed with `XYZ → linear Rec.2020`). No clip. 2a. **Phase 3.0: DNG `OpcodeList3` applied**. Post-color
   `WarpRectilinear` for lens distortion, `GainMap` if any. On iPhone ProRAW, the optional `WarpRectilinear` fires here.
   2b. **Phase 4.0: lens correction via `lensfun-rs`** (`color::lens_correction::apply_lens_correction`). Uses the
   bundled LensFun community database (~1,041 cameras, 1,543 lenses) to apply distortion (ptlens / poly3 / poly5), TCA
   (linear / poly3), and vignetting (pa model) in place on the linear Rec.2020 buffer. Matches on
   `raw.camera.make/model` + EXIF `lens_model/focal_length/fnumber`. Silent no-op when any of those is missing, when the
   lens isn't in the DB, or when the DNG's `OpcodeList3::WarpRectilinear` already fired (the caller tracks
   `warp_rectilinear_applied` from step 2a and skips to avoid double correction). Order within: vignetting → distortion
   → TCA.
3. Default crop. 3a. **Phase 6.1: chroma noise reduction** (`color::chroma_denoise::apply_default_chroma_denoise`).
   Splits linear Rec.2020 RGB into Y + Cb + Cr (Rec.2020 weights), blurs Cb and Cr with a small separable Gaussian
   (`σ = 1.5 px`, 11 taps), reconstructs RGB. Luma stays sharp; chroma smooths. Matches the silent chroma-NR default in
   Preview.app and Affinity. Toggleable via `flags.chroma_denoise` (default `true`); per-image output at `false` is
   bit-identical to pre-6.1.
4. `raw.rs::apply_exposure` lifts the linear buffer by the baseline EV picked by `baseline_exposure_ev` (DNG
   `BaselineExposure` tag first, fallback +0.5 EV, clamped to [-2, +2]). Linear-space multiply so relative luminance
   stays correct. 4a. **Phase 3.1: highlight recovery** (`color::highlight_recovery::apply_default_highlight_recovery`).
   Pixels whose brightest channel exceeds `DEFAULT_THRESHOLD` (0.95) are blended toward their own luminance via a
   smoothstep between threshold and `DEFAULT_CEILING` (1.20) in linear Rec.2020. In-gamut pixels pass through untouched.
   Keeps bright skies and specular highlights from drifting magenta/cyan when one channel clips while the other two keep
   rising. Runs post-exposure so it catches exposure-induced overflow too, and pre-tone-curve so the curve sees a
   hue-consistent input. 4b. **Phase 3.2 / 3.3 / 3.4: DCP** (`color::dcp::apply_if_available`). Finds a DCP matching the
   camera, either embedded in a DNG (Phase 3.3, preferred) or a standalone `.dcp` under `$PRVW_DCP_DIR` / Adobe Camera
   Raw (Phase 3.2, fallback). Applies its `ProfileHueSatMap` as a trilinearly-interpolated 3D LUT in linear-light HSV.
   Since Phase 3.4: dual-illuminant profiles blend `HueSatMap1` + `HueSatMap2` by the scene's estimated color
   temperature (compromise fidelity), and a `ProfileLookTable` fires after the HueSatMap when present. Silent no-op for
   files without a matching profile. Still deferred: `ForwardMatrix` swap, full iterative CCT convergence. See
   `docs/notes/raw-support-phase3.md`.
5. **Tone curve.** When the active DCP carries a `ProfileToneCurve` (Phase 3.4),
   `color::tone_curve::apply_tone_curve_lut` runs it via piecewise-linear interpolation on the pixel's Rec.2020
   luminance, then scales RGB uniformly by `Y_out / Y_in`, same hue-preserving pattern as the default curve. Otherwise
   `apply_default_tone_curve` shapes luminance with a mild filmic S-curve: shadow Hermite → midtone line (slope 1.08,
   anchored at 0.25) → highlight shoulder. Either way hue and chroma are preserved through the shoulder.
6. `color::saturation::apply_saturation_boost` scales each pixel's chroma around its luminance axis by `(1 + 0.08)` in
   linear Rec.2020 space. Preserves hue and luminance; adds the "vibrancy" Apple/Affinity bake in via per-camera tuning
   tables.
7. **Color conversion, branching on HDR.** SDR path (`hdr_active == false`): `raw::rec2020_to_sdr_display` calls
   `color::transform_linear_f32_to_display`, which changes the primaries into the user's display profile in f32, clips
   in the display's primaries, and lets `moxcms` apply only the display's curves (or a LUT-only profile's LUTs). A
   saturated channel reaches it above 1.0 (the SDR tone curve caps luminance, not channels), and clipping before the
   matrix washed those highlights pastel (`color/CLAUDE.md`). Clamp to [0, 1] on the way out to RGBA8. HDR path
   (`hdr_active == true`): `color::profiles::rec2020_to_hdr_display_inplace` applies a direct 3×3 matrix with **no
   clipping** (moxcms clips at 1.0, which eats the HDR headroom the whole path exists for). **Which matrix is the
   platform's call**, because the compositor has to be told what the numbers mean and the two platforms accept different
   answers: linear Display P3 on macOS, matching the `CAMetalLayer`'s `extendedLinearDisplayP3`, and linear BT.709
   (scRGB) on Windows, which is the only colour space DXGI presents an `Rgba16Float` swapchain as.
   `color::profiles::HdrDisplaySpace` owns that decision and `color/CLAUDE.md` argues it. HDR brightness gain
   (`flags.hdr_gain`, default 2.0) multiplies the buffer before the matrix to push scene-white content into the EDR
   headroom. Without it, HDR output reads timidly SDR-bright rather than "HDR-bright" against Preview. 7b. **Phase 5:
   HDR branch.** If the caller's `hdr_active` is true, skip the `[0, 1]` clamp and quantise the f32 buffer into
   `PixelBuffer::Rgba16F` (half-floats via the `half` crate), preserving values above 1.0 for the EDR-capable
   compositor. Sharpening still fires on this branch via `color::sharpen::sharpen_rgba16f_inplace` (same luminance-only
   unsharp mask as the 8-bit path, computed in f32 with no `[0, 1]` clamp so above-white highlights survive). Otherwise
   the SDR path below fires. 7c. **Phase 6.2: clarity (local contrast).**
   `color::clarity::apply_clarity_rgba8_inplace_with` runs a larger- radius (`σ ≈ 10 px`) separable-Gaussian unsharp
   mask on **luminance only**, lifting midtone features (shape silhouettes, textures) that survive display downscaling
   so the image reads crisper at every zoom level. Same math as capture sharpening, different defaults. Runs before step
   8 so the order is midtone lift → fine-edge sharpening. The HDR branch at 7b calls
   `apply_clarity_rgba16f_inplace_with` on the half-float buffer with the same semantics. Toggleable via `flags.clarity`
   (default `true`).
8. `color::sharpen::sharpen_rgba8_inplace` runs a mild unsharp mask on **luminance only** (Rec.709 weights) of the
   display-space RGBA8 buffer: separable Gaussian blur (σ = 0.8 px, 7 taps) on Y in f32, unsharp-mask formula on Y, then
   rescale RGB by `Y_out / Y_in`. Post-ICC rather than pre-ICC so we match the perceptual response of the gamma-encoded
   buffer and avoid halos. Luminance-only avoids the color fringes per-channel sharpening produces at colored edges.

The linear Rec.2020 `ColorProfile` is built programmatically in `color::profiles::linear_rec2020_profile`. No bundled
ICC file.

## HDR / EDR flow

Whether a decode lands in HDR or SDR is decided once, up front, by the caller, and every format reads the same answer:

```
hdr_active = color.hdr_output && display_hdr.capable     // App::hdr_active
```

`color.hdr_output` is Settings → Color → "HDR output" (default on). `display_hdr.capable` is whether the display can
show HDR at all (potential headroom above 1.0 on macOS, HDR mode on in Windows;
`render::display_hdr::DisplayHdr::from_info` reads it). `load_image` takes the result as `hdr_active` and hands it to
the backend; `OutputDepth::Rgba8` forces it off. Off, RAW develops SDR, a gain-map JPEG is its plain primary, and PQ,
HLG, and float HDR take the CPU's tone-mapped SDR rendition: every image renders as it did before HDR support.
`App::sync_hdr_active` is the one place that reacts to a flip (preloader, cache budget, cache flush), so toggling the
setting on an SDR display decodes nothing.

**Decoders never see the current headroom.** It moves with screen brightness on macOS, and a decode that read it would
have to re-run on every tweak; the shader takes it per frame instead (`render/CLAUDE.md`). `App` also tracks
`current_image_encoding` (an `EncodingKind`), and `App::want_edr_surface` fuses `hdr_active`, image mode, and
`EncodingKind::is_hdr()` to flip the window's surface between SDR and EDR modes per decode.

Once inside `raw::decode`, `hdr_active` gates four different behaviors:

1. **Tone-curve peak.** `DEFAULT_PEAK_HDR` (4.0) vs `DEFAULT_PEAK_SDR` (1.0) picks the filmic shoulder's asymptote. HDR
   lets above-1.0 content live; SDR clips at display-white.
2. **Color conversion** (see step 7 in the pipeline list above). A direct Rec.2020 → platform-HDR-space matrix for HDR;
   `color::transform_linear_f32_to_display` into the user's display ICC for SDR.
3. **HDR brightness gain.** `flags.hdr_gain` (default 2.0) multiplies the post-tone-curve buffer before the matrix,
   pushing scene-white into EDR headroom. SDR path ignores the knob. See `docs/notes/raw-support-phase5.md` Phase 5.2
   for rationale.
4. **Output format.** `rec2020_to_rgba16f` (preserving above-1.0) vs `rec2020_to_rgba8` (clamp + quantize). Sharpening /
   clarity pick the f16 or 8-bit variant to match.

The renderer side observes the same decision via `App::want_edr_surface` and calls
`color::display_profile::set_layer_edr_state` to flip the `CAMetalLayer` between
`kCGColorSpaceExtendedLinearDisplayP3 + RGBA16Float` (HDR) and the user's display ICC + `BGRA8Unorm_sRGB` (SDR). Two
diagnostic peak-value log lines (`peak linear value` pre-conversion, `peak post-ICC` post-conversion, plus `peak f16` on
the HDR-output summary line) make the decision audit-able without a debugger. If HDR looks wrong, the logs tell you
whether the pipeline clipped, quantized, or rendered the above-1.0 range away.

## Per-stage timing (Phase 6.4)

`raw::decode` instruments every pipeline stage with a `StageTimings` helper (private to `raw.rs`). Each stage mark emits
one `log::debug!` line, and a summary `log::debug!` line fires at the end:

```
RAW pipeline stages [293.2 ms total]: raw_image=4.4, opcode1=0.0, rescale=2.2,
  opcode2=0.0, demosaic=34.7, cam_matrix=6.0, opcode3=0.0, lens=83.0, crop=5.4,
  chroma_nr=49.3, exposure=2.8, hl_recovery=2.7, dcp=38.9, tone=7.7,
  hdr_diag_pre=2.6, saturation=2.4, color_conv=6.2, hdr_diag_post=2.2,
  to_rgba16f=4.7, clarity=14.0, sharpen=21.4, hdr_diag_f16=2.3 for …
```

Flag-gated stages with near-zero elapsed confirm their gate fired (useful sanity check). Turn it on with
`RUST_LOG=prvw::decoding::raw=debug`.

### Typical warm-decode budget on a 20 MP ARW

Reference numbers: `/tmp/raw/sample3.arw` (Sony α7R IV, 5456×3632 ≈ 20 MP), Apple Silicon M3 Max, release build, all RAW
flags at their defaults (HDR output active on an EDR-capable display, so the numbers below include the half-float path).
Re-run measurements for a fresh cold decode by flipping Settings → General → "Preload next/prev images" off and
restarting. The preloader otherwise warms caches and decode-time drops.

| Stage           | Warm ms | Notes                                                    |
| --------------- | ------: | -------------------------------------------------------- |
| `raw_image`     |     4.4 | Inside rawler: file parse + mosaic extraction            |
| `opcode1`       |     0.0 | DNG only; ARW no-op                                      |
| `rescale`       |     2.2 | rawler's black-level subtract + linear rescale           |
| `opcode2`       |     0.0 | DNG only; ARW no-op                                      |
| `demosaic`      |    34.7 | Inside rawler; bilinear (no Markesteijn)                 |
| `cam_matrix`    |     6.0 | camera → linear Rec.2020 (WB + color matrix in one pass) |
| `opcode3`       |     0.0 | DNG only; ARW no-op                                      |
| `lens`          |    83.0 | LensFun distortion + TCA + vignetting (SIMD, 6.3)        |
| `crop`          |     5.4 | Default crop                                             |
| `chroma_nr`     |    49.3 | σ=1.5 blur on Cb/Cr (SIMD, 6.1)                          |
| `exposure`      |     2.8 | Baseline EV lift                                         |
| `hl_recovery`   |     2.7 | Highlight recovery                                       |
| `dcp`           |    22.5 | DCP HueSatMap + LookTable (trilinear 3D LUT, SIMD 6.5)   |
| `tone`          |     7.7 | Filmic shoulder curve on luminance                       |
| `hdr_diag_pre`  |     2.6 | Info-level diagnostic (0.1 ms when info off)             |
| `saturation`    |     2.4 | Global chroma scale                                      |
| `color_conv`    |     6.2 | Rec.2020 → the platform's HDR space (HDR) or display ICC |
| `hdr_diag_post` |     2.2 | Info-level diagnostic (0.1 ms when info off)             |
| `to_rgba16f`    |     4.7 | f32 → half-float quantize                                |
| `clarity`       |    14.0 | σ=10 downsample fast path (6.4; was 144 ms pre-6.4)      |
| `sharpen`       |    21.4 | σ=0.8 luminance-only unsharp                             |
| `hdr_diag_f16`  |     2.3 | Info-level diagnostic (0.1 ms when info off)             |
| **total**       | **277** | Warm cache; cold decode ≈ 650 ms                         |

**How to read the table:** numbers are guide-rail order-of-magnitude, not a contract. They shift with image resolution,
scene content, cache state, thermal throttling, and flag combinations. The absolute values matter less than the
_ranking_: `lens`, `chroma_nr`, `dcp`, `demosaic`, `sharpen`, `clarity` are the six stages that dominate. Demosaic and
rawler parse live inside the `rawler` crate; everything else is our code.

**If you're optimizing:** start by reading the DEBUG summary line on your own hardware / input. If you see a stage
taking ≥ 2× the value here, that specific stage is the suspect. If every stage is proportionally higher, it probably
isn't our code. Check `log::info!` lines above for clues (e.g. "RAW applied DCP" means DCP matched and the trilinear 3D
LUT ran). See Phase 6.2 (clarity downsample) and 6.3 (lens-correction SIMD) in `docs/notes/raw-support-phase6.md` for
examples of how past per-stage wins were found and landed.

## Testing the RAW pipeline

The RAW pipeline has two kinds of tests:

- **Unit tests in `raw.rs`**: malformed bytes, cancellation. Cheap, always on.
- **Golden regression test in `mod.rs` (`synthetic_dng_matches_golden`).** Runs the full `load_image` path on
  `tests/fixtures/raw/synthetic-bayer-128.dng` (a tiny synthetic DNG, ~33 KB) and compares the RGBA8 output against a
  checked-in golden PNG via CIE76 Delta-E. Tolerances: mean < 0.5, max < 3.0. Runs on every platform: the target profile
  is `color::srgb_icc_bytes()`, which `moxcms` generates rather than reading from the OS.

Regenerating the golden after an intentional pipeline change:

```sh
cd apps/desktop
PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden
```

Delta-E lives in `src/color/delta_e.rs`. The `raw-dev-dump` example (`cargo run --example raw-dev-dump -- <raw_path>`)
dumps per-stage PNGs for visual inspection during pipeline development.

See `tests/fixtures/raw/README.md` for fixture details and `docs/notes/raw-support-phase2.md` for the pipeline-evolution
plan.
