# RAW fixture licenses and provenance

Every RAW file in this directory is listed here with its source, license, and any
attribution required. Don't check in a fixture that isn't accounted for below.

## `synthetic-bayer-128.dng`

- **Source**: generated at development time via `rawler::dng::writer::DngWriter`.
  The Rust code that produced it is the minimal example in
  `apps/desktop/tests/fixtures/raw/README.md` and in this project's git history.
- **Dimensions**: 128 × 128 pixels, uncompressed Bayer RGGB, 16-bit.
- **Size**: ~33 KB.
- **License**: 0BSD (public domain equivalent). The file is a pure function of
  our own source code — a gradient pattern written into a standard Adobe DNG
  container. No third-party content.
- **Attribution**: not required.

## Why only one fixture?

A wider search for a small (≤ 5 MB), license-clean vendor RAW (ARW, NEF, CR2,
and so on) didn't turn up usable candidates. The smallest free-for-testing
vendor RAW we found was 10 MB (f-spot/raw-samples Leica M8 DNG, CC-licensed).
Signature Edits, pixls.us, and manufacturer sample galleries all ship full-res
files of 15 MB and up. For a regression test suite, we'd rather keep the repo
small and lean on a deterministic synthetic DNG.

Phase 2.x can add a vendor RAW when we either find a small one or spend time
cropping one down. See `docs/notes/raw-support-phase2.md` for the rationale.
