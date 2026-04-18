# RAW pipeline roadmap

Quick checklist across phases. Detailed design notes live in
`raw-support-phase1.md` and `raw-support-phase2.md`. This file is the single
source for "what's done, what's next."

Last updated: 2026-04-17 (Phase 6.1 shipped).

## Phase 1 — shipped in v0.9.0 🎉

- [x] Decode DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW via `rawler`
- [x] Extension whitelist + directory-scan support for all 10 formats
- [x] macOS file associations: `Info.plist` UTIs for every supported format
- [x] Orientation from EXIF metadata (rawler hard-codes `RawImage.orientation`)
- [x] Cancellation checks at four RAW pipeline stage boundaries
- [x] Settings > File associations: two-section layout, tri-state master
      toggles, vendor labels, per-row handler transparency captions
- [x] Onboarding window: four-step checklist, SVG checkmarks, natural-language
      defaults sentence, left-aligned layout
- [x] Linux CI fix (gate macOS-only tests), Node 24 action bumps

## Phase 2 — shipped pre-v0.10.0 🎉

- [x] Test infrastructure: synthetic Bayer DNG fixture, CIE76 Delta-E util,
      golden-image regression test, `raw-dev-dump` example
- [x] Wide-gamut intermediate: linear Rec.2020 preserved end-to-end, no sRGB
      clip during color conversion
- [x] Baseline exposure: DNG `BaselineExposure` tag or +0.5 EV default, clamped
      to ±2 EV, applied in linear space
- [x] Default tone curve: piecewise Hermite S-curve, shadow lift + highlight
      shoulder, applied in linear Rec.2020
- [x] Capture sharpening: unsharp mask, σ = 0.8 px, amount = 0.3, applied
      post-ICC on RGBA8

## Phase 2.5 — in flight

### Phase 2.5a — structural fix (done, 2026-04-17)

- [x] Move tone curve to luminance-only (RGB → Y → curve → scale RGB).
      Preserves saturation at the highlight shoulder.
- [x] Move sharpening to luminance-only. Cleaner crispness, no color fringes.
- [x] Mild global saturation boost (default +8%, linear Rec.2020, post-tone
      pre-ICC).

### Phase 2.5b — empirical tuning (done, 2026-04-17)

- [x] Empirical parameter tuning harness: `apps/desktop/examples/raw-tune.rs`,
      grid-searches against reference PNGs, cross-validates across N files,
      ranks by mean-of-means Delta-E. Since the 2.5b rerun, the tuner
      Lanczos3-downsamples our output when the reference is smaller
      (Preview.app screenshot references land at fit-to-window resolution).
- [x] Structural refactor: `DEFAULT_MIDTONE_ANCHOR` + `apply_tone_curve`
      (parametric), `DEFAULT_SIGMA` / `DEFAULT_AMOUNT` +
      `sharpen_rgba8_inplace_with` (parametric). Production stays on
      `apply_default_*` wrappers; Phase 3's DCP work will thread per-camera
      values through the parametric entry points.
- [x] First pass: grid searched against `sips -s format png` references
      across three RAW files. Winning combo matched the Phase 2.5a
      educated-guess defaults (`anchor=0.25, amount=0.30, boost=+0.08`).
      Shipped unchanged. On visual QA against Preview.app on an M3 XDR
      display, the output read as "washed out and blurrier" — `sips` is
      Apple's conservative export path, not what Preview renders on screen.
- [x] Rerun: grid searched against a Preview.app screenshot of
      `sample3.arw` (single reference, 288 combos). New winner:
      `anchor=0.40, amount=0.30, boost=+0.00`. Beat the old defaults by
      0.81 Delta-E on sample3 and 0.50 Delta-E on sample1 (reference-vs-
      reference). Visual spot-check on sample1 and sample2 confirmed no
      broken output under the new defaults. Amount stays at 0.30 — the
      screenshot metric is resolution-limited and Delta-E couldn't
      distinguish amount 0.10 from 0.65; Phase 2.4's Laplacian measurement
      remains the authority. See `raw-support-phase2.md` for the full
      ranked table, per-axis sub-optima, and the single-reference overfit
      caveat.

## Phase 3 — per-camera color fidelity

### Phase 3.0 — DNG correctness (done, 2026-04-17)

- [x] DNG `OpcodeList1` application (pre-linearization gain maps + bad
      pixels). No fixture exercises it, but the pipeline slot is wired.
- [x] DNG `OpcodeList2` application (post-linearization, pre-demosaic
      CFA-level `GainMap`s + bad-pixel fix). Closes the iPhone ProRAW
      lens-shading correctness gap. Fires on sample2.dng (4 per-Bayer-
      phase GainMaps).
- [x] DNG `OpcodeList3` application (post-color `WarpRectilinear` for lens
      distortion + bad pixels). Fires on sample2.dng (1 WarpRectilinear).
- [x] DNG `LinearizationTable` investigation (tag 50712). **Rawler already
      applies this** in its own `apply_linearization` path during raw
      decode, so we skip reimplementing. Documented in
      `raw-support-phase3.md`.

See `docs/notes/raw-support-phase3.md` for per-opcode status, pipeline
diagram, and iPhone ProRAW specifics.

### Phase 3.1 — highlight recovery (done, 2026-04-17)

- [x] Desaturate-to-neutral highlight recovery in linear Rec.2020. Pixels
      whose brightest channel exceeds 0.95 are blended toward their own
      luminance via a smoothstep that lands at full desaturation by 1.20.
      Runs between exposure and tone curve; in-gamut pixels pass through
      untouched. Fixes the magenta / cyan drift that appeared in bright
      skies and specular highlights when one channel clipped while the
      other two kept rising. See `docs/notes/raw-support-phase3.md` for
      algorithm, parameter rationale, and smoke-test observations.

### Phase 3.2 — DCP profile support (done, 2026-04-17)

- [x] Parse Adobe `.dcp` files (TIFF-like `IIRC` container): extract
      `UniqueCameraModel`, `ProfileName`, `ProfileCopyright`,
      `ProfileCalibrationSignature`, `CalibrationIlluminant1/2`,
      `ProfileHueSatMapDims`, `ProfileHueSatMapData1/2`, and
      `ProfileHueSatMapEncoding`.
- [x] Apply `ProfileHueSatMap` as a trilinearly-interpolated 3D LUT in
      linear-light HSV (cyclic hue, clamped sat / val axes). Runs post-
      highlight-recovery, pre-tone-curve. ~35 ms on a 20 MP buffer,
      rayon-parallel.
- [x] DCP discovery: `$PRVW_DCP_DIR` + Adobe Camera Raw's default
      `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`.
      Matches by `UniqueCameraModel` (case- and whitespace-insensitive)
      or `ProfileCalibrationSignature`. Silent no-op when no DCP
      matches, so users without ACR installed see zero change.
- [x] End-to-end smoke test covers: (1) env unset = Phase 3.1 output,
      (2) env set but no match = Phase 3.1 output, (3) env set with
      match = visible color shift (57.8 % of bytes changed on our
      Sony ARW fixture, mean Δ = 3.16 per channel).

**Deferred** to Phase 3.x: `LookTable` (second LUT), `ProfileToneCurve`
(our default is already close to Adobe neutral), dual-illuminant
interpolation (we use D65 straight through), `ForwardMatrix1/2` swap
(our matrix already targets Rec.2020). See
`docs/notes/raw-support-phase3.md` for rationale and format details.

### Phase 3.3 — DCP embedded in DNG (done, 2026-04-17)

- [x] **Apply DCP data embedded in DNG files.** Smartphone DNGs (Pixel,
      Samsung Galaxy, iPhone ProRAW) and Adobe-converted DNGs carry
      `ProfileHueSatMapDims`, `ProfileHueSatMapData1/2`, and friends in
      their main IFD. New `color::dcp::embedded::from_dng_tags` reads
      them into the same `Dcp` struct the standalone parser produces, so
      `apply_hue_sat_map` runs unchanged. Embedded wins over filesystem
      DCP — the manufacturer's profile is authoritative. Non-DNG files
      and DNGs without profile tags are byte-for-byte identical to
      Phase 3.2. On sample2.dng (Pixel 6 Pro), the embedded profile
      produces a visible warmer / better-balanced output (63 % of bytes
      changed, mean |Δ| = 3.28). INFO log line spells out the source
      (`"RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' …"`).
      See `docs/notes/raw-support-phase3.md`.

### Phase 3.4 — DCP LookTable + tone curve + dual-illuminant (done, 2026-04-17)

- [x] DCP `LookTable` application (second HSV LUT after `HueSatMap`).
      Parses tags 50981 / 50982 / 51108 from both standalone `.dcp` files
      and embedded DNG IFDs. Applies via the existing `apply_hue_sat_map`
      so the LUT math path stays single-sourced. Silent no-op when the
      profile carries no LookTable.
- [x] DCP `ProfileToneCurve` (tag 50940). Parsed as `(x, y)` float pairs,
      applied via a new `tone_curve::apply_tone_curve_lut` helper that
      shapes luminance only and scales RGB uniformly, matching the
      default curve's pattern. When a DCP (embedded or filesystem)
      carries a tone curve, we apply **it instead of** our default —
      the camera's intended tonality wins. Logged at INFO so users can
      tell which curve ran.
- [x] DCP dual-illuminant interpolation. Compromise fidelity: simple
      `temp ≈ 7000 − 2000 × (R/G − 1)` scene-temperature estimate from
      rawler's `wb_coeffs`, linear blend between `HueSatMap1` and
      `HueSatMap2` weighted by where the estimate falls between the two
      illuminant temperatures (clamped outside the endpoints). The
      spec's full iterative procedure (ForwardMatrix1/2 + scene neutral
      + CCT convergence) remains future work — the compromise gets the
      direction and order-of-magnitude right, which is enough for a
      viewer. See `docs/notes/raw-support-phase3.md` for the algorithm
      choice and limitations.

### Phase 3.5 — bundled collection + fuzzy matching (done, 2026-04-17)

- [x] Bundle RawTherapee DCP collection (161 profiles, BSD-redistributable)
      at build time into a zstd-compressed blob (~10 MB binary delta).
      New search tier: embedded → PRVW_DCP_DIR → Adobe dir → bundled →
      fuzzy aliases → None.
- [x] Fuzzy DCP matching fallback via a curated `FAMILY_ALIASES` table
      (20 entries covering Sony, Fujifilm, Nikon, Canon, Olympus,
      Panasonic). When exact matching fails on all tiers, try each alias
      across filesystem then bundled tiers. Logs at INFO so users see the
      substitution. Conservative seed list — better to miss than mismatch.
- [x] Fuzzy-alias matches skip the whole DCP color stage (HueSatMap +
      LookTable). `apply_if_available` takes a new `allow_fuzzy: bool`
      parameter; the RAW pipeline passes `false`, so cross-sensor
      spectral-response mismatches no longer push skin tones and
      magentas toward "unrealistic vibrancy". Users who want the fuzzy
      profile applied anyway can drop an exact-match DCP under
      `$PRVW_DCP_DIR`. See `docs/notes/raw-support-phase3.md`.

### Phase 3.6 — DNG GainMap + bad-pixel spec compliance (done, 2026-04-17)

- [x] GainMap: honor `Planes > MapPlanes` fallback on the RGB path. When
      `MapPlanes < Planes`, the last gain-map plane now fans out to all
      remaining output planes, matching the CFA path's semantics and DNG
      spec § 6.2.2. No current fixture hits this; deferred from Phase 3.0
      commit `ecc9973`.
- [x] Bad-pixel opcodes: honor `bayer_phase`. `FixBadPixelsConstant` and
      `FixBadPixelsList` now sample neighbors at step 2 (same-phase only)
      instead of the unrestricted 3×3 neighborhood (step 1). New helper
      `same_phase_neighbor_offsets` returns the eight `{±2}` offset pairs.
      Deferred from Phase 3.0 commit `ecc9973`.

### Phase 3.7 — pipeline transparency settings (done, 2026-04-17)

- [x] `RawPipelineFlags` struct with one bool per stage (10 toggles across
      sensor, color, tone, detail). Defaults all-true reproduce today's
      pipeline bit-for-bit.
- [x] Threaded through `decoding::load_image(_cancellable)` →
      `raw::decode` → each stage and through `color::dcp::apply_if_available`
      so DCP HueSatMap / LookTable each have their own gate.
- [x] Settings → RAW panel with grouped per-stage toggles and a
      "Reset to defaults" button. Toggling flushes the image cache and
      re-decodes via a new `AppCommand::SetRawPipelineFlags`.
- [x] **Custom DCP directory** picker in the same panel. Writes to
      `Settings.custom_dcp_dir`, which `App` pushes into `$PRVW_DCP_DIR`
      so `color::dcp::discovery` honors it.
- [x] One INFO log line per decode when any flag is non-default, listing
      the disabled steps. Silent on the default path.

### Phase 3.x — still ahead

- [ ] DCP dual-illuminant, full fidelity: iterate ForwardMatrix1/2 +
      `AsShotNeutral` to converge a proper scene CCT instead of the
      one-shot WB-ratio approximation.

## Phase 4 — Lens correction (via lensfun-rs)

Complements Phase 3's color work. Phase 3 handles color fidelity (DCP,
HueSatMap, DNG opcodes); Phase 4 handles geometry — distortion, transverse
chromatic aberration, and vignetting. Different math, different data,
different upstream source of truth.

Approach: port LensFun's C++ core to pure Rust in a **separate crate**
(`github.com/vdavid/lensfun-rs`), then depend on it from Prvw. The port spec
is at `docs/notes/lensfun-rs.md` — 7,756 LoC of C++ with minimal deps,
~6-8 weeks focused work. Delivering it as a standalone crate keeps Prvw
pure Rust and gives the wider Rust imaging ecosystem its first LensFun.

### Phase 4.0 — integration (done, 2026-04-17)

- [x] `lensfun-rs` crate scaffolded and ported per
      `docs/notes/lensfun-rs.md`. LGPL-3.0. v0.1 covers distortion + TCA +
      vignetting for the 1,543 lenses and 1,041 camera bodies in LensFun's
      database. Bundled database ships inside the crate via `build.rs`
      (gzipped XML), so `Database::load_bundled()` has no runtime I/O.
- [x] Integrated into Prvw via `color::lens_correction` (new module).
      Looks up body + lens via rawler's metadata (`raw.camera.make/model`
      + EXIF `lens_model/focal_length/fnumber`), builds a `Modifier`, and
      applies vignetting → distortion → TCA in place on the linear
      Rec.2020 buffer. Pipeline slot matches DNG `OpcodeList3`'s —
      post-demosaic, pre-exposure. Skipped on DNGs whose
      `OpcodeList3::WarpRectilinear` already handled distortion (avoids
      double correction).
- [x] `RawPipelineFlags::lens_correction` toggle (defaults to `true`) and
      a "Geometry" section in Settings → RAW. Wires through the same
      `AppCommand::SetRawPipelineFlags` path as the other Phase 3.7
      toggles.
- [x] Smoke tested on sample1.arw + sample3.arw (Sony ILCE-5000 + E PZ
      16-50mm f/3.5-5.6 OSS): 66-71 % of output bytes change, visible
      barrel-distortion rectification, corner-vignette lift, and closed
      color fringing. sample2.dng (Pixel 6 Pro) is bit-identical
      with/without the toggle because its `OpcodeList3::WarpRectilinear`
      already baked in the manufacturer's correction — exactly the
      intended skip. See `docs/notes/raw-support-phase4.md`.

## Phase 5 — HDR / EDR output

### Phase 5.0 — filmic curve, f16 cache, SDR fallback (done, 2026-04-17)

- [x] Filmic Reinhard-style highlight shoulder asymptoting at 4.0 for EDR
      output (1.0 for SDR). Replaces the Phase 4 Hermite shoulder that
      clipped at 1.0. C¹ continuous with the midtone line at the highlight
      knee. SDR peak = 1.0 reproduces Phase 4 output bit-for-bit so
      non-EDR displays see no regression.
- [x] `DecodedImage.pixels: PixelBuffer` enum with `Rgba8(Vec<u8>)` and
      `Rgba16F(Vec<u16>)` variants. The RAW decoder emits half-float only
      when `hdr_output == true` **and** the display reports EDR headroom
      above 1.0. JPEG/PNG/WebP/etc. stay RGBA8 always.
- [x] EDR headroom query via
      `NSScreen.maximumExtendedDynamicRangeColorComponentValue`. Refreshed
      on `AppCommand::DisplayChanged`. Returns 1.0 on SDR displays / SSH /
      headless so the rest of the pipeline drops back to RGBA8 cleanly.
- [x] `RawPipelineFlags::hdr_output` (defaults to `true`). New toggle in
      Settings → RAW → "Output" so users on SDR or who dislike the wider
      shoulder can opt out per-pipeline.
- [x] Preloader cache budget auto-scales: 512 MB in SDR mode, 1024 MB in
      HDR mode. User decision: keep preload count at 6 for 20 MP RAWs by
      trading RAM rather than caching fewer images. See
      `docs/notes/raw-support-phase5.md` for the trade-off.
- [x] Renderer uploads RGBA16F half-float textures as
      `TextureFormat::Rgba16Float` — the shader samples as `vec4<f32>`
      either way.

### Phase 5.1 — surface format switch (done, 2026-04-17)

- [x] Switch the wgpu surface format to `Rgba16Float` when an HDR RAW is
      displayed on an EDR-capable screen, and set the three CAMetalLayer
      EDR properties in lockstep: `wantsExtendedDynamicRangeContent = YES`,
      `pixelFormat = MTLPixelFormatRGBA16Float`, and
      `colorspace = kCGColorSpaceExtendedDisplayP3`. Flips back to the
      SDR surface format + ICC colorspace on navigate-away, display
      change, or Settings toggle. Pipeline rebuild for the image-quad and
      overlay paths is extracted into format-agnostic helpers; the
      glyphon text renderer is rebuilt wholesale (its atlas pins format
      at construction). Shader module + pipeline layout are cached so
      rebuild is cheap. See `docs/notes/raw-support-phase5.md`.
- [x] Capture sharpening on the HDR path via
      `color::sharpen::sharpen_rgba16f_inplace`. Same luminance-only
      unsharp-mask algorithm as the 8-bit path, run in f32 without a
      `[0, 1]` clamp so above-white HDR highlights aren't pinned at 1.0.
      Toggling capture sharpening in Settings → RAW now actually
      changes the HDR preview on an EDR display; before, the HDR branch
      skipped the step unconditionally.

## Phase 6 — tuning knobs and performance polish

### Phase 6.0 — user-facing tuning sliders (done, 2026-04-17)

- [x] `RawPipelineFlags` gains three float knobs alongside the bools:
      `sharpen_amount` (0.0 – 1.0), `saturation_boost_amount` (0.0 – 0.30),
      and `midtone_anchor` (0.20 – 0.50). Defaults land on the existing
      `DEFAULT_AMOUNT` / `DEFAULT_SATURATION_BOOST` / `DEFAULT_MIDTONE_ANCHOR`
      constants, so at-default decoding is bit-identical to Phase 5.
- [x] Persisted via `serde` with `#[serde(default = …)]` so old
      settings.json files (missing the new fields) silently default. The
      decoder clamps knobs into their valid ranges once per decode via
      `RawPipelineFlags::clamp_knobs`, guarding against hand-edited values.
- [x] Threaded through `raw.rs` into `color::tone_curve::apply_tone_curve`,
      `color::saturation::apply_saturation_boost`, and
      `color::sharpen::sharpen_rgba{8,16f}_inplace_with` (both SDR and HDR
      branches).
- [x] New "Tuning" section in Settings → RAW, sitting between the "Output"
      toggle and the "DCP profile" row. Three NSSlider rows with a title,
      a description, the slider, and a 2-decimal numeric label.
      `setContinuous(false)` — the action fires once on mouse release, not
      on every pixel during drag, so a single gesture triggers a single
      re-decode. Reset-to-defaults snaps the sliders back in one atomic
      step.
- [x] Settings persistence round-trip tests cover the three floats at the
      `RawPipelineFlags` level and at the `Settings` level (JSON path).
- [x] Rationale for the three-knob shortlist:
      - **Sharpening amount** — Phase 2.4's Laplacian tuning concluded
        that amount is the most visually load-bearing sharpen parameter,
        not σ. σ (the blur radius) stays internal to keep the kernel
        cheap and predictable.
      - **Saturation boost** — the one knob that users with "Fuji-pop"
        or "muted" preferences reach for most. Post-tone-curve global
        chroma lift in linear Rec.2020, hue- and luminance-preserving.
      - **Midtone anchor** — lifts or crushes midtones without touching
        the shoulder. The filmic `DEFAULT_PEAK_SDR` / `DEFAULT_PEAK_HDR`
        values stay internal; peak is a display decision, not a taste
        decision.

See `docs/notes/raw-support-phase6.md`.

### Phase 6.1 — chroma noise reduction (done, 2026-04-17)

- [x] `color::chroma_denoise` module: Rec.2020 Y / Cb / Cr split,
      separable Gaussian blur (`σ = 1.5 px`, 11 taps) on Cb and Cr,
      RGB reconstruction. Luminance-preserving per pixel within f32
      rounding. Rayon-parallel, inner blur rows annotated with
      `#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]`
      and `f32::mul_add` for FMA hints.
- [x] `RawPipelineFlags::chroma_denoise` defaults to `true` — matches
      the silent chroma-NR default in Preview.app and Affinity Photo.
      Runs in linear Rec.2020 post-crop, pre-baseline-exposure. At
      `false`, per-image output is bit-identical to pre-6.1.
- [x] Settings → RAW → new "Denoise" section with one toggle. Tag
      constant `TAG_CHROMA_DENOISE = 160`. Row indices in the panel
      layout assembly bumped (`rows[11]` = chroma denoise, shifted
      `lens_correction` and `HDR output` down by one).
- [x] Golden regenerated (`synthetic_dng_matches_golden`). Synthetic
      fixture looks visually identical (one color boundary, tiny
      chroma drift). Real-sample smoke test on sample1 / 2 / 3 shows
      "same scene, slightly cleaner flats" with sharp edges intact.
- [x] Perf impact on the three samples: +25 ms on 12 MP sample2.dng,
      +58 ms on 20 MP sample1.arw, +72 ms on 20 MP sample3.arw. See
      `docs/notes/raw-support-phase6.md`.

### Phase 6.3 — SIMD-vectorize lens correction resampler (done, 2026-04-17)

- [x] `resample_distortion_row` and `resample_tca_row` extracted from the
      `apply_distortion_resample` / `apply_tca_resample` per-row rayon
      closures and annotated with `#[multiversion(targets("aarch64+neon",
      "x86_64+avx+avx2+fma"))]`. On aarch64 the compiler emits `fmadd`
      scalar NEON float ops throughout.
- [x] `sample_rgb_bilinear_fast`: branchless bilinear sampler using
      `f32::mul_add` for FMA hints. NaN/inf coords handled via `if` select
      (not multiply — `NaN × 0.0 = NaN`) so the loop body is branch-free.
- [x] `sample_single_channel_bilinear_fast`: per-channel variant for the TCA
      path. Eliminates 2/3 of the redundant `sample_rgb_bilinear` computation
      the original code did (3 calls each returning all 3 channels).
- [x] Measured per-row speedup on Apple M-series: distortion ≈1.0× (memory-
      bandwidth-bound on scatter-gather reads), TCA ≈1.6× (wins from single-
      channel sampling). Serial per-row benchmark in `resample_20mp_bench`
      (`#[ignore]`; run with `cargo test --release -- --ignored --nocapture`).
- [x] Three new correctness tests: `fast_sampler_matches_scalar_on_finite_coords`,
      `fast_sampler_nan_inf_returns_zero`, `single_channel_fast_matches_scalar`.
      All pass bit-for-bit (tolerance 1e-5, covering FMA rounding).

## Phase 7 — nice-to-haves, probably never

- [ ] Better Bayer demosaic: AMaZE or RCD instead of PPG. Editor-grade
      sharpness on edges. ~2000 LoC per algorithm.
- [ ] Fujifilm X-Trans demosaic: Markesteijn 3-pass instead of bilinear.
      ~1500 LoC.
- [ ] Foveon (Sigma X3F) proper development pipeline. rawler decodes but
      its develop path hits `todo!()` for Foveon photometrics.
- [ ] Optional luma NR (chroma NR shipped in Phase 6.1). Editor
      territory.
- [ ] Embedded JPEG preview fast-path: extract the JPEG baked into every
      RAW for instant first paint, upgrade to full decode in the
      preloader's background pass.

## Ancillary improvements

- [ ] Vendor RAW test fixture (≤ 5 MB, CC-licensed). Currently only synthetic
      DNG is checked in.
- [ ] SIMD (NEON) the sharpen kernel. Current perf is ~60 ms on 20 MP; a
      NEON pass would land around ~20 ms.
- [ ] Smaller, better-than-perceptual gamut mapping. moxcms's matrix-only
      profiles don't differentiate intents — fine for in-gamut content, less
      so for highly saturated subjects.

## Sequencing principle

Ship each phase independently. Each phase is a coherent improvement; no phase
should block on a later one. Phase 2.5 before Phase 3 because structural fixes
should land before the big DCP investment. Phase 4 after 3 because HDR math is
easier once color fidelity is right.
