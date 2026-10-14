# HDR fixtures

Test fixtures for HDR decoding: gain-map JPEGs, PQ/HLG PNGs, float TIFFs, and 16-bit SDR files. How the decoders use
them: `apps/desktop/src/decoding/CLAUDE.md`. Every file is listed in `licenses.md` with its source and license; don't
check in one that isn't.

## Files

- `iphone-air-gain-map-776.jpg`: a real iPhone Air camera JPEG, 776×776, 306 KB, byte-identical to the Wikimedia Commons
  upload. It carries both gain-map flavors iOS 18 and later write, on one 388×388 single-channel map appended through
  MPF: Apple's (headroom in the Apple maker note, maker tags 33 = 1.01 and 48 = 1.766, and the map tagged
  `urn:com:apple:photo:2020:aux:hdrgainmap`) and ISO 21496-1. Apple writes the ISO parameters as XMP on the map, in
  Adobe's `hdrgm` namespace under the prefix `HDRToneMap`, with ISO's field names (`BaseHeadroom`, `AlternateHeadroom`)
  and `0`/`1` booleans; there's no ISO APP2 segment. ImageIO's `CGImageSourceCopyAuxiliaryDataInfoAtIndex` reports both
  (checked 2026-10-13). It's the real-world counterpart to the synthetic fixtures, so a parser bug shared by our fixture
  writer and our reader can't hide.

Synthetic gain-map JPEGs, written by `cargo run --example make-hdr-fixtures` (its module doc says what each holds): a
64×48 SDR base with a horizontal and a vertical ramp, plus a 16×12 map, brightest top-left, appended through MPF.

- `ultra-hdr-xmp-64x48.jpg`: Ultra HDR v1 XMP, `GContainer` directory, single-channel map. Gain 0 to 2 stops, capacity 0
  to 2.
- `ultra-hdr-xmp-orientation-6.jpg`: the same, with EXIF orientation 6, so the map has to turn with the base.
- `iso-21496-64x48.jpg`: ISO 21496-1 APP2 segments, a three-channel map with per-channel parameters, and a Display P3
  base profile.
- `apple-64x48.jpg`: Apple's older flavor alone: maker-note headroom (tags 33 = 1.01, 48 = 0.5, so 2.64 stops) and a map
  tagged as Apple's HDR gain map.

Synthetic scene-linear fixtures, written by the same example: 32×16, a grey ramp across the top half and four patches
along the bottom (red, green, and blue at about reference white, then a warm highlight at the ramp's top), so a wrong
transfer, wrong primaries, or a clip all show.

- `pq-bt2020-32x16.png`: 16-bit PQ, BT.2020, `cICP` 9/16/0/1, light up to 1,000 nits, and a `cLLI` of 1,000 nits.
- `hlg-bt2020-32x16.png`: 16-bit HLG, BT.2020, `cICP` 9/18/0/1: the PQ file's display light, taken back through
  BT.2100's OOTF at its nominal 1,000 nits, so the two decode to the same light (reference white on the 75% signal, the
  1,000-nit grey on full signal). Only the warm highlight's red differs, by about 6%: it needs a little more than full
  signal, so it clips. `the_pq_and_hlg_fixtures_decode_to_the_same_light` holds them together.
- `sdr-cicp-display-p3-32x16.png`: 8-bit SDR, `cICP` 12/13/0/1 (Display P3, sRGB curve) and no ICC profile, so a reader
  that ignores the chunk reads the saturated patches as sRGB.
- `float-linear-peak-4-32x16.tif`: `Rgb32F`, linear BT.709, no ICC profile, peaking at 4.0 (four times SDR white).
- `float-linear-sdr-32x16.tif`: the same scene peaking at exactly 1.0: SDR in a float container.

## Not checked in

Larger real-world samples live outside the repo; `docs/notes/hdr-images.md` lists them under "Real files for checking by
hand": two Pixel Ultra HDR photos on David's machine (personal, never commit them) and three more iPhone JPEGs on
Wikimedia Commons (3–4 MB each).
