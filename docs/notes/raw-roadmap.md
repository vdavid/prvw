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

### Phase 2.5b — empirical tuning

- [ ] Sharpen amount tuning: currently 0.3, target around 0.4-0.5 after
      luminance-only change.
- [ ] Tone curve midtone anchor tuning: currently 0.25 (over-bright), target
      around 0.35-0.45 after luminance-only change.
- [ ] Saturation boost tuning: currently +8%, data-drive the final value.
- [ ] Empirical parameter tuning harness: grid-search against Preview.app or
      `sips` reference output, report Delta-E per combo, pick winner. Or
      env-var overrides for live eyeball tuning.

## Phase 3 — per-camera color fidelity

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
