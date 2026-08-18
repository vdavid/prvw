# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning:
[Semantic Versioning](https://semver.org/).

## [Unreleased]

Window-chrome fixes for macOS 26: the traffic lights click where you see them, and the window comes back whole after a
fullscreen round trip.

### Fixed

- The traffic lights are clickable where they're drawn. The offset that keeps them clear of the rounded corner was
  moving only their drawing on macOS 26, leaving the clickable circle 6 pt up and to the left, so anything but a
  dead-centre click on the red button did nothing ([cc2f5be3](https://github.com/vdavid/prvw/commit/cc2f5be3))
- Leaving fullscreen from the green traffic light no longer leaves the window dressed as fullscreen: the title bar came
  back missing, the background stayed black, and the corner drew as a double cutout at two radii until the next resize.
  The F key no longer toggles the wrong way afterwards either
  ([e56d08fe](https://github.com/vdavid/prvw/commit/e56d08fe))

- Fullscreen no longer gets stuck when you toggle it twice quickly: AppKit drops a transition asked for while one is
  still animating, so the second F (or green-button click) did nothing. Requests made mid-transition now wait their turn
  ([7ffe621b](https://github.com/vdavid/prvw/commit/7ffe621b))

### Non-app

- Refresh the toolchain and dependency floor: pnpm 11.22.0, Node 26.7.0, in-range website bumps, and ~197
  semver-compatible crate updates, each at least three days old
  ([41a330aa](https://github.com/vdavid/prvw/commit/41a330aa),
  [f00bbab8](https://github.com/vdavid/prvw/commit/f00bbab8))
- Take the dependency majors too: `nom-exif` 3.6 (its API moved under us: no more `parse_jpeg_exif`, rationals became a
  struct, `EntryValue::Time` became `DateTime`), `muda` 0.19, `moxcms` 0.9, and `base64` 0.23 on the app; Astro 7 and
  `eslint-plugin-astro` 3 on the website ([a201f1ca](https://github.com/vdavid/prvw/commit/a201f1ca),
  [880e9cbb](https://github.com/vdavid/prvw/commit/880e9cbb))
- Add debug-only QA endpoints that dump the window's AppKit view and layer tree and drive both window-zoom paths, so
  window-chrome bugs can be inspected rather than guessed at
  ([61eed499](https://github.com/vdavid/prvw/commit/61eed499))
- Build releases on GitHub-hosted runners, so a release no longer depends on the laptop staying awake and the three
  architecture builds run in parallel ([19716866](https://github.com/vdavid/prvw/commit/19716866))
- Stop a stray `--remove-orphans` in the website deploy from taking down the getcmdr.com site, by giving this stack its
  own Compose project name ([85a1f090](https://github.com/vdavid/prvw/commit/85a1f090))
- Relax the Renovate dependency cooldown from 14 days to 3, matching the pnpm install-side gate
  ([c68c44f4](https://github.com/vdavid/prvw/commit/c68c44f4))
- Answer pnpm 11.22's new build-script prompt for `sharp` with "don't run it", matching what the deploy image has done
  since the pnpm 11 move ([3edac64f](https://github.com/vdavid/prvw/commit/3edac64f))
- Restore the non-macOS (Linux CI) build broken by browse mode and clear the dead-code errors it exposed
  ([9d3faddd](https://github.com/vdavid/prvw/commit/9d3faddd),
  [0f202122](https://github.com/vdavid/prvw/commit/0f202122))

## [0.15.0] - 2026-06-17

Browse mode: a native folder tree and thumbnail gallery to flip through a folder's photos, plus live folder sync so the
viewer and browser follow changes on disk.

### Added

- Browse mode: press Enter to open a native folder sidebar and thumbnail gallery, flip through a folder's photos, and
  open one back into the viewer. Arrow keys move through the tree and grid, Tab switches panes, double-click or Enter
  opens, and Esc returns to the image you came from. Thumbnails stream in from QuickLook, and the tree and grid load off
  the main thread so even a slow network share never freezes the UI
  ([7328c461](https://github.com/vdavid/prvw/commit/7328c461),
  [59e19ebc](https://github.com/vdavid/prvw/commit/59e19ebc),
  [3a48c9e0](https://github.com/vdavid/prvw/commit/3a48c9e0),
  [4acfca79](https://github.com/vdavid/prvw/commit/4acfca79),
  [27c497a6](https://github.com/vdavid/prvw/commit/27c497a6),
  [a0fe94fc](https://github.com/vdavid/prvw/commit/a0fe94fc),
  [a20f861b](https://github.com/vdavid/prvw/commit/a20f861b),
  [f81d2474](https://github.com/vdavid/prvw/commit/f81d2474),
  [de873204](https://github.com/vdavid/prvw/commit/de873204),
  [0ffa5bba](https://github.com/vdavid/prvw/commit/0ffa5bba),
  [c4fc7f48](https://github.com/vdavid/prvw/commit/c4fc7f48))
- Launch straight into a folder: pass a directory on the command line and Prvw boots into browse at that folder
  ([32ee7e22](https://github.com/vdavid/prvw/commit/32ee7e22))
- Live folder sync: the viewer and browser follow changes on disk with no manual refresh — newly added images appear,
  edits re-decode in place, and deleting the image you're on navigates to the next (or shows an empty state when the
  folder runs out) ([f88f3760](https://github.com/vdavid/prvw/commit/f88f3760),
  [e6f54379](https://github.com/vdavid/prvw/commit/e6f54379),
  [68418f2f](https://github.com/vdavid/prvw/commit/68418f2f),
  [f7384a2e](https://github.com/vdavid/prvw/commit/f7384a2e))

### Fixed

- Keep the image title and zoom readout readable in macOS Light mode — they were white text that vanished on the light
  glass title bar ([a598857d](https://github.com/vdavid/prvw/commit/a598857d))

### Non-app

- Rename the internal `thumbnails` module to `previews`, freeing the name for the new grid-thumbnail cache
  ([1f5d358a](https://github.com/vdavid/prvw/commit/1f5d358a))
- Share the QuickLook worker through a second request path and add the headless grid-thumbnail plumbing behind the
  browse grid ([87729a9b](https://github.com/vdavid/prvw/commit/87729a9b),
  [fec427a1](https://github.com/vdavid/prvw/commit/fec427a1))
- Make browse fully observable from the QA server and cover the flow with end-to-end integration tests
  ([de7602df](https://github.com/vdavid/prvw/commit/de7602df))
- Capture the native-AppKit-over-Metal layering gotcha and consolidate the browse-mode docs to shipped state
  ([5ea7f0bb](https://github.com/vdavid/prvw/commit/5ea7f0bb),
  [0691865f](https://github.com/vdavid/prvw/commit/0691865f),
  [e700eb9c](https://github.com/vdavid/prvw/commit/e700eb9c),
  [d0151a33](https://github.com/vdavid/prvw/commit/d0151a33),
  [9718cd0e](https://github.com/vdavid/prvw/commit/9718cd0e))
- Upgrade the website to Astro 6, ESLint 10, and TypeScript 6, plus in-range dependency bumps
  ([6378682c](https://github.com/vdavid/prvw/commit/6378682c),
  [c67d2996](https://github.com/vdavid/prvw/commit/c67d2996))
- Gate Renovate dependency updates behind a 14-day release age to blunt supply-chain attacks
  ([8a9ec7bc](https://github.com/vdavid/prvw/commit/8a9ec7bc))
- Introduce CodeGraph code intelligence: gitignore its local index and document its call-graph reliability for prvw
  ([fce9f5ef](https://github.com/vdavid/prvw/commit/fce9f5ef),
  [700dacfa](https://github.com/vdavid/prvw/commit/700dacfa))
- Gitignore `.claude/worktrees/` ([db459776](https://github.com/vdavid/prvw/commit/db459776))

## [0.14.1] - 2026-06-15

Window-chrome polish: the traffic lights stay put, the title bar fits on one line and zooms on double-click, and zoom
respects your settings in fullscreen.

### Changed

- Double-click the title bar to zoom the window (fill the screen / restore) like a native macOS app, instead of toggling
  the image fit ([68bb58bc](https://github.com/vdavid/prvw/commit/68bb58bc))

### Fixed

- Keep the traffic-light buttons put across resizes, fullscreen, title changes, and navigation instead of snapping back
  to the corner, and place them correctly from the very first frame
  ([2bd58a7e](https://github.com/vdavid/prvw/commit/2bd58a7e),
  [68bb58bc](https://github.com/vdavid/prvw/commit/68bb58bc))
- Keep the title bar text on a single ellipsized line at every window width, instead of wrapping to two lines in a band
  of widths ([ef26fe7c](https://github.com/vdavid/prvw/commit/ef26fe7c))
- Re-fit the zoom when you turn on "Auto-fit window" on a zoomed-in image, so it no longer overflows the resized window
  ([5d9f06c3](https://github.com/vdavid/prvw/commit/5d9f06c3))
- Respect "Enlarge small images" in fullscreen, and stop disabling the toggle when auto-fit is on
  ([c742b845](https://github.com/vdavid/prvw/commit/c742b845))

### Non-app

- Fix the website deploy silently failing on pnpm 11: the server-side Docker build choked on ignored build scripts,
  leaving getprvw.com stuck on the old version ([d627b9a0](https://github.com/vdavid/prvw/commit/d627b9a0),
  [71b8af3a](https://github.com/vdavid/prvw/commit/71b8af3a))
- Finish the pnpm 11 migration so local dev, CI, and the deploy build all run the same pinned version
  ([bd432d7d](https://github.com/vdavid/prvw/commit/bd432d7d))
- Restore the non-macOS (Linux CI) build broken by the RAW-preview and thumbnail RAM-cap work, and clear the remaining
  dead-code errors it exposed ([ad008aa3](https://github.com/vdavid/prvw/commit/ad008aa3),
  [dabbc466](https://github.com/vdavid/prvw/commit/dabbc466))
- Force a clean install in the release checks so fresh-install failures surface locally, not just in CI
  ([cb6365f7](https://github.com/vdavid/prvw/commit/cb6365f7))
- Port release troubleshooting from Cmdr's guide: keep the Mac awake, create-dmg TCC degradation, and `latest.json`
  recovery ([0ba51dd7](https://github.com/vdavid/prvw/commit/0ba51dd7))
- The release agent pushes without asking when the release finishes clean
  ([453ece75](https://github.com/vdavid/prvw/commit/453ece75))

## [0.14.0] - 2026-06-15

A slideshow, Copy and Print, and instant previews the moment you open a RAW. The viewer also picks up a Liquid Glass
frame on macOS 26.

### Added

- Slideshow (⌘S): auto-advance through the folder with a crossfade, faster/slower on `]` / `[`, and time, crossfade, and
  loop settings; it waits for the next image to decode so there's no "Loading…" flash
  ([4012fde8](https://github.com/vdavid/prvw/commit/4012fde8),
  [3d916255](https://github.com/vdavid/prvw/commit/3d916255))
- Copy the image to the clipboard (⌘C, right-click): pastes the real file with full quality and EXIF, or pixels into
  editors and chat apps ([aa3750a8](https://github.com/vdavid/prvw/commit/aa3750a8))
- Print the current image (⌘P, right-click) through the system print dialog
  ([b7882b5f](https://github.com/vdavid/prvw/commit/b7882b5f))
- Show the camera's embedded preview instantly when you open a RAW (from Finder or by navigating), then snap to the
  sharp develop — no more ~450 ms blank wait ([175d01ef](https://github.com/vdavid/prvw/commit/175d01ef),
  [53927b9b](https://github.com/vdavid/prvw/commit/53927b9b))
- Liquid Glass window frame on macOS 26, matching Quick Look, across the viewer, Settings, About, and Onboarding
  ([625b5907](https://github.com/vdavid/prvw/commit/625b5907),
  [089b0376](https://github.com/vdavid/prvw/commit/089b0376),
  [4988055d](https://github.com/vdavid/prvw/commit/4988055d))

### Changed

- Thumbnail cache size now scales with your Mac's RAM instead of a fixed window, keeping memory respectful on small
  machines and generous on big ones ([d8cae942](https://github.com/vdavid/prvw/commit/d8cae942))
- Navigating away from a huge JPEG no longer blocks the next image, and a cancelled decode that finishes anyway is
  salvaged into the cache instead of wasted ([40f2f892](https://github.com/vdavid/prvw/commit/40f2f892),
  [65bfceec](https://github.com/vdavid/prvw/commit/65bfceec))
- Decode JPEGs straight to RGBA: a 24 MP photo opens in ~80 ms instead of ~250 ms and uses about half the peak memory
  ([37f6691e](https://github.com/vdavid/prvw/commit/37f6691e))
- Bump the Exif and histogram backdrop opacity so both panels stay legible over bright images
  ([8f6e5d5e](https://github.com/vdavid/prvw/commit/8f6e5d5e))
- Tidy the menu bar: Edit holds only "Copy image", macOS's auto-injected items are pruned, H/E/F shortcut hints show,
  and a Help menu appears ([15ccfa2d](https://github.com/vdavid/prvw/commit/15ccfa2d))
- Quiet non-actionable RAW decode warnings ([9bb360f0](https://github.com/vdavid/prvw/commit/9bb360f0))

### Fixed

- Show the photo's capture date in the Exif overlay again (it was blank on JPEG, HEIC, and WebP)
  ([a9ceb7ea](https://github.com/vdavid/prvw/commit/a9ceb7ea))
- Let WebP actually be set as Prvw's default format (it was registered under a UTI macOS never uses)
  ([8df9cf0f](https://github.com/vdavid/prvw/commit/8df9cf0f))
- Stop the file-association toggle knob flying off its track when setting a default
  ([0a9305b4](https://github.com/vdavid/prvw/commit/0a9305b4))
- Stop showing macOS's generic file-type icons as cache-miss placeholders
  ([1dad2fb4](https://github.com/vdavid/prvw/commit/1dad2fb4))

### Non-app

- Optimize the hot image dependencies in dev builds (the overrides were silently ignored in a member manifest)
  ([ed2f9430](https://github.com/vdavid/prvw/commit/ed2f9430))
- Fix the CI pnpm install broken by an upstream asset change, by switching mise to the npm backend
  ([c51af9e1](https://github.com/vdavid/prvw/commit/c51af9e1))
- Stop E2E test windows stealing the developer's focus, and add nextest retries to absorb GPU-contention flakes
  ([8ea24961](https://github.com/vdavid/prvw/commit/8ea24961))
- Refresh the agent docs (commit-at-will, worktrees, naming)
  ([1b5c3a9a](https://github.com/vdavid/prvw/commit/1b5c3a9a),
  [6297d580](https://github.com/vdavid/prvw/commit/6297d580),
  [0c5c00d2](https://github.com/vdavid/prvw/commit/0c5c00d2))

## [0.13.0] - 2026-05-05

Sort the folder by name, date, or file type.

### Added

- View → Sort by → Name, Date, or File type, with natural alphanumeric name sorting (so `photo_2` comes before
  `photo_10`) that persists across launches ([9a80ec3e](https://github.com/vdavid/prvw/commit/9a80ec3e),
  [bb95700e](https://github.com/vdavid/prvw/commit/bb95700e),
  [04ca1311](https://github.com/vdavid/prvw/commit/04ca1311))

### Non-app

- Run the full check suite before bumping the version, auto-detach stale Prvw mounts, and revive the self-hosted runner
  if its LaunchAgent died ([eb9b4196](https://github.com/vdavid/prvw/commit/eb9b4196))
- Caffeinate the Mac through the build and verify the GitHub Release assets and `latest.json` after the run succeeds
  ([d09d6471](https://github.com/vdavid/prvw/commit/d09d6471))
- Fix flaky integration tests that raced a fixed sleep under load
  ([5a567c0e](https://github.com/vdavid/prvw/commit/5a567c0e))

## [0.12.0] - 2026-04-27

RGB histogram and Exif overlays, loop navigation, and Home/End jumps.

### Added

- RGB histogram overlay (View → Histogram or `H`), with per-bin R/G/B counts on hover; persists across launches
  ([d811bb32](https://github.com/vdavid/prvw/commit/d811bb32),
  [a6c833c6](https://github.com/vdavid/prvw/commit/a6c833c6))
- Exif info overlay (View → Exif info or `E`): camera, exposure, lens, date, dimensions, software, and GPS — hidden when
  there's no Exif data ([419c6d54](https://github.com/vdavid/prvw/commit/419c6d54),
  [6846416a](https://github.com/vdavid/prvw/commit/6846416a))
- Loop navigation (Navigate → Loop navigation or `L`): wrap from the last image to the first and back, with a wrap-aware
  preloader ([88b37740](https://github.com/vdavid/prvw/commit/88b37740))
- Home / End jump to the first / last image ([1e8c316c](https://github.com/vdavid/prvw/commit/1e8c316c))

### Changed

- Bump the histogram and Exif backdrop opacity for legibility against bright images
  ([6846416a](https://github.com/vdavid/prvw/commit/6846416a))

### Fixed

- Keep the `prvw://state` snapshot fresh on background preloads
  ([88b37740](https://github.com/vdavid/prvw/commit/88b37740))
- Clear the previous image's histogram on cache-miss navigation so the right curve shows
  ([a6c833c6](https://github.com/vdavid/prvw/commit/a6c833c6))

### Non-app

- Add a dev-only `screenshot_window` MCP tool that captures the full native window, overlays included (debug builds
  only) ([9954a91d](https://github.com/vdavid/prvw/commit/9954a91d))
- Show the file size on the website download buttons ([b3b17f46](https://github.com/vdavid/prvw/commit/b3b17f46))
- Unify markdown and prose formatting across the monorepo with `oxfmt`, replacing Prettier
  ([a30c1e5d](https://github.com/vdavid/prvw/commit/a30c1e5d),
  [cdaa80af](https://github.com/vdavid/prvw/commit/cdaa80af),
  [47742664](https://github.com/vdavid/prvw/commit/47742664))
- Trigger website deploys from the Actions tab via `workflow_dispatch`
  ([446b8ae0](https://github.com/vdavid/prvw/commit/446b8ae0))
- Fix the website pricing card clipping the Download dropdown
  ([c1e467ec](https://github.com/vdavid/prvw/commit/c1e467ec))

## [0.11.0] - 2026-04-26

Navigation stays smooth on slow network shares, with a blurry thumbnail placeholder while the full image decodes.

### Added

- Show a blurry QuickLook thumbnail placeholder while the full image decodes on a cache miss
  ([80fa5102](https://github.com/vdavid/prvw/commit/80fa5102))
- Prefetch pixel dimensions in parallel so placeholders appear in 1 – 2 ms instead of up to 1.3 s on slow shares
  ([d02e61d7](https://github.com/vdavid/prvw/commit/d02e61d7))
- Surface "update available" on no-file launches from Finder and Dock too
  ([0ee6b337](https://github.com/vdavid/prvw/commit/0ee6b337))

### Changed

- Stop navigation freezing on slow network shares by moving QuickLook work off the main thread
  ([16d63b46](https://github.com/vdavid/prvw/commit/16d63b46))
- Prioritize immediate-neighbour thumbnails first so the next image has a placeholder ready
  ([d02e61d7](https://github.com/vdavid/prvw/commit/d02e61d7))
- Make the updater bundle swap atomic, so a crash mid-update always leaves one intact app
  ([0b52f850](https://github.com/vdavid/prvw/commit/0b52f850))
- Deregister the old bundle before swapping so "Open With" stops showing two Prvws after an update
  ([bb0bbbe0](https://github.com/vdavid/prvw/commit/bb0bbbe0))
- Only run the updater on apps installed in `/Applications`, skipping dev and Downloads copies
  ([0ee6b337](https://github.com/vdavid/prvw/commit/0ee6b337))

### Fixed

- Escape closes the About, Onboarding, and Settings windows again
  ([dc8ee8da](https://github.com/vdavid/prvw/commit/dc8ee8da))

### Non-app

- Refresh the website tagline, features, and terms ([0664402f](https://github.com/vdavid/prvw/commit/0664402f),
  [5bae3ce1](https://github.com/vdavid/prvw/commit/5bae3ce1))
- Give each integration test its own temp data dir so they stop flaking on a shared port-keyed path
  ([5fd0f0de](https://github.com/vdavid/prvw/commit/5fd0f0de))

## [0.10.0] - 2026-04-19

A deep RAW overhaul: wide-gamut color, HDR/EDR output on XDR displays, lens correction, DCP profiles, denoise and
clarity, and near-instant navigation in RAW folders.

### Added

- HDR / EDR output for RAW, visible on XDR displays, with a brightness gain slider
  ([01e53506](https://github.com/vdavid/prvw/commit/01e53506),
  [ce0e93ca](https://github.com/vdavid/prvw/commit/ce0e93ca),
  [95b78a91](https://github.com/vdavid/prvw/commit/95b78a91))
- Lens correction for non-DNG RAWs (distortion, TCA, vignetting) from a bundled LensFun database
  ([66386f45](https://github.com/vdavid/prvw/commit/66386f45))
- Clarity (local contrast) for RAW, on by default, with radius and amount sliders
  ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91))
- Chroma noise reduction for RAW, on by default ([9e44c7da](https://github.com/vdavid/prvw/commit/9e44c7da))
- RAW tuning sliders for sharpening, saturation, and tone midtone anchor, persisted
  ([9ea0fdae](https://github.com/vdavid/prvw/commit/9ea0fdae))
- RAW Settings panel: per-stage toggles for the pipeline, a custom DCP directory, and Reset to defaults
  ([dd834f17](https://github.com/vdavid/prvw/commit/dd834f17))
- Bundled DCP profile library (161 RawTherapee profiles) with fuzzy family matching for known-compatible cameras
  ([ed0787e1](https://github.com/vdavid/prvw/commit/ed0787e1))
- Adobe DCP support: filesystem and DNG-embedded profiles, LookTable, ProfileToneCurve, and dual-illuminant
  interpolation ([bf188289](https://github.com/vdavid/prvw/commit/bf188289),
  [2979a055](https://github.com/vdavid/prvw/commit/2979a055),
  [30347086](https://github.com/vdavid/prvw/commit/30347086))
- Highlight recovery so clipped skies and specular highlights stop drifting magenta or cyan
  ([cf24edf4](https://github.com/vdavid/prvw/commit/cf24edf4))
- DNG opcode support (GainMap, WarpRectilinear, FixBadPixels) so iPhone ProRAW gets correct lens-shading and distortion
  correction ([ecc99733](https://github.com/vdavid/prvw/commit/ecc99733))

### Changed

- Make navigation near-instant in RAW folders: route the current image through the background pipeline on a cache miss
  instead of blocking the main thread, with direction-aware preload and explicit GPU texture cleanup that fixes 4 GB+
  RSS bloat ([da9408a4](https://github.com/vdavid/prvw/commit/da9408a4),
  [128f7ee7](https://github.com/vdavid/prvw/commit/128f7ee7),
  [88e590d4](https://github.com/vdavid/prvw/commit/88e590d4),
  [4619f8ec](https://github.com/vdavid/prvw/commit/4619f8ec),
  [f0009a04](https://github.com/vdavid/prvw/commit/f0009a04))
- Decode 20 MP ARW previews in ~300 – 450 ms (was ~2 s) by switching the preloader to one dedicated worker that shares
  the global thread pool ([9408804b](https://github.com/vdavid/prvw/commit/9408804b))
- Preserve wide-gamut color through the whole RAW develop instead of clipping to sRGB, with a baseline exposure lift, a
  mild filmic tone curve, and luminance-only capture sharpening
  ([35998bdf](https://github.com/vdavid/prvw/commit/35998bdf),
  [51460ec5](https://github.com/vdavid/prvw/commit/51460ec5),
  [fa4a04f1](https://github.com/vdavid/prvw/commit/fa4a04f1),
  [a30fe79c](https://github.com/vdavid/prvw/commit/a30fe79c),
  [e71b850d](https://github.com/vdavid/prvw/commit/e71b850d))
- Speed up Clarity ~10× and DCP ~1.6× on 20 MP via downsampled blur and SIMD LUTs, and SIMD-accelerate the lens
  resampler ([c29fdba8](https://github.com/vdavid/prvw/commit/c29fdba8),
  [aa919dd4](https://github.com/vdavid/prvw/commit/aa919dd4))
- Run capture sharpening on the HDR path too, not just SDR ([4e76bcc8](https://github.com/vdavid/prvw/commit/4e76bcc8))
- Auto-skip DCP HueSatMap and tone curve when the camera match is fuzzy-alias only
  ([ca38eab4](https://github.com/vdavid/prvw/commit/ca38eab4),
  [76ca2786](https://github.com/vdavid/prvw/commit/76ca2786))
- Apply the DNG GainMap to all channels when `Planes = 1`, matching Adobe's reference
  ([237cba78](https://github.com/vdavid/prvw/commit/237cba78))
- Retune RAW defaults against Affinity and Preview, with a new baseline-exposure-offset slider
  ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91),
  [03e60478](https://github.com/vdavid/prvw/commit/03e60478),
  [d6c0f469](https://github.com/vdavid/prvw/commit/d6c0f469),
  [4c555581](https://github.com/vdavid/prvw/commit/4c555581))

### Fixed

- Fix lens correction doubling distortion instead of correcting it (reversed sign)
  ([146da606](https://github.com/vdavid/prvw/commit/146da606))
- Make HDR / EDR output actually render HDR-bright on XDR by bypassing a clipping color layer on the half-float path
  ([95b78a91](https://github.com/vdavid/prvw/commit/95b78a91))
- Stop scrolling through a RAW folder stuttering by deferring the render until the decode lands
  ([da9408a4](https://github.com/vdavid/prvw/commit/da9408a4))
- Fix iPhone and Pixel DNGs rendering with a radial red cast (GainMap plane indexing)
  ([723d1433](https://github.com/vdavid/prvw/commit/723d1433))

### Non-app

- Make preloader debug logs structured and greppable, gated behind `RUST_LOG=prvw::navigation=debug`
  ([aecd9390](https://github.com/vdavid/prvw/commit/aecd9390))
- Run HDR-diagnostic log scans only when info logging is on, and spell out EDR grant + peak values in the HDR log
  ([696da51d](https://github.com/vdavid/prvw/commit/696da51d),
  [6c9fc03f](https://github.com/vdavid/prvw/commit/6c9fc03f))
- Add per-stage RAW decode timing and a benchmark table, plus a "Preload next/prev images" toggle for clean cold-start
  measurement ([fb982318](https://github.com/vdavid/prvw/commit/fb982318))
- Add RAW pipeline test infrastructure: a synthetic Bayer DNG fixture, a delta-E metric, and a golden regression test
  ([706a400b](https://github.com/vdavid/prvw/commit/706a400b))

## [0.9.0] - 2026-04-17

Camera RAW support arrives.

### Added

- Camera RAW support (DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW) via `rawler`, with NEON SIMD on Apple Silicon
  ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a))
- File associations for all 10 RAW formats, split into standard and camera-RAW groups in Settings with vendor labels
  ([c2f5b567](https://github.com/vdavid/prvw/commit/c2f5b567))

### Changed

- Redesign onboarding into a four-step checklist ([4596d0e1](https://github.com/vdavid/prvw/commit/4596d0e1))

### Fixed

- Fix `apply_orientation` underflowing on zero-size input for EXIF orientation 2
  ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a))
- Restore per-row handler transparency in Settings → File associations
  ([4596d0e1](https://github.com/vdavid/prvw/commit/4596d0e1))

### Non-app

- Split `decoding.rs` into per-backend modules ([b4bc775a](https://github.com/vdavid/prvw/commit/b4bc775a))
- Gate macOS-only modules so cross-platform builds compile cleanly
  ([e9b5de49](https://github.com/vdavid/prvw/commit/e9b5de49),
  [3f009797](https://github.com/vdavid/prvw/commit/3f009797),
  [815b7275](https://github.com/vdavid/prvw/commit/815b7275),
  [96218dd4](https://github.com/vdavid/prvw/commit/96218dd4))

## [0.8.0] - 2026-04-17

A real Settings window, color rendering-intent control, and scroll and pinch zoom.

### Added

- Settings window with General, Zoom, Color, and File associations sections
  ([dc43505a](https://github.com/vdavid/prvw/commit/dc43505a),
  [0dd48491](https://github.com/vdavid/prvw/commit/0dd48491))
- File associations panel: per-UTI toggles, a master "Set all", and previous-handler rollback
  ([17b76a34](https://github.com/vdavid/prvw/commit/17b76a34))
- Rendering intent toggle (⌘⇧R) ([b42814f0](https://github.com/vdavid/prvw/commit/b42814f0))
- Scroll-to-zoom toggle, off by default; ⌘+scroll always zooms
  ([d55b7e9a](https://github.com/vdavid/prvw/commit/d55b7e9a))
- Pinch-to-zoom on the trackpad, cursor-centred ([ef8d0bfe](https://github.com/vdavid/prvw/commit/ef8d0bfe))
- Zoom keyboard shortcuts: ⌘= / ⌘- / ⌘0 ([ec2aba4a](https://github.com/vdavid/prvw/commit/ec2aba4a))
- Title bar toggle that reserves a strip so the overlays don't cover the image
  ([64e0d87b](https://github.com/vdavid/prvw/commit/64e0d87b))
- Title bar vibrancy: Liquid Glass on macOS 26, classic frosted glass on older versions
  ([7eede14b](https://github.com/vdavid/prvw/commit/7eede14b))

### Changed

- Restructure the source into infrastructure plus feature modules, each owning its runtime state
  ([27eca5e5](https://github.com/vdavid/prvw/commit/27eca5e5),
  [e88027ba](https://github.com/vdavid/prvw/commit/e88027ba))

### Fixed

- Closing the onboarding window now quits the app ([e81bbdfa](https://github.com/vdavid/prvw/commit/e81bbdfa))
- Fix CGColor / CGColorSpace encoding crashes in `setColorspace:` and the Settings separator
  ([17b76a34](https://github.com/vdavid/prvw/commit/17b76a34))

### Non-app

- Add an integration test suite (17 tests), each on its own dynamic port
  ([0dd48491](https://github.com/vdavid/prvw/commit/0dd48491))

## [0.7.0] - 2026-04-16

ICC color management — embedded profiles transform to your actual display.

### Added

- ICC color management: embedded source profiles transform to the display profile, re-decoding when the display changes
  ([ee226acc](https://github.com/vdavid/prvw/commit/ee226acc),
  [94820a8a](https://github.com/vdavid/prvw/commit/94820a8a))
- View toggles: ICC color management (⌘⇧I) and Color match display (⌘⇧C)
  ([a0883307](https://github.com/vdavid/prvw/commit/a0883307),
  [b952b64c](https://github.com/vdavid/prvw/commit/b952b64c))

### Changed

- Swap the ICC engine from lcms2 to moxcms (pure Rust, NEON SIMD), cutting a 24 MP transform from 247 ms to 45 ms
  ([f568b18f](https://github.com/vdavid/prvw/commit/f568b18f))

### Fixed

- Use the authoritative display ID for screen detection ([fcdefe3c](https://github.com/vdavid/prvw/commit/fcdefe3c))
- Fix a BGRA → RGBA swap in screenshot capture ([ee226acc](https://github.com/vdavid/prvw/commit/ee226acc))

## [0.6.3] - 2026-04-15

Finder double-click finally works reliably.

### Fixed

- Finder double-click works: inject `application:openURLs:` directly onto winit's app delegate
  ([329ba2a8](https://github.com/vdavid/prvw/commit/329ba2a8))

### Changed

- Use logical pixels for zoom, so 100% means native size on Retina (was 200%)
  ([a32f3950](https://github.com/vdavid/prvw/commit/a32f3950))
- Add `Logical<T>` / `Physical<T>` newtypes to prevent mixing pixel spaces
  ([80d795a0](https://github.com/vdavid/prvw/commit/80d795a0))

### Non-app

- Remove 329 lines of dead modal onboarding code ([86460872](https://github.com/vdavid/prvw/commit/86460872),
  [9ebca1e4](https://github.com/vdavid/prvw/commit/9ebca1e4))

## [0.6.2] - 2026-04-15

Finder open and updater fixes.

### Fixed

- Fix "cannot open files in JPEG Image format" by adding `CFBundleTypeRole` to the `Info.plist` document types
  ([6664a2ff](https://github.com/vdavid/prvw/commit/6664a2ff))
- Re-register document types after an update so macOS picks them up
  ([b518d438](https://github.com/vdavid/prvw/commit/b518d438))

### Non-app

- Add `libxdo-dev` to the Linux CI build for `muda` ([bf82cb74](https://github.com/vdavid/prvw/commit/bf82cb74))

## [0.6.1] - 2026-04-15

Finder double-click via Apple Events.

### Fixed

- Finder double-click works: keep the event loop running and wait briefly for Apple Events before showing onboarding
  ([5b4d86a9](https://github.com/vdavid/prvw/commit/5b4d86a9))

### Changed

- Make the onboarding window non-modal so Apple Events and QA commands arrive while it shows
  ([5b4d86a9](https://github.com/vdavid/prvw/commit/5b4d86a9))

### Non-app

- Refactors: `scale_factor` on App, a `TextBlock` builder, a `MonitorBounds` helper, and `Logical` aliases
  ([e7fb92c8](https://github.com/vdavid/prvw/commit/e7fb92c8))

## [0.6.0] - 2026-04-15

Auto-fit window, native AppKit dialogs, and settings persistence.

### Added

- Auto-fit window: resize and center to each image, clamped to the screen
  ([6a8e03db](https://github.com/vdavid/prvw/commit/6a8e03db))
- Auto-fit zoom: resize the window as you zoom, keeping the cursor pivot fixed when growing
  ([6c4764f2](https://github.com/vdavid/prvw/commit/6c4764f2))
- Enlarge small images toggle, off by default ([c2c73c8f](https://github.com/vdavid/prvw/commit/c2c73c8f))
- Checkerboard background for transparent images ([d4817745](https://github.com/vdavid/prvw/commit/d4817745))
- Overlay text with pill backgrounds and middle truncation for long filenames
  ([d0006fca](https://github.com/vdavid/prvw/commit/d0006fca))
- Native AppKit windows for About, Settings, and Onboarding, with ESC-to-close
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb))
- Settings persistence, with a `PRVW_DATA_DIR` override for dev and test
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb))
- View → Refresh (R) ([593cac91](https://github.com/vdavid/prvw/commit/593cac91))

### Changed

- Make the zoom model absolute (1.0 = one image pixel per screen pixel)
  ([3b2f51ef](https://github.com/vdavid/prvw/commit/3b2f51ef))
- Slow scroll zoom to 5% per tick (was 15%) ([d2ce180b](https://github.com/vdavid/prvw/commit/d2ce180b))
- Unify keyboard, menu, and QA input through `AppCommand` ([4dbf3266](https://github.com/vdavid/prvw/commit/4dbf3266))
- Change the background from dark grey to black ([644132bb](https://github.com/vdavid/prvw/commit/644132bb))
- Set file associations via direct CoreServices FFI instead of `swift -e` scripts
  ([644132bb](https://github.com/vdavid/prvw/commit/644132bb))

### Non-app

- Improve the MCP server: JSON state responses, synchronous completion, a `prvw://settings` resource, and new geometry
  and zoom tools ([593cac91](https://github.com/vdavid/prvw/commit/593cac91),
  [c2c73c8f](https://github.com/vdavid/prvw/commit/c2c73c8f))

## [0.5.0] - 2026-04-12

Text rendering, an onboarding screen, and a styled DMG installer.

### Added

- Text rendering via glyphon, an onboarding screen on no-file launch, a header overlay, a frosted-glass title bar, and a
  styled DMG installer ([177febfe](https://github.com/vdavid/prvw/commit/177febfe),
  [34d748d5](https://github.com/vdavid/prvw/commit/34d748d5),
  [c74a12e7](https://github.com/vdavid/prvw/commit/c74a12e7))

## [0.4.0] - 2026-04-12

An auto-updater and website downloads.

### Added

- Auto-updater: background check, DMG download, and `.app` replacement, with a `PRVW_UPDATE_URL` test override
  ([850d5e16](https://github.com/vdavid/prvw/commit/850d5e16))

### Fixed

- Fix `.app` replacement failing over a non-empty directory ([1d7fb33e](https://github.com/vdavid/prvw/commit/1d7fb33e))

### Non-app

- Add website download buttons with architecture detection (Apple Silicon / Intel / Universal)
  ([6709c3e3](https://github.com/vdavid/prvw/commit/6709c3e3))
- Add PostHog session replay (cookieless) and Umami download tracking
  ([304788e9](https://github.com/vdavid/prvw/commit/304788e9),
  [88f14d29](https://github.com/vdavid/prvw/commit/88f14d29),
  [7affe0f2](https://github.com/vdavid/prvw/commit/7affe0f2))
- Add a sitemap ([3937ac94](https://github.com/vdavid/prvw/commit/3937ac94))
- Add terms and conditions and privacy policy pages ([75b85166](https://github.com/vdavid/prvw/commit/75b85166))
- Add deploy infrastructure: webhook, CI auto-deploy on push to main, and Hetzner Docker / nginx
  ([381fee20](https://github.com/vdavid/prvw/commit/381fee20),
  [6f63be09](https://github.com/vdavid/prvw/commit/6f63be09))
- Fix the website download dropdown not opening ([424fd59c](https://github.com/vdavid/prvw/commit/424fd59c))

## [0.3.0] - 2026-04-12

Multiple-file args, keyboard shortcuts, native menus, and the macOS `.app` bundle.

### Added

- Open multiple files as the navigation set, for Finder multi-select "Open With"
  ([c49761dc](https://github.com/vdavid/prvw/commit/c49761dc))
- Keyboard shortcuts: Space / `]` next, Backspace / `[` previous, F / Enter fullscreen, 1 actual size
  ([f0c24f8a](https://github.com/vdavid/prvw/commit/f0c24f8a))
- Clickable menus (About, View, Navigate) and an About dialog
  ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8),
  [f0c24f8a](https://github.com/vdavid/prvw/commit/f0c24f8a))
- macOS `.app` bundle with `Info.plist`, file type associations, and an app icon
  ([4863e682](https://github.com/vdavid/prvw/commit/4863e682),
  [6abdcc65](https://github.com/vdavid/prvw/commit/6abdcc65))
- Apple Events handler for opening files while the app is running
  ([af4ccac3](https://github.com/vdavid/prvw/commit/af4ccac3))

### Fixed

- Always preserve aspect ratio on resize, fix zoom-out-past-fit and the zoom pivot after resize, and clamp pan to image
  edges ([7e9d0dd8](https://github.com/vdavid/prvw/commit/7e9d0dd8))
- Fix a blank startup when the wgpu surface is `Occluded` during window creation
  ([b5c0a81b](https://github.com/vdavid/prvw/commit/b5c0a81b))

### Non-app

- Add release infrastructure: GitHub Actions workflow, signing, DMG creation, and notarization
  ([4863e682](https://github.com/vdavid/prvw/commit/4863e682))
- Add a root Cargo workspace ([35c1e805](https://github.com/vdavid/prvw/commit/35c1e805))
- Fix the Linux CI build deps for winit ([4e54a6ff](https://github.com/vdavid/prvw/commit/4e54a6ff))

## [0.2.0] - 2026-04-11

Faster JPEG decoding and parallel preloading, plus EXIF orientation and the embedded MCP server.

### Added

- JPEG decoding via `zune-jpeg` with SIMD, ~27% faster than the `image` crate
  ([2e67fd35](https://github.com/vdavid/prvw/commit/2e67fd35))
- Parallel preloading with rayon, ~2 – 3× faster for NAS browsing
  ([2e67fd35](https://github.com/vdavid/prvw/commit/2e67fd35))
- Priority preloading with cancellation tokens, for sub-2 ms cancellation on NAS
  ([68dbe31e](https://github.com/vdavid/prvw/commit/68dbe31e))
- EXIF orientation support so phone photos display right-side-up
  ([d2d95bc9](https://github.com/vdavid/prvw/commit/d2d95bc9))

### Changed

- New window title format with position first (`3 / 60 – photo.jpg`)
  ([7509317e](https://github.com/vdavid/prvw/commit/7509317e))

### Fixed

- Fix a crash on Left/Right arrow navigation from muda accelerators
  ([5aa98e8b](https://github.com/vdavid/prvw/commit/5aa98e8b))
- Fix fullscreen toggling via the QA server ([e34b0f83](https://github.com/vdavid/prvw/commit/e34b0f83))

### Non-app

- Add the embedded MCP server (streamable HTTP) with navigate, key, zoom, fullscreen, open, and screenshot tools, plus
  diagnostics ([c7f4875f](https://github.com/vdavid/prvw/commit/c7f4875f),
  [3751813b](https://github.com/vdavid/prvw/commit/3751813b))
- Add Cmdr-style logging with timestamps and coloured levels
  ([ca94104f](https://github.com/vdavid/prvw/commit/ca94104f))
- Add a JPEG decode benchmark (zune-jpeg vs turbojpeg) ([1956496b](https://github.com/vdavid/prvw/commit/1956496b))

## [0.1.0] - 2026-04-11

The first release — a fast, GPU-accelerated image viewer for macOS.

### Added

- Initial release: GPU-accelerated macOS image viewer (`winit` + `wgpu` + `muda`). JPEG, PNG, GIF, WebP, BMP, and TIFF;
  zoom and pan; directory navigation with background preloading; fullscreen; native menus; render-on-demand; and the
  getprvw.com marketing site ([2e9aa754](https://github.com/vdavid/prvw/commit/2e9aa754))
