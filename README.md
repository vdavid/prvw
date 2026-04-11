# Prvw

![License](https://img.shields.io/badge/license-BSL--1.1-blue)

A fast, minimal image viewer for macOS. Inspired by ACDSee 2.41 (if you know, you know).

Open an image, see it instantly, zoom and pan with GPU acceleration, arrow keys for next/prev with background preloading.
That's it. No bloat, no editing tools, no 200 MB of Electron.

Built in Rust with `winit` + `wgpu` + `muda`. Native macOS menus, Metal rendering, ~5 MB binary.

## What it does

- **Instant display**: open an image, see it immediately. No splash screen, no loading bar.
- **GPU-accelerated zoom and pan**: scroll to zoom (centered on cursor), click-drag to pan. Smooth and immediate.
- **Background preloading**: adjacent images are decoded ahead of time. Left/Right arrow keys feel instant.
- **Keyboard-first**: navigate, zoom, pan, fullscreen, quit, all from the keyboard.
- **Native macOS menus**: real system menus with proper shortcuts via `muda`.
- **Minimal chrome**: the image takes up 99% of the window. No sidebars, no toolbars, no distractions.
- **Format support**: JPEG, PNG, GIF (first frame), WebP, BMP, TIFF.

## Status

Early development. Not usable yet.

## Tech stack

Prvw is built with pure **Rust**: `winit` for windowing, `wgpu` for GPU-accelerated rendering (Metal on macOS), `muda`
for native menus, and `image` for decoding. No UI framework, no webview.

## Pricing

- **Personal use**: free forever
- **Commercial use**: $29/year per user

Purchase at [getprvw.com](https://getprvw.com).

## Someday/maybe

Things I'd love to add eventually:

- GPU-accelerated image pipeline (compute shaders for decode)
- EXIF-aware auto-rotation
- ICC color management
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
