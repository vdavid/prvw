# RAW pipeline roadmap

Quick checklist across phases. Detailed design notes live in
`raw-support-phase1.md` and `raw-support-phase2.md`. This file is the single
source for "what's done, what's next."

Last updated: 2026-04-17.

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

- [ ] Retune defaults against a wider reference set. The 2.5b rerun grid-
      searched against a single Preview.app screenshot (a vibrant outdoor
      scene with a subject). Likely scene-class gaps: portraits / skin
      tones, low-light / high-ISO, near-neutral scenes. Collect three to
      five more Preview.app screenshot references and rerun the grid.
- [ ] DNG `OpcodeList1` application (pre-demosaic gain maps, vignette fix,
      bad pixels). Closes the iPhone ProRAW correctness gap.
- [ ] DNG `OpcodeList2` application (post-demosaic lens distortion via
      `WarpRectilinear`, bad-pixel fix).
- [ ] DNG `OpcodeList3` application (post-color, rarely used in practice —
      may skip).
- [ ] DNG `LinearizationTable` application (tag 50712). Nikon NEFs use this
      heavily; currently we rely on rawler's per-decoder LUT handling.
- [ ] Highlight recovery: reconstruct blown channels from unclipped ones
      (desaturate-to-neutral or channel-blend).
- [ ] DCP profile support: parse Adobe `.dcp` files, apply `HueSatMap` 3D LUT,
      per-camera tone curve. Biggest single quality lift for portraits.
- [ ] DCP discovery: bundle common profiles or read from the user's
      `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`.

## Phase 4 — HDR / EDR output

- [ ] Don't clip highlights to 1.0 during tone curve. Shape the shoulder to
      asymptote around 2-4× (classical filmic tone mapping).
- [ ] Output in a float16 pixel format to the wgpu surface.
- [ ] `CAMetalLayer.wantsExtendedDynamicRangeContent = YES`.
- [ ] Read current EDR headroom via
      `NSScreen.maximumExtendedDynamicRangeColorComponentValue` and adapt the
      shoulder dynamically per frame.
- [ ] Graceful SDR fallback: re-clamp to 1.0 when no EDR headroom is
      available (external SDR monitor, battery save, etc.).

## Phase 5 — nice-to-haves, probably never

- [ ] Better Bayer demosaic: AMaZE or RCD instead of PPG. Editor-grade
      sharpness on edges. ~2000 LoC per algorithm.
- [ ] Fujifilm X-Trans demosaic: Markesteijn 3-pass instead of bilinear.
      ~1500 LoC.
- [ ] Foveon (Sigma X3F) proper development pipeline. rawler decodes but
      its develop path hits `todo!()` for Foveon photometrics.
- [ ] Noise reduction: mild chroma NR by default, optional luma NR.
      Editor territory.
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
