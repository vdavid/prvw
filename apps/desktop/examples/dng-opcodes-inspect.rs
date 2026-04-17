//! Dump a DNG file's raw `OpcodeList1` / `OpcodeList2` / `OpcodeList3` bytes,
//! parsed into `(id, version, flags, byte_count)` tuples. A quick-look tool
//! for debugging which opcodes a given camera's DNGs actually carry.
//!
//! Usage: `cargo run --example dng-opcodes-inspect -- path/to/file.dng`

use std::path::Path;
use std::sync::Arc;

use rawler::decoders::WellKnownIFD;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <dng_path>", args[0]);
        std::process::exit(1);
    }
    let path = Path::new(&args[1]);
    let bytes = std::fs::read(path).expect("couldn't read DNG");
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);
    let decoder = rawler::get_decoder(&src).expect("couldn't open DNG");
    let raw = decoder
        .raw_image(&src, &rawler::decoders::RawDecodeParams::default(), false)
        .expect("couldn't decode");
    println!(
        "Raw: {}x{}, cpp={}, photometric={:?}, cfa={:?}, active_area={:?}",
        raw.width, raw.height, raw.cpp, raw.photometric, raw.camera.cfa.name, raw.active_area,
    );

    for (name, tag) in [
        ("OpcodeList1", DngTag::OpcodeList1),
        ("OpcodeList2", DngTag::OpcodeList2),
        ("OpcodeList3", DngTag::OpcodeList3),
    ] {
        let Some(ifd) = decoder.ifd(WellKnownIFD::VirtualDngRawTags).ok().flatten() else {
            continue;
        };
        let Some(entry) = ifd.get_entry(tag) else {
            println!("{name}: (not present)");
            continue;
        };
        let bytes: Vec<u8> = match &entry.value {
            rawler::formats::tiff::Value::Byte(b) => b.clone(),
            rawler::formats::tiff::Value::Undefined(b) => b.clone(),
            other => {
                println!("{name}: unexpected TIFF type {}", other.value_type_name());
                continue;
            }
        };
        println!("{name}: {} bytes", bytes.len());
        if bytes.len() < 4 {
            continue;
        }
        let count = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        println!("  opcode count: {count}");
        let mut cursor = 4;
        for i in 0..count {
            if cursor + 16 > bytes.len() {
                println!("  (truncated)");
                break;
            }
            let id = u32::from_be_bytes([
                bytes[cursor],
                bytes[cursor + 1],
                bytes[cursor + 2],
                bytes[cursor + 3],
            ]);
            let ver = u32::from_be_bytes([
                bytes[cursor + 4],
                bytes[cursor + 5],
                bytes[cursor + 6],
                bytes[cursor + 7],
            ]);
            let flags = u32::from_be_bytes([
                bytes[cursor + 8],
                bytes[cursor + 9],
                bytes[cursor + 10],
                bytes[cursor + 11],
            ]);
            let len = u32::from_be_bytes([
                bytes[cursor + 12],
                bytes[cursor + 13],
                bytes[cursor + 14],
                bytes[cursor + 15],
            ]) as usize;
            // Dump the GainMap header fields if this is opcode 9 (GainMap).
            if id == 9 && cursor + 16 + 76 <= bytes.len() {
                let p = cursor + 16;
                let rd = |off: usize| {
                    u32::from_be_bytes([
                        bytes[p + off],
                        bytes[p + off + 1],
                        bytes[p + off + 2],
                        bytes[p + off + 3],
                    ])
                };
                let rdf = |off: usize| {
                    f64::from_be_bytes([
                        bytes[p + off],
                        bytes[p + off + 1],
                        bytes[p + off + 2],
                        bytes[p + off + 3],
                        bytes[p + off + 4],
                        bytes[p + off + 5],
                        bytes[p + off + 6],
                        bytes[p + off + 7],
                    ])
                };
                println!(
                    "      GainMap: rect=({},{})-({},{}), plane={}, planes={}, pitch=({},{}), points_vh=({},{}), spacing_vh=({:.4},{:.4}), origin_vh=({:.4},{:.4}), map_planes={}",
                    rd(0),
                    rd(4),
                    rd(8),
                    rd(12),
                    rd(16),
                    rd(20),
                    rd(24),
                    rd(28),
                    rd(32),
                    rd(36),
                    rdf(40),
                    rdf(48),
                    rdf(56),
                    rdf(64),
                    rd(72),
                );
            }
            let name = match id {
                1 => "WarpRectilinear",
                2 => "WarpFisheye",
                3 => "FixVignetteRadial",
                4 => "FixBadPixelsConstant",
                5 => "FixBadPixelsList",
                6 => "TrimBounds",
                7 => "MapPolynomial",
                8 => "MapTable",
                9 => "GainMap",
                10 => "DeltaPerRow",
                11 => "DeltaPerColumn",
                12 => "ScalePerRow",
                13 => "ScalePerColumn",
                _ => "Unknown",
            };
            let optional = flags & 0x1 != 0;
            let preview_only = flags & 0x2 != 0;
            println!(
                "  [{i}] id={id} ({name}), ver={ver}, flags={flags:#x} (optional={optional}, preview_only={preview_only}), {len} bytes"
            );
            cursor += 16 + len;
        }
    }
}
