# HDR fixtures

Test fixtures for HDR decoding: gain-map JPEGs, PQ/HLG PNGs, float TIFFs, and 16-bit SDR files. The plan that uses them
is `docs/specs/hdr-images.md`. Every file is listed in `licenses.md` with its source and license; don't check in one
that isn't.

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

## Not checked in

Larger real-world samples live outside the repo; the spec lists them under "Fixtures": two Pixel Ultra HDR photos on
David's machine (personal, never commit them) and three more iPhone JPEGs on Wikimedia Commons (3–4 MB each).
