# Prvw

![License](https://img.shields.io/badge/license-BSL--1.1-blue)

The fastest image viewer for macOS.
Inspired by ACDSee 2.41, which was an amazing piece of non-bloated software in the 1990s.

Open an image, see it instantly, zoom and pan, use arrow keys for next/prev.
In the background, there is Rust, GPU acceleration, background preloading of images, which make it the fastest image viewer imaginable on macOS.

No editing tools, no fancy anims, just a ~8 MB binary and your pics.

Built in Rust with `winit` + `wgpu` + `muda`. Native macOS menus, Metal rendering, ~19 MB binary.

**Download at [getprvw.com](https://getprvw.com).**

## What it does

### Top stuff

- **Instant display**: open an image, see it in about 600 ms.
- **Lightning fast navigation**: Use `Space`/`→`/`]` for next pic, `Backspace`/`←`/`[` for prev pic, or mouse wheel.
  The previous+next two pics are preloaded in the background, so switching is instant.
- **Zoom and pan**: pinch or scroll to zoom (depends on settings), click-drag to pan. Buttery smooth thanks to your GPU.
- **Fullscreen view**: Press `F` or `⌘F` to toggle fullscreen.
- **True colors**: The app converts the embedded ICC profiles (Adobe RGB, ProPhoto, Display P3) to your monitor's native color space.
  If you have a MacBook or fancy Mac or gaming monitor, you see the full color range instead of everything clamped to sRGB.
  
### More goodies

- **EXIF orientation**: phone photos display right-side-up automatically.
- **Transparency support**: checkerboard background for transparent PNGs (Photoshop-style, fixed in screen space).
- **Format support**: JPEG (SIMD-accelerated via `zune-jpeg`), PNG, GIF (first frame only for now for anims), WebP, BMP, TIFF.
- **Native macOS feel**: real system menus, SF Pro overlay text, transparent titlebar, Finder double-click integration.
  This is actually surprisingly tricky to integrate with the GPU-accelerated main window.
- **Auto-fit window**: Window resizes to match each image. Zoom in/out and the window follows. Toggle in View menu.
- **Display-aware color management**: Mentioned above, but just to reiterate: NOT all image viewers go all the way to
  1. read the ICC profile from the image file
  2. actually check your _active_ screen's native color space
  3. convert the image to that space with a pure Rust lib that does it in 40 ms on an M3 MacBook Pro
  4. auto-update the conversion if you move the image to a different screen
  5. even let you switch your [rendering intent](https://en.wikipedia.org/wiki/Color_management#Rendering_intent) between Relative Colorimetric and Perceptual.
  Honestly, how cool is that?
- **Keyboard support**: the app is mouse-first by nature, but you can navigate and zoom go fullscreen and quit with the keyboard. No panning by kb for now.
- **Auto-updater**: checks for updates on startup, downloads and installs in the background. (You can turn this off in settings.)

## Tech stack

Pure **Rust**: `winit` for windowing, `wgpu` for GPU rendering (Metal on macOS), `muda` for native menus, `glyphon`
for text rendering, `zune-jpeg` for fast JPEG decoding, `image` for other formats, `objc2` for AppKit integration.

An embedded MCP/HTTP server (`PRVW_QA_PORT=19447`) lets AI agents and E2E tests control the viewer programmatically.
Good for testing and debugging, but if it's also handy for your agents, use it.

## Roadmap / planned features

This is a side project, so consider these someday/maybe-s, but if you open an issue or PR, it's a good signal to me about what people would like to see. 

- Live reload: watch the file; if it gets updated on disk, refresh it immediately
- Slideshow mode
- Image metadata overlay (EXIF, dimensions, file size)
- Thumbnail view of a folder
- Thumbnail strip at the bottom
- Play videos
- Play JPEG-embedded videos
- Make it cross-platform: Linux, Windows
- Add IPC daemon mode (to open even faster from [Cmdr](https://getcmdr.com))
- Diff view in git repos
- Some very minimal editing
  - Lossless 90/180 degree manual rotation
  - "Save as smaller JPEG" export: Convert big pics to a sensible size for sending etc.

## Pricing

- **Free for now**: this is very beta so not charging for it yet.
- **Intended price later**: $29 per device, with 1 year of free updates, then $19/year for updates.

Download for free for now at [getprvw.com](https://getprvw.com).

## License

Prvw is **source-available** under the [Business Source License 1.1](LICENSE).

The source becomes [AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.html) after three years (rolling per release).
Until then, you can view, modify, and learn from the code, but not use it commercially without a license.

## Contributing

Contributions are welcome! Report issues and feature requests in the [issue tracker](https://github.com/vdavid/prvw/issues).

Legal blah blah: By submitting a contribution, you agree to license your contribution under the same terms as the project (BSL 1.1,
converting to AGPL-3.0) and grant the project owner the right to use your contribution under any commercial license
offered for this project.

## FAQ

Q: Sigh. Is this some vibe-coded soulless crap?

A: No. All user-facing parts are handcrafted by a human. And about the internals, neither you, nor I should really care
about besides the fact that they are taken care of. If you _do_ care about the internals for safety concerns or whatever,
the source code is right here. It's honestly a really nice app. Check it out!

With ❤️ by [David](https://veszelovszki.com)
