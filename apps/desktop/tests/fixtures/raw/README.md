# RAW fixtures

Test fixtures for the RAW decoding pipeline. Every fixture is small (tens of KB),
deterministic, and license-clean. See `licenses.md` for provenance.

## Files

| File                                   | Purpose                                                                |
| -------------------------------------- | ---------------------------------------------------------------------- |
| `synthetic-bayer-128.dng`              | 128×128 uncompressed Bayer RGGB DNG, gradient pattern. Decode-path fixture. |
| `synthetic-bayer-128.golden.png`       | Expected RGB output after `load_image` (ICC, orientation, ICC transform). |

## How the tests use these

`src/decoding/mod.rs` ships a `synthetic_dng_matches_golden` test that runs the
full `load_image` path on the DNG, converts the RGBA8 output to RGB8, and diffs
it against the golden PNG via CIE76 Delta-E. Thresholds are mean < 0.5 and
max < 3.0. Any real pipeline drift trips the assertion.

The test runs unconditionally on macOS (no `#[ignore]`). Cross-platform CI
skips it via the `target_os = "macos"` gate because the decode path depends on
macOS's system sRGB profile.

## Regenerating goldens after an intentional output change

Phase 2+ will deliberately change the pipeline output (wide-gamut, exposure,
tone curve, sharpening). When it does, the assertion fires. To accept the new
output as the new baseline:

```sh
cd apps/desktop
PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden
```

Check the regenerated PNG in with a commit message that references the phase
responsible (for example, "RAW: wide-gamut pipeline, update golden").

## Adding a new fixture

1. Drop the file here.
2. Add an entry to `licenses.md` with source URL, license, and dimensions.
3. Add a test in `src/decoding/` that follows the `synthetic_dng_matches_golden`
   pattern (load it, compare to a golden PNG, tight Delta-E threshold).
4. Generate the golden with `PRVW_UPDATE_GOLDENS=1 cargo test <new_test>`.

## Generating `synthetic-bayer-128.dng` from scratch

If the file ever needs regenerating (format changes in rawler's DNG writer, for
example), here's the minimal Rust program:

```rust
use std::{fs::File, io::BufWriter, path::Path};
use rawler::{
    CFA, RawImage,
    decoders::Camera,
    dng::{CropMode, DNG_VERSION_V1_6, DngCompression, DngPhotometricConversion, writer::DngWriter},
    imgop::xyz::Illuminant,
    pixarray::PixU16,
    rawimage::{BlackLevel, CFAConfig, RawPhotometricInterpretation, WhiteLevel},
};

fn main() {
    let out = Path::new("synthetic-bayer-128.dng");
    let f = File::create(out).unwrap();
    let mut buf = BufWriter::new(f);
    let mut dng = DngWriter::new(&mut buf, DNG_VERSION_V1_6).unwrap();

    let (w, h) = (128usize, 128usize);
    let mut data: Vec<u16> = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            let is_red = (x % 2 == 0) && (y % 2 == 0);
            let is_green = (x + y) % 2 == 1;
            let is_blue = (x % 2 == 1) && (y % 2 == 1);
            let gradient = ((x + y) as u32 * 65535 / (w + h) as u32) as u16;
            data[y * w + x] = if is_red { gradient }
                else if is_green { gradient / 2 }
                else if is_blue { gradient / 4 }
                else { 0 };
        }
    }
    let px = PixU16::new_with(data, w, h);

    let mut cam = Camera::new();
    cam.cfa = CFA::new("RGGB");
    cam.make = "Synthetic".into();
    cam.model = "TestCam".into();
    cam.clean_make = cam.make.clone();
    cam.clean_model = cam.model.clone();
    let mut matrix = std::collections::HashMap::new();
    matrix.insert(
        Illuminant::D65,
        vec![0.6097, 0.0, 0.3203, -0.2181, 1.2359, -0.0178, -0.0504, 0.1629, 0.8875],
    );
    cam.color_matrix = matrix;

    let wb = [2.0f32, 1.0, 1.5, f32::NAN];
    let bl = Some(BlackLevel::new(&[0u32], 1, 1, 1));
    let wl = Some(WhiteLevel::new_bits(16, 1));
    let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new_from_camera(&cam));

    let raw = RawImage::new(cam, px, 1, wb, photometric, bl, wl, false);
    let mut frame = dng.subframe(0);
    frame
        .raw_image(&raw, CropMode::None, DngCompression::Uncompressed,
                   DngPhotometricConversion::Original, 1)
        .unwrap();
    frame.finalize().unwrap();
    dng.close().unwrap();
}
```

After regenerating the DNG, rerun the golden update command above to refresh
`synthetic-bayer-128.golden.png`.

## Inspecting pipeline output by hand

The `raw-dev-dump` example dumps per-stage PNGs for any RAW file:

```sh
cd apps/desktop
cargo run --example raw-dev-dump -- tests/fixtures/raw/synthetic-bayer-128.dng
cargo run --example raw-dev-dump -- /path/to/any.arw --out-dir /tmp/arw-dump
```

Phase 1 has two stages: `post-rawler.png` (rawler's develop output, what the
app currently ships) and `final.png` (same as `post-rawler` in Phase 1). Phase
2+ grows this list: `linear-widegamut.png`, `post-exposure.png`,
`post-tone.png`, `post-sharpen.png`.
