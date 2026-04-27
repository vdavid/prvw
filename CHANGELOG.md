# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning:
[Semantic Versioning](https://semver.org/).

## [0.12.0] - 2026-04-27

### Added

- **RGB histogram overlay.** Toggle via View → Histogram or bare `H`. Top-right of the window, rounded translucent
  backdrop matching the existing pill style, off by default. Hover the histogram for per-bin R/G/B counts. Computed
  lazily on first toggle and cached per-image; rayon-parallel scan handles 20+ MP buffers in a few ms. Persists
  across launches ([d811bb32](https://github.com/vdavid/prvw/commit/d811bb32),
  [a6c833c6](https://github.com/vdavid/prvw/commit/a6c833c6)).
- **EXIF info overlay.** Toggle via View → Exif info or bare `E`. Below the histogram with a small gap when both
  visible, same width and backdrop. Curated photographer-friendly grouping: camera, exposure triplet
  (`1/250 s  f/2.8  ISO 400`), lens, date taken, image dimensions, software, GPS. Hidden entirely on formats with no
  EXIF data. Long values wrap and grow the panel while preserving the row pitch. Backed by `nom-exif` for JPEG /
  Generic and `rawler::RawMetadata::exif` for RAW ([419c6d54](https://github.com/vdavid/prvw/commit/419c6d54),
  [a6c833c6](https://github.com/vdavid/prvw/commit/a6c833c6),
  [6846416a](https://github.com/vdavid/prvw/commit/6846416a)).
- **Loop navigation.** Toggle via Navigate → Loop navigation or bare `L`. At the last image Next wraps to the first
  and vice versa. The preloader is wrap-aware too — toggling on near a directory edge triggers preloads of the
  wrap-side indices, toggling off evicts them, so the cache always matches the active window. Off by default,
  persists ([88b37740](https://github.com/vdavid/prvw/commit/88b37740)).
- **Home / End jump to first / last image.** Navigate menu gains "Go to first" (Home) and "Go to last" (End)
  entries, with separators grouping the menu by intent. Absolute jumps; loop navigation does not affect them
  ([1e8c316c](https://github.com/vdavid/prvw/commit/1e8c316c)).
- **Dev-only `screenshot_window` MCP tool** that captures the entire native window — overlays, vibrancy, modal
  panels — by shelling out to `/usr/sbin/screencapture -l <windowID>`. Compile-time gated to debug builds; release
  builds neither register the tool nor link the dispatch arm. Requires Screen Recording permission on first use
  ([9954a91d](https://github.com/vdavid/prvw/commit/9954a91d)).
- Website download buttons now show the file size next to the architecture
  ([b3b17f46](https://github.com/vdavid/prvw/commit/b3b17f46)).

### Changed

- **Markdown / prose formatting unified across the monorepo via `oxfmt`** (Cmdr's setup). Replaces Prettier; runs
  from repo root over `docs/`, every `CLAUDE.md`, root markdown, and website source. Auto-fixes locally, checks in
  CI ([a30c1e5d](https://github.com/vdavid/prvw/commit/a30c1e5d),
  [cdaa80af](https://github.com/vdavid/prvw/commit/cdaa80af),
  [47742664](https://github.com/vdavid/prvw/commit/47742664)).
- Histogram and EXIF overlay backdrop alpha bumped from 0.55 to 0.66 for legibility against bright images
  ([6846416a](https://github.com/vdavid/prvw/commit/6846416a)).
- `workflow_dispatch` can now trigger website deploys directly from the Actions tab
  ([446b8ae0](https://github.com/vdavid/prvw/commit/446b8ae0)).

### Fixed

- **Stale `prvw://state` snapshot on background preloads.** Found while building loop navigation: `poll_preloader`
  only updated shared state when a `Ready` matched the pending current index, so neighbor preloads left
  `cache_indices` (and other fields) stale until the next user action. Now updated on every preload arrival
  ([88b37740](https://github.com/vdavid/prvw/commit/88b37740)).
- Histogram briefly showed the previous image's curve on cache-miss navigation while the QuickLook thumbnail
  placeholder was on screen. `display_thumbnail_placeholder` now clears the cached histogram data so the right
  histogram appears once the full image arrives ([a6c833c6](https://github.com/vdavid/prvw/commit/a6c833c6)).
- Pricing card on the website was clipping the Download dropdown via `overflow-hidden`; the 1px accent line is now
  inset within the rounded corners so the parent doesn't have to clip
  ([c1e467ec](https://github.com/vdavid/prvw/commit/c1e467ec)).

## [0.11.0] - 2026-04-26

### Added

- **Blurry thumbnail placeholder while the full image decodes.** Cache-miss navigation now uploads the QuickLook
  thumbnail through the same display pipeline as the full image, so zoom, EDR, and auto-fit match. New `thumbnails/`
  module with a centered-outward scheduler ([80fa5102](https://github.com/vdavid/prvw/commit/80fa5102)).
- **Parallel pixel-dimensions prefetcher.** A 16-thread worker reads dimensions for every index in
  `current ± WINDOW_RADIUS` upfront, so placeholder display drops from 200 ms – 1.3 s on slow shares to 1 – 2 ms
  post-warmup. Three-tier dispatcher: `image::image_dimensions` for PNG/GIF/BMP, a single-buffer JPEG parse for one SMB
  round trip, ImageIO for the rest ([d02e61d7](https://github.com/vdavid/prvw/commit/d02e61d7)).
- Updater now surfaces "update available" on no-file launches from Finder / Dock too; download is deferred to viewer
  init so an admin prompt can't interrupt onboarding ([0ee6b337](https://github.com/vdavid/prvw/commit/0ee6b337)).

### Changed

- **Navigation no longer freezes on slow network shares.** QuickLook submission moved off the main thread to a dedicated
  `prvw-thumbgen` worker (was costing ~150 ms per submit on SMB), completion blocks batch into a shared queue so a
  38-thumb burst wakes winit 1 – 2 times instead of 38, and `mark_ready` no longer pre-reads source dims (~7 s of
  main-thread blocking on a 38-thumb burst, now lazy) ([16d63b46](https://github.com/vdavid/prvw/commit/16d63b46)).
- **Thumbnail scheduler prioritises immediate neighbours first**, then outside-window, then current. The previous order
  left the next-arrow target without a placeholder for ~5 s after launch on slow folders
  ([d02e61d7](https://github.com/vdavid/prvw/commit/d02e61d7)).
- **Updater bundle swap is now atomic** via `renamex_np(RENAME_SWAP)` — no window where the app is absent or partial,
  and a crash mid-update leaves one intact bundle either way
  ([0b52f850](https://github.com/vdavid/prvw/commit/0b52f850)).
- **Updater deregisters the old bundle from Launch Services before swapping** so the "Open With" menu no longer shows
  both old and new Prvw side by side after an update ([bb0bbbe0](https://github.com/vdavid/prvw/commit/bb0bbbe0)).
- Updater only runs on bundles installed in `/Applications`; dev builds and `~/Downloads` copies are skipped
  ([0ee6b337](https://github.com/vdavid/prvw/commit/0ee6b337)).
- Website tagline + features section refreshed ([0664402f](https://github.com/vdavid/prvw/commit/0664402f)); terms and
  conditions updated ([5bae3ce1](https://github.com/vdavid/prvw/commit/5bae3ce1)).

### Fixed

- Escape now closes About, Onboarding, and Settings windows again. The hidden Escape button was excluded from
  `performKeyEquivalent:` traversal; switched to borderless + zero-size frame
  ([dc8ee8da](https://github.com/vdavid/prvw/commit/dc8ee8da)).
- Integration tests no longer flake from a port-keyed `PRVW_DATA_DIR`; each `TestApp` now gets its own
  `tempfile::TempDir` ([5fd0f0de](https://github.com/vdavid/prvw/commit/5fd0f0de)).

## [0.10.0] - 2026-04-19

### Changed

- **Navigation is now near-instant in RAW folders.** End-to-end preloader rework that routes the current image through
  the background pipeline on cache miss instead of blocking the main thread. Preload priority is direction-aware
  (`N+1, N+2, N-1, N-2` forward, mirrored backward), in-flight decodes for indices still wanted keep their cancellation
  token, a 30 ms wheel-spin debounce coalesces rapid input, and out-of-window cache entries are evicted on every
  navigation. The wgpu `Texture` for the displayed image is now explicitly `destroy()`'d on swap (fixes 4 GB+ RSS bloat
  on long sessions) and file I/O moved to an abandonable detached thread so a wedged network share can't block the
  caller ([da9408a4](https://github.com/vdavid/prvw/commit/da9408a4),
  [128f7ee7](https://github.com/vdavid/prvw/commit/128f7ee7),
  [88e590d4](https://github.com/vdavid/prvw/commit/88e590d4),
  [4619f8ec](https://github.com/vdavid/prvw/commit/4619f8ec),
  [f0009a04](https://github.com/vdavid/prvw/commit/f0009a04)).
- **Preloader pool switched from a custom rayon pool to a single dedicated `std::thread` worker** so rawler's internal
  `par_iter` (demosaic, chroma_nr, sharpen) inherits the global pool instead of starving on a 1-thread custom one. 20 MP
  ARW preload now decodes in ~300 – 450 ms instead of ~2 s ([9408804b](https://github.com/vdavid/prvw/commit/9408804b)).
- **Preloader debug logs are now structured and greppable** (`Initiated loading 9.jpg (N+1, 10/17)` /
  `Fully loaded ... in 25ms` / `Evicted ... - 2.5 MB freed (out of window)`). Enable with
  `RUST_LOG=prvw::navigation=debug` ([aecd9390](https://github.com/vdavid/prvw/commit/aecd9390)).
- **Clarity ~10× faster on 20 MP** (Phase 6.4): for σ ≥ 4 and images ≥ 1 MP, the local-contrast pass now downsamples
  luma 4×, blurs at σ' = σ/4, then bilinearly upsamples — the Gaussian-blurred signal is low-frequency by design. ~14 ms
  instead of ~144 ms; total per-decode budget on a 20 MP HDR ARW drops from ~650 ms to ~293 ms warm
  ([fb982318](https://github.com/vdavid/prvw/commit/fb982318)).
- **DCP ~1.6× faster** (Phase 6.5): the per-camera `HueSatMap` / `LookTable` LUT apply is now `multiversion`-annotated
  (NEON / AVX2+FMA) with branchless HSV conversion and `mul_add` on all seven trilinear lerps. ~36 ms → ~22 ms per 20 MP
  decode ([c29fdba8](https://github.com/vdavid/prvw/commit/c29fdba8)).
- **HDR-diagnostic log scans now run only when info logging is active**; the three peak-value scans are gated behind
  `log_enabled!(Info)` and parallelised on the HDR branch. ~40 ms saved per HDR decode when info logging is off
  ([696da51d](https://github.com/vdavid/prvw/commit/696da51d)).
- **RAW defaults retuned against Affinity / Preview** (Phase 6.1.1): `DEFAULT_SATURATION_BOOST` 0.08 → 0.18,
  `DEFAULT_MIDTONE_ANCHOR` 0.40 → 0.45, new `baseline_exposure_offset` slider (default +0.73 EV). Settings layout
  co-locates each slider under its matching toggle ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91)).
- Lens correction resampler is now SIMD-accelerated (NEON / AVX2+FMA via `multiversion`); TCA path measured ~1.6× faster
  on M-series ([aa919dd4](https://github.com/vdavid/prvw/commit/aa919dd4)).
- Capture sharpening now runs on the HDR RGBA16F path too, not just SDR. Operates in f32 with no [0, 1] clamp so
  above-white highlights survive ([4e76bcc8](https://github.com/vdavid/prvw/commit/4e76bcc8)).
- DCP `HueSatMap` / `ProfileToneCurve` are auto-skipped when the match is fuzzy-alias only (different sensor, same
  family); the two halves are now independent toggles ([ca38eab4](https://github.com/vdavid/prvw/commit/ca38eab4),
  [76ca2786](https://github.com/vdavid/prvw/commit/76ca2786)).
- DNG `GainMap` now applies to all channels when `Planes = 1`, matching Adobe's reference; tightens bad-pixel opcode
  spec compliance ([237cba78](https://github.com/vdavid/prvw/commit/237cba78)).
- HDR decode INFO log now spells out EDR grant + post-pipeline peak values so "is HDR flowing end-to-end" is auditable
  without a debugger ([6c9fc03f](https://github.com/vdavid/prvw/commit/6c9fc03f)).

### Added

- **HDR / EDR output for RAW, visible on XDR** (Phase 5.0 + 5.1). Filmic Reinhard shoulder asymptotes at 4× on
  EDR-capable displays and at 1.0 on SDR (SDR bit-identical to Phase 4). On HDR images, the wgpu surface reconfigures to
  `Rgba16Float` and `CAMetalLayer` enters EDR mode with `extendedDisplayP3`. New "Output" group in Settings → RAW; EDR
  headroom queried via `NSScreen.maximumExtendedDynamicRangeColorComponentValue`
  ([01e53506](https://github.com/vdavid/prvw/commit/01e53506),
  [ce0e93ca](https://github.com/vdavid/prvw/commit/ce0e93ca)).
- **HDR brightness gain slider** (Phase 5.2) at 0.5 – 4.0 (default 2.0) pushes scene-white into EDR headroom so HDR
  output reads genuinely "HDR-bright" on XDR / OLED, matching Preview.app and Photos. SDR path ignores it
  ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91)).
- **Lens correction for non-DNG RAWs** (Phase 4.0): distortion, TCA, and vignetting from the LensFun community DB
  (~1,041 cameras, 1,543 lenses). Powered by the pure-Rust `lensfun-rs` port, bundled DB, no runtime I/O
  ([66386f45](https://github.com/vdavid/prvw/commit/66386f45)).
- **Clarity (local contrast) for RAW** (Phase 6.2): larger-radius unsharp on luminance before capture sharpening, σ ≈ 10
  px. Lifts midtone features that survive display downscaling. New toggle + radius/amount sliders in Settings → RAW →
  Detail; on by default ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91)).
- **Chroma noise reduction for RAW** (Phase 6.1): Gaussian blur on Cb/Cr in linear Rec.2020 with luma sharp. σ = 1.5 px
  (11-tap), the same polish Preview.app and Affinity apply silently. New "Denoise" section in Settings → RAW; on by
  default ([9e44c7da](https://github.com/vdavid/prvw/commit/9e44c7da)).
- **RAW tuning sliders** (Phase 6.0): three continuous-valued sliders for sharpening amount, saturation boost, and tone
  midtone anchor. One re-decode on mouse release; values persist in `settings.json`
  ([9ea0fdae](https://github.com/vdavid/prvw/commit/9ea0fdae)).
- **RAW Settings panel** (Phase 3.7): per-stage toggles for the 10 RAW pipeline steps, custom DCP directory picker, and
  "Reset to defaults". Defaults reproduce today's output bit-for-bit
  ([dd834f17](https://github.com/vdavid/prvw/commit/dd834f17)).
- **Bundled DCP library** (Phase 3.5): 161 community-contributed RawTherapee profiles packed into the binary at build
  time (~10 MB via zstd) as a fourth search tier. Plus fuzzy DCP family matching for known-compatible cameras (Sony
  α5000 → α6000, etc.) ([ed0787e1](https://github.com/vdavid/prvw/commit/ed0787e1)).
- **DCP `LookTable`, `ProfileToneCurve`, and dual-illuminant interpolation** (Phase 3.3): closes the three Phase
  3.2-deferred items so the camera's intended tonality wins
  ([30347086](https://github.com/vdavid/prvw/commit/30347086)).
- **DCP profiles embedded in DNG files** (Phase 3.2.1): every Pixel, Galaxy, iPhone ProRAW, and Adobe-converted DNG
  ships a `ProfileHueSatMap`; embedded wins over a matching filesystem DCP
  ([2979a055](https://github.com/vdavid/prvw/commit/2979a055)).
- **Adobe DCP support** (Phase 3.2): if a `.dcp` matches the camera's `UniqueCameraModel` in `$PRVW_DCP_DIR` or Adobe
  Camera Raw's default dir, Prvw applies its `ProfileHueSatMap` as a trilinear LUT in linear-light HSV
  ([bf188289](https://github.com/vdavid/prvw/commit/bf188289)).
- **Highlight recovery** between baseline-exposure lift and tone curve. Pixels above 0.95 in linear Rec.2020 blend
  toward their luminance via a smoothstep finishing at 1.20, so bright skies and specular highlights stop drifting
  magenta / cyan when one channel clips ([cf24edf4](https://github.com/vdavid/prvw/commit/cf24edf4)).
- **DNG `OpcodeList1/2/3`** per Adobe's spec 1.6 — `GainMap`, `WarpRectilinear`, `FixBadPixelsConstant`,
  `FixBadPixelsList`. iPhone ProRAW now renders with correct lens-shading and optical distortion correction
  ([ecc99733](https://github.com/vdavid/prvw/commit/ecc99733)).
- RAW pipeline test infrastructure: synthetic Bayer DNG fixture, `color::delta_e` CIE76 metric, golden regression test,
  `raw-dev-dump` example ([706a400b](https://github.com/vdavid/prvw/commit/706a400b)).
- **RAW per-stage timing + benchmark table** (Phase 6.4): one DEBUG log line per stage plus a comma-separated summary,
  with a reference table in `apps/desktop/src/decoding/CLAUDE.md`. New "Preload next/prev images" toggle disables
  background preloading so cold-start decodes can be measured cleanly
  ([fb982318](https://github.com/vdavid/prvw/commit/fb982318)).

### Changed

- **RAW decode preserves wide-gamut color end-to-end.** Previously rawler's pipeline clipped to sRGB; new pipeline runs
  rawler's demosaic only, applies our white balance + camera matrix into linear Rec.2020, and lets moxcms transform to
  the display profile ([35998bdf](https://github.com/vdavid/prvw/commit/35998bdf)).
- RAW decode now applies a baseline exposure lift in linear Rec.2020 before the ICC transform; clamped to ±2 EV
  ([51460ec5](https://github.com/vdavid/prvw/commit/51460ec5)).
- Default tone curve between exposure lift and ICC transform — mild filmic S, monotonic, endpoint-preserving
  ([fa4a04f1](https://github.com/vdavid/prvw/commit/fa4a04f1)).
- Capture-sharpening pass on the display-space buffer as the last step before orientation (separable Gaussian σ = 0.8
  px, 7 taps + unsharp at amount 0.3) ([a30fe79c](https://github.com/vdavid/prvw/commit/a30fe79c)).
- RAW tone curve and capture sharpening now act on luminance only, with a mild global saturation boost between them —
  preserves hue and chroma where per-channel passes were producing color fringes
  ([e71b850d](https://github.com/vdavid/prvw/commit/e71b850d)).
- RAW defaults retuned against a Preview.app screenshot rather than `sips` output: midtone anchor 0.25 → 0.40,
  saturation boost 0.08 → 0.00 → 0.08 (restored), sharpen amount 0.30
  ([03e60478](https://github.com/vdavid/prvw/commit/03e60478),
  [d6c0f469](https://github.com/vdavid/prvw/commit/d6c0f469),
  [4c555581](https://github.com/vdavid/prvw/commit/4c555581)).

### Fixed

- **Lens correction polarity was doubling distortion instead of correcting it.** Sign reversed in
  `apply_distortion_resample`, so barrel correction turned into amplification on some lenses
  ([146da606](https://github.com/vdavid/prvw/commit/146da606)).
- **HDR / EDR output now actually renders HDR-bright on XDR.** Phase 5.1 routed half-floats through `moxcms` with a
  gamma-encoded P3 layer that clipped above-1.0 values; on the HDR path, bypass `moxcms` and apply a direct linear
  Rec.2020 → linear Display P3 matrix, then flip `CAMetalLayer` to `extendedLinearDisplayP3`
  ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91)).
- **Scrolling through a folder of RAWs no longer stutters.** Priority-zero render now defers until the decode lands on
  the main thread via `poll_preloader`, with a clean "Loading…" title in the meantime
  ([da9408a4](https://github.com/vdavid/prvw/commit/da9408a4)).
- **iPhone / Pixel DNGs no longer render with a radial red cast.** Phase 3.0's `GainMap` applier treated `Plane` as a
  CFA color index; per spec 1.6 §6.2.2 it indexes into photometric image planes. Now scales every pixel the rect and
  pitch select ([723d1433](https://github.com/vdavid/prvw/commit/723d1433)).

## [0.9.0] - 2026-04-17

### Added

- **Camera RAW support** via `rawler`: DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW. Pipeline includes black/white
  level correction, PPG demosaic for Bayer (bilinear for X-Trans), white balance, camera matrix with Bradford CAT, sRGB
  gamma, NEON SIMD on Apple Silicon. EXIF orientation pulled separately since rawler hard-codes it
  ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a)).
- File associations for all 10 RAW UTIs in `Info.plist` (16 document types total)
  ([c2f5b567](https://github.com/vdavid/prvw/commit/c2f5b567)).
- Settings → File associations: split into "Standard image formats" and "Camera RAW formats" with vendor labels, master
  "Set all" toggle with tri-state Mixed indicator ([c2f5b567](https://github.com/vdavid/prvw/commit/c2f5b567)).

### Changed

- Onboarding window: redesigned into a four-step checklist with a custom green checkmark rendered at runtime via
  `NSBezierPath`. Step 4's copy adapts to whether Prvw is the default viewer
  ([4596d0e1](https://github.com/vdavid/prvw/commit/4596d0e1)).
- Decoding module: split single-file `decoding.rs` into `decoding/` with per-backend files plus `dispatch.rs` and
  `orientation.rs` ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a)).
- CI: macOS-only modules gated behind `#[cfg(target_os = "macos")]` so cross-platform builds compile cleanly
  ([e9b5de49](https://github.com/vdavid/prvw/commit/e9b5de49),
  [3f009797](https://github.com/vdavid/prvw/commit/3f009797),
  [815b7275](https://github.com/vdavid/prvw/commit/815b7275),
  [96218dd4](https://github.com/vdavid/prvw/commit/96218dd4)).

### Fixed

- `apply_orientation` underflowed on zero-width or zero-height input for EXIF orientation 2; now early-returns
  ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a)).
- Restored per-row handler transparency in Settings → File associations
  ([4596d0e1](https://github.com/vdavid/prvw/commit/4596d0e1)).

## [0.8.0] - 2026-04-17

### Added

- Settings window: sidebar layout with General, Zoom, Color, and File associations sections; cross-dependencies disable
  dependent toggles automatically ([dc43505a](https://github.com/vdavid/prvw/commit/dc43505a),
  [0dd48491](https://github.com/vdavid/prvw/commit/0dd48491)).
- File associations panel: per-UTI toggles, master "Set all", 1 s polling of handler state, previous-handler rollback on
  toggle off ([0dd48491](https://github.com/vdavid/prvw/commit/0dd48491),
  [17b76a34](https://github.com/vdavid/prvw/commit/17b76a34)).
- Rendering intent toggle (View menu, Cmd+Shift+R); persisted as `use_relative_colorimetric`
  ([b42814f0](https://github.com/vdavid/prvw/commit/b42814f0)).
- Scroll-to-zoom toggle (off by default). When off, scroll navigates between images; Cmd+scroll always zooms
  ([d55b7e9a](https://github.com/vdavid/prvw/commit/d55b7e9a)).
- Pinch-to-zoom on trackpad, cursor-centred ([ef8d0bfe](https://github.com/vdavid/prvw/commit/ef8d0bfe)).
- Keyboard shortcuts for zoom: Cmd+= / Cmd+- / Cmd+0 ([ec2aba4a](https://github.com/vdavid/prvw/commit/ec2aba4a)).
- Title bar toggle (on by default): reserves a 32 px strip so the filename and zoom pills don't cover the image
  ([64e0d87b](https://github.com/vdavid/prvw/commit/64e0d87b)).
- Title bar vibrancy: Liquid Glass on macOS 26, classic frosted glass on older versions; opaque black in fullscreen
  ([7eede14b](https://github.com/vdavid/prvw/commit/7eede14b)).
- Integration test suite (17 tests), each spawning its own app instance on a dynamic port
  ([0dd48491](https://github.com/vdavid/prvw/commit/0dd48491)).

### Changed

- Source layout: flat `src/` with infrastructure (`app/`, `render/`, `platform/`) and features as siblings; each feature
  owns its runtime state via a `State` struct ([27eca5e5](https://github.com/vdavid/prvw/commit/27eca5e5),
  [e88027ba](https://github.com/vdavid/prvw/commit/e88027ba)).

### Fixed

- Closing the onboarding window now quits the app — no-file launches no longer left the event loop running with nothing
  visible ([e81bbdfa](https://github.com/vdavid/prvw/commit/e81bbdfa)).
- CGColor / CGColorSpace encoding crashes in `setColorspace:` and the Settings separator: `msg_send!` was encoding these
  as `^v` instead of the proper struct types; fix uses raw `objc_msgSend`
  ([17b76a34](https://github.com/vdavid/prvw/commit/17b76a34)).

## [0.7.0] - 2026-04-16

### Added

- **ICC color management.** Embedded source profiles (JPEG, PNG, TIFF, WebP) transform to accurate output. L1 → sRGB, L2
  → actual display profile via `CGDisplayCopyColorSpace`. Display changes flush the cache and re-decode
  ([ee226acc](https://github.com/vdavid/prvw/commit/ee226acc),
  [94820a8a](https://github.com/vdavid/prvw/commit/94820a8a)).
- View menu toggles: "ICC color management" (Cmd+Shift+I) and "Color match display" (Cmd+Shift+C)
  ([a0883307](https://github.com/vdavid/prvw/commit/a0883307),
  [b952b64c](https://github.com/vdavid/prvw/commit/b952b64c)).

### Changed

- ICC engine swapped from lcms2 to moxcms (pure Rust, NEON SIMD). 24 MP transform: 247 ms → 45 ms on M3 Max. No C
  toolchain needed for cross-compilation ([f568b18f](https://github.com/vdavid/prvw/commit/f568b18f)).

### Fixed

- Screen detection now uses `NSWindow.screen.deviceDescription` for the authoritative `CGDirectDisplayID` instead of
  unreliable `current_monitor()` + `CGDisplayBounds` position matching
  ([fcdefe3c](https://github.com/vdavid/prvw/commit/fcdefe3c)).
- Pre-existing BGRA → RGBA swap bug in screenshot capture ([ee226acc](https://github.com/vdavid/prvw/commit/ee226acc)).

## [0.6.3] - 2026-04-15

### Fixed

- **Finder double-click finally works.** `NSAppleEventManager` was being overridden by AppKit's `NSDocumentController`;
  now uses ObjC runtime method injection (`class_addMethod`) to add `application:openURLs:` directly to winit's
  `WinitApplicationDelegate` class ([329ba2a8](https://github.com/vdavid/prvw/commit/329ba2a8)).

### Changed

- Zoom model uses logical pixels: zoom = 1.0 means one image pixel per logical pixel. The overlay correctly shows 100 %
  for naturally-sized images on Retina (was 200 %) ([a32f3950](https://github.com/vdavid/prvw/commit/a32f3950)).
- Compiler-enforced `Logical<T>` / `Physical<T>` newtypes prevent mixing logical and physical pixel values
  ([80d795a0](https://github.com/vdavid/prvw/commit/80d795a0)).
- Removed 329 lines of dead modal onboarding code ([86460872](https://github.com/vdavid/prvw/commit/86460872),
  [9ebca1e4](https://github.com/vdavid/prvw/commit/9ebca1e4)).

## [0.6.2] - 2026-04-15

### Fixed

- Finder double-click "cannot open files in JPEG Image format": `CFBundleTypeRole` was missing from `Info.plist`
  document type entries ([6664a2ff](https://github.com/vdavid/prvw/commit/6664a2ff)).
- CI: added `libxdo-dev` to Linux apt-get for `muda` ([bf82cb74](https://github.com/vdavid/prvw/commit/bf82cb74)).
- Auto-updater: call `lsregister -f` after replacing the `.app` so macOS picks up new document types in future updates
  ([b518d438](https://github.com/vdavid/prvw/commit/b518d438)).

## [0.6.1] - 2026-04-15

### Fixed

- Finder double-click now works: macOS sends file paths via Apple Events (not CLI args), but the app was exiting before
  the event loop started. Event loop now always runs, with a 500 ms wait for Apple Events before showing onboarding
  ([5b4d86a9](https://github.com/vdavid/prvw/commit/5b4d86a9)).

### Changed

- Onboarding window is now non-modal (doesn't block the event loop), so Apple Events and QA commands arrive while it's
  showing ([5b4d86a9](https://github.com/vdavid/prvw/commit/5b4d86a9)).
- Code refactors: `scale_factor` stored on App, `TextBlock` builder pattern, `MonitorBounds` helper, `LogicalF64` /
  `LogicalF32` aliases ([e7fb92c8](https://github.com/vdavid/prvw/commit/e7fb92c8)).

## [0.6.0] - 2026-04-15

### Added

- **Auto-fit window:** window resizes to match each loaded image, centred on screen. Clamped to min 200 px, max 90 % of
  monitor. Toggle in View menu and Settings ([6a8e03db](https://github.com/vdavid/prvw/commit/6a8e03db)).
- Auto-fit zoom: when auto-fit is on, zooming in/out resizes the window to match the zoomed image; cursor pivot stays
  fixed when growing ([6c4764f2](https://github.com/vdavid/prvw/commit/6c4764f2)).
- Enlarge small images toggle (off by default): small images display at native pixel size instead of being stretched.
  Disabled when auto-fit is on ([c2c73c8f](https://github.com/vdavid/prvw/commit/c2c73c8f)).
- Checkerboard background for transparent images, screen-space so it doesn't zoom
  ([d4817745](https://github.com/vdavid/prvw/commit/d4817745)).
- Custom overlay text with pill backgrounds: SF Pro bold 13.5 pt on semi-transparent rounded rectangles, middle
  truncation for long filenames. Native title bar text hidden
  ([d0006fca](https://github.com/vdavid/prvw/commit/d0006fca)).
- Native AppKit windows for About, Settings, and Onboarding. Frosted glass, ESC-to-close, deduplication guard
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb)).
- Settings persistence with `PRVW_DATA_DIR` env var override for dev / test isolation
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb)).
- View → Refresh menu item (R key) ([593cac91](https://github.com/vdavid/prvw/commit/593cac91)).
- MCP server improvements: JSON state responses, synchronous command completion, `prvw://settings` resource,
  `set_window_geometry` / `scroll_zoom` / `zoom_in` / `zoom_out` tools
  ([593cac91](https://github.com/vdavid/prvw/commit/593cac91),
  [c2c73c8f](https://github.com/vdavid/prvw/commit/c2c73c8f)).

### Changed

- **Zoom model is now absolute** (1.0 = one image pixel per screen pixel). Zoom % shows actual pixel scale; enables
  auto-fit zoom without feedback loops ([3b2f51ef](https://github.com/vdavid/prvw/commit/3b2f51ef)).
- Scroll zoom slowed to 5 % per tick (was 15 %) ([d2ce180b](https://github.com/vdavid/prvw/commit/d2ce180b)).
- Input handling unified through `AppCommand`: all keyboard, menu, and QA key events mapped in one place
  ([4dbf3266](https://github.com/vdavid/prvw/commit/4dbf3266)).
- Background colour changed from dark grey to black ([644132bb](https://github.com/vdavid/prvw/commit/644132bb)).
- File association setup now uses direct CoreServices FFI instead of `swift -e` scripts (near-instant)
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb)).

## [0.5.0] - 2026-04-12

### Added

- Text rendering via glyphon (wgpu-native, cross-platform); onboarding screen on no-file launch; header overlay
  (filename, position, zoom) in the title bar; transparent titlebar with frosted glass on macOS; styled DMG installer
  (icon positioning, window sizing via create-dmg). LSHandlerRank changed to Default so Prvw appears higher in "Open
  With" menus ([177febfe](https://github.com/vdavid/prvw/commit/177febfe),
  [34d748d5](https://github.com/vdavid/prvw/commit/34d748d5),
  [c74a12e7](https://github.com/vdavid/prvw/commit/c74a12e7)).

## [0.4.0] - 2026-04-12

### Added

- Auto-updater: background update check, DMG download, .app bundle replacement; `PRVW_UPDATE_URL` env override for
  testing ([850d5e16](https://github.com/vdavid/prvw/commit/850d5e16)).
- Direct download buttons on the website with architecture detection (Apple Silicon / Intel / Universal)
  ([6709c3e3](https://github.com/vdavid/prvw/commit/6709c3e3)).
- PostHog session replay (cookieless, proxied through `/ph/`)
  ([304788e9](https://github.com/vdavid/prvw/commit/304788e9)).
- Umami download tracking with arch, version, and source properties
  ([88f14d29](https://github.com/vdavid/prvw/commit/88f14d29),
  [7affe0f2](https://github.com/vdavid/prvw/commit/7affe0f2)).
- Sitemap via `@astrojs/sitemap` ([3937ac94](https://github.com/vdavid/prvw/commit/3937ac94)).
- Terms and conditions and privacy policy pages ([75b85166](https://github.com/vdavid/prvw/commit/75b85166)).
- Deploy infrastructure: webhook, CI auto-deploy on push to main, Hetzner Dockerfile / nginx / docker-compose
  ([381fee20](https://github.com/vdavid/prvw/commit/381fee20),
  [6f63be09](https://github.com/vdavid/prvw/commit/6f63be09)).

### Fixed

- Download dropdown not opening — used `DOMContentLoaded` instead of `astro:page-load`
  ([424fd59c](https://github.com/vdavid/prvw/commit/424fd59c)).
- Updater: fix `.app` replacement (`fs::rename` over non-empty dir fails on macOS)
  ([1d7fb33e](https://github.com/vdavid/prvw/commit/1d7fb33e)).

## [0.3.0] - 2026-04-12

### Added

- Multiple file args: `prvw photo1.jpg photo2.jpg` uses the provided files as the navigation set instead of scanning the
  directory. Supports multi-select "Open With" from Finder ([c49761dc](https://github.com/vdavid/prvw/commit/c49761dc)).
- Keyboard shortcuts: Space / `]` for next, Backspace / `[` for previous, F / Enter for fullscreen, 1 for actual size
  ([f0c24f8a](https://github.com/vdavid/prvw/commit/f0c24f8a)).
- Clickable menu items: About Prvw, View, Navigate. Fixed root cause — Menu object must be kept alive to prevent
  dangling pointer in NSMenuItems ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8)).
- About dialog showing version, author, and website links ([f0c24f8a](https://github.com/vdavid/prvw/commit/f0c24f8a)).
- macOS .app bundle with `Info.plist`, file type associations (JPEG, PNG, GIF, WebP, BMP, TIFF), app icon
  ([4863e682](https://github.com/vdavid/prvw/commit/4863e682),
  [6abdcc65](https://github.com/vdavid/prvw/commit/6abdcc65)).
- Apple Events handler via `NSAppleEventManager` for opening files while the app is running
  ([af4ccac3](https://github.com/vdavid/prvw/commit/af4ccac3)).
- Release infrastructure: GitHub Actions workflow, signing, DMG creation, notarization
  ([4863e682](https://github.com/vdavid/prvw/commit/4863e682)).
- Root Cargo workspace (matching Cmdr's structure) ([35c1e805](https://github.com/vdavid/prvw/commit/35c1e805)).

### Fixed

- Aspect ratio always preserved during window resize (rewrote view transform with single uniform scale)
  ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8)).
- Zoom: can't zoom out past fit-to-window; zoom pivot correct after resize
  ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8)).
- Pan clamped to image edges, re-clamped on window resize ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8)).
- Blank startup: retry render when wgpu surface is `Occluded` during window creation
  ([b5c0a81b](https://github.com/vdavid/prvw/commit/b5c0a81b)).
- CI: install `libglib2.0-dev` + `libgtk-3-dev` for winit on Ubuntu
  ([4e54a6ff](https://github.com/vdavid/prvw/commit/4e54a6ff)).

## [0.2.0] - 2026-04-11

### Added

- JPEG decoding via `zune-jpeg` with SIMD acceleration (NEON / AVX), ~27 % faster than the `image` crate's built-in
  decoder ([2e67fd35](https://github.com/vdavid/prvw/commit/2e67fd35)).
- Parallel preloading with rayon (uses all cores instead of one), ~2 – 3× faster for NAS browsing
  ([2e67fd35](https://github.com/vdavid/prvw/commit/2e67fd35)).
- Priority preloading with cancellation tokens — navigating cancels stale preloads, current image gets priority via
  `spawn_fifo`, chunked file reads (64 KB) allow sub-2 ms cancellation on NAS
  ([68dbe31e](https://github.com/vdavid/prvw/commit/68dbe31e)).
- EXIF orientation support via `nom-exif` — phone photos (orientation 6/8) now display right-side-up
  ([d2d95bc9](https://github.com/vdavid/prvw/commit/d2d95bc9)).
- Embedded MCP server (streamable HTTP on port 19447) for agent testing and E2E. Tools: navigate, key, zoom, fullscreen,
  open, screenshot. Resources: state, menu, diagnostics ([c7f4875f](https://github.com/vdavid/prvw/commit/c7f4875f),
  [3751813b](https://github.com/vdavid/prvw/commit/3751813b)).
- Performance diagnostics via MCP and HTTP — cache state with per-image decode times, navigation history with timing,
  process RSS ([3751813b](https://github.com/vdavid/prvw/commit/3751813b)).
- Cmdr-style logging with timestamps, coloured log levels, and short module scopes
  ([ca94104f](https://github.com/vdavid/prvw/commit/ca94104f)).
- JPEG decode benchmark (zune-jpeg vs turbojpeg, 20 Pixel photos). Key finding: NAS I/O is the bottleneck, not CPU
  decode ([1956496b](https://github.com/vdavid/prvw/commit/1956496b)).

### Changed

- Window title format: `3 / 60 – photo.jpg` (position first for quick scanning), loading state: `3 / 60 – Loading...`
  ([7509317e](https://github.com/vdavid/prvw/commit/7509317e)).

### Fixed

- Crash on Left/Right arrow nav — muda 0.17 panics with `ZeroWidth` icon error when processing keyboard accelerators on
  macOS. All accelerators removed from menu items, shortcuts handled directly in keyboard event handler
  ([5aa98e8b](https://github.com/vdavid/prvw/commit/5aa98e8b)).
- Fullscreen on/off via QA server now uses `set_fullscreen` directly instead of toggling
  ([e34b0f83](https://github.com/vdavid/prvw/commit/e34b0f83)).

## [0.1.0] - 2026-04-11

### Added

- Initial release: GPU-accelerated image viewer for macOS. `winit` 0.30 windowing with `ApplicationHandler`, `wgpu` 29
  Metal rendering, `muda` 0.17 native menus. Image formats: JPEG, PNG, GIF (first frame), WebP, BMP, TIFF. Zoom and pan
  (scroll wheel cursor-centred, click-drag, +/-/0, double-click toggle fit / actual size). Directory navigation with N±2
  background preloading. Fullscreen toggle (Cmd+F, F11), ESC to exit. Native macOS menu bar. Render-on-demand.
  getprvw.com marketing site (Astro + Tailwind v4). Go check runner (14 checks, parallel with dependency graph). GitHub
  Actions CI (Ubuntu + macOS) ([2e9aa754](https://github.com/vdavid/prvw/commit/2e9aa754)).
