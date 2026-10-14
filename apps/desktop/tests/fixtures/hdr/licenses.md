# HDR fixture licenses and provenance

Every file in this directory is listed here with its source, license, and the attribution it requires.

## `iphone-air-gain-map-776.jpg`

- **Source**: "IPhone 16 Pro Super Retina XDR OLED Display (100X Mag.).jpg" by ConformationXII, Wikimedia Commons,
  https://commons.wikimedia.org/wiki/File:IPhone_16_Pro_Super_Retina_XDR_OLED_Display_(100X_Mag.).jpg
- **Camera**: Apple iPhone Air (EXIF `Model`), taken 2026-08-15. The title names the phone in the picture.
- **Downloaded**: 2026-10-13. SHA-1 `4b12aa8969cb8bd244a448836c4266fd0b8ecfcc`, matching the SHA-1 Commons reports for
  the upload.
- **Changes**: none. Renamed only.
- **License**: Creative Commons Attribution-ShareAlike 4.0 International (CC BY-SA 4.0),
  https://creativecommons.org/licenses/by-sa/4.0/
- **Attribution**: this entry. Any modified version of the image (a crop, a re-encode) has to stay under CC BY-SA 4.0
  and say what changed. The license covers this file only, not the code that reads it.

## `ultra-hdr-xmp-64x48.jpg`, `ultra-hdr-xmp-orientation-6.jpg`, `iso-21496-64x48.jpg`, `apple-64x48.jpg`

- **Source**: generated for this repo by `apps/desktop/examples/make-hdr-fixtures.rs` from synthetic ramps. No
  third-party content; the Display P3 profile in `iso-21496-64x48.jpg` is the one `moxcms` builds from the published
  primaries.
- **License**: the repo's own, like the code that writes them.

## `pq-bt2020-32x16.png`, `hlg-bt2020-32x16.png`, `sdr-cicp-display-p3-32x16.png`, `float-linear-peak-4-32x16.tif`, `float-linear-sdr-32x16.tif`

- **Source**: generated for this repo by `apps/desktop/examples/make-hdr-fixtures.rs` from a synthetic ramp and flat
  patches. No third-party content.
- **License**: the repo's own, like the code that writes them.
