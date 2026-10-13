# HDR fixtures

Test fixtures for HDR decoding: gain-map JPEGs, PQ/HLG PNGs, float TIFFs, and 16-bit SDR files. The plan that uses them
is `docs/specs/hdr-images.md`. Every file is listed in `licenses.md` with its source and license; don't check in one
that isn't.

## Files

- `iphone-air-gain-map-776.jpg`: a real iPhone Air camera JPEG, 776×776, 306 KB, byte-identical to the Wikimedia Commons
  upload. It carries both gain-map flavors iOS 18 and later write: Apple's (headroom in the Apple maker note, maker tags
  33 = 1.01 and 48 = 1.766, and a 388×388 map tagged `urn:com:apple:photo:2020:aux:hdrgainmap`) and ISO 21496-1 (a
  388×388 map with ISO metadata). Both were confirmed with ImageIO's `CGImageSourceCopyAuxiliaryDataInfoAtIndex` on
  2026-10-13. It's the real-world counterpart to the synthetic fixtures, so a parser bug shared by our fixture writer
  and our reader can't hide.

## Not checked in

Larger real-world samples live outside the repo; the spec lists them under "Fixtures": two Pixel Ultra HDR photos on
David's machine (personal, never commit them) and three more iPhone JPEGs on Wikimedia Commons (3–4 MB each).
