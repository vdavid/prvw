# Prvw: project bootstrap plan

Goal: bootstrap the Prvw project from zero to a runnable image viewer with an Astro website, full CI, tooling, and docs,
all modeled after Cmdr's quality standards. The result should be a working app that opens an image file, renders it
full-screen with GPU acceleration, supports zoom/pan, and navigates to prev/next images with background preloading.

## Context and key decisions

**What Prvw is**: A fast, minimal image viewer for macOS. Think ACDSee 2.41: open a pic, see it instantly in full
screen, zoom/pan, arrow keys for next/prev (preloaded in background), ESC to close. Written in Rust with `winit` +
`wgpu` + `muda`. No UI framework, no webview. Eventually cross-platform (Linux, Windows), but macOS-first.

**Why winit+wgpu instead of Dioxus/Tauri**: Maximum performance and minimum binary size. An image viewer has almost no
UI chrome, so a reactive UI framework adds overhead without benefit. `wgpu` gives GPU-accelerated rendering (Metal on
macOS, Vulkan on Linux, DX12 on Windows). `muda` gives native OS menus with shortcuts and checkmarks (same lib Tauri
uses). This stack keeps the binary lean and the startup instant.

**Why no tokio**: The preloader does CPU-bound image decoding, not async I/O. `std::thread` + channels is the right tool
here. `tokio` would add unnecessary weight and event-loop integration complexity with `winit`. Use `pollster` to block on
`wgpu`'s async adapter/device requests.

**Why a separate app from Cmdr**: Different concerns (image rendering vs file management), different tech stacks (pure
Rust vs Tauri+Svelte), different release cadences. Cmdr launches Prvw the same way Finder would, via `open`, but with
an IPC fast path when the Prvw daemon is already running.

**Monorepo structure**: Mirrors Cmdr. The desktop app and the getprvw.com website live in `apps/`. Scripts, docs, and CI
at the root. pnpm workspace for the website.

**Blue color palette**: The brand is light/happy sky blue with sun-yellow sub-accents. Not Cmdr's mustard yellow. Think
sky + sun: `#4da6ff` primary blue, darker blues for depth, warm yellow `#ffc206` for small highlights.

**License**: BSL 1.1, same terms as Cmdr (Rymdskottkarra AB), same change date structure.

**Code signing**: Reuse the existing `Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)` certificate. The app
needs its own bundle identifier (`com.veszelovszki.prvw`) and entitlements.

**winit 0.30 API note**: `winit` 0.30 uses the `ApplicationHandler` trait (not the old closure-based `run`). The app
struct must implement `ApplicationHandler`, and the `wgpu` surface must be created in `resumed()`, not at startup. This
is required for correctness on macOS and shapes the architecture of the entire event loop.

## Milestones

### Milestone 1: repo scaffolding, tooling, and initial CI

**Intention**: get the monorepo structure, git, tooling, CI, and docs in place before writing any app code. This is the
foundation everything else builds on, and it's where Cmdr's lessons pay off most. A dev (or agent) landing in this repo
should immediately know what's what, how to check their code, and what the project values. CI is included here so that
every subsequent milestone develops under CI protection.

Steps:

1. **Init git repo** in `~/projects-git/vdavid/prvw`. Create `.gitignore` (based on Cmdr's, adapted for no Tauri/Svelte).
2. **Create `AGENTS.md`** in the spirit of Cmdr's. Include: project description, principles (adapted from Cmdr but
   shorter, focused on performance and simplicity), file structure, testing/checking instructions, debugging tips,
   critical rules. Keep it lean, this is a small project. Include Prvw-specific gotchas like "wgpu surface must be
   created in `resumed()`, not at startup" and "use `std::thread` for CPU-bound work, not `tokio`".
3. **Create `README.md`** in David's writing style. Short, punchy, welcoming. No em dashes. Use sentence case. Mention
   the ACDSee inspiration. Include a "Someday/maybe" section with: GPU-accelerated image pipeline, EXIF-aware rotation,
   ICC color management. Pricing: free for personal use, $29/year per user for commercial use.
4. **Create `LICENSE`** file: BSL 1.1, licensor Rymdskottkarra AB, licensed work "Prvw", same structure as Cmdr's. Change
   date: 2029-04-11 (3 years from now).
5. **Create `.mise.toml`**: pin Go 1.26, Node 25, pnpm 10. Rust managed by `rust-toolchain.toml` at repo root
   (intentionally at repo root, not `apps/desktop/` like Cmdr, because Prvw has a single Rust crate).
6. **Create root `package.json`** and `pnpm-workspace.yaml` for the monorepo.
7. **Set up `scripts/check/`**: Port the Go check runner from Cmdr. Strip all Cmdr-specific checks (Svelte, Tauri, etc.).
   Keep: `gofmt`, `go-vet`, `staticcheck`, `misspell`, `gocyclo`, `deadcode`, `go-tests` for the scripts themselves.
   Add: `rustfmt`, `clippy`, `cargo-test` for the desktop app. Add: website checks (prettier, eslint, typecheck, build).
   Keep the same architecture (parallel execution, dependency graph, colored output). Create `scripts/check.sh` wrapper.
8. **Create `.claude/` structure**: `settings.local.json` (with sensible permissions, no MCP servers yet),
   `rules/docs-maintenance.md`, `rules/git-conventions.md`. Copy/adapt the `plan.md` and `execute.md` commands.
9. **Create `docs/` structure**: `architecture.md` (lean, will grow), `style-guide.md` (reference Cmdr's, adapted),
   `design-principles.md` (adapted for an image viewer: speed, simplicity, instant response, respect resources).
10. **GitHub Actions CI** (`.github/workflows/ci.yml`): Set up the CI workflow early. Initially covers only the Go
    scripts checks (gofmt, go-vet, staticcheck, etc.). Rust and website jobs are added as stubs that get fleshed out
    when those milestones land. Mirror Cmdr's structure: paths-filter for change detection, pinned action SHAs,
    `CI OK` summary job for branch protection. Include both an **Ubuntu runner** (for Rust compilation, Go checks) and
    a **macOS runner** (for `cargo build` to catch Metal/macOS-specific issues with winit/wgpu). **Renovate config**:
    `renovate.json` at repo root.
11. **Create the GitHub repo**: `gh repo create vdavid/prvw --private --source .` and push the initial commit.

### Milestone 2: Rust desktop app (core)

**Intention**: get a working image viewer that opens a file, renders it in a window with GPU acceleration, and handles
basic interactions. This is the core product. Optimize for correctness and performance, not features.

Steps:

1. **Create `apps/desktop/` with Cargo project**: `apps/desktop/Cargo.toml`. Use `rust-toolchain.toml` at repo root
   (Rust stable, channel = "stable", no pinned version, so Rustup picks the latest). Key dependencies:
   - `winit` 0.30 (windowing + events, `ApplicationHandler` trait)
   - `wgpu` 29.0 (GPU rendering)
   - `pollster` (block on wgpu async adapter/device requests)
   - `muda` 0.17 (native menus)
   - `image` 0.25 (image decoding)
   - `log` + `env_logger` (logging)
   - `clap` (CLI arg parsing)
2. **App entry point** (`src/main.rs`): Parse CLI args (file path), init logging, create event loop. The app struct
   implements `winit::application::ApplicationHandler`. The event loop calls into the app struct's methods. Clean,
   readable, well-structured.
3. **Window management** (`src/window.rs`): Create a resizable window in `resumed()` (not at startup, this is required by
   winit 0.30). Support toggling between windowed and fullscreen (F11 or Cmd+F on macOS). Handle window resize events
   (re-render at new size). Handle close event (actually quit the process, don't just hide). Set window title to
   filename.
4. **GPU renderer** (`src/renderer.rs`): Init `wgpu` surface in `resumed()` (after window creation). Create device and
   queue via `pollster::block_on()`. Create a WGSL shader that renders a textured quad. The image is uploaded as a GPU
   texture. Zoom and pan are a 2D transform applied to the quad via a uniform buffer. Render loop re-renders on window
   events (resize, zoom, pan), not continuously (save CPU/GPU when idle).
5. **Image loading** (`src/image_loader.rs`): Load image from file path using the `image` crate. Decode to RGBA8. Upload
   to GPU texture. Handle errors gracefully (show the error in the window title bar, not as a text overlay, because text
   rendering in pure wgpu needs a library like `glyphon` and that's overkill for v1). Support common formats: JPEG, PNG,
   GIF (first frame), WebP, BMP, TIFF.
6. **Zoom and pan** (`src/view.rs`): Track current zoom level and pan offset. Scroll wheel zooms (centered on cursor
   position). Click-drag pans. Keyboard: `+`/`-` or `Cmd+=`/`Cmd+-` to zoom, arrow keys to pan (when navigation is added
   in M3, arrow keys switch to prev/next and pan moves to Shift+arrows). Double-click to toggle fit-to-window vs 100%.
   `0` to reset to fit-to-window. Zoom is smooth and immediate (GPU transform only, no re-decode).
7. **Native menus** (`src/menu.rs`): Use `muda` to create a menu bar. Menus: File (Close: Cmd+W, Quit: Cmd+Q), View
   (Zoom In, Zoom Out, Actual Size, Fit to Window, Fullscreen), Navigate (Previous: Left, Next: Right). Wire menu
   events to app actions.
8. **App lifecycle**: On macOS, Cmd+Q quits. Closing the window quits (don't keep the process running yet, daemon mode
   is a future feature). ESC in fullscreen exits fullscreen. ESC in windowed mode closes the app.
9. **Code signing**: A shell script (`scripts/build-and-sign.sh`) that runs `cargo build --release`, then `codesign`
   with the Developer ID cert, then creates a `.dmg` or `.app` bundle. Bundle identifier: `com.veszelovszki.prvw`.
   Create `Entitlements.plist` (minimal: hardened runtime, no webview entitlements needed). Document the build and sign
   process in `docs/guides/building.md`.
10. **CLAUDE.md** for the desktop app: document the winit 0.30 `ApplicationHandler` pattern, the wgpu surface lifecycle
    (create in `resumed()`, drop in `suspended()`), the render-on-demand approach, and any gotchas discovered.

### Milestone 3: navigation and preloading

**Intention**: this is the feature that makes Prvw feel magical. When the user presses Left/Right, the next image appears
instantly because it's already decoded and uploaded to the GPU. This is the ACDSee killer feature.

Steps:

1. **Directory scanner** (`src/directory.rs`): Given a file path, scan its parent directory for image files (filter by
   extension: jpg, jpeg, png, gif, webp, bmp, tiff). Sort alphabetically (case-insensitive). Track current position in
   the list.
2. **Preloader** (`src/preloader.rs`): Background `std::thread` (not tokio) that keeps N images ahead and N behind the
   current position decoded in memory (as RGBA8 buffers). N=2 is a good default. Communication via
   `std::sync::mpsc::channel` (or `crossbeam-channel` if ordering gets complex). When the user navigates, the preloader
   shifts its window. Decoded images are held in a bounded cache (LRU eviction). Max memory budget: configurable, default
   512 MB. Each image's memory cost is `width * height * 4` bytes.
3. **Navigation integration**: Left/Right arrow keys (and menu items) navigate to prev/next. If the image is preloaded,
   display is instant (upload texture to GPU, render). If not preloaded (user jumped far), show the image as soon as
   it's decoded, with the filename in the title bar as the loading indicator. Update window title on navigation.
4. **Edge cases**: Handle reaching the start/end of the directory (no-op or wrap, configurable later). Handle files that
   fail to decode (skip them in navigation, log a warning). Handle the directory changing while viewing (don't
   live-watch for v1, but don't crash either).

### Milestone 4: website (getprvw.com)

**Intention**: a clean, simple landing page that explains what Prvw is and offers a download. Sibling of getcmdr.com in
structure and quality, but with its own blue personality.

Steps:

1. **Create `apps/website/`**: Astro + Tailwind v4, mirroring Cmdr's website structure. Same dev/build tooling.
   `package.json` with `@prvw/website` name. Astro config with `site: 'https://getprvw.com'`. Port number: use a random
   high port (for example, 14829).
2. **Color palette** in `global.css`:
   - **Dark mode** (default/landing): Background `#0f1419` (deep dark blue-gray). Surface `#151c24`. Text primary
     `#f0f4f8`. Text secondary `#8899aa`. Accent `#4da6ff` (happy sky blue). Accent hover `#6bb8ff`. Accent glow
     `rgba(77, 166, 255, 0.35)`. Accent contrast (text on buttons) `#0f1419`.
   - **Light mode** (sub-pages): Background `#f8fafb`. Surface `#eef2f5`. Text primary `#1a2433`. Text secondary
     `#5c6b7a`. Same accent blue.
   - **Sub-accent**: `#ffc206` (Cmdr's mustard yellow, used sparingly for small highlights, stars, or callouts).
3. **Layout and pages**: `Layout.astro` (base, OG tags, theme-color), `index.astro` (landing page with hero, features,
   pricing, download CTA). Simple and focused. No blog for v1.
4. **Hero section**: Tagline that captures the ACDSee nostalgia. Something like "See your photos. Instantly." Subtitle
   about speed and simplicity. Download button. A screenshot or simple illustration of the app (placeholder image for
   now).
5. **Pricing section**: Free for personal use. $29/year per user for commercial use. No in-app licensing enforcement for
   v1 (that can wait), but show the pricing on the website. Keep it honest and friendly, same vibe as Cmdr's pricing.
6. **Footer**: minimal, links to GitHub, Cmdr, David's site.
7. **Light/dark mode support**: same dual-selector pattern as Cmdr's website (media query + data-theme attribute).
8. **Self-hosted Inter font**: same setup as Cmdr's website.

### Milestone 5: final polish and verification

**Intention**: verify everything works end-to-end, polish docs, make the whole thing something David would be proud to
show. CI is already running from Milestone 1, so this is about expanding CI coverage and final checks.

Steps:

1. **Expand CI**: Flesh out the Rust and website CI jobs that were stubbed in Milestone 1. Add the macOS runner for
   `cargo build`. Note: wgpu renderer tests can't run on headless CI (no GPU). Unit tests for zoom math, directory
   scanning, and cache logic run fine. Mark any GPU-dependent tests with `#[ignore]` and a comment explaining why.
2. **Verify the full build works**: `cargo build --release` in `apps/desktop/`, `pnpm build` in `apps/website/`,
   `./scripts/check.sh` passes green.
3. **Final doc review**: re-read all docs, make sure they're in David's style (no em dashes, sentence case, casual,
   concise). Review AGENTS.md, README, architecture.md, CLAUDE.md files.

## Testing strategy

- **Rust unit tests**: test image loading with sample images (embed small test images in `tests/fixtures/`). Test
  directory scanning. Test preloader cache eviction. Test zoom/pan math. GPU-dependent tests (renderer) are `#[ignore]`
  for CI (no GPU on headless runners). Can run locally with `cargo test -- --ignored`.
- **Website**: Astro build succeeds. Prettier/ESLint pass. Optionally Playwright E2E for basic page load (can add later).
- **Go check runner**: port Cmdr's existing Go tests for the check runner framework.
- **Manual testing**: open various image formats, zoom/pan, navigate prev/next, fullscreen toggle, menu items work.

## Someday/maybe features (captured in README)

- GPU-accelerated image pipeline (compute shaders for decode)
- EXIF-aware auto-rotation
- ICC color management
- IPC daemon mode (Cmdr fast path)
- 90/180 degree manual rotation
- "Save as smaller JPEG" export
- Slideshow mode
- Image metadata overlay (EXIF, dimensions, file size)
- Thumbnail strip at the bottom
- Cross-platform: Linux, Windows

## File structure (target state)

```
prvw/
  .github/workflows/ci.yml
  .claude/
    commands/plan.md, execute.md
    rules/docs-maintenance.md, git-conventions.md
    settings.local.json
  apps/
    desktop/
      Cargo.toml
      Entitlements.plist
      src/
        main.rs
        window.rs
        renderer.rs
        image_loader.rs
        view.rs
        menu.rs
        directory.rs
        preloader.rs
        shader.wgsl
      tests/
        fixtures/  (small test images)
      CLAUDE.md
    website/
      package.json
      astro.config.mjs
      tsconfig.json
      src/
        layouts/Layout.astro
        pages/index.astro
        styles/global.css
        components/Header.astro, Hero.astro, Footer.astro
      public/
        fonts/inter-latin-variable.woff2
        favicon.ico, favicon.png
      CLAUDE.md
  scripts/
    check/
      main.go, runner.go, colors.go, utils.go, stats.go
      checks/common.go, registry.go, registry_test.go
      checks/desktop-rust-*.go
      checks/website-*.go
      checks/scripts-go-*.go
      go.mod, go.sum
      CLAUDE.md
    check.sh
    build-and-sign.sh
  docs/
    architecture.md
    design-principles.md
    style-guide.md
    guides/building.md
    specs/  (this file lives here)
  .gitignore
  .mise.toml
  rust-toolchain.toml
  AGENTS.md
  LICENSE
  README.md
  package.json
  pnpm-workspace.yaml
  renovate.json
```

## Parallelism notes

- Milestone 1 (repo scaffolding + initial CI) runs first since everything depends on it.
- **Milestones 2+3 and 4 (desktop app + website) can run in parallel** after milestone 1 is done. They're in different
  directories, different languages, and have no shared code. The root package.json and pnpm-workspace.yaml from
  milestone 1 must exist before the website agent starts. Milestones 2 and 3 run sequentially (3 depends on 2).
- Milestone 5 (final polish) runs last after everything else is complete.
