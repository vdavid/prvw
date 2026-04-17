# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Bundled DCP library** (Phase 3.5): 161 community-contributed RawTherapee DCP profiles are now packed into the
  binary at build time (~10 MB via zstd, Strategy B). They serve as a fourth search tier after `PRVW_DCP_DIR`,
  Adobe Camera Raw's install dir, and ahead of "return None." Zero user setup required — cameras in the collection
  get per-camera color fidelity out of the box. DCPs are from
  [RawTherapee's repository](https://github.com/Beep6581/RawTherapee/tree/dev/rtdata/dcpprofiles), contributed
  by Maciej Dworak, Lawrence Lee, Alberto Griggio, and others; attribution in
  `apps/desktop/build-assets/dcps/LICENSE`. User-provided profiles (`PRVW_DCP_DIR`) always win.
- **Fuzzy DCP family matching** (Phase 3.5): when exact `UniqueCameraModel` matching fails across all tiers, Prvw
  now tries a curated list of known-compatible camera families. For example, a Sony α5000 falls back to the α6000
  profile (same 20.1 MP sensor). Logs at INFO so users see the substitution. The seed list is conservative (20
  cameras across Sony, Fujifilm, Nikon, Canon, Olympus, and Panasonic); extend via PR with evidence (same sensor
  chip or close-family color science).

### Fixed

- iPhone / Pixel DNGs no longer render with a radial red cast. Phase 3.0's DNG `OpcodeList2` applier treated the
  `GainMap` opcode's `Plane` field as a CFA color index, which meant the R-phase lens-shading correction fired while
  the G1 / G2 / B ones were skipped — corners of the frame wanted a uniform gain lift but got it on the red channel
  only. Per DNG spec 1.6 § 6.2.2, `Plane` indexes into photometric image planes, not CFA colors; a CFA image is a
  single plane, and Bayer-phase selection comes from `Top/Left` + `RowPitch`/`ColPitch`. The applier now scales every
  pixel the rect and pitch select, matching what LibRaw, RawTherapee, and Adobe's own SDK do. Non-DNG files (ARW,
  CR2, NEF, and so on) and DNGs without `GainMap` opcodes decode byte-for-byte identically to before. See
  `docs/notes/raw-support-phase3.md`

### Added

- RAW pipeline closes the three Phase 3.2-deferred DCP items: **`LookTable`**, **`ProfileToneCurve`**, and
  **dual-illuminant interpolation**. `LookTable` (tags 50981 / 50982 / 51108) runs as a second HSV LUT after the
  `HueSatMap`, capturing Adobe's "Look" refinement on top of the neutral calibration. When the active DCP ships a
  `ProfileToneCurve` (tag 50940), Prvw applies it piecewise-linearly on luminance only in place of the default
  Hermite S-curve — the camera's intended tonality wins, and an INFO log line spells out which curve ran. For
  profiles that carry both `HueSatMap1` and `HueSatMap2`, the two maps now blend based on the scene's estimated color
  temperature (compromise fidelity: a one-shot formula `temp ≈ 7000 − 2000 × (R/G − 1)` from the camera's WB
  coefficients, clamped to `[2000, 10000] K`, feeds a linear blend between the illuminant temperatures). All three
  features fire on sample2.dng's embedded Pixel 6 Pro profile and on the SONY ILCE-7M3 DCP; mean per-byte Δ vs. Phase
  3.3 is ~17 on the Pixel sample (up from 3.28) because `ProfileToneCurve` is a big part of a profile's visual
  character. DCPs that carry only a single map, no `LookTable`, or no tone curve continue to no-op through those
  stages. Still deferred: `ForwardMatrix1/2` swap, the spec's full iterative CCT convergence. See
  `docs/notes/raw-support-phase3.md`
- RAW pipeline now applies **DCP profiles embedded in DNG files**. Every Pixel, Samsung Galaxy, iPhone ProRAW, and
  Adobe-converted DNG ships with a `ProfileHueSatMap` baked into its main IFD; Prvw reads and applies it the same way
  Phase 3.2 applies a standalone `.dcp` file, with zero user config. Embedded wins over a matching filesystem DCP — the
  manufacturer's profile is the authoritative source. On sample2.dng (Pixel 6 Pro) the embedded "Google Embedded Camera
  Profile" shifts 63 % of output bytes with a mean per-channel delta of 3.28, rendering a visibly more balanced image
  (warmer tile grays, less cool / greenish cast). Non-DNG RAWs (Sony ARW, Canon CR2 / CR3, Nikon NEF, Olympus ORF,
  Fujifilm RAF, Panasonic RW2, Pentax PEF, Samsung SRW) and DNGs without profile tags decode byte-for-byte identically
  to Phase 3.2. INFO log line names the source (`"RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' …"` vs.
  `"RAW applied filesystem DCP …"`). See `docs/notes/raw-support-phase3.md`
- RAW pipeline gained **opt-in Adobe DCP (Digital Camera Profile) support**. If the user has a `.dcp` matching the
  camera's `UniqueCameraModel` in `$PRVW_DCP_DIR` or Adobe Camera Raw's default directory
  (`~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`), Prvw applies its `ProfileHueSatMap` as a
  trilinearly-interpolated 3D LUT in linear-light HSV, right after highlight recovery and before the default tone
  curve. Catches the per-camera color character a generic 3×3 matrix can't — skin tones, saturated reds / greens,
  and so on. Cyclic hue axis handles the 360°/0° wraparound cleanly; neutrals short-circuit so grays never drift
  chroma. Deferred for a later phase: `LookTable` (second LUT), per-camera tone curve, dual-illuminant
  interpolation, `ForwardMatrix` swap. Most users will have no matching DCP available, which is a silent no-op —
  output stays bit-for-bit identical to the pre-3.2 pipeline. Parse time is ~16 µs once per decode; apply time is
  ~35 ms on a 20 MP buffer (rayon-parallel). See `docs/notes/raw-support-phase3.md`
- RAW pipeline now applies highlight recovery between the baseline-exposure lift and the tone curve. Pixels whose
  brightest channel exceeds 0.95 in linear Rec.2020 blend toward their own luminance via a smoothstep that finishes at
  1.20, and pass through untouched below that. Keeps bright skies and specular highlights from drifting magenta / cyan
  when one channel clips while the others keep rising, which used to produce pink clouds and cyan window-frames in
  high-contrast scenes. In-gamut pixels are byte-identical; a ~20 MP decode pays about one extra linear-domain pass
  (~260 ms, rayon-parallel). Parametric `apply_highlight_recovery` is exposed alongside the default wrapper so Phase
  3.3's per-camera DCP work can override the thresholds. See `docs/notes/raw-support-phase3.md`
- RAW pipeline now applies DNG `OpcodeList1`, `OpcodeList2`, and `OpcodeList3` per Adobe's DNG spec 1.6. `rawler`
  parses those tags but doesn't apply them; we pick them up in a new `decoding::dng_opcodes` module. `GainMap`
  (opcode 9), `WarpRectilinear` (opcode 1), `FixBadPixelsConstant` (opcode 4), and `FixBadPixelsList` (opcode 5) are
  implemented end-to-end; other opcodes log and skip. iPhone ProRAW files now render with correct lens-shading and
  optical distortion correction: sample2.dng's four per-Bayer-phase gain maps fire on the mosaic, and the post-color
  `WarpRectilinear` fires after our color matrix. `LinearizationTable` (tag 50712) is already handled inside rawler
  itself, so we don't reimplement it. ARW / CR2 / NEF and other non-DNG formats get a silent no-op — zero overhead.
  The `raw-dev-dump` example gained `after-opcode1`, `after-opcode2`, and `after-opcode3` stages. See
  `docs/notes/raw-support-phase3.md`
- RAW pipeline test infrastructure: a tiny synthetic Bayer DNG fixture (128×128, ~33 KB, 0BSD), a `color::delta_e`
  CIE76 metric module, a `synthetic_dng_matches_golden` regression test that diffs `load_image` output against a
  checked-in golden PNG, and a `raw-dev-dump` example that dumps per-stage PNGs for visual inspection. Goldens
  regenerate via `PRVW_UPDATE_GOLDENS=1 cargo test`. Sets up Phase 2 of RAW work (wide-gamut + exposure + tone curve +
  sharpening). See `docs/notes/raw-support-phase2.md`

### Changed

- RAW decode now preserves wide-gamut color data end-to-end. Previously, rawler's develop pipeline clipped output to
  sRGB during color conversion, discarding any P3 or Rec.2020 coverage the sensor captured. The new pipeline runs
  rawler's demosaic stages only, then applies our own white balance and camera matrix into a linear Rec.2020
  intermediate, which moxcms transforms to the display profile. On P3 displays, RAW output now shows colors that were
  previously clipped — saturated reds, greens, and blues near the edge of the gamut. Pipeline details in
  `docs/notes/raw-support-phase2.md`
- RAW decode now applies a baseline exposure lift in linear Rec.2020 space, right before the ICC transform. Source is
  the DNG `BaselineExposure` tag (50730) when the file carries one, otherwise +0.5 EV — Adobe's neutral default and
  roughly what Preview.app and Apple Photos apply silently. Clamped to ±2 EV for safety. Real-world RAW output now
  lands within ~97 % of Preview.app's brightness; the final gap closes when Phase 2.3's tone curve lands. See
  `docs/notes/raw-support-phase2.md`
- RAW decode now applies a default tone curve between the exposure lift and the ICC transform. Mild filmic S with a
  shadow Hermite knee, a midtone line of slope 1.08 anchored at 0.25, and a highlight shoulder landing softly on 1.0.
  Analytical, monotonic, and endpoint-preserving. Adds the contrast punch and highlight roll-off that viewers like
  Preview.app and Affinity bake in by default, closing the "flat look" gap on linear wide-gamut output. See
  `docs/notes/raw-support-phase2.md`
- RAW decode now applies a mild capture-sharpening pass on the display-space RGBA8 buffer as the last step before
  orientation. Separable 1D Gaussian blur (σ = 0.8 px, 7 taps) followed by an unsharp-mask combine (amount = 0.3).
  Runs post-ICC so the sharpening sees the same gamma-encoded buffer the eye will see, matching Lightroom and Camera
  Raw's traditional slot and avoiding the halos linear-space unsharp produces on bright edges. Measured Laplacian
  edge energy on a Sony ARW jumps ~18 % vs. pre-sharpen; brightness is unchanged. Closes the "slightly soft"
  perception gap against Preview.app. Adds ~60 ms to a 20 MP decode. Concludes Phase 2 of RAW polish. See
  `docs/notes/raw-support-phase2.md`
- RAW tone curve and capture sharpening now act on luminance only rather than per-channel, and a mild global
  saturation boost sits between them. Per-channel tone curves were desaturating colors near the highlight shoulder,
  and per-channel sharpening was introducing color fringes at colored edges. Both passes now compute luminance in f32
  (Rec.2020 weights for the linear-space tone curve, Rec.709 for the display-space sharpen), reshape Y, and scale
  each pixel's RGB by `Y_out / Y_in` so hue and chroma are preserved. The saturation boost (+8 % default) scales
  chroma around the luminance axis in linear Rec.2020, approximating the "vibrancy" Apple and Affinity bake into
  their per-camera tuning tables. Hue and luminance are preserved exactly. Parameter values unchanged (midtone anchor
  0.25, sharpen amount 0.3); empirical tuning lands in Phase 2.5b. Sony ARW end-to-end decode speeds up a bit
  (~180 ms steady-state vs. ~220 ms pre-change) since the luminance-only sharpen runs the separable blur on one
  plane instead of three. See `docs/notes/raw-support-phase2.md`
- RAW defaults retuned against a Preview.app screenshot rather than `sips` output. The first Phase 2.5b tuner run
  grid-searched against Apple's conservative `sips -s format png` export and shipped the Phase 2.5a educated-guess
  values unchanged; the resulting output read as "washed out and blurrier" next to Preview.app on XDR displays.
  Rerun against a CleanShot capture of Preview.app rendering the same RAW: midtone anchor 0.25 → 0.40, saturation
  boost +0.08 → 0.00, sharpen amount stays at 0.30. New defaults beat the old ones by 0.81 Delta-E on the reference
  scene. The tuner now handles references smaller than the decoded buffer (typical for fit-to-window screenshots) by
  Lanczos3-downsampling our output before the metric runs. See `docs/notes/raw-support-phase2.md`

## [0.9.0] - 2026-04-17

### Added

- Camera RAW support via `rawler`: open DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, and SRW files. Decode pipeline
  includes black/white level correction, PPG demosaic for Bayer sensors, bilinear for Fuji X-Trans, white balance,
  camera color matrix with Bradford chromatic adaptation, and sRGB gamma. NEON SIMD on Apple Silicon, rayon
  parallelism. Orientation is pulled from EXIF metadata since `rawler` hard-codes `RawImage.orientation`. Known limits
  in this first pass: no DNG `OpcodeList` interpretation (iPhone ProRAW gain maps), no DCP profiles, X-Trans demosaic
  is bilinear. See `docs/notes/raw-support-phase1.md` for design decisions and the Phase 2/3 outlook
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))
- File associations for all 10 RAW formats: Finder now recognizes Prvw as a handler for DNG, CR2, CR3, NEF, ARW, ORF,
  RAF, RW2, PEF, and SRW via their standard Apple UTIs. `Info.plist` carries all 16 document types now (6 standard + 10
  RAW)
- Settings > File associations: redesigned into two sections, "Standard image formats" (JPEG, PNG, GIF, WebP, BMP,
  TIFF) and "Camera RAW formats" (DNG, CR2 + CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW) with vendor labels. Each section
  has a master "Set all" toggle with tri-state support: when some formats are on and others off, the master shows a
  "Mixed" indicator; clicking mixed or off sets all on, clicking on sets all off. Per-format small toggles keep fine
  control

### Changed

- Onboarding window: redesigned into a four-step checklist (Install Prvw.app, Set as default viewer, Move to
  /Applications, Open images). Each step uses a custom green checkmark (dimmed for pending steps) rendered at runtime
  from the source SVG path via `NSBezierPath`. Step 2 holds the "Set Prvw as the default viewer for all of these
  files" button and shows a natural-language sentence summarizing which app currently opens each of the 16 supported
  image formats. Step 3 checks whether the binary is in `/Applications/` and shows a drag hint when it isn't. Step 4's
  copy adapts to step 2's state: "double-click any image" once Prvw is the default, or "right-click any image and
  choose Open with → Prvw" beforehand. Content is left-aligned, the window is wider and taller to give the checklist
  breathing room, and the title drops the `v` prefix ("Prvw 0.8.0")
- Decoding module: single-file `decoding.rs` split into a `decoding/` module with per-backend files (`jpeg.rs`,
  `generic.rs`, `raw.rs`) plus shared `dispatch.rs` and `orientation.rs`. Public API unchanged
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))
- CI: macOS-only modules (AppKit settings panels, color transform tests) gated behind `#[cfg(target_os = "macos")]`
  so cross-platform builds compile cleanly. Groundwork for Windows and Linux support later
  ([e9b5de4](https://github.com/vdavid/prvw/commit/e9b5de4),
  [3f00979](https://github.com/vdavid/prvw/commit/3f00979),
  [815b727](https://github.com/vdavid/prvw/commit/815b727),
  [96218dd](https://github.com/vdavid/prvw/commit/96218dd))

### Fixed

- `apply_orientation` underflowed on zero-width or zero-height input for EXIF orientation 2. Now early-returns
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))
- Restored per-row handler transparency in Settings > File associations: each format row again shows which app currently
  handles it, or which app handled it before Prvw took over. Covers all 16 formats (6 standard + 10 RAW)

## [0.8.0] - 2026-04-17

### Added

- Settings window: new sidebar layout with General, Zoom, Color, and File associations sections. Cross-dependencies
  disable dependent toggles automatically (ICC off → Color match / Relative colorimetric disabled; Auto-fit on →
  Enlarge disabled) ([dc43505](https://github.com/vdavid/prvw/commit/dc43505),
  [0dd4849](https://github.com/vdavid/prvw/commit/0dd4849))
- File associations panel: per-UTI toggles, "Set all" master toggle, 1-second polling of handler state, previous
  handler rollback when you turn a toggle off ([0dd4849](https://github.com/vdavid/prvw/commit/0dd4849),
  [17b76a3](https://github.com/vdavid/prvw/commit/17b76a3))
- Rendering intent toggle (View menu + Settings > Color, Cmd+Shift+R). Default is perceptual; toggle to relative
  colorimetric. Disabled when ICC color management is off. Persisted as `use_relative_colorimetric`
  ([b42814f](https://github.com/vdavid/prvw/commit/b42814f))
- Scroll-to-zoom toggle in Settings > General (off by default). When off, scroll navigates between images (down = next,
  up = prev). Cmd+scroll always zooms regardless of the setting
  ([d55b7e9](https://github.com/vdavid/prvw/commit/d55b7e9))
- Pinch-to-zoom on trackpad, cursor-centered. Works with auto-fit window resize, same as scroll zoom
  ([ef8d0bf](https://github.com/vdavid/prvw/commit/ef8d0bf))
- Keyboard shortcuts for zoom: Cmd+= (zoom in), Cmd+- (zoom out), Cmd+0 (actual size)
  ([ec2aba4](https://github.com/vdavid/prvw/commit/ec2aba4))
- Title bar toggle in Settings > General (on by default): reserves a 32px strip at the top so the filename and zoom
  pills don't cover the image. Screenshot-friendly when off
  ([64e0d87](https://github.com/vdavid/prvw/commit/64e0d87))
- Title bar vibrancy: Liquid Glass on macOS 26, classic frosted glass on older versions. The area around the image
  (when the image doesn't fill the window) gets a darker HUD-style vibrancy. Fullscreen switches both to opaque black
  so screenshots and projector-style viewing aren't distracted by the desktop blurring through
  ([7eede14](https://github.com/vdavid/prvw/commit/7eede14))
- Integration test suite (17 tests): Settings open/close/switch, zoom in/out, fit/actual, auto-fit toggle, navigate,
  refresh, window geometry. Each test spawns its own app instance on a dynamic port
  ([0dd4849](https://github.com/vdavid/prvw/commit/0dd4849))

### Changed

- Source layout: flat `src/` with infrastructure (`app/`, `render/`, `platform/`) and features (`about`, `color`,
  `decoding`, `navigation`, `onboarding`, `qa`, `settings`, `window`, `zoom`, …) as siblings. Each feature owns its
  runtime state via a `State` struct on `App` instead of ~20 flat fields. No behavior change
  ([27eca5e](https://github.com/vdavid/prvw/commit/27eca5e),
  [e88027b](https://github.com/vdavid/prvw/commit/e88027b))

### Fixed

- Closing the onboarding window now quits the app. Previously, a no-file launch (Dock or `cargo run` with no args)
  left the event loop running with nothing visible after the user clicked Close, because the onboarding is a raw
  AppKit window and doesn't generate a winit close event
  ([e81bbdf](https://github.com/vdavid/prvw/commit/e81bbdf))
- CGColor / CGColorSpace encoding crashes in `setColorspace:` (display profile) and the Settings separator:
  `msg_send!` encoded these as `^v` instead of `^{CGColorSpace=}` / `^{CGColor=}`. Fix uses raw `objc_msgSend` to
  bypass the type check ([17b76a3](https://github.com/vdavid/prvw/commit/17b76a3))

## [0.7.0] - 2026-04-16

### Added

- ICC color management: embedded source profiles (JPEG, PNG, TIFF, WebP) are transformed to accurate output colors.
  Level 1 converts to sRGB, Level 2 targets the actual display profile via CoreGraphics FFI
  (`CGDisplayCopyColorSpace`). Images without profiles assumed sRGB. Display changes flush the cache and re-decode
  ([ee226ac](https://github.com/vdavid/prvw/commit/ee226ac),
  [94820a8](https://github.com/vdavid/prvw/commit/94820a8))
- View menu toggles: "ICC color management" (Cmd+Shift+I) and "Color match display" (Cmd+Shift+C), both persisted in
  settings. Disabling ICC grays out color matching (L2 depends on L1)
  ([a088330](https://github.com/vdavid/prvw/commit/a088330),
  [b952b64](https://github.com/vdavid/prvw/commit/b952b64))

### Changed

- ICC engine: replaced lcms2 (C bindings) with moxcms (pure Rust, NEON SIMD). 24MP transform: 247ms -> 45ms on M3 Max.
  No C toolchain needed for cross-compilation
  ([f568b18](https://github.com/vdavid/prvw/commit/f568b18))

### Fixed

- Screen detection: replaced unreliable `current_monitor()` + `CGDisplayBounds` position matching with
  `NSWindow.screen.deviceDescription` for authoritative `CGDirectDisplayID`
  ([fcdefe3](https://github.com/vdavid/prvw/commit/fcdefe3))
- Pre-existing BGRA->RGBA swap bug in screenshot capture
  ([ee226ac](https://github.com/vdavid/prvw/commit/ee226ac))

## [0.6.3] - 2026-04-15

### Fixed

- Finder double-click file opening: replaced `NSAppleEventManager` handler (overridden by AppKit's
  `NSDocumentController`) with ObjC runtime method injection (`class_addMethod`) that adds
  `application:openURLs:` directly to winit's `WinitApplicationDelegate` class
  ([9417ab0](https://github.com/vdavid/prvw/commit/9417ab0))

### Changed

- Zoom model uses logical pixels: zoom=1.0 = 100% = one image pixel per logical pixel. The overlay
  correctly shows 100% for naturally-sized images on Retina displays (was 200%)
- Compiler-enforced `Logical<T>` / `Physical<T>` newtypes prevent mixing logical and physical pixel
  values. Winit interop via `from_logical_size`, `to_logical_pos`, etc.
- Removed 329 lines of dead modal onboarding code

## [0.6.2] - 2026-04-15

### Fixed

- Finder double-click "cannot open files in JPEG Image format": `CFBundleTypeRole` was missing from `Info.plist`
  document type entries. macOS requires this to know the app can actually open files, not just be registered as a handler
- CI: add `libxdo-dev` to Linux apt-get for `muda` dependency
- Auto-updater: call `lsregister -f` after replacing the `.app` bundle so macOS picks up new document types in future
  updates

## [0.6.1] - 2026-04-15

### Fixed

- Finder double-click now works: macOS sends file paths via Apple Events (not CLI args), but the app was exiting before
  the event loop started. Now the event loop always runs, with a 500ms wait for Apple Events before showing onboarding
  ([f6e0fef](https://github.com/vdavid/prvw/commit/f6e0fef))

### Changed

- Onboarding window is now non-modal (doesn't block the event loop), allowing Apple Events and QA commands to arrive
  while it's showing
- Code refactors: `scale_factor` stored on App, `TextBlock` builder pattern, `MonitorBounds` helper, `LogicalF64` /
  `LogicalF32` type aliases for coordinate clarity

## [0.6.0] - 2026-04-15

### Added

- Auto-fit window: window resizes to match each loaded image, centered on screen. Clamped to min 200px, max 90% of
  monitor, proportionally scaled. Toggle in View menu and Settings window
  ([6a8e03d](https://github.com/vdavid/prvw/commit/6a8e03d))
- Auto-fit zoom: when auto-fit is on, zooming in/out resizes the window to match the zoomed image. Cursor pivot stays
  fixed when growing, symmetric shrink when reducing. Screen boundary preserved
  ([6c4764f](https://github.com/vdavid/prvw/commit/6c4764f))
- Enlarge small images toggle (off by default): small images display at native pixel size instead of being stretched to
  fill the window. Toggle in View menu and Settings window, disabled when auto-fit is on
  ([c2c73c8](https://github.com/vdavid/prvw/commit/c2c73c8))
- Checkerboard background for transparent images (Photoshop-style, screen-space so it doesn't zoom)
  ([d481774](https://github.com/vdavid/prvw/commit/d481774))
- Custom overlay text with pill backgrounds: SF Pro system font (bold, 13.5pt), semi-transparent rounded rectangles
  sized from actual measured text width, middle truncation for long filenames (`prefix…suffix`), right-aligned zoom
  percentage. Native title bar text hidden
  ([d0006fc](https://github.com/vdavid/prvw/commit/d0006fc))
- Native AppKit windows for About (with icon, links), Settings (toggles with live apply), and Onboarding (file
  association setup with live polling). Frosted glass backgrounds, ESC-to-close, deduplication guard
  ([644132b](https://github.com/vdavid/prvw/commit/644132b))
- Settings persistence with `PRVW_DATA_DIR` env var override for dev/test isolation
- View > Refresh menu item (R key)
- MCP server improvements: JSON state responses, synchronous command completion, `prvw://settings` resource,
  `set_window_geometry`, `scroll_zoom`, `zoom_in`/`zoom_out` tools, window position in state
  ([593cac9](https://github.com/vdavid/prvw/commit/593cac9),
  [c2c73c8](https://github.com/vdavid/prvw/commit/c2c73c8))

### Changed

- Zoom model: now absolute (1.0 = one image pixel per screen pixel). Zoom % in titlebar shows actual pixel scale.
  Enables auto-fit zoom without feedback loops
  ([3b2f51e](https://github.com/vdavid/prvw/commit/3b2f51e))
- Scroll zoom slowed to 5% per tick (was 15%)
  ([d2ce180](https://github.com/vdavid/prvw/commit/d2ce180))
- Input handling unified through `AppCommand`: all keyboard, menu, and QA key events mapped in one place (`input.rs`).
  Central `execute_command()` handler
  ([4dbf326](https://github.com/vdavid/prvw/commit/4dbf326))
- Background color changed from dark gray to black
- Settings window buttons changed from "OK" to "Close" (toggles apply immediately)
- File association setup uses direct CoreServices FFI instead of `swift -e` scripts (near-instant)

## [0.5.0] - 2026-04-12

### Added

- Text rendering via glyphon (wgpu-native, cross-platform)
- Onboarding screen when launched with no file: shows welcome, file association status, "Set as default viewer"
- Header overlay in image view: filename, position, zoom level in the transparent title bar
- Transparent titlebar with frosted glass effect on macOS (fullSizeContentView)
- LSHandlerRank changed to Default (Prvw appears higher in "Open With" menus)
- Styled DMG installer (icon positioning, window sizing via create-dmg)

## [0.4.0] - 2026-04-12

### Added

- Auto-updater: background update check, DMG download, .app bundle replacement. Restart to use the new version.
  PRVW_UPDATE_URL env var override for testing.
- Direct download buttons on website with architecture detection (Apple Silicon / Intel / Universal)
- PostHog session replay (cookieless, proxied through /ph/)
- Umami download tracking with arch, version, and source properties
- Sitemap via @astrojs/sitemap
- UptimeRobot monitoring for getprvw.com
- Terms and conditions and privacy policy pages
- Deploy infrastructure: webhook, CI auto-deploy on push to main

### Fixed

- Download dropdown not opening (used DOMContentLoaded instead of astro:page-load)
- Updater: fix .app replacement (fs::rename over non-empty dir fails on macOS)

## [0.3.0] - 2026-04-12

### Added

- Multiple file args: `prvw photo1.jpg photo2.jpg` uses the provided files as the navigation set instead of scanning
  the directory. Supports multi-select "Open With" from Finder
  ([c49761d](https://github.com/vdavid/prvw/commit/c49761d))
- Keyboard shortcuts: Space/] for next, Backspace/[ for previous, F/Enter for fullscreen, 1 for actual size
  ([f0c24f8](https://github.com/vdavid/prvw/commit/f0c24f8))
- Clickable menu items: About Prvw, View (zoom, fit, actual size, fullscreen), Navigate (prev/next). Fixed root cause
  (Menu object must be kept alive to prevent dangling pointer in NSMenuItems)
  ([7e9d0dd](https://github.com/vdavid/prvw/commit/7e9d0dd))
- About dialog showing version, author, and website links
- macOS window config: disabled system tab bar and native fullscreen (we have our own borderless fullscreen)
- Poll menu events in `about_to_wait` for instant response (was delayed until next window event)
- macOS .app bundle with Info.plist, file type associations (JPEG, PNG, GIF, WebP, BMP, TIFF), app icon
- Apple Events handler via NSAppleEventManager for opening files while app is running
- Release infrastructure: GitHub Actions workflow, signing, DMG creation, notarization
- Root Cargo workspace (matching Cmdr's structure)

### Fixed

- Aspect ratio always preserved during window resize (rewrote view transform with single uniform scale)
- Zoom: can't zoom out past fit-to-window, zoom pivot correct after resize
- Pan clamped to image edges, re-clamped on window resize
- Blank startup: retry render when wgpu surface is Occluded during window creation
- CI: install libglib2.0-dev + libgtk-3-dev for winit on Ubuntu

## [0.2.0] - 2026-04-11

### Added

- JPEG decoding via `zune-jpeg` with SIMD acceleration (NEON on Apple Silicon, AVX on x86), ~27% faster than the
  `image` crate's built-in decoder ([2e67fd3](https://github.com/vdavid/prvw/commit/2e67fd3))
- Parallel preloading with rayon thread pool (uses all available cores instead of a single thread), ~2-3x faster for
  NAS browsing ([2e67fd3](https://github.com/vdavid/prvw/commit/2e67fd3))
- Priority preloading with cancellation tokens — navigating cancels stale preloads, current image gets priority via
  `spawn_fifo`, chunked file reads (64 KB) allow sub-2ms cancellation on NAS
  ([68dbe31](https://github.com/vdavid/prvw/commit/68dbe31))
- EXIF orientation support via `nom-exif` — phone photos (portrait orientation 6/8) now display right-side-up
  ([d2d95bc](https://github.com/vdavid/prvw/commit/d2d95bc))
- Embedded MCP server (streamable HTTP on port 19447) for agent testing and E2E — tools: navigate, key, zoom,
  fullscreen, open, screenshot; resources: state, menu, diagnostics
  ([c7f4875](https://github.com/vdavid/prvw/commit/c7f4875),
  [3751813](https://github.com/vdavid/prvw/commit/3751813))
- Performance diagnostics via MCP and HTTP — cache state with per-image decode times, navigation history with timing,
  process RSS ([3751813](https://github.com/vdavid/prvw/commit/3751813))
- Cmdr-style logging with timestamps, colored log levels, and short module scopes. `RUST_LOG=debug` shows the full
  decode/preload/navigation flow ([ca94104](https://github.com/vdavid/prvw/commit/ca94104))
- JPEG decode benchmark (zune-jpeg vs turbojpeg, 20 Pixel photos). Key finding: NAS I/O is the bottleneck, not CPU
  decode ([1956496](https://github.com/vdavid/prvw/commit/1956496))

### Changed

- Window title format: `3 / 60 – photo.jpg` (position first for quick scanning), loading state: `3 / 60 – Loading...`
  ([7509317](https://github.com/vdavid/prvw/commit/7509317))

### Fixed

- Crash on navigation (Left/Right arrow) — muda 0.17 panics with `ZeroWidth` icon error when processing keyboard
  accelerators on macOS. All accelerators removed from menu items, shortcuts handled directly in keyboard event handler
  ([5aa98e8](https://github.com/vdavid/prvw/commit/5aa98e8))
- Fullscreen on/off via QA server now uses `set_fullscreen` directly instead of toggling
  ([e34b0f8](https://github.com/vdavid/prvw/commit/e34b0f8))

## [0.1.0] - 2026-04-11

### Added

- Initial release: GPU-accelerated image viewer for macOS
- `winit` 0.30 windowing with `ApplicationHandler` trait, `wgpu` 29 Metal rendering, `muda` 0.17 native menus
- Image formats: JPEG, PNG, GIF (first frame), WebP, BMP, TIFF
- Zoom and pan: scroll wheel (cursor-centered), click-drag, keyboard shortcuts (+/-/0), double-click toggle
  fit-to-window vs actual size
- Directory navigation: Left/Right arrows, background preloading of adjacent images (N±2)
- Fullscreen toggle (Cmd+F, F11), ESC to exit fullscreen or close
- Native macOS menu bar: File, View (zoom, fullscreen), Navigate (prev/next)
- Render-on-demand (no continuous GPU loop when idle)
- getprvw.com marketing website (Astro + Tailwind v4), sky blue brand palette
- Go check runner (14 checks: Rust, Astro, Go) with parallel execution and dependency graph
- GitHub Actions CI (ubuntu + macOS runners, change detection)
- Full docs: AGENTS.md, CONTRIBUTING.md, architecture, design principles, style guide
