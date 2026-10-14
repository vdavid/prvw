# HDR images: decisions, measurements, and evidence

Background for HDR stills beyond RAW: gain-map JPEGs, PQ and HLG PNGs, float TIFFs, and the 16-bit SDR path. How the
code works lives next to it: `apps/desktop/src/decoding/CLAUDE.md` ("16-bit SDR PNGs and TIFFs", "Gain-map JPEGs", "PQ
and HLG PNGs, and float TIFFs", "HDR / EDR flow"), `apps/desktop/src/render/CLAUDE.md` (the shader modes, the two
uniform groups, the headroom poll, the parity harness), and `apps/desktop/src/color/CLAUDE.md` (`color/hdr/`, the
working spaces, the tone curve). This note holds the numbers behind those decisions, the alternatives that lost, and
what's still unverified. Measurements are on the MacBook Pro M3 Max (XDR panel), release builds, unless they say
otherwise; all are from 2026-10-13 and 2026-10-14.

## Why gain maps and tone mapping run in the shader

macOS changes the EDR headroom continuously with screen brightness. A CPU-side gain map or tone map would need a full
re-decode on every brightness change; in the shader, a headroom change is one uniform write. It also keeps a gain-map
image at 8 bits per channel plus a small map, so the preload cache holds about as many of them as SDR images. Skia
(Chrome, Android) applies gain maps the same way.

Accepted costs: a second HDR mechanism next to RAW's CPU path (RAW stays on the CPU, because its HDR comes out of the
develop, not out of a map), the GPU parity harness, and care around color spaces.

The budgets the measurements below are held against:

- **SDR images: no regression.** An 8-bit JPEG, PNG, or TIFF decodes within noise (±3%) of the path before HDR support.
- **Gain-map JPEG, 12 MP, HDR display:** at most 1.3× the plain SDR decode of the same file, memory within 10%.
- **Gain-map JPEG on an SDR display:** identical to the SDR decode (the map is never read).
- **PQ or HLG PNG, 12 MP:** the added CPU work (light, peak histogram, half-float pack) under 25 ms.
- **Headroom change:** under one frame, no decode, cache untouched.
- **GPU cost:** two extra texture samples (four loads for the gain map), a few 3×3 multiplies, and some `exp2`/`pow` per
  fragment, only when rendering. Not measured; it hasn't dropped a frame.

## Display headroom, per platform

**macOS.** wgpu's `DisplayHdrInfo::headroom` carries all three `NSScreen` values (`current` =
`maximumExtendedDynamicRangeColorComponentValue`, `potential` = `maximumPotential...`, `reference` =
`maximumReference...`, read in `wgpu-hal`'s `metal/surface.rs`), and `tone_map_headroom()` returns `current`.

- A scratch Swift probe read current 1.0, potential 16.0, and reference 0.0 (AppKit's "unknown"). With a RAW on screen
  and the EDR layer on, polled every 500 ms for eight seconds: potential 16.0 and current 1.2 on every read, reference
  `None`. An awake screen at high brightness reports a small current headroom, not a flat 1.0.
- ❌ Don't use wgpu's `coarse.high_dynamic_range` on macOS: wgpu derives it from `current > 1.0`, which is the gate that
  used to decode RAWs as SDR at launch when current read 1.0.

**Windows.** wgpu reads `IDXGIOutput6::GetDesc1` plus `DISPLAYCONFIG_SDR_WHITE_LEVEL` on every call (`wgpu-hal`'s
`auxil/dxgi/hdr.rs`; the factory is cached, the monitor lookup and the adapter/output walk aren't).

- `coarse.high_dynamic_range` is "the output's color space is G2084 or G10", which is HDR mode on: the potential side.
  "Headroom > 1.0" would be wrong there, because a panel whose EDID peak sits at or below its SDR white reads as not
  capable once `display_hdr` filters values under 1.0.
- `tone_map_headroom()` is 1.0 in SDR mode, else `max_nits / sdr_white_nits`, which moves with the SDR brightness
  slider: the current side.
- That per-call walk is why `Renderer::display_hdr` is one query returning both numbers, and why polling every 500 ms is
  fine where polling per frame wouldn't be.

**Neither platform announces a brightness or SDR-white change.** Nothing in wgpu or the platform APIs turned up, so the
headroom is polled. An SDR JPEG makes one read at launch and none after.

## 16-bit SDR PNGs and TIFFs

**Banding, measured.** A 4,096-wide 16-bit red ramp, sRGB → Display P3, compared with `moxcms`' f32 transform of the
same samples: the 8-bit path (round to 8 bits, then the transform) is up to 1.55 steps off, and 48% of samples land a
step away from single rounding. The 16-bit path is under 0.1 steps and 1.7% (`moxcms`' 16-bit transform sits a few
hundredths of a step off its f32 one, which tips samples sitting on a rounding boundary). Anchors in
`decoding/generic.rs`.

**Decode cost** (`sixteen_bit_decode_perf`, `#[ignore]`): 12 MP (4000×3000) RGB16, a smooth gradient plus noise, median
of seven. `OutputDepth::Rgba8` is the 8-bit path, so one binary compares both. Quietest run:

- PNG, Display P3 target: 8-bit 81.1 ms, 16-bit 87.0 ms (+7%). sRGB target (transform skipped): 63.5 → 64.1 ms.
- TIFF (uncompressed), Display P3 target: 8-bit 24.2 ms, 16-bit 31.5 ms (+30%, +7 ms). sRGB target: 7.3 → 7.7 ms.
- Stage split of the 16-bit path: `into_rgba16` 2.2 ms, the 16-bit ICC transform 23 ms, the EOTF into f16 1.7 ms
  (rayon). The added cost is almost all `moxcms`' 16-bit transform being slower than its 8-bit one.
- Two reruns ran at a load average of about 50 from other sessions and scattered from −9% to +85%. Rerun on an idle
  machine before quoting the TIFF number anywhere or deciding whether +30% matters.
- Memory: a 16-bit image costs 8 B/px in the cache and on the GPU, double an 8-bit one. `image_memory_cost` counts real
  bytes, so the budget holds and fewer of them fit.

**Not done: a float surface for 16-bit SDR.** The SDR surface stays 8-bit; the 16-bit buffer removes the double
rounding, which is where the visible banding came from, and the output still quantizes to 8 bits once. If a gradient on
the XDR panel still bands, the next step is an `Rgba16Float` surface while a 16-bit image is on screen, so more than 8
bits reach the compositor. That needs the shader to output display-encoded values on a float surface whose layer color
space is the display ICC: new surface plumbing.

## Gain-map JPEGs

### What `ultrahdr-core` gives us, and what it doesn't

- **`ultrahdr-core` 0.6.0 and `zencodec` 0.1.26 parse neither `hdrgm` XMP nor MPF.** Their docs say both moved to
  `zenjpeg::container`, and `zenjpeg` (0.8.4) is `AGPL-3.0-only OR LicenseRef-Imazen-Commercial`. `Error::XmpParse` and
  `Error::MpfParse` are leftovers. What's left and permissive: `parse_iso21496_fmt` (from `zencodec`, re-exported), the
  Apple trio (`parse_exif_for_apple_hdr`, `parse_apple_makernote`, `from_apple_headroom`), and `zencodec::Metadata::xmp`
  as raw bytes only. Hence our own MPF and XMP parsers.
- ❌ **Never `ultrahdr-rs`**: it bundles `zenjpeg` (AGPL) and turns on `ultrahdr-core`'s AGPL defaults.
- **The license guard is `cargo-deny`.** `Cargo.lock` lists the AGPL `zentone`, because `ultrahdr-core`'s `std` feature
  names it weakly (`zentone?/std`), though cargo never builds it. `cargo-deny` resolves features the way the build does:
  it passes with `features = ["std"]`, and with `"tonemap"` added it fails on `zentone`'s license (both verified).
- **Where the per-pixel math lives upstream**, for whoever bumps the crate: `GainMapLut::new` (gain =
  `exp((log_min + pow(v, 1/gamma) × (log_max − log_min)) × weight)`, clamped to [2⁻³⁰, 2³⁰], non-finite → 1.0) and
  `apply_simd::apply_gain_row_presampled` (`out = (sdr + base_offset) × gain − alternate_offset`).
  `apply_gain_row_scalar` exists but is a single-channel shortcut without offsets; it's not what we port.
- **`calculate_weight` returns 1.0 whenever the alternate headroom is at or below the base's**, so an HDR-base map
  always gets the full gain, and `backward_direction` is never read. Ours takes libavif's
  `clamp((log2 H − base) / (alt − base), 0, 1)` for the HDR base (1.0 at H = 1, 0.0 at H = 2^base) and is tested against
  `calculate_weight` for an SDR base only. Tests in `color/hdr/mod.rs` pin the upstream behavior, so an upgrade that
  changes it fails loudly.

### Real files, and how they're built

- **iPhone (iOS 18+):** see `apps/desktop/tests/fixtures/hdr/README.md` for the committed iPhone Air file. Its ISO
  reading says 1.77 stops; upstream's maker-note formula said 2.08, Apple's own says 1.7678 (below).
- **Pixel (Ultra HDR):** MPF plus `GContainer`, primary XMP with `hdrgm:Version` only, the map's XMP carrying the
  parameters as attributes, single-channel maps: 816×614 for an 8160×6144 base (a non-integer 10.006 ratio vertically)
  and 768×1020 for 3072×4080.

### Map sampling: exact Shepard

Plain bilinear on linear gains (let alone hardware bilinear on encoded values) misses
`ultrahdr_core::gainmap::apply_gainmap` by 2.5% mean and 82% max on a busy map, and the GPU parity test catches the
hardware-sampler version at a texel edge (4% off). The exact port is 3e-5 mean and 5e-4 max on a busy synthetic map, and
6e-4 mean on the real iPhone file (its dark, noisy channels sit near the 1e-3 floor of the relative-error measure). The
cell-edge rounding that put 23% errors on the iPhone file is in `render/CLAUDE.md`.

### The identity snap stays at 1e-3

Apple's Display P3 profile (as written into an iPhone file) is 3.5e-4 off `moxcms`' P3; `moxcms` against itself is about
1e-5. A 1e-4 tolerance brought the channel leak back, and two iPhone tests failed. `IDENTITY_TOLERANCE`'s comment states
the worst case 1e-3 lets through.

### Apple's maker-note flavor

- **Headroom.** `ultrahdr-core`'s last branch is `-0.44 × maker48 + 2.86`; Apple's published one ("Applying Apple HDR
  effect to your photos", developer.apple.com, fetched 2026-10-14) is `-0.303 × maker48 + 2.303`. On the iPhone Air
  fixture (maker33 1.01, maker48 1.766) Apple's gives 1.7678 stops, exactly the file's ISO `AlternateHeadroom`
  (1.767799); upstream's gave 2.08, about 24% too bright at the peak. ImageIO agrees: an Apple-only copy of the fixture
  (its ISO XMP namespace and `HDRGainMapHeadroom` renamed in a scratch copy, so ImageIO reports no ISO map) fits best at
  1.77 stops in a sweep from 1.4 to 1.95. The fixture's two readings agree to 2e-3 stops, and the synthetic Apple
  fixture's maker-note headroom is 2.1515.
- **Curve.** Apple reconstructs `hdr = sdr × (1 + (headroom − 1) × g)`, linear, no offsets; upstream maps it onto ISO's
  log2 curve with 1/64 offsets.
- **The map's transfer is gamma 2.2, not Rec.709.** Apple's article says to linearize the map with the Rec.709 transfer.
  A Swift probe applied candidate curves to ImageIO's own SDR decode and aux map of an Apple-only iPhone 14 Plus photo
  and compared with ImageIO's HDR decode (`kCGImageSourceDecodeToHDR`). Mean relative error: gamma 2.2 0.5%, sRGB curve
  2.4%, gamma 2.4 3.2%, Skia's `pow(v, 1.961)` 5.5%, Rec.709's inverse OETF 13%, upstream's log2 curve 22%.
- **Weight at a partial headroom: log2, like every other flavor**, Skia's choice (`exp(log(1 + (H − 1)·g) × W)` in
  `SkGainmapShader`). The alternative, weighting the headroom alone (`1 + (H^w − 1)·g`), keeps midtones brighter at low
  headroom, and nothing we have says Apple does it.
- **Whole-decode check.** The Apple-only iPhone 14 Plus photo (orientation 6, half-res map) matches ImageIO's HDR decode
  through our whole decode to 0.54% mean relative error at full headroom; a second file from the same phone, 0.67%. Both
  have maker48 0 (3.0 stops on either formula), so they check the curve and the map's turn, not the constants.

### Cost

`gain_map_decode_perf`, `#[ignore]`, median of seven, load average 5–8:

- 12.5 MP (Pixel, 3072×4080): SDR 33.8 ms, gain map 34.8 ms (1.03×) with a P3 display target; 50.5 against 34.0 ms
  (0.67×) with an sRGB one, since the gain-map path's P3 transform is free for a P3 base and the display's isn't. Memory
  50.1 MB against 53.3 MB (+6.2%, the map's sixteenth).
- 50 MP (Pixel, 8160×6144): 88.6 against 89.2 ms (1.01×) with P3, 156.8 against 88.8 ms (0.57×) with sRGB; memory +1.0%
  (that map is a hundredth).

## PQ and HLG PNGs, and float TIFFs

### Reference values

1.0 = 203 nits (BT.2408 reference white). PQ code 0.5806 ≈ 203 nits, 0.7518 ≈ 1,000 nits, 1.0 = 10,000 nits. HLG's 75%
signal is reference white. The curves are pinned against BT.2100's and BT.2408's published values, not against a real
file (see "Not verified yet").

### `png` and `cICP`

`png` 0.18.1's decoder fills `Info::coding_independent_code_points`, but its encoder's `write_header` ignores that field
(and the mastering and content-light ones).
`writer.write_chunk(png::chunk::cICP, &[primaries, transfer, matrix, full_range])` right after `write_header()`
round-trips (decoded `Some(9, 16, 0, true)`); `make-hdr-fixtures` writes `cICP` and `cLLI` that way.

### Edge decisions

- **PQ and HLG stay `SceneLinear` with HDR active whatever their peak**; only float TIFFs take the SDR shortcut.
  Unusable primaries (reserved, unspecified) on a PQ or HLG PNG are taken as BT.2020.
- **An SDR float TIFF converts in f32**, then stores the 16-bit SDR shape. Running the 16-bit transform would have
  quantized linear light to 16 bits before the transform. "SDR" is a brightest pixel at or under 1 + 1/1024, so white
  through luminance weights that sum to 1.0000001 still counts.
- **EETF edges:** the 0.1% of pixels above the content peak clamp onto the target with it; the knee never goes below 0
  and the spline's output never above `maxLum` (both only matter past about 10⁸ nits, where the spline would turn back);
  black-lift normalization drops PQ(0), which is 7e-7.
- **The WGSL can't name a variable `target`** (a reserved word). naga's error says so.

### Diffuse white on an SDR target

BT.2390 with the target at SDR white puts 203-nit white at 0.90 (400-nit master), 0.78 (1,000), 0.63 (4,000), and 0.55
(10,000) of SDR white: 0.15 to 0.87 stop dim, with midtones compressed from 24 nits up for the brightest masters. That's
structural: BT.2390's knee `KS = 1.5 × maxLum − 0.5` is exactly the highest its slope-1 PQ-domain Hermite allows without
its slope going negative near the peak, so no other knee fixes it inside BT.2390. With the white-anchored shoulder
(`color/CLAUDE.md`), diffuse white sits at 0.900–0.903 of SDR white for 1,000- to 10,000-nit masters (about −0.15 stop,
243 of 255 in sRGB), and nothing under 0.8 × SDR white moves.

Alternatives that lost:

- **BT.2446 Method C**: an HDR-to-SDR conversion for broadcast, with its own crosstalk and a different look from what
  the GPU shows.
- **A shoulder only on the CPU target:** the SDR fallback would then disagree with the same file at headroom 1 on an HDR
  display. The shader makes the same choice per fragment (one extra PQ decode), so the CPU fallback and the GPU at a low
  headroom (an MBP at full brightness reads about 1.2) agree, and the parity test covers headrooms 1 and 1.5, where the
  shoulder runs.

Anchors: `diffuse_white_stays_near_sdr_white_on_an_sdr_target_at_any_peak`,
`the_curve_is_monotonic_and_smooth_across_peaks_and_headrooms` (four masters by six headrooms, slope 1 at the knee),
`bt2390_runs_where_its_knee_leaves_white_alone`.

### HLG: the fixed 1,000-nit OOTF, then the tone map

BT.2100's HLG is display-referred: a display at `Lw` nits would use system gamma `1.2 + 0.42 × log10(Lw / 1000)` and
need no tone map. Prvw renders HLG at its nominal 1,000-nit display and tone-maps from there, because:

1. One light for both renditions: the CPU SDR rendition and the GPU at headroom 1 stay the same image, which a
   per-target gamma would break (the GPU's target moves with brightness, and its OOTF would have to move with it in the
   shader).
2. With the white-anchored shoulder, HLG's reference white lands at 0.90 of SDR white on an SDR target, so the dimming
   that made a display-adaptive gamma attractive is gone.
3. Every HLG file is mastered for the 1,000-nit reference display the 1.2 gamma belongs to; rendering it there first is
   the faithful starting point.

**The alternative, if HLG's SDR rendition looks off:** on the CPU fallback only, render with the display-adaptive gamma
at `Lw` = 203 nits (BT.2100's extended formula gives about 0.95) and skip the tone map. It would match a native HLG SDR
display more closely, at the cost of the SDR rendition no longer matching headroom 1 on an HDR display.

### Cost

`pq_decode_perf`, `#[ignore]`, median of seven, load average 5–10, 12 MP RGB16 with a smooth gradient plus noise:

- Added work on the HDR path (light through a 64K lookup, the peak histogram, the half-float pack, one parallel pass):
  **PQ 2.9 ms, HLG 8.1 ms** (HLG's OOTF is a `powf` per pixel). `into_rgba16` of the RGB16 decode is another 2.0 ms.
- The `cICP` probe every PNG pays: 1.3 µs.
- The CPU SDR fallback: 27 ms on top (tone map 21, `moxcms` f32 3.8, quantize 2.7). A version that set the EETF up per
  pixel, round-tripped every pixel through PQ, and ran the transform and quantize on one thread measured 115 ms; that's
  why `ToneCurve` sets up once and returns pixels under the knee untouched, and why the f32 transform runs in rayon
  chunks. This gradient puts most pixels above the knee; a photo spends less in the tone map.
- Whole decodes of the same file (15 ms with HDR, 50 ms through the fallback) flatter the PNG inflate: the synthetic
  gradient compresses far better than a photo, so don't quote those as a real file's.

### The peak-4 float TIFF and "it doesn't glow"

At headroom 5 the fixture's ramp reaches `[4.00, 4.00, 4.00]` and its highlight `[3.79, 2.84, 1.73]` (linear Display P3)
through the real GPU pipeline; the log reads `Float TIFF: content peak 4.000× SDR white, brightest 4.000×, HDR`. The PQ
and HLG fixtures look brighter because they peak at 4.93 and their BT.2020 highlight's red reaches 5.5, past most
headrooms. At a low current headroom (bright ambient, or the first frames before macOS grants EDR) everything compresses
toward 1.0 and a neutral ramp reads as plain white. Check `/state`'s `pixel_encoding` and the headroom before calling it
a bug.

## Linear f32 into an SDR display: clip after the matrix

**The scene-linear fallback.** `moxcms`' in-place f32 transform clamps each input channel to [0, 1] before its matrix,
so a saturated BT.2020 orange the tone map left at R ≈ 2.6 came out salmon in sRGB (`[255, 164, 102]` against the true
`[255, 114, 91]`). `the_sdr_fallback_clips_in_the_displays_primaries` checks a saturated orange and an out-of-sRGB green
against an independent reference.

**RAW's SDR develop, measured before and after switching it** to `transform_linear_f32_to_display`:

- Saturated Rec.2020 highlights into sRGB, clip-before vs clip-after: `[1.2, 0.5, 0.2]` → `[255, 177, 110]` vs
  `[255, 173, 109]`; `[2.0, 0.6, 0.1]` → `[255, 196, 51]` vs `[255, 175, 33]`; `[1.5, 1.4, 0.2]` → `[255, 255, 91]` vs
  `[255, 255, 67]`; `[0.9, 0.9, 1.3]` → `[243, 243, 255]` vs `[240, 243, 255]`. In-gamut and negative-only cases are
  identical.
- A 24 MP Sony A7 III ARW into sRGB: 8.7% of its pixels reach the conversion with a channel above 1.0 (up to 3.68), and
  4.1% change on screen: 865,000 by up to four codes, 104,000 by 5–10, 15,000 by 11–20, and 3,000 (0.012%) by more, up
  to 107. The big ones are saturated blue lights (Rec.2020 blue 2–3): pale sky blue before, a saturated blue-cyan now,
  the way the HDR path shows them (`[0.22, 0.33, 3.06]` was `[90, 157, 255]`, now `[0, 153, 255]`).
- The golden `synthetic-bayer-128.dng` into sRGB: 481 of 16,384 pixels move, up to 8 codes (114 by one). None of them
  reach the conversion above 1.0; the 378 with a negative Rec.2020 channel (down to −0.124, deep reds and magentas) are
  what moves, and clarity and sharpening spread it to their neighbors (419 changed with both off). The golden was
  regenerated (the old one read max ΔE 3.96 against the new output, past the test's 3.0 bound).
- Cost: the conversion went from 7.2–8.6 ms to 9.4–10.7 ms on 24 MP (the extra matrix pass); a whole SDR decode of the
  ARW measured 475–586 ms both before and after, so it's under the noise (`raw_sdr_decode_perf`, `#[ignore]`).

## The setting

"HDR output" has no menu item, though the other color settings do: it's a set-and-forget preference.

## Known gaps and quirks

- **Crossfades follow the incoming image's surface for the whole fade.** `App::display_from_cache` runs
  `prepare_display` (which applies the surface state) before `begin_crossfade`; both bind groups share one layout, so
  the reconfigure doesn't break the fade. SDR → HDR: the outgoing SDR texture (display-ICC bytes, sRGB-decoded) lands on
  the linear extended Display P3 surface read as linear P3, close only when the display profile is near P3 (the MacBook
  case). HDR → SDR: an outgoing RAW HDR, `SceneLinear`, or gain-mapped image lands on `Bgra8UnormSrgb` with the display
  ICC as color space, its linear light read as display-encoded (dark, clipped at 1.0), for one fade. The cleaner rule
  would keep the EDR surface while either image is HDR and convert the `DisplaySdr` one in the shader; not done.
- **Windows HDR has never run on a real HDR display.** The scRGB surface, the SDR-white scale, and the headroom poll are
  unit- and GPU-tested only. The overlay pills and text draw through their own pipeline and stay unscaled on an EDR
  surface, so they look dim next to the image on Windows HDR.
- **No real PQ or HLG file has been checked.** The fixtures are ours; any real one is worth a look.
- **An Ultra HDR photo is only checked locally.** CI's one real-world gain-map input is the iPhone Air fixture.

## Real files for checking by hand

Not committed; the committed ones are in `apps/desktop/tests/fixtures/hdr/README.md`.

- **Pixel (Ultra HDR v1 XMP with MPF):** David's photos at
  `~/Downloads/takeout-unique-samples/photo-27 [Photos from 2025] PXL_20250219_153250675.jpg` and
  `photo-28 [Photos from 2025] PXL_20251008_104424078.jpg`. Personal: ❌ never commit them.
- **iPhone, original camera JPEGs on Wikimedia Commons** (it keeps uploads byte for byte). The file names name the phone
  in the picture, not the camera; trust the EXIF `Model`.
  - Apple flavor only (maker-note headroom, `urn:com:apple:photo:2020:aux:hdrgainmap` map at half resolution, no ISO
    metadata), iPhone 14 Plus, 4032×3024, 3.0 MB, CC BY-SA 4.0:
    https://upload.wikimedia.org/wikipedia/commons/2/23/About_iPhone_15_Pro_Max_Natural_Titanium.jpg
  - Apple flavor only, a second one from the same phone, CC BY-SA 4.0:
    https://upload.wikimedia.org/wikipedia/commons/c/cb/Back_view_of_iPhone_14_Pro_Max_Deep_Purple.jpg
  - Apple and ISO 21496-1 both present, iPhone 16e, 4284×5712 portrait, 3.9 MB, CC BY 4.0:
    https://upload.wikimedia.org/wikipedia/commons/e/e3/IPhone_15_pro_model.jpg
- **macOS oracle:** ImageIO decodes gain-map JPEGs to HDR (`kCGImageSourceDecodeRequest` = `kCGImageSourceDecodeToHDR`,
  macOS 15+), and `CGImageSourceCopyAuxiliaryDataInfoAtIndex` reports which maps a file carries. A throwaway Swift
  script reading a few pixel values is an independent reference; keep it out of the repo.

**On the XDR panel**, with brightness well below full (the headroom is what's left above it) and Preview side by side:
gain-map highlights should glow like Preview's; dragging the brightness slider should adapt them within about half a
second with no re-decode (`RUST_LOG=prvw::app=debug,prvw::decoding=info` shows `Display headroom now: …` lines and no
new `Decoded …` line); on an SDR monitor, or with "HDR output" off, every image shows its SDR rendition.

## How AVIF plugs in later

The design leaves AVIF almost nothing new to build:

- SDR AVIF: `avif-decode` → our own `colr` box reader (ICC or CICP; neither crate exposes it, verified 2026-10-13) →
  `moxcms` (`new_from_cicp` for CICP) → `DisplaySdr`.
- PQ/HLG AVIF → `SceneLinear` (mode 3), unchanged.
- Gain-map AVIF (`tmap` derived item) → `GainMapped` (mode 2), with `ultrahdr-core`'s ISO 21496-1 parser.

Background from the AVIF discussion: `avif-decode` 3.0.0 decodes a 12 MP AVIF in about 115 ms on the M3 Max with
`rav1d`'s assembly off (its aarch64 assembly fails to build: `memorysafety/rav1d#1449`).
