# Prvw: cross-platform plan (Windows first, Linux second)

Status: proposal, not started. Written 2026-08-23 against `v0.15.1-2-gaefca22`.

This plan answers one question: what does it take to run Prvw on Windows (and, with less priority, Linux), and in what
order should we do it. It's written so an implementing agent can pick up any milestone without re-deriving the research.
Every number below was measured against the source, and the file:line citations are there so you can re-check them
rather than trust them.

## The short answer

The engine is already portable; the app around it isn't. 61.0% of `apps/desktop/src` sits in files that never mention
`target_os` at all, and 71.6% sits outside the files gated to macOS wholesale. Call it about two thirds of the codebase
that needs no Windows work. The rest is AppKit, and roughly 8,800 of those lines are app chrome (settings, onboarding,
about, browse mode) with no Windows equivalent, which would have to be written a second time.

So the difficulty splits in three:

- **The first Windows window showing an image: one to two weeks.** That's M0. Most of it is deleting macOS assumptions
  from code that's already portable; the rest is making CI able to see all three platforms.
- **A Windows build a photographer would actually use** (viewer, RAW, display-aware color, real menus, no settings UI):
  **six to nine weeks.** M0 through M3. Not a ship target given the full-parity decision, but the point where the thing
  becomes usable enough to dogfood.
- **Windows parity with what macOS ships today: three to six months.** Most of that is re-implementing UI we already
  have, in a second toolkit, forever.

The gap between the first and second bullets is the interesting part. Everything genuinely hard about Prvw (RAW
decoding, ICC transforms, GPU rendering, preloading) is already platform-neutral. What isn't portable is the **launch
and input surface**: how the app starts, how a file reaches it, what the mouse wheel means, what the keyboard sends.
That's where "portable Rust that compiles" and "an app a Windows user can use" diverge, and it's why M1 is the biggest
non-UI milestone here.

## Where we stand today

Measured on 2026-08-23:

- **`apps/desktop/src`**: 47,394 lines across 103 `.rs` files.
- **Files that never mention `target_os`**: 74 files, 28,904 lines, 61.0% of `src`.
- **`#[cfg(target_os = "macos")]` attributes**: 233. Plus 16 `#[cfg(not(target_os = "macos"))]` fallbacks, nine
  `cfg_attr(not(...))` forms, and 18 `all(debug_assertions, target_os = "macos")` gates.
- **`#[cfg(target_os = "windows")]` attributes**: zero. No non-Apple code path has ever been written.
- **Files gated to macOS in their entirety**: 33 files, 13,476 lines, 28.4% of `src`.
- **Of those 13,476 lines, 2,059 call an Apple API directly.** The rest is ordinary Rust wrapped around them.
- **C dependencies on the Windows MSVC target**: two today (`zstd-sys` and `aws-lc-sys`), and M0 step 7 removes one of
  them. Linux carries 10 more (nine GTK plus `libxdo-sys`), and M0 step 8 removes all 10.

That second-to-last bullet is the one to sit with. Only 2,059 lines are literally `objc2`, `msg_send!`, or
CoreFoundation calls. The other 11,417 in those files splits two ways: some is genuinely AppKit-shaped (delegate
classes, `Retained<>` lifetime juggling, Auto Layout constraint building) and dies with the platform; some is fully
portable and gated only because its one consumer happens to be macOS. The clearest example is `previews/`: `mod.rs`
(422), `scheduler.rs` (405), and `dim_prefetch.rs` (185) contain **zero** Apple API calls, and `browser/grid_listing.rs`
(122) is the same story. Roughly 1,100 lines there come along for free the moment their consumer exists on Windows.

### The whole-file count hides where the work is

Four files carry 171 of the 233 gates between them and appear in **no** "macOS-only file" list, because they're mostly
portable code with macOS branches threaded through:

- `src/app.rs`: **64 gates** in 3,560 lines. The most-gated file in the crate.
- `src/window.rs`: **55 gates** in 1,687 lines.
- `src/browser/mod.rs`: **32 gates** in 1,132 lines.
- `src/app/executor.rs`: **20 gates** in 803 lines.

Then `src/commands.rs` (12), `src/main.rs` (7), `src/menu.rs` (6), `src/browser/tree_model.rs` (6), `src/platform.rs`
(5), `src/settings/mod.rs` (4), and a long tail.

Most of those gates are browse-mode-shaped and land in M5. But an agent reading only the "macOS-only files" list would
have no idea `app.rs` is involved at all, which is why this section exists. It's also where the launch and input defects
in M1 live.

### Four findings that change the shape of the work

**1. The non-macOS build already compiles, and CI keeps it that way.** `.github/workflows/ci.yml` runs the
`desktop-rust` job on `ubuntu-latest`: `rustfmt`, `clippy -- -D warnings`, and `cargo nextest run`. It installs
`libglib2.0-dev libgtk-3-dev libxdo-dev` for muda's Linux backend. There are commits dedicated to keeping it green
(`9d3fadd` "Bugfix: restore the non-macOS build broken by browse mode", plus `ad008aa`, `dabbc46`, and `0f20212`, all
2026-06-15 to 2026-06-17). We're not starting from a wall of compile errors; we're starting from a build that already
type-checks on a second OS.

Read that carefully, though: on Linux, nextest **compiles everything and runs the platform-neutral unit tests**. It does
not run the full suite. `tests/integration.rs:5` and `tests/color_management.rs:7` are both
`#![cfg(target_os = "macos")]`, and a large share of unit tests are macOS-gated too (`decoding/raw.rs:1517`,
`decoding/mod.rs:515`, `color/transform.rs:194`, `color/dcp/mod.rs:419`).

**2. It compiles but it can't launch.** `color::State::from_settings` (`src/color/mod.rs:40`, whose body calls
`srgb_icc_bytes()` at `:45`, reached from `src/app.rs:229`) reads `/System/Library/ColorSync/Profiles/sRGB Profile.icc`
and **panics** when the file is missing (`src/color/transform.rs:10`). `App::new` builds that state unconditionally, so
a Linux or Windows binary dies at startup. The codebase already knows: `src/color/dcp/mod.rs:419` gates tests "to avoid
the `srgb_icc_bytes` panic on Linux". Fixing it takes about an hour (M0 step 2) and it's the highest-leverage change in
this whole document.

**3. Even with that fixed, launching Prvw the normal Windows way gives you an invisible process.** `app.rs:3257`: when
`waiting_for_file` is true, `resumed()` sets `ControlFlow::Poll` and **returns without calling `initialize_viewer`**, so
there's no window, no renderer, nothing. The 500 ms timer at `app.rs:3300` then calls
`crate::onboarding::show_window()`, which is macOS-only (`main.rs:28-29` gates the whole module). And the File menu
(`menu.rs:129-141`) is Print, a separator, and Close window: **there is no Open item anywhere in the app**.

On macOS none of that matters, because Finder delivers files through Apple Events (`platform/macos/open_handler.rs`). On
Windows, a Start-menu shortcut, a taskbar pin, or a desktop icon are the normal ways to launch, and all of them pass no
argv. The user gets a process with no window and no way to recover, because `AppCommand::OpenFile` only ever arrives
from an Apple Event or the debug QA server. This is M1 step 1, and it's the single most important thing in the
milestone.

**4. The macOS side of CI is weaker than the Linux side.** `.github/workflows/ci.yml:101-127`: the `desktop-rust-macos`
job's only step is `cargo build`. No rustfmt, no clippy, no tests. Combined with the crate-level gates above, **the E2E
suite and the color-management suite never run in CI at all**, on any platform. They run on David's Mac or nowhere.

This is a cross-platform problem before it's a tidiness one. M0 changes color behavior on macOS (see the
`profiles_match` warning below), and every later milestone touches shared code. Without macOS tests in CI, "green on all
platforms" is unachievable for the platform that actually ships today. Fixing it is M0 step 1.

## What's already portable

These need no work beyond compiling:

- **`decoding/` (6,220 lines).** `zune-jpeg`, `image`, `rawler` for camera RAW. All pure Rust.
- **`color/` minus display detection: 8,719 of 9,420 lines**, including all 3,925 lines of `dcp/`. `moxcms` for ICC
  transforms, DCP profile parsing, tone curves, highlight recovery, clarity, sharpening. Pure Rust. (The macOS remainder
  is `display_profile.rs` at 595 and `settings_panel.rs` at 106.) One caveat: `color/dcp/discovery.rs` is portable code
  that behaves wrongly off macOS, see M0 step 3.
- **`lensfun`.** It's `vdavid/lensfun-rs`, a **pure-Rust port** of LensFun with no C dependency. Worth stating out loud,
  because a C LensFun would have been a real Windows headache and it isn't one.
- **`render/` (1,844 lines).** wgpu with `Backends::all()`, so it compiles and runs on Windows with no code change,
  though M1 step 14 pins DX12 there. `glyphon` for text, and the font-family lookup needs a Windows answer; see M1
  step 8.
- **`navigation/` (2,309), `histogram/` (407), `exif_overlay/` (489), `slideshow/` core (143).**
- **`folder_watch.rs`.** `notify` 8.2 maps to `ReadDirectoryChangesW` on Windows and inotify on Linux.
- **The pure cores under `browser/`:** `grid_model.rs` (333), `grid_scheduler.rs`, `thumbnail_cache.rs` (318),
  `tree_model.rs` (631). `tree_model::build_roots` is already pure and platform-agnostic; only `enumerate_roots` /
  `enumerate_volumes` (the `NSFileManager` call) is macOS-specific. That seam is cut in the right place.
- **`pixels.rs`** and **`diagnostics.rs`** (bar one `ps` call at `diagnostics.rs:59`). **`input.rs`** too, bar its Enter
  binding at `input.rs:57`, which M1 step 3 turns off outside macOS.

**Three things that look portable and aren't:**

- **`commands.rs` (12 gates).** The `AppCommand` enum itself carries macOS-only variants.
- **`qa/` (1,694 lines, 13 gates).** It compiles, but the debug endpoints at `qa/http.rs:138,154,164` and the
  `screenshot_window` tool at `qa/mcp.rs:803` are `#[cfg(all(debug_assertions, target_os = "macos"))]` and shell out to
  `/usr/sbin/screencapture`. Two browse routes have `not(macos)` stubs that return HTTP 400 (`qa/http.rs:487`, `:513`).
  So the tool agents use for visual QA on this project has no Windows implementation. See M1 step 11.
- **`zoom/` (529 lines).** The math is portable, but the wheel and gesture handling that drives it lives in
  `app.rs:3443-3486` and is macOS-shaped in four separate ways. See M1 step 5, the highest-density collection of real
  bugs in this plan.

## What's macOS-only, and what Windows offers instead

Ordered by cost.

### 1. Browse mode, about 3,100 lines plus most of `browser/mod.rs`'s 32 gates

`NSOutlineView` source list, `NSCollectionView` thumbnail grid, `NSSplitView`, all layered as siblings of winit's
content view above the Metal layer (`grid.rs` 1,033, `outline.rs` 1,361, `split_view.rs` 569, `grid_listing.rs` 122).

Windows has no drop-in. `SysTreeView32` plus an owner-drawn `ListView` is the closest Win32 analogue, and composing
child HWNDs against a wgpu swapchain brings its own z-order and redraw fights. The AppKit version already fights this
exact problem: a transparent Metal pixel occludes in-window content, which is why browse mode hides one surface and
shows the other rather than compositing. That constraint is not macOS-specific; it'll be there on DXGI too.

**This is the largest item, and the main reason to consider drawing the browse UI ourselves in wgpu.**

### 2. Settings window, about 3,400 lines

`settings/window.rs` (863), `widgets.rs` (87), `panels/general.rs` (133), `panels/raw.rs` (1,157), plus the per-feature
panels that live with their features: `color/settings_panel.rs` (106), `zoom/settings_panel.rs` (87),
`slideshow/settings_panel.rs` (237), `file_associations/settings_panel.rs` (748).

That's a lot of `NSStackView`, `NSSwitch`, and `NSSlider` form-building. Raw Win32 dialog equivalents are worse to write
and worse to maintain. This is the second reason to consider a shared GPU-drawn UI.

### 3. Onboarding and about, 2,349 lines

`onboarding/mod.rs` (812), `onboarding/defaults_sentence.rs` (624), `onboarding/checkmark.rs` (661), `about.rs` (252).
Hand-built AppKit panels that run **before** `EventLoop::new()` to dodge the nested-run-loop segfault. On Windows that
constraint disappears (no autorelease pools), but the UI still has to be written. Note that onboarding is also the
**only** thing the no-argument launch path shows today, which is why M1 step 1 can't wait for M6.

### 4. File associations, 1,102 lines, and it can't reach parity

macOS lets us call `LSSetDefaultRoleHandlerForContentType` and claim a UTI outright. **Windows removed that.** Since
Windows 10 20H2, an app cannot programmatically set itself as the default handler for an existing user account; registry
edits and `assoc` / `ftype` are ignored. The most an app can do is register a ProgID under
`HKCU\Software\Classes\Applications\prvw.exe` plus `OpenWithProgids` entries, then deep-link the user to
`ms-settings:defaultapps` and let them confirm.

**Product consequence:** the onboarding "Set as default viewer" button and the whole 16-toggle file-associations panel
become, on Windows, "register the types, then show a one-line explainer and a button that opens Windows Settings". Write
new copy for that rather than translating the macOS flow.

### 5. Previews and thumbnails, 1,807 lines

`QLThumbnailGenerator` backed by `quicklookd`'s system-wide cache. Windows has a good analogue in
`IShellItemImageFactory::GetImage` / `IThumbnailCache`: async, system-owned cache, zero disk cost to us. The scheduler,
byte-budget cache, and dim-prefetcher are already pure, so only the submission worker changes.

**Two things the module's name hides.** First, `previews` also feeds **launch-time window sizing**: `app.rs:843` gates
`apply_preview_auto_fit` behind macOS because `source_dimensions` (`previews/mod.rs:257`, backed by
`previews/metadata.rs`) is where the pre-decode dimensions come from. Off macOS the window keeps its initial size until
the full RAW develop lands, which is a visible pop on the slowest open path. Second, `metadata.rs` has a three-tier
dispatch (`metadata.rs:172-183`) and the tiers do **not** split the way you'd hope: PNG/GIF/BMP go to the `image` crate,
JPEG to a combined read, and `_ => read_dimensions(path)` sends **RAW, HEIC, WebP, TIFF, and every unknown extension to
ImageIO**. RAW, the format that motivates the whole thing, is on the Apple path. See M1 step 4.

The Windows catch on the shell route: it only produces RAW thumbnails when Microsoft's Raw Image Extension is installed.
Since Prvw already extracts embedded RAW previews itself (`decoding/raw_preview.rs`, portable), a self-hosted preview
path is a real option: more work now, less work forever, and identical behavior on all three platforms.

### 6. Display color profile, 595 lines, and the good news about HDR

`display_profile.rs` uses `CGDisplayCopyColorSpace` plus `CGColorSpaceCopyICCData` to read the active display's ICC
bytes, and sets `CAMetalLayer.colorspace` for the EDR path.

Windows equivalents exist and are smaller: `MonitorFromWindow` to find the window's display, then `GetICMProfileW` on
that monitor's DC for the SDR profile, or `ColorProfileGetDisplayDefault` for advanced-color displays. Feed the bytes
into the existing `moxcms` transform and everything downstream is unchanged.

**The HDR path got much cheaper recently.** `Cargo.toml` requires `wgpu = "29.0.1"` and the lock resolves 29.0.4, which
has no surface color-space API, so scRGB on DX12 would have meant reaching through `Surface::as_hal` to call
`IDXGISwapChain3::SetColorSpace1` by hand. **wgpu 30.0.0 (2026-07-01) added `SurfaceColorSpace`, and
`ExtendedSrgbLinear` (scRGB, the exact analogue of Metal's EDR) is supported on DX12.** A version bump plus a config
field replaces a hal-level hack. Note the DX12 caveats in the wgpu docs: `DisplayP3`, `ExtendedSrgb`,
`ExtendedDisplayP3`, and `Bt2100Hlg` are unsupported there; you get `Auto`, `Srgb`, `ExtendedSrgbLinear`, and
`Bt2100Pq`.

This matters more than its line count suggests. Display-aware color management is the README's flagship differentiator.
A Windows build without it is another image viewer.

### 7. Auto-updater, 489 lines, full rewrite

Every line is macOS: `hdiutil` mounting a DMG, `renamex_np(RENAME_SWAP)` for the atomic bundle swap, `lsregister` to
keep Launch Services honest, `osascript` for the admin-escalation fallback.

Windows does let you rename a running `.exe`, so the classic rename-then-replace-then-restart approach works, or we lean
on an installer-based updater. Either way it's from scratch, and it's coupled to whichever installer we pick. The
manifest fetch and version comparison at the top of the file are portable; everything below is not.

### 8. Window chrome, 55 gates in `window.rs`

Liquid Glass / `NSGlassEffectView`, vibrancy fallback, traffic-light nudging via `NSViewFrameDidChangeNotification`,
native title and zoom `NSTextField` labels, and reading fullscreen state from AppKit because winit's cache goes stale.

Windows: **the obvious answer doesn't work.** A wgpu HWND swapchain paints the whole client area, so a DWM backdrop
(`DWMWA_SYSTEMBACKDROP_TYPE`, Mica or Acrylic) sits fully occluded behind it. Getting system material behind our own
content needs `CreateSwapChainForComposition` plus DirectComposition, which wgpu's DX12 backend doesn't use for HWND
surfaces. So Liquid Glass has no v1 equivalent on Windows.

Related and concrete: `render/renderer.rs:301-312` prefers `PostMultiplied` or `PreMultiplied` alpha "so the title bar
area can show vibrancy through the transparent clear color", falling back to `surface_caps.alpha_modes[0]`. On DXGI that
fallback is `Opaque`, so the `LoadOp::Clear(wgpu::Color::TRANSPARENT)` at `renderer.rs:1033` renders as black. Harmless
for a black-background viewer, but the title-strip design degrades silently. Accept winit's standard decorations and an
opaque black surface for v1.

A custom caption needs `WM_NCCALCSIZE` if we revisit later. Non-macOS stubs already exist here (`window.rs:1507` falls
back to winit's own `fullscreen()`), so the seams are cut. `MonitorBounds::from_window` (`window.rs:1521-1535`) is the
one piece with a genuine Windows bug waiting in it; see M1 step 6.

### 9. Menus, 428 lines, and Windows is served far better than Linux

muda exposes `init_for_hwnd(hwnd)` and `show_context_menu_for_hwnd`, both of which take **any** HWND, including winit's,
and muda installs its own `SetWindowSubclass` proc to catch `WM_COMMAND` from menu clicks. So the menu bar itself
composes with winit's window procedure and `menu::poll_menu_event` keeps working unchanged.

**Keyboard accelerators are a different story and need real work.** muda's own docs on `init_for_hwnd`
(`muda-0.19.3/src/menu.rs:212-216`) say: "For accelerators to work, the event loop needs to call `TranslateAcceleratorW`
with the `HACCEL` returned from `Menu::haccel`". winit's Windows event loop does not do that, so after the Cmd-to-Ctrl
remap, Ctrl+C, Ctrl+P, Ctrl+= / Ctrl+-, and Ctrl+, would all silently do nothing. See M1 step 9.

**Linux is worse.** muda offers only `init_for_gtk_window` there, and winit can't hand it a `gtk::Window`. `menu.rs:337`
calls `init_for_nsapp()` as the sole wiring, so on Linux today the menu bar never attaches at all. Linux needs an in-app
menu (drawn by us) or no menu bar.

### 10. The small stuff, roughly 600 lines total

- `platform/macos/clipboard.rs` (115): `NSPasteboard` file URL plus bitmap. Windows: `SetClipboardData` with `CF_DIB`
  and `CF_HDROP`, or the `arboard` crate.
- `platform/macos/print.rs` (198): `NSPrintOperation` sheet. Windows: `PrintDlgEx` plus GDI. The `aspect_fit_rect` core
  is already pure and tested, so only the shell changes.
- `platform/macos/open_handler.rs` (107): Apple Event injection so Finder double-clicks reach us. Windows needs no
  equivalent **for the double-click case**: Explorer passes paths as argv, which `clap` handles. It needs something else
  entirely for the no-argv case; see M1 step 1. If we later want "reuse the running window" (the roadmap's IPC daemon
  mode), that's an opt-in named pipe, and it'd serve the Cmdr integration too.
- `platform.rs` `total_physical_ram_bytes` (`sysctlbyname`), and `diagnostics.rs:59` (`ps`). Note `libc` is a macOS-only
  dependency in `Cargo.toml`, so the Linux versions have to be plain `/proc` file reads, which is fine.
- `render/text.rs`: see M1 step 8; it's five call sites and a weight hack rather than one constant.
- `settings/persistence.rs` `data_dir()` (`persistence.rs:184-190`): `$HOME/Library/Application Support/...` with a
  `/tmp` fallback that resolves to a drive-relative `\tmp` on Windows.

## Build, CI, and distribution

### The TLS question has a better answer than "install NASM"

Two C dependencies reach the Windows MSVC target: `zstd-sys` (the bundled DCP blob, builds fine with MSVC) and
`aws-lc-sys`, pulled in by `reqwest`'s rustls feature, which wants NASM and sometimes CMake.

Two things rule out the obvious workaround and point at a better one:

- **Switching rustls to the `ring` provider does not remove the NASM requirement.** `ring` 0.17.14's `build.rs` also
  requires NASM on x86_64 Windows. `ring` 0.17.14 is already in `Cargo.lock` via `quinn-proto` and `rustls-webpki`, so
  this is checkable locally.
- **`reqwest` appears in exactly four places**: `updater.rs:126` and `:175` (inside the macOS-gated `updater` module,
  `main.rs:35-36`), and `tests/integration.rs:14` and `:74`. The test harness talks **plain HTTP to `127.0.0.1`**
  (`qa/server.rs:42`), so it needs no TLS provider at all.

So: move `reqwest` into `[target.'cfg(target_os = "macos")'.dependencies]`, and add a dev-dependency with
`default-features = false, features = ["blocking", "json"]` for the harness. That removes `aws-lc-sys` from the Windows
target entirely, retires the NASM question, and shrinks the Windows binary. Keep installing `nasm` in CI as the fallback
if something unexpected still pulls it in. (When M7 brings a Windows updater, it'll need a TLS story again; Schannel via
`native-tls` is the C-free choice then.)

### Cross-compiling from macOS doesn't work

Verified: `cargo check --target x86_64-pc-windows-msvc` from a Mac dies in `aws-lc-sys`'s `cc` invocation, and the Linux
target additionally needs glib and gtk headers for muda. Use real runners. GitHub's `windows-latest` and `ubuntu-latest`
have everything.

### CI changes

Add a `desktop-rust-windows` job on `windows-latest` running build, clippy, and tests, and add it to `ci-ok`'s `needs`
list. Also fix the macOS job (M0 step 1). Three shape details a copy-paste of the Linux job gets wrong:

- Every job that runs checks builds `scripts/check/check` and invokes it by relative path (`ci.yml:85-96`). On
  `windows-latest` that's `check.exe` under a PowerShell default shell.
- `checks/desktop-rust-tests.go:22-27` runs `cargo install cargo-nextest --locked` when the binary is missing, which
  costs minutes on a cold Windows runner. Use the prebuilt binary or cache it.
- `scripts/check.sh` is bash (`#!/bin/bash`, `BASH_SOURCE`). CI is fine because it calls the Go binary directly, but a
  Windows contributor needs Git Bash or WSL, or we add a `check.ps1`.

GitHub's Windows runners have a desktop session, unlike headless Linux, so the wgpu and window E2E suite may be able to
run there. Treat that as a hypothesis to test in M1 rather than a given: the runners have no real GPU, so wgpu falls
back to WARP (the software rasterizer), which is slow enough to make timing-sensitive tests flaky. Prove it with one
test before porting the other 59.

### Release and signing

**Release matrix:** add `x86_64-pc-windows-msvc`, and decide about `aarch64-pc-windows-msvc`. The proposed CI job is
`windows-latest`, which is x64, so an ARM64 leg would first compile at tag time with two untested things waiting:
`zstd-sys` needs the ARM64 MSVC cross toolchain on the runner, and `multiversion`'s `aarch64+neon` clones
(`color/lens_correction.rs:310,328`, `color/chroma_denoise.rs:238,259`, `color/dcp/apply.rs:113`) have never been built
for a non-Apple aarch64 target. Either add an ARM64 cross-build leg to the Windows CI job, or drop ARM64 from the matrix
and say why.

**Code signing is the real distribution cost, and it has lead time.** Extended Validation certificates lost their
SmartScreen bypass in 2024, so every new Windows app builds reputation from zero regardless of certificate type. The
cheapest path to full SmartScreen trust is **Azure Trusted Signing at about $9.99/month** on the Basic tier (up to 5,000
signatures). As of 2026-04 it's generally available and open to self-employed individuals and businesses in the US,
Canada, EU, and UK, with the old three-year-history requirement dropped. Rymdskottkärra AB is an EU business, so it
qualifies.

Onboarding includes Microsoft's identity validation of the company, which is calendar time you can't compress. **Start
the account during M1** rather than M7, so validation runs in parallel with the work. Budget for reputation-building on
top: the first few hundred downloads will see SmartScreen warnings whatever we sign with.

**Installer:** a bare `.exe` runs, but a paid product wants an installer for file-type registration, Start menu entry,
uninstall, and update handoff. WiX (MSI), Inno Setup, or NSIS. MSIX is the modern option and gives clean install and
uninstall, but its sandbox complicates both the auto-updater and the QA server's localhost port.

## The decision this plan can't make for you

`docs/design-principles.md` says: "Cross-platform comes later, but never at the cost of native feel. When we go
cross-platform, fork by OS (same approach as Cmdr)."

Worth flagging that comparison. Cmdr is Tauri: its UI is a webview, so "fork by OS" there forks a thin native layer
while the entire interface stays shared. Prvw has no such layer. Forking Prvw by OS means writing the settings window,
onboarding, about, and browse mode a second time in Win32, a third time for Linux, and then adding every future setting
in N places. The RAW panel alone is 1,157 lines with 16 toggles and eight sliders.

Three ways forward:

**(a) Full native fork.** Win32 for everything AppKit does. Best native feel, a permanent two-to-three-times UI cost,
and a tax on every future feature.

**(b) Native shell, GPU-drawn app UI.** Keep native what users actually perceive as native: the menu bar, window chrome,
file dialogs, clipboard, print, shell thumbnails. Move settings, onboarding, about, and browse mode into wgpu, drawn by
Prvw. One implementation, three platforms. It costs a macOS rewrite of those panels too, but the count stops growing. It
also fits "minimal chrome" and render-on-demand (draw only when the panel changes), and it retires an entire class of
`Retained<>` lifetime segfaults the AppKit code documents at length.

**(c) Windows-lite first, decide later.** Ship image mode only: viewer, decode, RAW, color management, zoom, pan,
fullscreen, slideshow, histogram, EXIF overlay, real Win32 menus. Settings live in `settings.json` with no UI. No browse
mode, no onboarding, no auto-update. Then watch what Windows users complain about before spending three months on it.

**Decision (David, 2026-08-23): full parity from the start.** Windows ships matching macOS, so there is no viewer-only
beta and (c) is off the table as an endpoint.

**That makes (b) the strong recommendation.** Under full parity every one of those roughly 8,800 chrome lines gets
written for Windows regardless, and again for Linux if Linux ships. (a) writes them two more times and taxes every
future setting forever; (b) writes them once, on the GPU surface the app already owns, and deletes the AppKit originals
in the same pass. The case for (a) was always "native feel where the user notices", and the pieces users actually
perceive as native (the menu bar, window chrome, file dialogs, clipboard, print, shell thumbnails) stay native under (b)
anyway. What moves to the GPU is settings, onboarding, about, and browse mode, none of which a user reads as an OS
widget.

If (b) wins, M4 stops being a decision point and becomes the milestone that builds the shared widget layer, and M5 and
M6 become its consumers rather than separate ports. Reorder accordingly.

## Milestones

Each milestone is independently landable and leaves `main` green on all platforms in CI.

**Ordering, given the full-parity decision.** The numbering is no longer a shipping sequence, because everything ships
together. It's a dependency order, and four dependencies are real:

- **M0 gates everything.** Nothing else compiles or runs off macOS until it lands.
- **M1 step 4 (the `previews::metadata` Windows tier) gates M3**, and M1 step 10 (path handling) gates M5.
- **M1 step 14 (pinning DX12) gates M2's HDR path**, because `ExtendedSrgbLinear` is a DX12 capability.
- **M4's widget layer gates M5 and M6** if the answer to the UI question is (b).

Everything else can move. In particular, M2's wgpu 29-to-30 bump is worth pulling forward next to M0: it's isolated,
it's the riskiest change here, and M1 step 11's screenshot readback wants it done first.

### M0: make the non-macOS build run, and make CI able to tell (one to two weeks)

**Intent:** Today's Linux build type-checks but panics at startup, and macOS CI can't catch a regression because it only
runs `cargo build`. Fix both before touching anything else, or every later milestone's "green in CI" claim is hollow.

1. **Fix the macOS CI job first.** Add clippy and `cargo nextest run` to `desktop-rust-macos` (`ci.yml:101-127`).
   Without this, step 2 changes color behavior on macOS with zero automated coverage. Expect a shakedown: the 60 E2E
   tests in `tests/integration.rs` spawn real GPU windows and have never run on a hosted runner, so budget a flake pass.
   If they're too slow for every push, run them on `main` or on a schedule, but run them somewhere.
2. **Replace the macOS system sRGB profile with a generated one.** `moxcms::ColorProfile::new_srgb()`
   (`moxcms/defaults.rs:255`) plus `.encode()` gives the bytes. `src/color/profiles.rs:20` already argues for exactly
   this ("nothing to license, nothing to bundle, nothing to keep in sync") and `profiles.rs:122`
   (`linear_rec2020_icc_bytes`) shows the encode pattern. Follow the precedent rather than shipping an `.icc` asset.
   This removes a macOS-only file read from a path that runs everywhere, including a latent fragility on macOS itself.
   - **There are three copies of that path, not one.** `examples/raw-dev-dump.rs:98` and `examples/raw-tune.rs:135` each
     declare their own `const SRGB_PROFILE_PATH`. They compile everywhere, so the new Windows `clippy --all-targets` job
     stays green while both examples stay dead on Windows. Point them at the generated profile too.
   - **Watch out:** `profiles_match` is byte-equality (`a == b`, `transform.rs:169`). Swapping Apple's system blob for a
     generated one means images tagged with Apple's exact sRGB profile stop short-circuiting and go through a
     near-identity transform. It only bites when "match display" is off. Measure it on a 24 MP JPEG before and after; if
     it matters, compare parsed primaries and TRC instead of bytes.
3. **Fix both `HOME` readers, not one.**
   - `settings::persistence::data_dir()` (`persistence.rs:184`): `%APPDATA%\Prvw` on Windows, `$XDG_CONFIG_HOME` or
     `~/.config/prvw` on Linux. Replace the `/tmp` fallback too; on Windows it resolves to a drive-relative `\tmp`. Keep
     the `PRVW_DATA_DIR` override, the integration tests depend on it.
   - `color::dcp::discovery::home_dir()` (`discovery.rs:107`) is **not** gated, so it runs on Windows and finds nothing.
     Its comment even says "Prvw is macOS-only and `HOME` is always set by launchd". Consequence: every user-installed
     Adobe camera profile is invisible and RAW rendering quietly falls back to the default pipeline. The Windows paths
     are `%APPDATA%\Adobe\CameraRaw\CameraProfiles` and `%PROGRAMDATA%\Adobe\CameraRaw\CameraProfiles`. More
     user-visible than the settings path, because RAW quality is a headline feature.
4. **`diagnostics::get_process_rss_mb`** (`diagnostics.rs:59`): `GetProcessMemoryInfo` on Windows, `/proc/self/statm` on
   Linux.
5. **`platform::total_physical_ram_bytes`**: `GlobalMemoryStatusEx` on Windows, `/proc/meminfo` on Linux. Un-gate it
   from macOS while you're there; it stops being previews-only the moment M3 lands.
   - **While you're in there, connect it to the preloader's budgets.** `navigation/preloader.rs:12` sets
     `SDR_MEMORY_BUDGET` to 512 MB and `:20` sets `HDR_MEMORY_BUDGET` to 1 GB, both absolute constants. On an 8 GB
     Windows laptop with no unified memory, a 1 GB HDR cache plus the GPU-side copies is a different proposition than on
     a 32 GB Mac. Scale them off total RAM the way `previews` already does.
6. **Add `windows = "0.62.2"`** under `[target.'cfg(target_os = "windows")'.dependencies]`. It's already in `Cargo.lock`
   transitively, so nothing new is downloaded, but per `AGENTS.md`'s critical rules this is still a new direct
   dependency and needs a license check and a crates.io version check. (0.62.2 is current, published 2025-10-06,
   MIT/Apache-2.0.)
7. **Scope `reqwest` to macOS** and give the test harness a TLS-free dev-dependency, per the TLS section above. This
   drops `aws-lc-sys` from the Windows target rather than working around it.
8. **Set `muda = { default-features = false }` on Linux.** muda's defaults are `["libxdo", "gtk"]`, the source of all
   nine GTK C dependencies in `Cargo.lock` and of the `apt-get` step in CI. muda's menu bar can't attach to a winit
   window on Linux anyway (see item 9 above), so that chain is dead weight. Dropping it removes the apt step, speeds up
   Linux CI, and makes a future AppImage genuinely dependency-free. Doing it here rather than in M8 pays off for every
   milestone in between.
9. **Port the Go check runner to Windows. It does not compile there today, and this blocks step 10.** Verified with
   `GOOS=windows GOARCH=amd64 go build`: `scripts/check/checks/common.go:111` calls
   `syscall.Kill(-cmd.Process.Pid, syscall.SIGTERM)` and `:130` sets `SysProcAttr{Setpgid: true}`, and neither exists on
   `windows/amd64`. Both live in `RunCommand` / `KillAllProcesses`, which every check goes through. Six more call sites
   shell out to POSIX `find`, which on Windows resolves to `C:\Windows\System32\find.exe`, a text search tool, and
   returns garbage: `common.go:288`, `common.go:409`, `desktop-rust-rustfmt.go:21`, `scripts-go-gofmt.go:22`,
   `scripts-go-gocyclo.go:35`, `scripts-go-misspell.go:25`. Split the process-group handling into `common_unix.go` /
   `common_windows.go` (job objects or `taskkill /T` on Windows) and replace the `find` shell-outs with
   `filepath.WalkDir`. Budget a day.
10. **Add the `desktop-rust-windows` CI job** on `windows-latest`: build, clippy, `cargo nextest run`. Give it
    `needs: changes` and `if: inputs.run_all || needs.changes.outputs.rust == 'true'` to match `ci.yml:54-55`, or it
    runs on every website-only push, and add it to `ci-ok`'s `needs`. Mind the shape details in the CI section, and
    **audit the POSIX path fixtures** that will now execute for the first time: `navigation/preloader.rs:766,894`,
    `decoding/mod.rs:658-660,708,728`, `decoding/raw.rs:1553-1902`, `settings/persistence.rs:245,263`,
    `browser/tree_model.rs:404-497`, and `folder_watch.rs:362-423` all hardcode POSIX absolute paths. Most are pure
    `PathBuf` string manipulation and will pass, but don't assume it.

**Tests:** unit tests for the per-OS helpers and both `HOME` replacements. A test that constructs `color::State` without
touching the filesystem. CI green on three platforms, with macOS finally running more than a build.

**Also re-open the five test gates step 2 retires.** `color/transform.rs:190`, `decoding/mod.rs:512`,
`decoding/raw.rs:1482` and `:1515`, and `color/dcp/mod.rs:419` are all `#[cfg(all(test, target_os = "macos"))]`, and
every one of their comments blames `srgb_icc_bytes` reading a macOS-only system file. Once that read is gone the
comments are wrong (the repo's "describe current behavior, not history" rule) and the new Windows and Linux jobs are
running less coverage than they could. Drop the gates the fix retires and delete the stale comments in the same pass.

**Docs:** note the sRGB change in `src/color/CLAUDE.md`; add a supported-platforms line to `AGENTS.md`.

**Done when:** `cargo run -- some.jpg` on a Windows box shows the image, **and** `cargo run` with no arguments does
something defensible rather than nothing. (That second half is M1 step 1's job; if it isn't done yet, at least make the
no-argument case log and exit rather than hang invisibly.)

### M1: Windows image mode at parity (three to four weeks)

**Intent:** Everything a person does with Prvw 95% of the time, working properly on Windows. This is what makes a
shippable-if-minimal Windows build. It's the biggest non-UI milestone here because the launch and input surface is where
macOS assumptions are densest, and none of them show up as compile errors.

Steps 1 to 7 plus 14 and 15 are the ones a naive port would miss: none of them is a compile error, and several look like
rendering bugs when they're really launch, input, or layout bugs. The first seven come first for that reason; the other
two sit near the end only because they're smaller.

1. **Make the no-argument launch work, and add File → Open.** See finding 3 above: `app.rs:3257` returns from
   `resumed()` without building a window when `waiting_for_file` is true, `app.rs:3300`'s fallback is macOS-only
   onboarding, and `menu.rs:129-141` has no Open item. Off macOS that's an invisible process with no recovery. Do two
   things:
   - Create the window unconditionally off macOS, showing an empty state (the black frame plus a hint) rather than
     nothing.
   - Add **File → Open…** to the menu on every platform, backed by a native file dialog. On Windows that's
     `IFileOpenDialog`; the `rfd` crate wraps all three platforms and would be a **new dependency to license-check and
     version-check** per `AGENTS.md`. macOS benefits too: there's currently no way to open a file from inside the app at
     all.
   - **Two traps in that dialog.** `rfd`'s Linux backend is GTK by default, so a plain `rfd = "..."` quietly reinstates
     the dependency chain M0 step 8 removed; take it with `default-features = false` plus the `xdg-portal` feature. And
     a Win32 `IFileOpenDialog` runs its own modal message loop on the calling thread, blocking winit's pump exactly the
     way the macOS modal gotcha does, and it needs an initialized COM apartment. Use the async API or a worker thread.
     The "no modals inside the event loop" rule is not a macOS quirk we get to leave behind.
2. **Decide what a directory argument does.** `main.rs:106` canonicalizes a lone directory argument and sets
   `launch_directory` with **no** `cfg` gate. `app.rs:602` then leaves `navigation.dir_list = None`, `app.rs:644` skips
   `display_initial_image`, and the block that opens browse mode (`app.rs:692`) is macOS-only. Off macOS: a window with
   no image and no error. Explorer's "Open with" on a folder and folder drag-and-drop both reach this, and step 12 adds
   drag-and-drop. Pick one: list the folder's images in image mode and open the first, or reject the argument with a
   message.
3. **Suppress browse mode on Windows until M5.** `menu.rs:271` builds the "Image browser" item unconditionally, and off
   macOS `App::set_view_mode` (`app.rs:1917`) calls `browser.toggle_mode()` and then stops requesting redraws. So
   pressing Enter flips the app into `ViewMode::Browse` with no visible change, changed key routing, a changed menu
   label, and `SharedAppState` reporting Browse to the QA server. Hide the menu item and make Enter a no-op off macOS.
4. **Give `previews::metadata` a Windows tier, and fix the catch-all arm.** `app.rs:843` gates `apply_preview_auto_fit`
   behind macOS, so a RAW launch keeps the window at its initial size until the full develop lands: a visible pop on the
   slowest open path. There are **four** gated call sites, not one: `app.rs:846` (launch), `:1237` (the navigation
   cache-miss path), `:1865`, and `:2221`. All four come back once the metadata tier exists. But un-gating the portable
   tiers is **not** enough, and doing only that won't compile: `metadata.rs:172-183` routes PNG/GIF/BMP to `image`, JPEG
   to the combined reader, and `_ => read_dimensions(path)`, the ImageIO tier, which is where RAW, HEIC, WebP, TIFF, and
   every unknown extension land. RAW is precisely the motivating case. So: keep tiers 1 and 2, add a portable tier for
   RAW (rawler already parses the headers, and `decoding/raw_preview.rs` is portable), and give the `_` arm a non-Apple
   fallback.
5. **Fix the four mouse and trackpad defects.** All in `app.rs:3443-3486`, none of which are compile errors:
   - `app.rs:3449` gates scroll-to-zoom on `self.modifiers.super_key()`. On Windows, Super is the Windows key.
     **Ctrl+wheel is the universal zoom gesture** there, and as written it falls through to image navigation instead.
   - `app.rs:3465`: `let forward = scroll_y < 0.0;`. macOS ships natural scrolling on by default, Windows ships it off,
     so the same physical gesture navigates the **opposite direction**. Flip the sign per platform.
   - `app.rs:3446`: `MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0` is a magic divisor tuned against macOS
     trackpad pixel deltas. Windows precision touchpads also send `PixelDelta` on a different scale, and a mouse wheel
     sends `LineDelta`. Recompiling isn't enough here; the constant needs re-tuning per input device.
   - `app.rs:3472` handles `WindowEvent::PinchGesture`, documented in `winit/src/event.rs:280-284` as "Only available on
     **macOS** and **iOS**". Pinch-to-zoom silently disappears on Windows. Decide whether that's acceptable or whether
     precision-touchpad zoom needs a `WM_POINTER` path.
   - Related: the double-click window is hardcoded to 400 ms at `app.rs:3513`. Windows exposes `GetDoubleClickTime()`.
6. **Handle mixed per-monitor DPI.** `window.rs:1521-1535`: `MonitorBounds::from_window` takes `window.scale_factor()`
   and applies it to `current_monitor()`'s position and size. Safe on macOS. On Windows a 150% laptop panel plus a 100%
   external monitor is routine, so the logical bounds come out wrong on the other display, and `max_window_size`,
   `clamp_to_screen`, and `resize_to_fit_image` all misplace or mis-size the window. Two related fixes while you're
   there: the doc comment says "work area" but the code uses the full monitor rect (macOS gets away with it via the 90%
   `MAX_SCREEN_FRACTION` fudge; Windows wants `GetMonitorInfo`'s `rcWork` so auto-fit doesn't tuck the window under the
   taskbar), and `app.rs:247` initialises `scale_factor: 2.0`, a hardcoded Retina assumption that never sees Windows'
   1.25 / 1.5 / 1.75 fractional factors.
7. **Ship a real Windows executable.** Three things macOS gets from the `.app` bundle and Windows does not:
   - No `#![windows_subsystem = "windows"]` exists anywhere, so every Explorer launch opens a console window. Adding it
     means there's no stderr at all, which collides with `main.rs:80-92` writing raw ANSI escapes (`\x1b[31m`) into the
     log formatter. **Decide how a Windows build logs before writing the attribute**: a file sink, `OutputDebugString`,
     or an opt-in console via `AttachConsole`.
   - No `embed-resource` / `winresource` in `build.rs` (both **new direct dependencies** under `AGENTS.md`'s rule, as is
     `dunce` in step 10), so the exe carries no `RT_GROUP_ICON` and no version info: generic icon in Explorer, the
     taskbar, and Alt+Tab.
   - `window.rs:133-135` never calls `with_window_icon`, which macOS doesn't need and Windows does.
8. **The overlay font is not a one-line change.** `render/text.rs` requests `Family::Name("System Font")` at lines 156,
   349, 352, 507, and 539, and the macOS block at 189-216 exists specifically because fontdb registers SFNS's variable
   `wght` axis as a single weight-400 face, so `Weight::BOLD` picks the wrong font. On Windows,
   `Family::Name("System Font")` matches nothing and cosmic-text falls back to an arbitrary face, affecting the title
   strip, zoom pill, EXIF overlay, and histogram labels. What's needed is a family-name abstraction plus a decision on
   whether Windows needs the same bold-alias trick for Segoe UI Variable.
9. **Menus, and the accelerator problem.** `muda::Menu::init_for_hwnd` with the HWND from winit's raw window handle
   gives the menu bar and `show_context_menu_for_hwnd` gives the context menu, both composing with winit's window
   procedure. But **accelerators need `TranslateAcceleratorW(msg.hwnd, menu.haccel(), &msg)` in the message pump**
   (`muda/src/menu.rs:212-216`), which winit doesn't do. The escape hatch is
   `winit::platform::windows::EventLoopBuilderExtWindows::with_msg_hook` (`winit/src/platform/windows.rs:225`), whose
   doc example is literally this case. Two consequences: the `EventLoop` construction in `main.rs` forks per platform,
   and the `HACCEL` has to stay fresh across the runtime menu mutations Prvw already does (`set_browse_menu_label`, the
   slideshow Start/Stop swap, every `CheckMenuItem`). Also remap Cmd to Ctrl, restructure for Windows conventions (no
   app menu, so About moves to Help and Settings becomes "Options"), and audit every `PredefinedMenuItem`. Keep
   `MenuIds` and `input.rs` as the single source of action mapping; only the construction in `menu.rs` forks.
   - **Knock-on effect:** a Win32 menu bar eats client area, so `window::resize_to_fit_image` and the auto-fit path have
     to account for the menu-bar height or images come up subtly cropped. That looks like a rendering bug and is
     actually a layout one, so check it early.
10. **Path handling on Windows: a three-way rule, not a blanket one.** The affected sites are `main.rs:106` and `:121`
    (every CLI path), `navigation/directory.rs:17` and `:38` (index matching), `browser/tree_model.rs:165`
    (`starts_with` against roots), and `folder_watch.rs` (the watched-folder comparison against `event.path.parent()`).
    Three separate Windows behaviors collide here:
    - **Verbatim prefixes.** `Path::canonicalize` returns `\\?\C:\Users\...`, which breaks prefix matching against a
      `C:\` root (M5) and is rejected by Win32 shell APIs (`IShellItemImageFactory` in M3, `CF_HDROP` in step 12).
    - **`MAX_PATH`.** That same `\\?\` prefix is exactly what lifts the 260-character limit, so stripping it globally
      would stop deep photo libraries opening. **Do not de-verbatim everywhere.** De-verbatim at two boundaries only:
      display strings, and paths handed to shell APIs. Keep the verbatim form for filesystem I/O and internal
      comparison, and ship an app manifest with `longPathAware=true` (which pairs with the `winresource` work in step
      7).
    - **Case.** NTFS is case-insensitive while `Path::starts_with` and `PathBuf::eq` are byte-wise, and argv,
      `canonicalize`, and `GetLogicalDrives` can disagree on casing for the same directory. Compare case-insensitively
      on Windows.
    - **UNC and network paths deserve naming, because the target user keeps photo libraries on a NAS.** `canonicalize`
      on a UNC path yields `\\?\UNC\server\share\...`, so `starts_with` against a `\\server\share` root fails and feeds
      straight into M5's `reveal_path_chain`. `GetLogicalDrives` (M5) misses disconnected mapped drives.
      `ReadDirectoryChangesW` over SMB is best-effort with no delivery guarantee.
    - **Performance, same code:** `navigation/directory.rs:36-38` calls `canonicalize()` on **every file in the folder**
      to find the current index. On APFS that's a cheap `realpath`. On Windows it's `CreateFileW` plus
      `GetFinalPathNameByHandleW` per entry, so a 5,000-image folder costs 5,000 file opens on the launch path. This is
      the concrete thing most likely to blow the README's 600 ms promise, and over SMB it goes from expensive to
      unusable. Compare strings against the already-canonical target instead, and benchmark it here rather than
      discovering it in M7.
11. **Give the QA screenshot tool a Windows path.** `qa/mcp.rs:803` gates `screenshot_window`, which shells out to
    `/usr/sbin/screencapture` at `:827`. Either add a `PrintWindow` / `BitBlt` implementation, or do a **wgpu surface
    readback**, which would be portable and arguably better than either native path. Without this, agents can't do
    visual QA on Windows and step 13's visual assertions can't run. (The QA server itself is fine: `qa/server.rs:42`
    binds `127.0.0.1`, which doesn't raise a Defender Firewall prompt, so nobody needs to go hunting for a firewall
    rule.)
12. **Clipboard copy** (`CF_DIB` plus `CF_HDROP`) and **drag-and-drop** onto the window via winit's `DroppedFile`, a win
    on all three platforms. Keep the "load from the original file, not our transformed buffer" decision documented in
    `platform/macos/CLAUDE.md`; that reasoning is platform-independent.
13. **Port the E2E harness.** Drop the crate-level `#![cfg(target_os = "macos")]` in `tests/integration.rs` (60 tests)
    and gate only the genuinely macOS-specific ones (browse mode, file associations, onboarding). Start with one test on
    `windows-latest` to see whether WARP is fast and stable enough, then port the rest.
    - The harness keeps test windows out of the way with `ActivationPolicy::Prohibited` (`main.rs:164-166`), which is
      `winit::platform::macos` only. On Windows the closest thing is `WindowAttributes::with_active(false)` plus leaving
      the window un-raised; there's no app-level "cannot be activated" lever, so expect this to be less airtight.
    - `tests/integration.rs:69` sets `HOME` for the browse tests, and `:68` canonicalizes it so prefix matching works.
      Account for both when deciding which tests to gate.
14. **Pin the GPU backend and revisit adapter selection.** `renderer.rs:243` uses `Backends::all()`, so which backend
    wgpu picks on a given Windows machine isn't ours to decide. M2's entire HDR plan rests on
    `SurfaceColorSpace::ExtendedSrgbLinear`, which is a **DX12** capability, so pin `Backends::DX12` on Windows here
    rather than leaving the flagship feature conditional on an unowned choice. Separately, `renderer.rs:256` asks for
    `PowerPreference::LowPower`; on a Windows laptop with hybrid graphics that picks the integrated GPU, which on many
    machines isn't the one driving the display the photographer calibrated. Decide deliberately.
15. **Fix the rename semantics in `folder_watch`.** `folder_watch.rs:50-52` documents the invariant "Creates, removes,
    and renames are false" for `is_modify`, and `:111` implements it as `matches!(event.kind, EventKind::Modify(_))`.
    That holds on FSEvents and breaks on Windows: `notify-8.2.0/src/windows.rs:426` maps `FILE_ACTION_RENAMED_OLD_NAME`
    to `EventKind::Modify(ModifyKind::Name(RenameMode::From))` and `:434` maps the new name to `RenameMode::To`, so
    **both halves of every rename land in `FolderChange.modified`**. Since temp-write-then-rename is exactly the editor
    save pattern the coalescer was built for, the consumer gets told to re-decode a path that no longer exists on every
    save. Match on `ModifyKind::Data` and `ModifyKind::Metadata` instead of `Modify(_)`.
16. **Start the Azure Trusted Signing account now**, so Microsoft's identity validation of Rymdskottkärra AB runs in
    parallel with M2 through M6 rather than blocking M7.

**Ordering note:** step 11's wgpu surface readback and M2 step 3's wgpu 29-to-30 bump touch the same code. Either do the
bump first (it's isolated anyway) or use the native `PrintWindow` path for step 11 and revisit after M2.

**Tests:** the non-browse integration tests green on Windows CI is the acceptance criterion. Add unit coverage for the
accelerator remapping table, the directory-argument decision, the scroll-direction and zoom-modifier per-platform
constants, and the path-comparison helper.

**Docs:** a new `src/platform/windows/CLAUDE.md` mirroring the macOS one. Update `docs/architecture.md`'s layout table,
`AGENTS.md`, `README.md`, and `CONTRIBUTING.md`, all of which currently say macOS-only.

### M2: color management on Windows (one to two weeks)

**Intent:** The differentiator. Don't ship Windows without it.

1. **Display profile detection.** `MonitorFromWindow` for the window's current monitor, `GetICMProfileW` on its DC for
   the ICC path, read the bytes, hand them to `color::State.display_icc`. Use `ColorProfileGetDisplayDefault` for
   advanced-color displays, which `WcsGetDefaultColorProfile` doesn't cover.
2. **React to monitor changes.** The macOS version re-detects when the window moves between screens. On Windows, hook
   winit's `Moved` and `ScaleFactorChanged`, and consider `WM_DISPLAYCHANGE` for profile changes that happen in place.
   This shares plumbing with M1 step 6's per-monitor DPI work, so do them with that in mind.
3. **HDR and EDR.** Bump wgpu from the resolved 29.0.4 to 30 (30.0.0 landed 2026-07-01; 30.0.1 landed 2026-08-22, so
   check the three-day release-age rule before pinning it) and use `SurfaceColorSpace::ExtendedSrgbLinear` on the
   `Rgba16Float` surface. Verify Windows output against the Metal EDR path on the same file and the same monitor class.
   - **Bonus to check:** the macOS code pokes `CAMetalLayer.colorspace` by hand. Once wgpu owns surface color space,
     some of `display_profile.rs`'s CG plumbing may become redundant. Deleting it would be a real simplification, so
     look before assuming both are needed.
4. **Restructure `color/display_profile.rs`** into `color/display_profile/{mod,macos,windows}.rs` with a small shared
   interface: "give me the current display's ICC bytes" plus "tell me when they changed". The macOS file is 595 lines
   but most of it is CF and CG plumbing; the shared surface is tiny.

**Tests:** `tests/color_management.rs` is `#![cfg(target_os = "macos")]` today and never runs in CI. M0 step 1 fixes the
CI half; here, extend it with a Windows path asserting we read a profile and that a known input produces the expected
output pixel.

**Watch out, three things:**

- **The wgpu 29 to 30 bump is the riskiest single change in this plan.** It touches the renderer, the EDR transition,
  and both platforms. Give it its own branch and its own commit, and check macOS visually before merging.
- **Windows 11 Auto Color Management will fight us.** ACM (Windows 11 22H2 and later, on by default on supported
  displays) color-manages the whole desktop in hardware. For an app that already transforms to the display profile
  itself, that's a second transform on top of ours, and users report ACM overriding applied ICC profiles for color-aware
  apps. The fix is to target the advanced-color APIs and declare our output color space rather than assume the legacy
  ICC path, which is another argument for going through wgpu 30's `SurfaceColorSpace`. **Test with ACM on and off before
  shipping**, on a wide-gamut display. Getting this wrong makes Prvw look worse than an app that does nothing.
- **The differentiator is quieter on Windows.** Every Mac ships a real per-display factory profile, so display-aware
  color management visibly helps every Mac user. Windows assumes generic sRGB unless the user or the monitor vendor
  installed a profile, so for many Windows users we'll read sRGB and the transform is a no-op. It still matters
  enormously for the calibrated-monitor photographers who are Prvw's actual audience. State the claim per platform
  rather than repeating the macOS one.

### M3: previews and thumbnails on Windows (three to five days)

**Intent:** Without this, navigating outside the two-image preload window shows a blank screen instead of a blurry
placeholder, and browse mode (M5) has nothing to draw. (The launch-time sizing half of this module already moved into M1
step 4.)

The scheduler, byte-budget cache, and dim-prefetcher are pure, so the submission worker is the main fork. Two things
that fork with it:

- **`IShellItemImageFactory` is COM.** Every worker thread needs `CoInitializeEx` and a matching `CoUninitialize`, and
  the apartment model decides whether `GetImage` blocks. Not something an agent infers from "mirror the QuickLook
  worker".
- **The parallelism cap's rationale inverts.** `previews/mod.rs:125-127` caps `max_parallel` at half the cores because
  "out-of-process `quicklookd` does the real work, so this cap is about I/O and system courtesy". On Windows the shell
  thumbnail work happens in-process, so that cap is now throttling our own decode threads for a reason that no longer
  applies.

Two options; pick deliberately:

- **(a) Shell thumbnails.** `IShellItemImageFactory::GetImage`, mirroring the QuickLook worker's shape. Faster to build,
  system-cached, matches Explorer exactly. Depends on Microsoft's Raw Image Extension for RAW files. Needs the
  de-verbatim path work from M1 step 10.
- **(b) Self-hosted previews.** `decoding/raw_preview.rs` (embedded RAW preview extraction, already portable) plus a
  downscaled decode for everything else, with our own cache. More work now, no shell dependency, no RAW gap, identical
  on macOS and Linux, and it decouples us from `quicklookd`'s behavior. M1 step 4 already builds part of this.

**Recommendation: (a) for M3, with (b) as the likely end state.** Get parity fast; revisit if the Raw Image Extension
gap annoys real users.

**Docs:** `src/previews/CLAUDE.md` opens "Previews (macOS-only)". Restructure it per platform.

### M4: the settings decision point (half a day to four weeks)

**Intent:** This is where the fork-versus-unify question gets answered with real information instead of speculation. By
now there's a Windows build people can use, and the feedback tells us how much settings UI Windows users actually need.

**Before starting, decide (a), (b), or (c) from the section above.** The plan differs completely:

- **If (a) native fork:** roughly 3,400 lines of Win32 forms. Budget four weeks and accept the ongoing tax.
- **If (b) GPU-drawn:** design a small widget layer on top of the existing wgpu renderer (rows, toggles, sliders, a
  sidebar), render it only when something changes so render-on-demand holds, then port the six panels onto it. Migrate
  macOS to the same layer and delete the AppKit settings code. Budget four weeks, then it never costs again.
- **If (c) defer:** ship Windows with settings in `settings.json` and a menu item that opens the file. Honest, and a
  real option for a beta. Budget half a day.

Whichever way it goes, the "adding a new setting" recipe in `src/settings/CLAUDE.md` needs rewriting. That recipe is the
thing that gets twice as expensive under (a).

### M5: browse mode on Windows (three to six weeks)

**Intent:** The largest remaining feature, and the one most likely to be worth drawing ourselves.

The constraint carried over from macOS: the GPU surface occludes anything behind it, so native child controls have to
sit in front of it, and the two screens swap rather than composite. That applies on DXGI too. A GPU-drawn grid sidesteps
it entirely and reuses `grid_model`, `grid_scheduler`, `thumbnail_cache`, and `tree_model`, all already pure, tested,
and platform-neutral.

The Windows-specific pieces that must be native regardless:

- **`tree_model::enumerate_roots`**: drive letters via `GetLogicalDrives` plus `GetVolumeInformationW` for labels, and
  the known folders (Pictures, Desktop, Downloads) rather than a single Home. `build_roots` is pure and takes whatever
  the platform hands it, so the seam exists. `tree_model.rs:257` is a third `HOME` read (macOS-gated today).
- **Hidden-entry detection**: `tree_model.rs:202` tests for a leading dot. On Windows that's `FILE_ATTRIBUTE_HIDDEN`.
- **The verbatim and case-insensitive path work** from M1 step 10 lands here in `reveal_path_chain`.
- **Re-enable the browse menu item and Enter**, which M1 step 3 turned off.

**Also reconsider whether "one folder tree plus a grid" is the right Windows shape at all** before rebuilding it
verbatim. Windows users' mental model of a file tree differs from Finder's sidebar. Worth a design pass of its own
before any porting starts.

### M6: onboarding and about on Windows (one to two weeks)

Depends on M4's answer. Under (b) these become two more GPU-drawn screens and are cheap. Under (a) they're another 2,349
lines of Win32 (`onboarding/` is three files totalling 2,097, plus `about.rs` at 252).

Content changes regardless: the macOS onboarding's three steps are "open an image", "set as default viewer", and "move
to /Applications". On Windows, step three is meaningless (the installer handles placement) and step two can only
deep-link to `ms-settings:defaultapps`. Write new copy rather than translating the old.

Note that M1 step 1 already gave the no-argument launch a defensible empty state, so this milestone is a polish pass by
the time it arrives.

### M7: distribution (one to two weeks of work, plus signing lead time)

The Azure Trusted Signing account should already be validated if M1 step 16 happened. If not, this milestone stalls on
Microsoft rather than on us.

1. **Installer.** WiX/MSI, Inno Setup, or MSIX. Recommend Inno or WiX over MSIX for v1: MSIX's sandbox complicates both
   the auto-updater and the QA server's localhost port.
2. **Signing.** Wire Trusted Signing into the release workflow the way the Apple certificate import already is. Expect
   SmartScreen warnings for the first stretch regardless.
3. **File-type registration** in the installer: ProgIDs, `OpenWithProgids`, `RegisteredApplications`, icons. Plus the
   in-app "make Prvw your default" flow that opens Windows Settings.
4. **Auto-updater.** Rewrite for Windows: the rename-running-exe swap plus restart, or hand off to the installer. Reuse
   the `latest.json` manifest and version-comparison logic (portable); replace everything below it. This is where a
   Windows TLS story becomes necessary again, and Schannel via `native-tls` is the C-free choice.
5. **Release workflow.** Add the Windows matrix legs alongside the macOS ones, and extend `latest.json` to carry
   per-platform artifacts.

### M8: Linux (about a week)

Mostly free once Windows is done, because M0 step 8 already removed the GTK dependency chain:

- **Menus.** With muda's GTK backend gone, either draw an in-app menu (which M4's (b) answer would already provide) or
  ship without a menu bar. It never worked on Linux anyway.
- **Volume enumeration:** parse `/proc/mounts`, or talk to `udisks2` for friendly labels.
- **Packaging:** AppImage is the least-effort single-file option, and genuinely so with GTK out of the picture. Flatpak
  if we want the software centers, at the cost of sandbox work.
- **Wayland and X11:** winit handles both, but fullscreen, DPI, and window positioning behave differently. Test both.

## Effort summary

- **M0**, non-macOS build runs and CI can tell: one to two weeks. Cumulative: one to two weeks.
- **M1**, Windows image mode: three to four weeks. Cumulative: four to six weeks.
- **M2**, color management: one to two weeks. Cumulative: five to eight weeks.
- **M3**, previews: three to five days. Cumulative: six to nine weeks.
- **M4**, settings and the decision point: half a day to four weeks. Cumulative: six to 13 weeks.
- **M5**, browse mode: three to six weeks. Cumulative: nine to 19 weeks.
- **M6**, onboarding and about: one to two weeks. Cumulative: 10 to 21 weeks.
- **M7**, distribution: one to two weeks. Cumulative: 11 to 23 weeks.
- **M8**, Linux: about a week. Cumulative: 12 to 24 weeks.

A **usable Windows beta** (M0 through M3, settings deferred) lands at **six to nine weeks**. Full parity is **12 to 24
weeks, so three to six months**, and M4 plus M5 are about a third of that, which is why the (a)-versus-(b) decision
deserves a real answer rather than a default.

Two of those look larger than their headline suggests. M0 carries the Go check-runner port (step 9) and the first-ever
run of 60 GPU-window E2E tests on a hosted runner, either of which can eat a week on its own. M2 carries the wgpu
29-to-30 bump, which this plan elsewhere calls its riskiest single change, so it doesn't fit next to display detection
and a module restructure in a few days.

## Risks and unknowns

- **The wgpu 29 to 30 bump (M2).** Touches the renderer core and the EDR transition on both platforms. Highest-risk
  single change here. Isolate it.
- **Windows 11 Auto Color Management (M2).** It can double-transform or override what a color-aware app does, and it's
  on by default on supported displays. The risk most likely to make Prvw's flagship feature look broken on Windows
  rather than merely absent.
- **DX12 EDR behavior in practice.** wgpu 30 advertises `ExtendedSrgbLinear` on DX12, but DWM's honoring of
  `SetColorSpace1` has historically been uneven across vendors. Verify on both AMD and NVIDIA before promising HDR on
  Windows.
- **The macOS E2E suite has never run on a hosted runner (M0).** Sixty tests spawning real GPU windows, first run on
  someone else's hardware. Budget a flake-shakedown pass rather than assuming they light up green.
- **Windows CI E2E flakiness.** The runners have a desktop session, but no real GPU, so wgpu falls back to WARP. Confirm
  the adapter works and that timings behave before porting the whole suite.
- **Feature-flag drift.** Once three platforms have different feature sets, the settings JSON, the menu, and the QA
  server all need to agree on what exists where. `SharedAppState` and the MCP surface should describe capability
  explicitly rather than assuming. M1 step 3 is the first instance and won't be the last.
- **Sub-600 ms open on Windows.** The README promises "about 600 ms". Different GPU stack, different filesystem, no
  unified memory, the per-file `canonicalize` loop in M1 step 10, and Defender scanning every file on first read (a 60
  MB RAW on a cold cache is a real fraction of the budget). Tell developers to exclude `target/` while you're at it: a
  clean `cargo build` runs `build.rs`, which reads 161 DCPs and zstd-compresses about 83 MB every time. Measure early (a
  benchmark in M1 rather than a discovery in M7) and be ready to state the claim per platform.
- **Signing lead time (M7).** Microsoft's identity validation is calendar time we don't control. M1 step 16 exists to
  take it off the critical path.
- **One AppKit gotcha goes away; the other doesn't.** The `Retained<>` lifetime segfaults are genuinely macOS-only and
  Windows code shouldn't inherit that shape. But "never run a modal inside winit's loop" **does** carry over: a Win32
  `IFileOpenDialog` (M1 step 1) spins its own modal message loop on the calling thread and blocks winit's pump the same
  way. Different cause, same rule.

## Questions for David

1. **(a), (b), or (c) on the UI?** The design principles say "fork by OS", but that was written with Cmdr as the
   reference, and Cmdr is a Tauri app with a shared webview UI. For Prvw, forking means writing about 8,800 lines of
   chrome twice, then three times. Is that still the call, or is a GPU-drawn shared UI worth reconsidering?
2. ~~How much does Windows need to match macOS at launch?~~ **Answered 2026-08-23: full parity.** So the number that
   matters is 12 to 24 weeks, and the six-to-nine-week figure is a progress marker rather than a ship date.
3. **Windows 10 or Windows 11 only?** The advanced-color APIs M2 wants (`ColorProfileGetDisplayDefault`, and Auto Color
   Management's behavior) are Windows 11. Mica and Acrylic turn out to be irrelevant here, since a wgpu swapchain
   occludes them either way. So this is a color-management question rather than a looks question: dropping Windows 10
   buys a cleaner HDR and wide-gamut story, keeping it buys users.
4. **Is the Linux CI job load-bearing, or incidental?** It's currently the only thing keeping the non-macOS build
   compiling, and it does that job well. Worth keeping either way, but it's worth knowing whether Linux is a real target
   or a canary for portability.
5. **Should M0 step 1 and M1 step 1 land regardless of cross-platform?** Two findings here are worth fixing even if
   Windows never happens: `desktop-rust-macos` running only `cargo build` means the E2E and color-management suites
   never run anywhere but your laptop, and there is no File → Open anywhere in the app on any platform.
