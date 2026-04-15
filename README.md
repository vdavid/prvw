# Prvw

![License](https://img.shields.io/badge/license-BSL--1.1-blue)

A fast, minimal image viewer for macOS. Inspired by ACDSee 2.41 (if you know, you know).

Open an image, see it instantly, zoom and pan with GPU acceleration, arrow keys for next/prev with background preloading.
That's it. No bloat, no editing tools, no 200 MB of Electron.

Built in Rust with `winit` + `wgpu` + `muda`. Native macOS menus, Metal rendering, ~19 MB binary.

**Download at [getprvw.com](https://getprvw.com).**

## What it does

- **Instant display**: open an image, see it immediately. No splash screen, no loading bar.
- **GPU-accelerated zoom and pan**: scroll to zoom (centered on cursor), click-drag to pan. Smooth and immediate.
- **Background preloading**: adjacent images are decoded in parallel (rayon thread pool). Arrow keys feel instant.
- **Auto-fit window**: the window resizes to match each image. Zoom in/out and the window follows. Toggle in View menu.
- **Transparency support**: checkerboard background for transparent PNGs (Photoshop-style, fixed in screen space).
- **ICC color management**: embedded ICC profiles (Adobe RGB, ProPhoto, Display P3) are automatically converted to sRGB. Your photos look as the photographer intended.
- **EXIF orientation**: phone photos display right-side-up automatically.
- **Keyboard-first**: navigate, zoom, pan, fullscreen, quit — all from the keyboard.
- **Native macOS feel**: real system menus, SF Pro overlay text, transparent titlebar, Finder double-click integration.
- **Format support**: JPEG (SIMD-accelerated via `zune-jpeg`), PNG, GIF (first frame), WebP, BMP, TIFF.
- **Settings**: auto-fit window, enlarge small images, auto-update — persisted to `settings.json`.
- **Auto-updater**: checks for updates on startup, downloads and installs in the background.

## Tech stack

Pure **Rust**: `winit` for windowing, `wgpu` for GPU rendering (Metal on macOS), `muda` for native menus, `glyphon`
for text rendering, `zune-jpeg` for fast JPEG decoding, `image` for other formats, `objc2` for AppKit integration.

An embedded MCP/HTTP server (`PRVW_QA_PORT=19447`) lets AI agents and E2E tests control the viewer programmatically.

## Pricing

- **Personal use**: free forever
- **Commercial use**: $29/year per user

Purchase at [getprvw.com](https://getprvw.com).

## Someday/maybe

- GPU-accelerated image pipeline (compute shaders for decode)
- ICC color management level 2: display-aware (source profile -> monitor profile via ColorSync)
- IPC daemon mode (instant open from [Cmdr](https://getcmdr.com))
- 90/180 degree manual rotation
- "Save as smaller JPEG" export
- Slideshow mode
- Image metadata overlay (EXIF, dimensions, file size)
- Thumbnail strip at the bottom
- Cross-platform: Linux, Windows

## License

Prvw is **source-available** under the [Business Source License 1.1](LICENSE).

### Free for personal use

Use Prvw for free on any number of machines for personal, non-commercial projects. No nags, no trial timers, no
restrictions.

### Commercial use

For work projects, you'll need a license: **$29/year per user**. Purchase at
[getprvw.com](https://getprvw.com).

### Source code

The source becomes [AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.html) after three years (rolling per release).
Until then, you can view, modify, and learn from the code, but not use it commercially without a license.

---

## Contributing

Contributions are welcome! Report issues and feature requests in the
[issue tracker](https://github.com/vdavid/prvw/issues).

By submitting a contribution, you agree to license your contribution under the same terms as the project (BSL 1.1,
converting to AGPL-3.0) and grant the project owner the right to use your contribution under any commercial license
offered for this project.

Happy viewing!

David
