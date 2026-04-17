# Third-party DCP profiles

Prvw bundles a collection of community-contributed DCP (DNG Color Profile) files
from the RawTherapee project. This page documents their provenance, attribution,
and how they're redistributed.

## RawTherapee DCP collection

**Source**: https://github.com/Beep6581/RawTherapee/tree/dev/rtdata/dcpprofiles

**Count** (as of 2026-04-17): 161 profiles

**RawTherapee project license**: GPL v3. The DCP files themselves are
community contributions whose individual authors hold the copyright.

### Principal contributors

- **Maciej Dworak** — author of the majority of profiles in the collection,
  covering Canon, Fujifilm, Nikon, Olympus, Panasonic, Pentax, Ricoh, and Sony.
  Copyright strings appear in the embedded `ProfileCopyright` tag of his profiles
  as "Copyright Maciej Dworak".
- **Lawrence Lee** — Canon EOS R8 and several other Canon / Fujifilm profiles.
- **Alberto Griggio** — Panasonic DC-S5 Mark II and others.
- **Thanatomanic** — Fujifilm DBP for GX680.
- **Morgan Hardwood** — Sony ILCE-7M3 and others.
- **Other RawTherapee contributors** — see the git history at
  https://github.com/Beep6581/RawTherapee/commits/dev/rtdata/dcpprofiles

The source RAW files (color-target shots) used to generate many of these profiles
were provided by photographers under the CC0 license
(https://creativecommons.org/publicdomain/zero/1.0/).

### Redistribution

These DCP files are redistributed within Prvw consistent with RawTherapee's own
practice of distributing them as part of its open-source application. The profiles
have no explicit stand-alone license file in RT's repository; they are bundled
under the same community-open spirit as the project.

The full attribution text is in `apps/desktop/build-assets/dcps/LICENSE`.

If you are a DCP author and have concerns about redistribution, please file an
issue at https://github.com/vdavid/prvw.

### How they're bundled

At build time, `apps/desktop/build.rs` concatenates all 161 `.dcp` files in sorted
order and compresses the result with zstd (level 10) into a `bundled_dcps.zst`
blob (~11 MB) plus a `bundled_dcps.idx` plain-text offset index. Both are
`include_bytes!`'d into the binary at compile time. Runtime loading is via
`color::dcp::bundled::find_bundled_dcp`.

### Re-syncing the collection

Run `./scripts/sync-bundled-dcps.sh` to re-download the full collection from
RawTherapee's `dev` branch. The script is idempotent (skips files already present)
unless called with `--all`.
