# RAW fixtures

Real camera RAW files don't live in the repo. They're large, they carry varied licensing,
and checking them in would bloat clones for everyone. Put your local samples here and run
the ignored tests against them.

## Where to put files

Drop your RAW files into this directory (`apps/desktop/tests/fixtures/raw/`). The tests
currently hardcode two paths under `/tmp/raw/`:

- `/tmp/raw/sample1.arw`: a Sony ARW at 5456 × 3632, EXIF orientation 1
- `/tmp/raw/sample2.dng`: a DNG that rawler decodes as 3990 × 3000 with EXIF
  orientation 6 or 8 (dimensions swap after `apply_orientation`)

Easiest path: symlink the two paths into this folder so the tests find them.

```sh
mkdir -p /tmp/raw
ln -s "$PWD/apps/desktop/tests/fixtures/raw/sample1.arw" /tmp/raw/sample1.arw
ln -s "$PWD/apps/desktop/tests/fixtures/raw/sample2.dng" /tmp/raw/sample2.dng
```

If you want to test with different fixtures, point the symlinks at those files and adjust
the expected dimensions in the test assertions as needed.

## Running the ignored tests

```sh
cd apps/desktop
cargo test -- --ignored                                         # all ignored
cargo test decoding::raw::tests -- --ignored                    # raw module only
cargo test decoding::tests::arw_end_to_end -- --ignored         # one by name
```

## Where to get free RAW samples

- [Signature Edits free RAW photos](https://www.signatureedits.com/free-raw-photos/): a
  curated set across several brands, usable without attribution strings attached.
- [raw.pixls.us](https://raw.pixls.us/): wide coverage across camera models, CC-licensed
  files searchable by manufacturer.
- [Nikon sample images](https://imaging.nikon.com/support/index.htm#learn): NEF samples
  direct from Nikon.

Grab one file per format you care about, rename it to match the expected fixture name (or
update the test to match yours), and run the tests.
