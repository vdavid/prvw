//! Build script: pack bundled DCP profiles into a single compressed blob.
//!
//! Reads every `.dcp` file under `build-assets/dcps/`, concatenates them,
//! and zstd-compresses the result. Alongside it writes an index file so the
//! runtime can find each profile by filename without decompressing everything.
//!
//! ## Output files
//!
//! Both land in `OUT_DIR` (Cargo's per-build scratch space):
//!
//! - `bundled_dcps.zst`  — zstd-compressed blob of all DCP bytes.
//! - `bundled_dcps.idx`  — plain-text index: one line per DCP,
//!   `<filename>\t<byte_offset>\t<byte_length>\n`.
//!
//! Both are `include_bytes!`'d by `color::dcp::bundled` at compile time.
//! Cargo re-runs this script whenever a file under `build-assets/dcps/`
//! changes, so the bundle stays in sync automatically.
//!
//! ## Compression rationale
//!
//! DCP files consist mostly of binary float arrays (HueSatMap grids) that
//! compress poorly individually with gzip (~81 % of original). Concatenating
//! all 161 DCPs before compressing lets zstd exploit cross-file repetition
//! in the float data — profiles from the same manufacturer share structure.
//! This brings the combined blob down to ~11 MB (vs. ~67 MB with gzip).
//! We use zstd level 10 to balance build speed and compression ratio.

use std::{env, fs, path::Path};

fn main() {
    // Tell Cargo to re-run this script when the DCP assets change.
    println!("cargo:rerun-if-changed=build-assets/dcps");

    let dcp_dir = Path::new("build-assets/dcps");

    // Collect all .dcp files, sorted for reproducible output.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    if dcp_dir.is_dir() {
        let mut paths: Vec<_> = fs::read_dir(dcp_dir)
            .expect("failed to read build-assets/dcps")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("dcp"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();

        for path in paths {
            let bytes = fs::read(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            entries.push((name, bytes));
        }
    }

    // Build the concatenated blob and the offset index simultaneously.
    let mut blob: Vec<u8> = Vec::new();
    let mut index_lines: Vec<String> = Vec::new();

    for (name, bytes) in &entries {
        let offset = blob.len();
        let length = bytes.len();
        blob.extend_from_slice(bytes);
        index_lines.push(format!("{name}\t{offset}\t{length}"));
    }

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let zst_path = Path::new(&out_dir).join("bundled_dcps.zst");
    let idx_path = Path::new(&out_dir).join("bundled_dcps.idx");

    // Compress with zstd level 10. Cross-file patterns in the float arrays
    // give ~11 MB from ~83 MB uncompressed — much better than gzip's ~67 MB.
    let compressed =
        zstd::bulk::compress(&blob, 10).expect("zstd compression of DCP bundle failed");

    fs::write(&zst_path, &compressed)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", zst_path.display()));

    // Write the index as plain UTF-8 text.
    let index_text = index_lines.join("\n");
    fs::write(&idx_path, index_text.as_bytes())
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", idx_path.display()));

    eprintln!(
        "build.rs: packed {} DCPs ({} bytes uncompressed → {} bytes zstd)",
        entries.len(),
        blob.len(),
        compressed.len(),
    );
}
