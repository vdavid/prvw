# Prvw: cross-platform plan (Windows first, Linux second)

Status: M0 landed 2026-08-23 (`7a5a40a..7bf425f`); everything from M0.5 onward is still a proposal. Written 2026-08-23
against `v0.15.1-2-gaefca22`, so file:line citations outside M0 predate that milestone's refactors.

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
  from code that's already portable; the rest is making CI able to see every platform it builds for.
- **A Windows build worth dogfooding daily** (viewer, RAW, display-aware color, real menus, no settings UI): **seven to
  11 weeks.** M0 through M3. Not a ship target given the full-parity decision, but the checkpoint that tells you whether
  the rest is on track.
- **Windows parity with what macOS ships today: four to six months.** Most of that is re-implementing UI we already
  have, in a second toolkit, which the option (a) decision accepts deliberately.

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

Then `src/commands.rs` (12), `src/main.rs` (7), `src/menu/` (6), `src/browser/tree_model.rs` (6), `src/platform.rs` (5),
`src/settings/mod.rs` (4), and a long tail.

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
not run the full suite. The E2E suite and `tests/color_management.rs:7` were both `#![cfg(target_os = "macos")]`, and a
large share of unit tests used to be macOS-gated too.

**M0 changed the second half of that.** The five `#[cfg(all(test, target_os = "macos"))]` gates on unit tests are gone
(step 2 removed the reason for them), so Linux and Windows now run the color, decode, and DCP unit tests. The two
crate-level gates on `tests/` stay: those suites spawn a real window.

**2. It compiles but it can't launch. Fixed in M0 step 2.** `color::State::from_settings` (`src/color/mod.rs:40`, whose
body calls `srgb_icc_bytes()` at `:45`) read `/System/Library/ColorSync/Profiles/sRGB Profile.icc` and **panicked** when
the file was missing. `App::new` builds that state unconditionally, so a Linux or Windows binary died at startup.
`srgb_icc_bytes` (`color/transform.rs:13`) now generates the profile with `moxcms`, and
`color::tests::state_builds_without_reading_the_filesystem` holds the line.

**3. Even with that fixed, launching Prvw the normal Windows way gives you an invisible process.** `app.rs:3257`: when
`waiting_for_file` is true, `resumed()` sets `ControlFlow::Poll` and **returns without calling `initialize_viewer`**, so
there's no window, no renderer, nothing. The 500 ms timer at `app.rs:3300` then calls
`crate::onboarding::show_window()`, which is macOS-only (`main.rs:28-29` gates the whole module). And the File menu
(`menu/native.rs:326-338`) is Print, a separator, and Close window: **there is no Open item anywhere in the app**.

Partly addressed in M0: a no-argument launch off macOS now logs at `error` and exits 2 rather than waiting forever.
`resumed()` is untouched, so the window and the recovery path are still M1 step 1's job.

On macOS none of that matters, because Finder delivers files through Apple Events (`platform/macos/open_handler.rs`). On
Windows, a Start-menu shortcut, a taskbar pin, or a desktop icon are the normal ways to launch, and all of them pass no
argv. The user got a process with no window and no way to recover, because `AppCommand::OpenFile` only ever arrives from
an Apple Event or the debug QA server. M0's Done-when clause turned that into a message and exit 2; making the launch
actually work is M1 step 1, and it's the single most important thing in the milestone.

**4. The macOS side of CI is weaker than the Linux side. Fixed in M0 step 1.** The `desktop-rust-macos` job's only step
was `cargo build`. No rustfmt, no clippy, no tests. Combined with the crate-level gates above, **the E2E suite and the
color-management suite never ran in CI at all**, on any platform. The job now runs clippy and `cargo nextest run`, and a
`desktop-rust-windows` job sits beside it.

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
  binding at `input.rs:55`, which M1 step 3 turns off outside macOS.

**Three things that look portable and aren't:**

- **`commands.rs` (12 gates).** The `AppCommand` enum itself carries macOS-only variants.
- **`qa/` (1,694 lines, 13 gates).** It compiles, but the debug endpoints at `qa/http.rs:143,159,169` and the
  `screenshot_window` tool at `qa/mcp.rs:803` are `#[cfg(all(debug_assertions, target_os = "macos"))]` and shell out to
  `/usr/sbin/screencapture`. Two browse routes have `not(macos)` stubs that return HTTP 400 (`qa/http.rs:492`, `:518`).
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

**Linux is worse.** muda offers only `init_for_gtk_window` there, and winit can't hand it a `gtk::Window`.
`menu/native.rs:513` calls `init_for_nsapp()` as the sole wiring, so the menu bar never attached on Linux at all. M0
step 8 acted on that: muda is now a macOS-and-Windows dependency, and `menu/absent.rs` is the Linux side of the seam.
Linux still needs an in-app menu (drawn by us) when it gets a spec; `menu/CLAUDE.md` lists what is unreachable there
meanwhile.

### 10. The small stuff, roughly 600 lines total

- `platform/macos/clipboard.rs` (115): `NSPasteboard` file URL plus bitmap. Windows: `SetClipboardData` with `CF_DIB`
  and `CF_HDROP`, or the `arboard` crate.
- `platform/macos/print.rs` (198): `NSPrintOperation` sheet. Windows: `PrintDlgEx` plus GDI. The `aspect_fit_rect` core
  is already pure and tested, so only the shell changes.
- `platform/macos/open_handler.rs` (107): Apple Event injection so Finder double-clicks reach us. Windows needs no
  equivalent **for the double-click case**: Explorer passes paths as argv, which `clap` handles. It needs something else
  entirely for the no-argv case; see M1 step 1. If we later want "reuse the running window" (the roadmap's IPC daemon
  mode), that's an opt-in named pipe, and it'd serve the Cmdr integration too.
- `platform.rs` `total_physical_ram_bytes` (`sysctlbyname`), and `diagnostics.rs:59` (`ps`). Done in M0 steps 4 and 5:
  Linux reads `/proc/meminfo` and `/proc/self/statm`, Windows asks `GlobalMemoryStatusEx` and `GetProcessMemoryInfo`.
  `libc` is now a macOS **and** Linux dependency, for `sysconf(_SC_PAGESIZE)`.
- `render/text.rs`: see M1 step 8; it's five call sites and a weight hack rather than one constant.
- `settings/persistence.rs` `data_dir()`: `$HOME/Library/Application Support/...` with a `/tmp` fallback that resolved
  to a drive-relative `\tmp` on Windows. Done in M0 step 3; the per-platform layout now lives in `data_dir_for`
  (`persistence.rs:229`) and the fallback is `std::env::temp_dir()`.

## Build, CI, and distribution

### The TLS question has a better answer than "install NASM"

Two C dependencies reach the Windows MSVC target: `zstd-sys` (the bundled DCP blob, builds fine with MSVC) and
`aws-lc-sys`, pulled in by `reqwest`'s rustls feature, which wants NASM and sometimes CMake.

Two things rule out the obvious workaround and point at a better one:

- **Switching rustls to the `ring` provider does not remove the NASM requirement.** `ring` 0.17.14's `build.rs` also
  requires NASM on x86_64 Windows. `ring` 0.17.14 is already in `Cargo.lock` via `quinn-proto` and `rustls-webpki`, so
  this is checkable locally.
- **`reqwest` appears in exactly four places**: `updater.rs:126` and `:175` (inside the macOS-gated `updater` module,
  `main.rs:35-36`), and twice in the E2E harness (`tests/e2e/app.rs`). The harness talks **plain HTTP to `127.0.0.1`**
  (`qa/server.rs:42`), so it needs no TLS provider at all.

So: move `reqwest` into `[target.'cfg(target_os = "macos")'.dependencies]`, and add a dev-dependency with
`default-features = false, features = ["blocking", "json"]` for the harness. Done in M0 step 7. That removes
`aws-lc-sys` from the Windows target entirely, retires the NASM question, and shrinks the Windows binary. Keep
installing `nasm` in CI as the fallback if something unexpected still pulls it in. (When M7 brings a Windows updater,
it'll need a TLS story again; Schannel via `native-tls` is the C-free choice then.)

### Cross-compiling from macOS: Windows works, Linux doesn't yet

**Windows type-checks from a Mac, and it's wired up.** `./scripts/check.sh --check windows-cross` runs
`cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings` and comes back green on the current
tree. `aarch64-pc-windows-msvc` passes the same way. Setup steps live in `AGENTS.md`; the check is marked slow, so a
plain `./scripts/check.sh` leaves it out. Two things had to be true, and both are now:

- Step 7 scoping `reqwest` to macOS removed `aws-lc-sys`, which is what the earlier attempt died in.
- `zstd-sys` still compiles C for the target. `cargo-xwin` supplies the MSVC CRT and Windows SDK headers plus clang-cl
  and lld-link, and the last missing piece is `llvm-lib`, the MSVC archiver, which Apple's command line tools don't
  ship. rustup's `llvm-tools` component provides `llvm-ar`, and `llvm-ar` under the name `llvm-lib` **is** that
  archiver, so the check symlinks it into `target/cross-check-bin/` on every run.

Plain `cargo check --target x86_64-pc-windows-msvc` without cargo-xwin still fails, in `zstd-sys`:
`fatal error: 'stdlib.h' file not found`. Headers are the only thing it lacks.

The same toolchain **links a working binary too**: `cargo xwin build --target x86_64-pc-windows-msvc -p prvw` produces a
PE32+ `prvw.exe` in about 40 seconds warm, which is a real artifact to drop into the VM. So the Mac covers compile
errors and produces something to run; what it can't tell you is whether the thing behaves once it starts. The VM and the
CI runners still decide that.

**Linux is three `cfg` gates and a C toolchain away, and step 8 as written would break it.** Measured on the current
tree, in order:

1. `cargo check --target x86_64-unknown-linux-gnu` fails in `glib-sys`, `gobject-sys`, and `gio-sys`:
   `pkg-config has not been configured to support cross-compilation`. That's muda's GTK chain.
2. With `muda = { default-features = false }`, the GTK chain disappears and the next failure is `zstd-sys` looking for
   `x86_64-linux-gnu-gcc`. `zig cc` covers that: point `CC_x86_64_unknown_linux_gnu` at a wrapper that drops cc-rs's
   `--target=` flag and calls `zig cc -target x86_64-linux-gnu`, and `AR_x86_64_unknown_linux_gnu` at `zig ar`.
3. Then **muda itself stops compiling**: `platform_impl/mod.rs` gates its Linux backend behind the `gtk` feature and
   offers no fallback, so `default-features = false` leaves `pub(crate) use self::platform::*` unresolved (E0432). Step
   8's one-line change is therefore not enough on its own.
4. Moving muda out of the Linux target entirely leaves exactly three errors, all in our code: two `use muda::…` lines at
   the top of the old `menu.rs`, and one in `input.rs`. So step 8's real shape is "muda is a macOS and Windows
   dependency, and the menu module is `cfg`-gated to match", after which Linux type-checks the same way Windows does.
   That is what M0 step 8 built: `menu/` is now `mod.rs` (the seam), `native.rs` (muda), and `absent.rs` (Linux), and
   `input.rs` no longer mentions muda at all. `./scripts/check.sh --check linux-cross` covers it from a Mac.

### CI changes

Add a `desktop-rust-windows` job on `windows-latest` running build, clippy, and tests, and add it to `ci-ok`'s `needs`
list. Also fix the macOS job (M0 step 1). Both landed in M0, and all three shape details below were handled. They stay
here because a fourth job (Linux release, say) hits the same traps.

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

### Where Windows actually gets tested

Decided 2026-08-23: build (a) and (b) now, add (c) when the hardware is connected.

- **(a) A Windows 11 ARM64 VM in UTM on the M1 Max agent box.** Free (Apple's Virtualization framework, so near-native
  CPU), and the fast loop. Windows ships OpenSSH Server, so an agent can build and run inside the guest; better still,
  the E2E harness already drives the app over HTTP (`qa/server.rs:42` binds `127.0.0.1:19447`), so the whole suite runs
  in-guest with no host plumbing, and forwarding that one port lets a macOS-side agent drive the running app directly.
  With M1 step 11's screenshot path, that channel carries visual QA too.
- **(b) A `windows-latest` job in GitHub Actions.** Free on a public repo, real x64, and the thing that keeps `main`
  honest.
- **(c) A physical Windows PC on the home LAN**, connected later. The M1 box is denied the NAS and the Hetzner VPS by
  tailnet policy, but the home LAN works, so the same SSH-plus-HTTP loop reaches it.

**The gap this leaves, and it is not a small one.** Neither (a) nor (b) has a real GPU. UTM does not virtualize a GPU
for Windows guests, and paying doesn't fix it: Parallels and VMware Fusion both stop at DirectX 11 on Apple Silicon,
while M2 needs DX12 for `ExtendedSrgbLinear`. GitHub's runners fall back to WARP for the same reason. So both available
environments run wgpu on a software rasterizer.

That means **M2 is the one milestone this setup cannot verify**: no HDR round-trip, no real monitor ICC profile, no Auto
Color Management coexistence, no GPU performance numbers. It's also the differentiator. Two consequences for whoever
builds M2:

- **Write it to be verifiable offline.** Compute reference pixel values on macOS for a fixed set of input images and
  profiles, assert them in `tests/color_management.rs`, and make the Windows path produce the same numbers under WARP.
  Then connecting (c) is a confirmation pass rather than a discovery pass.
- **Treat M2 as provisional until (c) exists.** Don't call it done, and don't put an HDR claim on the website, on the
  strength of a software rasterizer.

Two smaller notes on the VM. It's ARM64, so either test the `aarch64-pc-windows-msvc` build natively (tests the code,
not the shipping binary) or run x64 under Prism emulation (tests the binary, but every timing assertion becomes
meaningless). And **Windows 10 cannot be tested there at all**: there's no practical Windows 10 ARM64 image, so the
full-fidelity-on-Windows-10 decision is unverified until (c), unless someone runs Windows 10 x64 under UTM's much slower
emulation mode for spot checks.

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

## Decisions

All five open questions were answered on 2026-08-23. This section is the record; the milestones below implement it.

**1. Fork the UI per OS (option (a)), with automated parity guarantees.** `docs/design-principles.md` already said "fork
by OS", and that stands: every platform gets its own native chrome, written against its own toolkit. Windows settings,
onboarding, about, and browse mode are Win32, not a shared GPU-drawn layer.

The recommendation on the table was the opposite, so the reasoning behind overriding it matters and is worth writing
down: a native fork buys real native feel in the four places a user spends the most time outside the image itself, and
Prvw's whole premise is being the app that feels made for the platform it's on. The cost is that "parity" stops being
structural and becomes something you have to enforce. So the fork comes with a condition:

**Parity is guaranteed by tooling, not by discipline.** M0.5 builds that harness before any Windows chrome exists,
because retrofitting registries after 3,400 lines of Win32 forms is far harder than growing them alongside. Nothing in
M4 through M6 starts until M0.5 lands.

**1b. Native feel beats cross-platform sameness, and the split runs down the middle of the app.** Decided 2026-08-23,
after M0.5 landed. The image window is custom by nature, so it stays the same everywhere: one wgpu renderer, one set of
gestures, one look. Everything around it goes the other way. Settings, menus, browse mode, onboarding, and about should
each feel like they were written for the platform they run on, using that platform's own toolkit and idioms, not like a
macOS app ported sideways.

The reasoning David gave, and it is worth keeping because it settles a whole class of later arguments: almost nobody
uses Prvw on both macOS and Windows, and the few who do already know how each system behaves and want their apps to
blend into that understanding. So there is no user being served by making the two look alike, and there is a real user
being served by making each one disappear into its platform. Make the Windows version very Windows-like.

Three practical consequences:

- **Parity means feature parity, never layout parity.** M0.5's registries already model this correctly: a `SettingKey`
  names the job (`Toggle`, `Slider`, `Choice`) and each platform picks its own widget, while `description` copy stays a
  per-platform argument. Keep that separation. A reviewer comparing two screenshots side by side and finding different
  layouts is seeing the design working, not a defect.
- **Surfaces may legitimately differ, not just their contents.** Windows is free to place something in a different
  dialog, a context menu, or a different part of the shell, as long as the capability is reachable and the registry says
  so. When a Windows placement genuinely has no macOS counterpart, that is `NotApplicable` with a real reason, not a
  gap.
- **The app's own lineage points the way on Windows.** `AGENTS.md` names ACDSee 2.41 as the model, which was a Win32 app
  with a conventional menu bar, a modal options dialog, and a tree-plus-thumbnails browser. That idiom is still the
  native one for this kind of viewer on Windows, so leaning into it is both the faithful and the native choice.

Design work for the Windows chrome is a draft for David to review before it gets built, per his standing rule that all
human-facing design is reviewed under his name.

**2. Full parity from the start.** Windows ships matching macOS. There is no viewer-only beta, so the number that
matters is the parity number in the effort summary, and the milestone order below is a dependency graph rather than a
release sequence.

**3. Windows 10 is supported, at full fidelity.** Windows 11 is the primary target and gets the first-class treatment,
but Windows 10 is not a degraded tier. See M2, which turns out to make this cheap: scRGB HDR through
`IDXGISwapChain3::SetColorSpace1` has worked since Windows 10 1703, and `GetICMProfileW` covers display profiles on
both. Exactly one API in the plan is genuinely unavailable on Windows 10 client, and its absence costs almost nothing.

**4. Linux keeps working, but gets no parity effort here.** The constraint is **no regressions against what Linux does
today**, which the `desktop-rust` CI job already enforces and M0 improves on (the build stops panicking at startup).
Linux parity is a separate spec, later. M0 step 8 stays in scope because dropping muda's GTK features is a strict
improvement rather than a regression: muda's menu bar has never attached to a winit window on Linux, so nothing that
works today stops working.

**5. Two macOS-only defects get fixed in this effort**, because both are broken today regardless of platform:
`desktop-rust-macos` running only `cargo build` (M0 step 1), and the missing File → Open (M1 step 1).

### What option (a) costs, stated plainly

So the implementing agent knows what it signed up for. Every user-visible setting now has to be added in two places and
kept in step, and the RAW panel alone is 1,157 lines with 16 toggles and eight sliders. `src/settings/CLAUDE.md`'s
"adding a new setting" recipe goes from seven steps to roughly eleven. When Linux gets its own spec, it becomes three
places.

That is the accepted trade. M0.5 exists so the cost is paid in mechanical work the compiler and CI enforce, rather than
in drift nobody notices until a Windows user files an issue.

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

**Status: landed on 2026-08-23 in `7a5a40a..7bf425f`.** All ten steps are done, along with the tests, the five test
gates, and the docs. Two things to know before you build on it:

- **The Done-when's second half is not done.** A no-argument launch still logs one `info` line and waits, rather than
  logging and exiting. It's M1 step 1's job and nothing here moved it.
- **The new CI jobs have never run.** `main` is ahead of `origin/main`, so `desktop-rust-windows` and the reworked
  `desktop-rust-macos` have only ever been exercised as local cross-compiles (`--check windows-cross`,
  `--check linux-cross`), which type-check and lint but run no tests. The first push is the real shakedown.

1. ✅ **Fix the macOS CI job first.** Add clippy and `cargo nextest run` to `desktop-rust-macos` (`ci.yml:101-127`).
   Without this, step 2 changes color behavior on macOS with zero automated coverage. Expect a shakedown: the 59 E2E
   tests spawn real GPU windows and have never run on a hosted runner, so budget a flake pass. If they're too slow for
   every push, run them on `main` or on a schedule, but run them somewhere.
2. ✅ **Replace the macOS system sRGB profile with a generated one.** `moxcms::ColorProfile::new_srgb()`
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
3. ✅ **Fix both `HOME` readers, not one.**
   - `settings::persistence::data_dir()` (`persistence.rs:184`): `%APPDATA%\Prvw` on Windows, `$XDG_CONFIG_HOME` or
     `~/.config/prvw` on Linux. Replace the `/tmp` fallback too; on Windows it resolves to a drive-relative `\tmp`. Keep
     the `PRVW_DATA_DIR` override, the integration tests depend on it.
   - `color::dcp::discovery::home_dir()` (`discovery.rs:107`) is **not** gated, so it runs on Windows and finds nothing.
     Its comment even says "Prvw is macOS-only and `HOME` is always set by launchd". Consequence: every user-installed
     Adobe camera profile is invisible and RAW rendering quietly falls back to the default pipeline. The Windows paths
     are `%APPDATA%\Adobe\CameraRaw\CameraProfiles` and `%PROGRAMDATA%\Adobe\CameraRaw\CameraProfiles`. More
     user-visible than the settings path, because RAW quality is a headline feature.
4. ✅ **`diagnostics::get_process_rss_mb`** (`diagnostics.rs:59`): `GetProcessMemoryInfo` on Windows, `/proc/self/statm`
   on Linux.
5. ✅ **`platform::total_physical_ram_bytes`**: `GlobalMemoryStatusEx` on Windows, `/proc/meminfo` on Linux. Un-gate it
   from macOS while you're there; it stops being previews-only the moment M3 lands.
   - **While you're in there, connect it to the preloader's budgets.** `navigation/preloader.rs:12` set
     `SDR_MEMORY_BUDGET` to 512 MB and `:20` set `HDR_MEMORY_BUDGET` to 1 GB, both absolute constants. On an 8 GB
     Windows laptop with no unified memory, a 1 GB HDR cache plus the GPU-side copies is a different proposition than on
     a 32 GB Mac. Scale them off total RAM the way `previews` already does.
   - **This one was reverted, and the reasoning above is where it went wrong.** M0 scaled the budget but left the
     preload window at a fixed ±2, and a window of ±2 against a budget that retains two images means every preload
     evicts the last one — worse than either number alone. Fixing the coupling then exposed the real problem: the window
     is capped at ±2 and `App::navigate_by` drops everything outside it on every navigation, so a bigger budget has
     nothing to buy upward, and shrinking it downward only takes navigation latency from the machines least able to
     absorb it. `previews` scales because its ±50 window of small thumbnails genuinely uses the RAM; the preloader
     doesn't, and now holds a flat 512 MB (twice that for HDR) with `preload_count()` derived from it. David's call:
     reasonable UX on low-RAM machines matters more than the RAM. Full history and rejected alternatives:
     `docs/notes/preload-window-and-cache-budget.md`.
6. ✅ **Add `windows = "0.62.2"`** under `[target.'cfg(target_os = "windows")'.dependencies]`. It's already in
   `Cargo.lock` transitively, so nothing new is downloaded, but per `AGENTS.md`'s critical rules this is still a new
   direct dependency and needs a license check and a crates.io version check. (0.62.2 is current, published 2025-10-06,
   MIT/Apache-2.0.)
7. ✅ **Scope `reqwest` to macOS** and give the test harness a TLS-free dev-dependency, per the TLS section above. This
   drops `aws-lc-sys` from the Windows target rather than working around it.
8. ✅ **Drop muda from the Linux target.** muda's defaults are `["libxdo", "gtk"]`, the source of all nine GTK C
   dependencies in `Cargo.lock` and of the `apt-get` step in CI. muda's menu bar can't attach to a winit window on Linux
   anyway (see item 9 above), so that chain is dead weight. Dropping it removes the apt step, speeds up Linux CI, and
   makes a future AppImage genuinely dependency-free. Doing it here rather than in M8 pays off for every milestone in
   between.
   - **`default-features = false` alone doesn't work**, so this is a module split rather than a one-line change. muda
     has no Linux backend without the `gtk` feature: the crate itself fails to compile (E0432 on `self::platform`). So
     muda moves under a `cfg(any(target_os = "macos", target_os = "windows"))` dependency section, and the menu module
     gets `cfg`-gated to match. See the cross-compiling section above for the measurements.
   - **What that turned into.** `menu.rs` is now a directory: `menu/mod.rs` picks an implementation and owns the API,
     `menu/native.rs` is the muda-backed menu bar, and `menu/absent.rs` is the platform with none (`AppMenu` there is an
     uninhabited enum and `create_menu_bar` returns `None`). `create_menu_bar` returns `Option<AppMenu>`, so `app.rs`
     and `app/executor.rs` carry no menu `#[cfg]` at all. `input::menu_to_command` moved into `menu/native.rs` next to
     the IDs it matches, and the 11 scattered `set_checked` calls collapsed into one `AppMenu::sync_from_settings`.
     Details and the reachability list for Linux: `src/menu/CLAUDE.md`.
9. ✅ **Port the Go check runner to Windows. It did not compile there, and this blocked step 10.** Verified with
   `GOOS=windows GOARCH=amd64 go build`: `scripts/check/checks/common.go:111` calls
   `syscall.Kill(-cmd.Process.Pid, syscall.SIGTERM)` and `:130` sets `SysProcAttr{Setpgid: true}`, and neither exists on
   `windows/amd64`. Both live in `RunCommand` / `KillAllProcesses`, which every check goes through. Six more call sites
   shell out to POSIX `find`, which on Windows resolves to `C:\Windows\System32\find.exe`, a text search tool, and
   returns garbage: `common.go:288`, `common.go:409`, `desktop-rust-rustfmt.go:21`, `scripts-go-gofmt.go:22`,
   `scripts-go-gocyclo.go:35`, `scripts-go-misspell.go:25`. Split the process-group handling into `common_unix.go` /
   `common_windows.go` (job objects or `taskkill /T` on Windows) and replace the `find` shell-outs with
   `filepath.WalkDir`. Budget a day.
10. ✅ **Add the `desktop-rust-windows` CI job** on `windows-latest`: build, clippy, `cargo nextest run`. Give it
    `needs: changes` and `if: inputs.run_all || needs.changes.outputs.rust == 'true'` to match `ci.yml:54-55`, or it
    runs on every website-only push, and add it to `ci-ok`'s `needs`. Mind the shape details in the CI section, and
    **audit the POSIX path fixtures** that will now execute for the first time: `navigation/preloader.rs:766,894`,
    `decoding/mod.rs:658-660,708,728`, `decoding/raw.rs:1553-1902`, `settings/persistence.rs:245,263`,
    `browser/tree_model.rs:404-497`, and `folder_watch.rs:362-423` all hardcode POSIX absolute paths. Most are pure
    `PathBuf` string manipulation and will pass, but don't assume it.

**Tests:** ✅ unit tests for the per-OS helpers and both `HOME` replacements. A test that constructs `color::State`
without touching the filesystem. CI green on three platforms, with macOS finally running more than a build. The helpers
take their environment lookup as a parameter (`platform::fixed_env`), so every platform's path layout is asserted from
whichever host runs the tests rather than only from its own.

**Also re-open the five test gates step 2 retires.** ✅ `color/transform.rs:190`, `decoding/mod.rs:512`,
`decoding/raw.rs:1482` and `:1515`, and `color/dcp/mod.rs:419` were all `#[cfg(all(test, target_os = "macos"))]`, and
every one of their comments blames `srgb_icc_bytes` reading a macOS-only system file. Once that read is gone the
comments are wrong (the repo's "describe current behavior, not history" rule) and the new Windows and Linux jobs are
running less coverage than they could. Drop the gates the fix retires and delete the stale comments in the same pass.

**Docs:** ✅ note the sRGB change in `src/color/CLAUDE.md`; add a supported-platforms line to `AGENTS.md`.

**Done when:** `cargo run -- some.jpg` on a Windows box shows the image, **and** `cargo run` with no arguments does
something defensible rather than nothing. (That second half is M1 step 1's job; if it isn't done yet, at least make the
no-argument case log and exit rather than hang invisibly.)

**Where that landed:** the first half is as verified as a Mac can make it —
`cargo xwin build --target x86_64-pc-windows-msvc` produces a `prvw.exe`, but nobody has started it on Windows yet. The
second half is the defensible-rather-than-nothing version: off macOS a no-argument launch logs
`Nothing to show. Pass an image or a folder: prvw <path>` at `error` and exits 2, instead of running an event loop that
will never build a window. macOS keeps waiting for its Apple Event. The real fix, an empty-state window plus File →
Open, is still M1 step 1.

### M0.5: the parity harness (one to two weeks)

**Intent:** Option (a) means the same feature gets built twice, so "are they the same?" has to be a question the build
answers, not one a human remembers to ask. Build the harness **before** any Windows chrome exists. Retrofitting it onto
3,400 lines of finished Win32 forms is a different and much worse job.

Numbered M0.5 rather than M1 because it's a cross-cutting prerequisite rather than a stage, and because renumbering the
16 steps of M1 would break every cross-reference in this document.

Three layers, weakest to strongest. Build all three; each catches what the others miss.

1. **Compile-time exhaustiveness for "does it exist".** This is the only layer that gives an actual guarantee, so lean
   on it hardest.
   - Give each settings field a variant in a `SettingKey` enum, and make every platform's panel builder consume it
     through an exhaustive `match` with **no** `_` arm. Adding a field then fails the build on every platform until each
     one handles it. That's the guarantee; everything else is a smoke alarm.
   - Same shape for `MenuIds` and for `AppCommand`: each platform's menu builder and command dispatcher matches
     exhaustively, and a variant that genuinely doesn't apply somewhere is spelled
     `SettingKey::Foo => NotApplicable { reason: "..." }` rather than silently omitted. The reason string feeds layer 2.
   - `Settings` (`settings/persistence.rs`) stays the single source of truth for what a setting _is_; `SettingKey` is
     the source of truth for what a UI owes it.
2. **A generated parity table, checked in CI.** A `cargo xtask parity` (or a new check in `scripts/check/checks/`) reads
   the registries and emits `docs/parity.md`: a matrix of feature by platform by status, where status is `done`,
   `not applicable` plus the reason string, or `missing`. Then a check fails when the generated file differs from the
   committed one. That's exactly the pattern `changelog-commit-links.go` already uses, so it fits the existing runner.
   The point of committing the generated file is that the diff shows up in review, so parity changes are visible rather
   than implicit.
   - **Do not hand-maintain this table.** A hand-written parity doc is wrong within a month and worse than nothing,
     because it looks authoritative.
3. ✅ **One behavioural E2E suite, run against every platform.** The strongest evidence that two implementations agree,
   and Prvw was unusually well set up for it: the suite already drove the app through the QA HTTP server rather than
   through the UI, so the same assertions run anywhere the server runs. Layer 1 proves a toggle exists on both; layer 3
   proves it does the same thing.
   - **What landed.** The one `#![cfg(target_os = "macos")]` file became `tests/e2e_shared.rs` (41 tests, no `cfg`, runs
     wherever the app does), `tests/e2e_macos.rs` (18 tests that poke a native widget), and `tests/e2e/` (the harness
     both share). The macOS half is browse mode's `NSOutlineView` + `NSCollectionView`, the AppKit settings window, the
     AppKit fullscreen round trip, and the `screencapture` MCP tool.
   - **The split is enforced, not documented.** A shared test can only get an app through `SharedApp::start`, which
     takes the `CommandKey` names it exercises and resolves them against `GET /parity` — layer 1's own answer. `done`
     runs it, `not applicable` skips it with the registry's reason, `missing` fails it by name. The five title-bar tests
     are the live case: they skip off macOS with the `TitleBar` reason and start running the day a platform claims the
     setting. `shared_suite_stays_platform_neutral` rejects `target_os` and a raw `TestApp` in the shared file.
   - **What's proven is compilation, not behaviour.** `--check windows-cross` and `--check linux-cross` build
     `--all-targets`, so the shared suite type-checks and lints for both. Nobody has run it on either. The Linux job
     skips it outright while the runner has no `DISPLAY`; give that job a session and it runs.
   - Explicitly **not** doing screenshot diffing across platforms. The two UIs are supposed to look different; that's
     the entire point of option (a). Pixel comparison across platforms would be pure noise. Per-platform screenshot
     baselines are fine later, but they answer a different question.

**Tests:** the harness needs its own tests. A deliberate omission (a `SettingKey` variant a platform doesn't handle)
must fail the build; a deliberate `NotApplicable` must pass and show up in the table with its reason.

**Docs:** rewrite the "adding a new setting" recipe in `src/settings/CLAUDE.md` around the new registries, and document
the `NotApplicable` escape hatch, including that using it without a real reason is how this whole layer rots.

**Done when:** adding a settings field to `Settings` and nothing else fails the Windows build, the macOS build, and the
parity check, with three messages that each say what's missing and where.

### M1: Windows image mode at parity (three to four weeks)

**Intent:** Everything a person does with Prvw 95% of the time, working properly on Windows. This is what makes a
shippable-if-minimal Windows build. It's the biggest non-UI milestone here because the launch and input surface is where
macOS assumptions are densest, and none of them show up as compile errors.

Steps 1 to 7 plus 14 and 15 are the ones a naive port would miss: none of them is a compile error, and several look like
rendering bugs when they're really launch, input, or layout bugs. The first seven come first for that reason; the other
two sit near the end only because they're smaller.

1. **Make the no-argument launch work, and add File → Open.** See finding 3 above: `app.rs:3257` returns from
   `resumed()` without building a window when `waiting_for_file` is true, `app.rs:3300`'s fallback is macOS-only
   onboarding, and `menu/native.rs:326-338` has no Open item. Off macOS that's an invisible process with no recovery. Do
   two things:
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
3. **Suppress browse mode on Windows until M5.** `menu/native.rs:448` builds the "Image browser" item unconditionally,
   and off macOS `App::set_view_mode` (`app.rs:1917`) calls `browser.toggle_mode()` and then stops requesting redraws.
   So pressing Enter flips the app into `ViewMode::Browse` with no visible change, changed key routing, a changed menu
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
   `MenuIds` and `menu::native::menu_to_command` as the single source of menu action mapping (M0 moved it there from
   `input.rs`, which now owns keys only); only the construction inside `menu/native.rs` forks.
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
13. **Run the shared E2E suite on Windows.** M0.5 layer 3 did the split: `tests/e2e_shared.rs` compiles and lints for
    Windows today and the `desktop-rust-windows` job already runs `cargo nextest run`, so the first push is when 41
    tests meet a Windows box for the first time. Expect that to be the work, and budget for it.
    - The harness keeps test windows out of the way with `ActivationPolicy::Prohibited` (`main.rs:164-168`), which is
      `winit::platform::macos` only. On Windows the closest thing is `WindowAttributes::with_active(false)` plus leaving
      the window un-raised; there's no app-level "cannot be activated" lever, so expect this to be less airtight.
    - The waits are all tuned to macOS: 500 ms after startup, 150 ms after a key press, and live-sync timeouts measured
      against FSEvents. `ReadDirectoryChangesW` has its own latency.
    - Check WARP against the real adapter early. If a hosted runner's GPU can't hold the suite, that decides whether
      these run on every push or on `main`.
14. **Pin the GPU backend and revisit adapter selection.** `renderer.rs:243` uses `Backends::all()`, so which backend
    wgpu picks on a given Windows machine isn't ours to decide. M2's entire HDR plan rests on
    `SurfaceColorSpace::ExtendedSrgbLinear`, which is a **DX12** capability, so pin `Backends::DX12` on Windows here
    rather than leaving the flagship feature conditional on an unowned choice. Separately, `renderer.rs:256` asks for
    `PowerPreference::LowPower`; on a Windows laptop with hybrid graphics that picks the integrated GPU, which on many
    machines isn't the one driving the display the photographer calibrated. Decide deliberately.
15. **Fix the rename semantics in `folder_watch`.** `folder_watch.rs:57-59` documents the invariant "Creates, removes,
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

**Windows 10 is a full-fidelity target here, and it turns out to be nearly free.** Checked against the API requirements:

- `IDXGISwapChain3::SetColorSpace1` with `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709` (scRGB, what wgpu 30 calls
  `ExtendedSrgbLinear`) has worked since **Windows 10 1703**. So the HDR path is not a Windows 11 feature.
- `GetICMProfileW` and `MonitorFromWindow` are ancient. Display profile detection is identical on both.
- `longPathAware`, `IFileOpenDialog`, `IShellItemImageFactory`, and `PrintWindow` are all Windows 10 or older.
- **Exactly one API is genuinely unavailable**: `ColorProfileGetDisplayDefault` requires build 20348, and Windows 10
  client tops out at 19045 (22H2), so it's Windows 11 and Server 2022 only in practice. It returns the advanced-color
  profile for a display in HDR mode, which is a refinement over `GetICMProfileW`, not a replacement. Fall back to
  `GetICMProfileW` on Windows 10 and the difference is confined to one narrow case.
- Auto Color Management is Windows 11 22H2 and later. Its absence on Windows 10 makes that platform **simpler**, not
  worse: there's no competing system transform to coexist with.

So Windows 10 gets the same pipeline, and the **Windows 10 22H2** floor (decided 2026-08-23) makes it simpler still:
every API above is present unconditionally on 22H2, so there are no version guards to write except one runtime probe for
`ColorProfileGetDisplayDefault`, which is the Windows 11 enhancement. Set `longPathAware` and the scRGB path without
guards.

1. **Display profile detection.** `MonitorFromWindow` for the window's current monitor, `GetICMProfileW` on its DC for
   the ICC path, read the bytes, hand them to `color::State.display_icc`. On Windows 11, prefer
   `ColorProfileGetDisplayDefault` when the display is in HDR mode, guarded by a runtime version check with a
   `GetICMProfileW` fallback.
2. **React to monitor changes.** The macOS version re-detects when the window moves between screens. On Windows, hook
   winit's `Moved` and `ScaleFactorChanged`, and consider `WM_DISPLAYCHANGE` for profile changes that happen in place.
   This shares plumbing with M1 step 6's per-monitor DPI work, so do them with that in mind.
3. **HDR and EDR.** Bump wgpu from the resolved 29.0.4 to 30 (30.0.0 landed 2026-07-01; 30.0.1 landed 2026-08-22, so
   check the three-day release-age rule before pinning it) and use `SurfaceColorSpace::ExtendedSrgbLinear` on the
   `Rgba16Float` surface. Verify Windows output against the Metal EDR path on the same file and the same monitor class.
   - **HDR is a user toggle on Windows, and that's a genuine behavioral difference from macOS, not a bug.** macOS EDR
     gives headroom whenever the display has it. On Windows, values above 1.0 only reach the display when the user has
     turned HDR on in Settings, and the app is expected to query `DXGI_OUTPUT_DESC1` and fall back to
     `DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709` when it's off. So the "HDR / EDR output" setting means something
     different per platform: on macOS it's ours to decide, on Windows it's ours to _respect_. Decide what the settings
     UI says when the display isn't in HDR mode, and check whether wgpu 30's `Surface::display_hdr_info` gives us the
     query or whether we need `IDXGIOutput6` through `as_hal`.
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

### M4: the Windows settings window (four weeks)

**Intent:** Build the Win32 counterpart of the AppKit settings window, registered through M0.5 so it can't drift.

**Blocked on M0.5.** Starting this before the registries exist means retrofitting them into finished code.

Roughly 3,400 lines of AppKit to mirror: `settings/window.rs` (863), `widgets.rs` (87), `panels/general.rs` (133),
`panels/raw.rs` (1,157), plus the per-feature panels that live with their features (`color/settings_panel.rs` 106,
`zoom/settings_panel.rs` 87, `slideshow/settings_panel.rs` 237, `file_associations/settings_panel.rs` 748).

1. **Pick the Win32 UI approach first, and write down why.** Raw `CreateWindowEx` plus `WM_COMMAND` is the lowest
   dependency and the most tedious. A dialog-template approach is less code but awkward for the dynamic enable and
   disable relationships the panels already have (ICC off disables Color match and Relative colorimetric). Whatever
   wins, it also has to look right under per-monitor DPI, which M1 step 6 sets up.
2. **Mirror the structure, not the pixels.** Same six panels in the same sidebar order (General, Zoom, Color, RAW,
   Slideshow, File associations), same immediate-apply-through-`AppCommand` behavior, same Close button. Windows
   conventions where they differ: this is "Options", and the window is a property sheet rather than a preferences
   window.
3. **Register every field through `SettingKey`** as M0.5 requires. The exhaustive match is what makes this milestone
   finishable: when it compiles, no setting is missing.
4. **File associations is the panel that can't mirror.** Windows removed programmatic default-handler setting, so the
   16-toggle grid becomes registration plus a button that opens `ms-settings:defaultapps`. This is the one panel where
   `NotApplicable` is the honest answer for most of the rows, and the reason strings will show up in `docs/parity.md`.
   Write new copy for it.

**Tests:** M0.5 layer 3 assertions for every toggle and slider, running on both platforms.

**Docs:** `src/settings/CLAUDE.md` gets the Windows half of the recipe.

### M5: browse mode on Windows (three to six weeks)

**Intent:** The largest remaining feature. Native Win32, per the option (a) decision, and **blocked on M0.5**.

`SysTreeView32` for the tree and an owner-drawn `ListView` (or a custom control) for the thumbnail grid. The pure cores
come along unchanged: `grid_model`, `grid_scheduler`, `thumbnail_cache`, and `tree_model` are already platform-neutral
and tested, so this milestone is the shell around them.

The constraint carried over from macOS: the GPU surface occludes anything behind it, so native child controls have to
sit in front of it, and the two screens swap rather than composite. That applies on DXGI too, and it's the biggest
source of surprise waiting in this milestone. Read `src/browser/CLAUDE.md`'s "The swap" section before starting; the
z-order and hide-one-show-the-other reasoning transfers even though the API doesn't.

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

Native Win32, per option (a), and **blocked on M0.5**. That's 2,349 lines of AppKit to mirror (`onboarding/` is three
files totalling 2,097, plus `about.rs` at 252). One thing gets easier: the macOS versions run before `EventLoop::new()`
to dodge the nested-run-loop segfault, and Windows has no autorelease pools, so these can be ordinary windows. One thing
gets harder: they're modal-shaped, and M1 step 1's warning applies, because a Win32 modal loop blocks winit's pump the
same way.

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

### M8: Linux stays green, and gets its own spec later

**Not a milestone in this effort.** The decision is no regressions against what Linux does today, and no deliberate
parity work. What that means in practice:

- **Keep the `desktop-rust` CI job on `ubuntu-latest` green** through every milestone. It's the canary that has kept the
  non-macOS build compiling, and it's the whole enforcement mechanism for "no regressions".
- **Linux gets strictly better from M0 anyway**, for free: the `srgb_icc_bytes` fix means the Linux build stops
  panicking at startup, and the `data_dir()` fix gives it a correct config path. Neither was a goal; both fall out.
- **M0 step 8 (dropping muda's GTK features) stays in scope.** It isn't a regression: muda's menu bar has never attached
  to a winit window on Linux, so nothing that works today stops working, and CI gets faster for every milestone after
  it.
- **What a future Linux spec owes:** an in-app menu (muda can't help), volume enumeration through `/proc/mounts` or
  `udisks2`, AppImage or Flatpak packaging, and Wayland-versus-X11 testing for fullscreen, DPI, and window positioning.
  Plus its own pass through M0.5's registries, which is where the parity harness earns its keep a second time.

## Effort summary

Recomputed after the option (a) decision, which adds M0.5 and fixes M4 at its native-fork estimate, and after Linux
dropped out of scope.

- **M0**, non-macOS build runs and CI can tell: one to two weeks. Cumulative: one to two weeks.
- **M0.5**, the parity harness: one to two weeks. Cumulative: two to four weeks.
- **M1**, Windows image mode: three to four weeks. Cumulative: five to eight weeks.
- **M2**, color management including Windows 10: one to two weeks. Cumulative: six to 10 weeks.
- **M3**, previews: three to five days. Cumulative: seven to 11 weeks.
- **M4**, the Windows settings window: four weeks. Cumulative: 11 to 15 weeks.
- **M5**, browse mode: three to six weeks. Cumulative: 14 to 21 weeks.
- **M6**, onboarding and about: one to two weeks. Cumulative: 15 to 23 weeks.
- **M7**, distribution: one to two weeks. Cumulative: 16 to 25 weeks.
- **M8**, Linux: out of scope, zero. Keeping the Linux CI job green is a constraint on every milestone above rather than
  work of its own.

**Full Windows parity: 16 to 25 weeks, so four to six months.** M0 through M3 (about seven to 11 weeks) is the point
where the thing is worth dogfooding daily, which is a useful checkpoint even though it isn't a ship target.

The option (a) decision costs roughly four weeks against the alternative: M0.5 is new, and M4 lands at four weeks rather
than being shared work. It buys native chrome on both platforms and, through M0.5, a build that refuses to compile when
the two drift.

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

## Open questions

None. All five questions this plan opened with, and the two follow-ups they produced, were answered on 2026-08-23. See
**Decisions** above and the log below.

## Decision log

- **2026-08-23**: native feel per platform beats sameness across platforms. The image window stays identical everywhere;
  settings, menus, browse, onboarding, and about are written to each platform's own idiom. Parity is measured as
  features reachable, never as layouts matching. See decision 1b above.

- **2026-08-23**: fork the UI per OS (option (a)), with M0.5's parity harness as the condition. Full parity from the
  start. Windows 10 supported at full fidelity, Windows 11 first. Linux held to no-regressions with its own spec later.
  The two macOS-only defects (macOS CI, File → Open) fixed in this effort. Test environments (a) local VM and (b) GitHub
  Actions now, (c) a physical Windows PC that David already owns, QA'd in person when the time comes.
- **2026-08-23**: the Windows floor is **Windows 10 22H2**, not 1703. 22H2 is the only Windows 10 still receiving
  consumer ESU (through 2026-10-13), so anything older is unsupported by Microsoft before this port ships. Practical
  effect: every API this plan calls is present unconditionally on 22H2, so the version checks collapse to a single
  runtime probe for `ColorProfileGetDisplayDefault` (Windows 11 only) and nothing else. Set `longPathAware` and the
  scRGB path without guards.
