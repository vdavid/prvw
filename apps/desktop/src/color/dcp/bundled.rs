//! Bundled DCP collection — RawTherapee community profiles packed at build
//! time into a single zstd-compressed blob.
//!
//! ## Build-time packing (see `build.rs`)
//!
//! `build.rs` reads every `.dcp` file under `apps/desktop/build-assets/dcps/`,
//! concatenates them in sorted order, and compresses the result with zstd
//! (level 10) into `OUT_DIR/bundled_dcps.zst`. A parallel plain-text index
//! (`bundled_dcps.idx`) records each profile's filename, byte offset, and
//! length in the decompressed stream. Both files are `include_bytes!`'d at
//! compile time, adding ~11 MB to the binary (vs. ~83 MB uncompressed or
//! ~67 MB with gzip — zstd exploits cross-file float-array repetition).
//!
//! ## Runtime layout
//!
//! The index contains newline-separated `<name>\t<offset>\t<length>` tuples.
//! On first DCP lookup we decompress the whole blob into a `OnceLock<Vec<u8>>`
//! (~83 MB, kept alive for the process lifetime), then slice offset windows
//! for individual profiles. Since DCP lookups happen at most once per file
//! open, the one-time decompression cost (~20–30 ms) is imperceptible.
//!
//! ## Search order
//!
//! Callers reach this module after the filesystem tiers (user dir + Adobe
//! dir) report no exact match. Fuzzy alias matching then uses this module too,
//! trying each alias through the bundled tier.
//!
//! ## Matching
//!
//! We parse each candidate and compare its `UniqueCameraModel` against the
//! caller's normalized camera ID — same normalization as `discovery.rs`. The
//! filename is not used for matching because RT's filenames sometimes differ
//! in case or punctuation from the DCP's internal `UniqueCameraModel`.

use std::sync::OnceLock;

use super::{
    discovery::normalize,
    parser::{Dcp, parse},
};

/// The zstd-compressed blob of all bundled DCP bytes (build-time generated).
static BUNDLE_ZST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundled_dcps.zst"));

/// The index: `<filename>\t<offset>\t<length>\n` lines pointing into the
/// decompressed blob (build-time generated).
static BUNDLE_IDX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundled_dcps.idx"));

/// Lazily decompressed blob. Allocated once on first DCP lookup.
static DECOMPRESSED: OnceLock<Vec<u8>> = OnceLock::new();

fn decompressed_blob() -> &'static [u8] {
    DECOMPRESSED.get_or_init(|| {
        zstd::bulk::decompress(BUNDLE_ZST, 128 * 1024 * 1024)
            .expect("bundled DCP blob zstd decompression failed")
    })
}

/// An entry from the index file.
struct IndexEntry<'a> {
    _name: &'a str,
    offset: usize,
    length: usize,
}

fn parse_index() -> Vec<IndexEntry<'static>> {
    let text = std::str::from_utf8(BUNDLE_IDX).unwrap_or("");
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let offset: usize = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let length: usize = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        if length > 0 {
            out.push(IndexEntry {
                _name: name,
                offset,
                length,
            });
        }
    }
    out
}

/// How many bundled DCPs are in the collection.
#[allow(dead_code)] // used in unit tests; not called from production code
pub fn bundled_count() -> usize {
    parse_index().len()
}

/// Search the bundled collection for a DCP matching `camera_id`.
///
/// Decompresses the blob on first call (cached in a `OnceLock`), then
/// iterates the index and returns the first DCP whose `UniqueCameraModel`
/// normalizes to the same string as `camera_id`.
pub fn find_bundled_dcp(camera_id: &str) -> Option<Dcp> {
    let target = normalize(camera_id);
    if target.is_empty() {
        return None;
    }
    let blob = decompressed_blob();
    for entry in parse_index() {
        let end = entry.offset + entry.length;
        if end > blob.len() {
            log::warn!(
                "DCP bundle: index entry out of range at offset {} len {}",
                entry.offset,
                entry.length
            );
            continue;
        }
        let bytes = &blob[entry.offset..end];
        match parse(bytes) {
            Ok(dcp) => {
                if let Some(ref m) = dcp.unique_camera_model
                    && normalize(m) == target
                {
                    return Some(dcp);
                }
            }
            Err(e) => {
                log::debug!("DCP bundle: parse failed for a bundled entry: {e}");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_count_is_nonzero() {
        // Basic sanity: the build packed at least one DCP.
        assert!(
            bundled_count() > 0,
            "bundled DCP count should be > 0 (build.rs packs build-assets/dcps)"
        );
    }

    #[test]
    fn find_bundled_dcp_returns_known_camera() {
        // SONY ILCE-7M3 is in RT's collection.
        let dcp = find_bundled_dcp("SONY ILCE-7M3");
        assert!(
            dcp.is_some(),
            "expected a bundled DCP for 'SONY ILCE-7M3'; check build-assets/dcps/"
        );
        let dcp = dcp.unwrap();
        let model = dcp.unique_camera_model.as_deref().unwrap_or("");
        assert_eq!(
            super::normalize(model),
            super::normalize("SONY ILCE-7M3"),
            "profile's UniqueCameraModel should normalize to 'sony ilce-7m3'"
        );
    }

    #[test]
    fn find_bundled_dcp_returns_none_for_unknown_camera() {
        // A made-up camera that can't be in the collection.
        assert!(
            find_bundled_dcp("NoSuch Camera XZY-99999").is_none(),
            "should return None for an unknown camera"
        );
    }

    #[test]
    fn bundled_count_matches_build_assets() {
        // The collection ships 161 DCPs from RawTherapee.
        let count = bundled_count();
        assert!(
            count >= 100,
            "expected at least 100 bundled DCPs, got {count}; is build-assets/dcps/ populated?"
        );
    }
}
