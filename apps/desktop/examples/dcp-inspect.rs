//! Inspect a DCP file's contents.
//!
//! Dumps the parsed fields (name, copyright, camera model, hue/sat map
//! dimensions, first few LUT entries) so we can sanity-check the parser
//! against third-party DCPs without running the full pipeline.
//!
//! ```sh
//! cd apps/desktop
//! cargo run --example dcp-inspect -- /path/to/profile.dcp
//! ```
//!
//! This example re-implements the DCP parser inline, keeping it as a
//! standalone binary per the project's example convention (the desktop
//! crate is a binary, not a library). Keep the constants and layout in
//! sync with `src/color/dcp/parser.rs`.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Dump a DCP's parsed fields")]
struct Args {
    dcp: PathBuf,
}

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 50932;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 50937;
const TAG_PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
const TAG_PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50939;
const TAG_PROFILE_COPYRIGHT: u16 = 50942;
const TAG_PROFILE_HUE_SAT_MAP_ENCODING: u16 = 51107;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let bytes = std::fs::read(&args.dcp)?;
    if &bytes[0..4] != b"IIRC" {
        return Err("not a DCP (missing IIRC magic)".into());
    }
    let ifd_offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let entry_count = u16::from_le_bytes([bytes[ifd_offset], bytes[ifd_offset + 1]]) as usize;

    println!("File     : {}", args.dcp.display());
    println!("IFD at   : {ifd_offset:#x}, {entry_count} entries");

    for i in 0..entry_count {
        let eo = ifd_offset + 2 + i * 12;
        let tag = u16::from_le_bytes([bytes[eo], bytes[eo + 1]]);
        let typ = u16::from_le_bytes([bytes[eo + 2], bytes[eo + 3]]);
        let count = u32::from_le_bytes([bytes[eo + 4], bytes[eo + 5], bytes[eo + 6], bytes[eo + 7]])
            as usize;
        let vo = u32::from_le_bytes([bytes[eo + 8], bytes[eo + 9], bytes[eo + 10], bytes[eo + 11]])
            as usize;
        let name = tag_name(tag);
        match tag {
            TAG_UNIQUE_CAMERA_MODEL
            | TAG_PROFILE_NAME
            | TAG_PROFILE_COPYRIGHT
            | TAG_PROFILE_CALIBRATION_SIGNATURE
                if typ == 2 =>
            {
                let slice = if count <= 4 {
                    &bytes[eo + 8..eo + 8 + count]
                } else {
                    &bytes[vo..vo + count]
                };
                let s = String::from_utf8_lossy(slice.split(|b| *b == 0).next().unwrap_or(slice));
                println!("  {name}: {s}");
            }
            TAG_CALIBRATION_ILLUMINANT_1 | TAG_CALIBRATION_ILLUMINANT_2
                if typ == 3 && count == 1 =>
            {
                let v = u16::from_le_bytes([bytes[eo + 8], bytes[eo + 9]]);
                println!("  {name}: {v} ({})", illuminant_name(v));
            }
            TAG_PROFILE_HUE_SAT_MAP_DIMS if typ == 4 && count == 3 => {
                let h =
                    u32::from_le_bytes([bytes[vo], bytes[vo + 1], bytes[vo + 2], bytes[vo + 3]]);
                let s = u32::from_le_bytes([
                    bytes[vo + 4],
                    bytes[vo + 5],
                    bytes[vo + 6],
                    bytes[vo + 7],
                ]);
                let v = u32::from_le_bytes([
                    bytes[vo + 8],
                    bytes[vo + 9],
                    bytes[vo + 10],
                    bytes[vo + 11],
                ]);
                println!("  {name}: {h} x {s} x {v}");
            }
            TAG_PROFILE_HUE_SAT_MAP_DATA_1 | TAG_PROFILE_HUE_SAT_MAP_DATA_2 if typ == 11 => {
                println!("  {name}: {count} floats at {vo:#x} (first 3 entries)");
                for j in 0..3.min(count / 3) {
                    let ho = vo + j * 12;
                    let hue = f32::from_le_bytes([
                        bytes[ho],
                        bytes[ho + 1],
                        bytes[ho + 2],
                        bytes[ho + 3],
                    ]);
                    let sat = f32::from_le_bytes([
                        bytes[ho + 4],
                        bytes[ho + 5],
                        bytes[ho + 6],
                        bytes[ho + 7],
                    ]);
                    let val = f32::from_le_bytes([
                        bytes[ho + 8],
                        bytes[ho + 9],
                        bytes[ho + 10],
                        bytes[ho + 11],
                    ]);
                    println!("    ({hue:+.3}, {sat:.3}, {val:.3})");
                }
            }
            TAG_PROFILE_HUE_SAT_MAP_ENCODING if typ == 4 && count == 1 => {
                let v = u32::from_le_bytes([
                    bytes[eo + 8],
                    bytes[eo + 9],
                    bytes[eo + 10],
                    bytes[eo + 11],
                ]);
                println!(
                    "  {name}: {v} ({})",
                    if v == 0 { "linear" } else { "sRGB-gamma" }
                );
            }
            _ => println!("  tag {tag:#06x} ({name}) type={typ} count={count}"),
        }
    }
    Ok(())
}

fn tag_name(tag: u16) -> &'static str {
    match tag {
        TAG_UNIQUE_CAMERA_MODEL => "UniqueCameraModel",
        TAG_CALIBRATION_ILLUMINANT_1 => "CalibrationIlluminant1",
        TAG_CALIBRATION_ILLUMINANT_2 => "CalibrationIlluminant2",
        TAG_PROFILE_CALIBRATION_SIGNATURE => "ProfileCalibrationSignature",
        TAG_PROFILE_NAME => "ProfileName",
        TAG_PROFILE_HUE_SAT_MAP_DIMS => "ProfileHueSatMapDims",
        TAG_PROFILE_HUE_SAT_MAP_DATA_1 => "ProfileHueSatMapData1",
        TAG_PROFILE_HUE_SAT_MAP_DATA_2 => "ProfileHueSatMapData2",
        TAG_PROFILE_COPYRIGHT => "ProfileCopyright",
        TAG_PROFILE_HUE_SAT_MAP_ENCODING => "ProfileHueSatMapEncoding",
        _ => "unknown",
    }
}

fn illuminant_name(v: u16) -> &'static str {
    match v {
        17 => "Standard Light A (2856 K)",
        18 => "Standard Light B (4874 K)",
        19 => "Standard Light C (6774 K)",
        20 => "D55 (5503 K)",
        21 => "D65 (6504 K)",
        22 => "D75 (7504 K)",
        23 => "D50 (5003 K)",
        _ => "other",
    }
}
