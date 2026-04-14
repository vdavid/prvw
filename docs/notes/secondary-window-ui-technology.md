# UI technology for secondary windows

Decision date: 2025-04-13. Decided: **AppKit via objc2**.

## Context

Prvw has four windows:

- **Main window**: Shows images. Uses winit + wgpu + glyphon for text overlays. This works well and stays as-is.
- **About window**: Currently an `osascript display dialog` — a dumb macOS system alert with no rich UI.
- **Onboarding window**: Currently rendered with wgpu + glyphon in the main window. Manual pixel positioning, hardcoded
  colors, no system font size awareness.
- **Settings window**: Doesn't exist yet.

The main window's renderer (glyphon) is good enough for overlays but isn't suitable for building native-feeling UI with
system controls. The three secondary windows need to look and feel like native macOS windows with system font, user's
font size settings, native controls (toggles, buttons, dropdowns), and consistent layout.

## Problem space

### What we want

1. **Liquid glass titlebars** on all windows (macOS Sequoia/Tahoe style). An agent managed this earlier via
   `setTitlebarAppearsTransparent` + `fullSizeContentView` NSWindow style mask, but that change was reverted along with
   other work. Needs to be redone. (This is an NSWindow property, independent of the content rendering technology.)
2. **System text rendering**: System font only, at the user's configured size (respecting System Settings font scale).
3. **Native controls**: Real OS toggles, buttons, dropdowns on Settings. OS-native-looking buttons on About and
   Onboarding.
4. **Easy layout**: Centering text, consistent paddings, line heights — without manually computing pixel offsets like the
   current glyphon onboarding code does.
5. **Rich UI on all three windows**, including About (currently a plain text alert).
6. **Cross-platform eventually** (Linux, Windows), but macOS-first.

### Technologies tried and rejected for secondary windows

| Technology | Tried for | Why it didn't work |
|---|---|---|
| **glyphon** | Main window overlays, onboarding | Very raw font rendering. Manual pixel positioning. No layout system, no system font size awareness, no native controls. Fine for overlays, wrong for forms/dialogs. |
| **CoreText** | Explored but not currently used | Low-level text shaping API, not a UI framework. Doesn't solve layout or controls. |
| **osascript** | About dialog | Extremely limited. Can show a text alert with buttons, nothing more. No rich UI. |
| **wry** | Explored | Would hit the same WebView overhead concerns as Tauri for the main window. For secondary windows specifically it's actually fine (see Option B below), but it's not truly native. |
| **Dioxus Native** | Explored | Uses its own text rendering (similar to glyphon). Not truly native macOS components. |
| **Tauri** | Explored for main window | Image loading was prohibitively slow: ~3000 ms to decode the same JPEG that zune-jpeg handles in ~80 ms. Out of the question for the main viewer, and if we're not using it for the main window, using it only for secondary windows adds weight for little gain. |

## Solution options

Three realistic approaches survived analysis.

### Option A: AppKit via objc2 (chosen)

Build the three windows using real AppKit components through the objc2 crate (already a dependency). Use `NSStackView`
for layout, `NSTextField` for labels, `NSButton` for buttons and toggles, `NSPopUpButton` for dropdowns.

**Strengths:**
- 100% native controls: dark mode, accent colors, accessibility, VoiceOver — all free
- System font at user's configured size via `NSFont::systemFontOfSize` / `.controlContentFontOfSize`
- Liquid glass titlebar via NSWindow property (already partially implemented in `window.rs`)
- Zero additional binary size (AppKit is a system framework)
- Zero build complexity — it's Rust code with `use objc2_app_kit::*`
- Same process, same address space — settings sharing via `Arc<Mutex<Settings>>` or `NSUserDefaults` with Cocoa Bindings
- Best debuggability: single process, single language, `log::info!()` everywhere, breakpoints work normally
- The objc2 crate is already a dependency and the gotchas are already documented in AGENTS.md

**Weaknesses:**
- Verbose: ~15 lines of Rust per labeled toggle. A Settings window with eight controls is ~200–300 lines.
- The `Retained<>` lifetime gotcha: every view must be kept alive through the modal/window session (documented in
  AGENTS.md and `apps/desktop/CLAUDE.md`)
- "Never run AppKit modals inside winit's event loop" constraint — these windows need to be independent windows or
  managed via `EventLoopProxy`
- `NSStackView` distribution modes are confusing (gravity areas vs. equal centering vs. equal spacing)
- `NSTextField` is both label AND input — must explicitly configure as non-editable, non-selectable for labels
- No cross-platform code reuse. Linux/Windows need entirely different implementations.

**Cross-platform strategy:** Define a `trait SecondaryWindow` with methods like `show_about()`, `show_settings()`.
macOS impl uses AppKit, Linux impl uses gtk4-rs, Windows impl uses Win32/WinUI. Each platform is ~200–400 lines per
window. This is how serious native apps (Firefox, Chrome) handle it.

### Option B: wry WebView

Embed a WKWebView (macOS) / WebView2 (Windows) / WebKitGTK (Linux) for the three secondary windows using the
[wry](https://github.com/nicotinetroll/nicotinetroll) crate. Main image window stays winit+wgpu.

**Strengths:**
- Easy layout: HTML/CSS (flexbox/grid) — trivially easy to center, pad, align
- Cross-platform out of the box with the same HTML/CSS/JS on all three platforms
- Can build any layout imaginable
- System font matching: CSS `-apple-system` font family works. `font: -apple-system-body` even respects user text size.
- Liquid glass: set WebView background transparent + `NSVisualEffectView` behind it (a few lines of objc2)
- Near-zero build complexity: `cargo add wry`, maybe embed HTML via `include_str!`
- Hot reload possible during dev
- Wry is backed by the Tauri project (well-funded, active)
- ~0.5 MB additional DMG size

**Weaknesses:**
- Controls are HTML, not native. Toggles, buttons, and dropdowns look close to native macOS with careful CSS but aren't
  pixel-perfect `NSSwitch` / `NSPopUpButton`. You'd either accept the web-styled look or spend effort on CSS theming.
- WKWebView runs in a separate process on macOS. Adds ~20–30 MB resident memory per WebView.
- Split-brain debugging: `console.log()` goes nowhere by default. Need Safari Web Inspector or IPC piping to Rust.
  JS errors fail silently without explicit `window.onerror` handling.
- Communication requires JSON IPC: Rust → JS via `evaluate_script()`, JS → Rust via `window.ipc.postMessage()`.
  Must define a protocol, handle serialization. Works but adds ceremony.
- Dark mode: must implement CSS theme via `prefers-color-scheme` media query — you write and maintain all the theme CSS.
- On Windows, WebView2 requires the WebView2 runtime (ships with Win11, optional on Win10). On Linux, WebKitGTK is a
  system package dependency.

### Option C: SwiftUI via FFI

Build the three windows as SwiftUI views in a small Swift package, compile as a `.framework`, call from Rust via FFI.

**Strengths:**
- Most beautiful, most native UI possible. SwiftUI liquid glass materials (`Material.ultraThinMaterial`), system
  controls, automatic dark mode, built-in animations.
- System font and sizing handled automatically.
- Most productive UI code: a toggle with label is `Toggle("Auto zoom", isOn: $autoZoom)`. A Settings window is ~50
  lines of Swift vs ~300 lines of objc2 Rust.
- Excellent accessibility support with `.accessibilityLabel()` modifiers.
- Apple is all-in on SwiftUI — it's the official future of Apple platform UI.

**Weaknesses:**
- **Build complexity is significant.** Need a `build.rs` that invokes `swiftc` or `xcodebuild`, handles linking, sets
  rpath so the dylib is found in the .app bundle. CI needs Xcode.
- Two languages in one project (Rust + Swift).
- FFI boundary design: Swift functions exposed as `@_cdecl` or `@objc` classes callable from objc2. All data crosses
  the boundary as primitive types, raw pointers, or C strings. No shared smart pointers between Swift ARC and Rust
  ownership.
- SwiftUI views must be created on the main thread.
- SwiftUI on macOS is less mature than on iOS. Some controls behave unexpectedly.
- SwiftUI APIs evolve fast — deprecations happen, code targeting macOS 13 may need updating for macOS 16.
- macOS only. Zero help for Linux/Windows cross-platform.
- ~1–2 MB additional DMG size for the compiled Swift dylib.
- Debuggability is good but requires setup: Swift's `print()` goes to stdout, but for unified logging you'd expose a
  Rust FFI function that Swift calls to route through the `log` crate.
- Communication: either FFI function calls for get/set settings, or use `NSUserDefaults` as shared medium (both Rust
  and Swift can read/write, SwiftUI's `@AppStorage` binds directly).

## Comparison

| Dimension | AppKit/objc2 | wry WebView | SwiftUI FFI |
|---|---|---|---|
| Native fidelity | Real native controls | Looks-like-native CSS | Real native controls |
| Liquid glass | NSWindow property | Transparent WebView + VisualEffect | `.ultraThinMaterial` |
| Debuggability | Best (single process) | Split (Rust + Safari DevTools) | Good (unified with setup) |
| DMG size delta | ~0 | ~0.5 MB | ~1–2 MB |
| Settings comms | Trivial (shared memory) | JSON IPC protocol | FFI functions or NSUserDefaults |
| Build complexity | Zero | Near-zero | Significant (Swift toolchain) |
| Cross-platform | Per-platform code | Same code everywhere | macOS only |
| Future-proof (macOS) | Stable but aging | Stable | Apple's bet |
| Future-proof (x-plat) | Good (trait pattern) | Best | N/A |
| UI code productivity | Low (verbose) | High (HTML/CSS) | Highest (for UI), medium (for FFI) |
| Maintenance | Low (AppKit is stable) | Low | Medium (SwiftUI API churn) |
| Accessibility | Best (automatic) | Good (semantic HTML + ARIA) | Excellent (built-in) |
| Testing | Hard from Rust | Testable in browser | XCTest available but separate setup |

## Decision

**AppKit via objc2**, for these reasons:

1. **Already in the project.** objc2 is a dependency, the gotchas are documented, and the team knows the patterns.
2. **Zero build/toolchain overhead.** No Swift compiler, no WebView runtime, no IPC protocol.
3. **Settings communication is trivial.** Shared memory in the same process — no serialization or message passing.
4. **Best debuggability.** Single process, single language, unified logging.
5. **True native controls.** For a "platform-native" image viewer (principle #5 in AGENTS.md), real NSButton/NSSwitch
   beats HTML styled to look like them.
6. **Manageable verbosity.** Three windows with 5–15 components each is ~600–800 lines total. Verbose but not unmanageable.
7. **Clean cross-platform path.** `trait SecondaryWindow` with per-platform implementations is the pattern used by
   serious native apps. When Linux/Windows support happens, each platform gets its own ~200–400 lines per window.

**Fallback plan:** If objc2 verbosity or Auto Layout pain proves worse than expected, wry is the next best option. It
adds ~0.5 MB, gives trivial layout via HTML/CSS, and is cross-platform. The trade-off is non-native controls and
split debugging. SwiftUI is the nuclear option for macOS polish but the build complexity makes it a last resort.

### Implementation status (2025-04-13)

All three secondary windows are implemented in `native_ui.rs` using AppKit via objc2. Each has transparent titlebar,
`NSVisualEffectView` frosted glass background, and `NSStackView`-based layout. The onboarding also replaced `swift -e`
scripts with direct CoreServices FFI (`LSCopyDefaultRoleHandlerForContentType`, `LSSetDefaultRoleHandlerForContentType`)
for near-instant file association queries. Settings persistence lives in `settings.rs`.
