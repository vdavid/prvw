# Camera RAW support: Phase 1

Decision date: 2026-04-17. Decided: **rawler alone, using its built-in develop pipeline**.

Licensing handled separately. See the project root.

## What we did

Added a RAW decode path to `apps/desktop/src/decoding/` built on the `rawler` crate. The
backend handles 10 formats out of the gate: DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF,
and SRW. Prvw reads the file, hands the bytes to rawler's `RawDevelop` pipeline, pulls the
developed RGB image back out, wraps it to RGBA8, and runs the existing ICC transform so
the pixels land in the display's color space like every other format.

## Options considered

| Option | Verdict |
| --- | --- |
| **Apple Image I/O** via `CGImageSource` | Rejected. macOS-only. Prvw is macOS-first today, but cross-platform is on the roadmap and tying the RAW path to CoreGraphics would make the port harder. |
| **rawler alone** (chosen) | Pure Rust, cross-platform, actively maintained, and ships its own develop pipeline. No extra crate needed for the demosaic/white-balance/gamma steps. |
| **rawler + imagepipe** | Rejected. `imagepipe` is four years stale and LGPL-3.0. rawler 0.7 already covers the baseline develop pipeline we need. |
| **rsraw** (LibRaw FFI) | Rejected for Phase 1. Adds a C++ toolchain to the build and complicates macOS codesigning. Keep it in mind as a fallback if rawler gaps ever matter. |

## Why rawler is enough for a viewer

rawler 0.7.x exposes a `RawDevelop` pipeline that does the work a viewer needs in one pass:

- Black and white level correction
- PPG demosaic for Bayer sensors, bilinear X-Trans for Fujifilm
- Camera white balance
- Bradford chromatic adaptation to D65
- Camera color matrix applied to get sRGB primaries
- sRGB gamma encoding

The pipeline is parallelised via `rayon` and ships SIMD variants via `multiversion`. Output
quality is correct per the camera matrix. Not as refined as Adobe Camera Raw in terms of
look, but honest, fast, and plenty for a viewer.

## Known Phase 1 limitations

These are the "didn't do, for good reasons" items:

- **DNG OpcodeList 1/2/3 not applied.** iPhone ProRAW uses these for gain-map shading
  correction. Without them, corners may look a touch different from Apple Photos on the
  same file. Planned for Phase 2.
- **DNG LinearizationTable not re-applied explicitly.** rawler's per-decoder decompression
  covers most cases. Nikon NEF is the one worth spot-checking once we have real fixtures.
- **No DCP profile application.** Colors are correct for the camera matrix but won't match
  Lightroom's camera-signature looks (Adobe Standard, Camera Neutral, and so on).
- **Bilinear X-Trans demosaic.** Good enough for viewing RAF files. Dedicated RAW editors
  use Markesteijn or similar. A viewer doesn't need the extra complexity.
- **No lens corrections and no noise reduction.** These live in editor territory and stay
  out of scope for a viewer.

## Benchmarks

From the proof-of-concept run in `/tmp/raw-poc`, release build on an M3 Max:

| Fixture | Pixels | Develop | Total (file read to RGB8 PNG write) |
| --- | --- | --- | --- |
| Sony ARW (full-frame) | 20 MP | ~170 ms | ~386 ms |
| Pixel 6 Pro DNG | 12 MP | ~74 ms | ~188 ms |

For reference, Apple Image I/O on the same machine lands in the same ballpark, sometimes a
bit faster thanks to Accelerate.framework SIMD. rawler is competitive, and crucially,
portable.

## Phase 2 and beyond

Short list of what would move the needle next:

- DNG OpcodeList1 to make iPhone ProRAW pixel-perfect against Apple Photos.
- DCP profile application for camera-signature color looks.
- Embedded-JPEG preview fast path: decode the baked-in JPEG for instant first paint, then
  swap in the developed RAW when it lands. Turns a ~200 ms cold open into a ~30 ms one.
