//! Render `resources/AppIcon.ico` from `resources/AppIcon.icns`.
//!
//! Windows wants a multi-size `.ico` where macOS wants an `.icns`, and both should show the same
//! artwork. This keeps the `.icns` as the single source: run it after the artwork changes, then
//! commit the regenerated `.ico`. `build.rs` embeds that file into `prvw.exe` as `RT_GROUP_ICON`
//! ordinal 1, which is what Explorer, the taskbar, and Alt+Tab draw.
//!
//! ```sh
//! cd apps/desktop
//! cargo run --example make-app-icon
//! ```

use std::error::Error;
use std::fs;
use std::path::Path;

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, RgbaImage};

/// The sizes Windows asks for. 16 is the Explorer list view and the window's small icon, 32 the
/// taskbar and Alt+Tab, 48 Explorer's medium icons, and 256 everything that scales (the shell
/// resamples 256 down for 96 and 64 rather than picking a nearer entry). 24, 64, and 128 are
/// cheap and cover the sizes the shell asks for at 125% to 200% scaling.
const TARGET_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Entries at or above this size go in PNG-compressed, smaller ones as a DIB. Both forms have
/// been legal since Windows Vista, but PNG at small sizes still renders blank in some shell
/// paths, and a 256-pixel DIB would add 256 KB to the executable for nothing.
const PNG_FROM_SIZE: u32 = 256;

fn main() -> Result<(), Box<dyn Error>> {
    let resources = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
    let icns = fs::read(resources.join("AppIcon.icns"))?;
    let mut sources = decode_icns_images(&icns)?;
    if sources.is_empty() {
        return Err("AppIcon.icns holds no PNG images to scale from".into());
    }
    // Largest first, so the pick below lands on the smallest source that is still big enough.
    sources.sort_by_key(|image| std::cmp::Reverse(image.width()));

    let mut encoded: Vec<(u32, Vec<u8>)> = Vec::with_capacity(TARGET_SIZES.len());
    for size in TARGET_SIZES {
        let source = sources
            .iter()
            .rfind(|image| image.width() >= size)
            .unwrap_or(&sources[0]);
        let scaled = if source.width() == size {
            source.to_rgba8()
        } else {
            source
                .resize_exact(size, size, FilterType::Lanczos3)
                .to_rgba8()
        };
        let bytes = if size >= PNG_FROM_SIZE {
            encode_png(&scaled)?
        } else {
            encode_dib(&scaled)
        };
        println!(
            "{size:>3}x{size:<3} from {:>4}px source, {} bytes",
            source.width(),
            bytes.len()
        );
        encoded.push((size, bytes));
    }

    let frames: Vec<IcoFrame> = encoded
        .iter()
        .map(|(size, bytes)| {
            IcoFrame::with_encoded(bytes.as_slice(), *size, *size, ExtendedColorType::Rgba8)
        })
        .collect::<Result<_, _>>()?;

    let out = resources.join("AppIcon.ico");
    let file = fs::File::create(&out)?;
    IcoEncoder::new(file).encode_images(&frames)?;
    println!("Wrote {} ({} entries)", out.display(), frames.len());
    Ok(())
}

/// Pull every PNG-encoded image out of an `.icns` container.
///
/// The format is a four-byte magic, a big-endian total length, then a flat list of
/// `[four-byte type][big-endian length including this header][payload]` chunks. Modern icon types
/// (`ic07` and up) carry a PNG; older ones carry raw ARGB or JPEG 2000 and are skipped, along with
/// the `info` metadata chunk.
fn decode_icns_images(icns: &[u8]) -> Result<Vec<DynamicImage>, Box<dyn Error>> {
    if icns.len() < 8 || &icns[..4] != b"icns" {
        return Err("that file doesn't start with an icns header".into());
    }
    let total = u32::from_be_bytes(icns[4..8].try_into()?) as usize;
    let end = total.min(icns.len());

    let mut images = Vec::new();
    let mut offset = 8;
    while offset + 8 <= end {
        let length = u32::from_be_bytes(icns[offset + 4..offset + 8].try_into()?) as usize;
        if length < 8 || offset + length > end {
            break;
        }
        let payload = &icns[offset + 8..offset + length];
        if payload.starts_with(b"\x89PNG\r\n\x1a\n") {
            images.push(image::load_from_memory_with_format(
                payload,
                image::ImageFormat::Png,
            )?);
        }
        offset += length;
    }
    Ok(images)
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    image.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )?;
    Ok(bytes)
}

/// Encode an image the way an ICO's DIB entry wants it: a 40-byte `BITMAPINFOHEADER` whose height
/// counts the AND mask too, then 32-bit BGRA rows bottom-up, then the mask itself. Windows takes
/// transparency from the alpha channel, so every mask bit is zero (meaning "opaque"), but the
/// rows have to be there or the icon reads as half its height.
fn encode_dib(image: &RgbaImage) -> Vec<u8> {
    let (width, height) = (image.width(), image.height());
    let mask_stride = width.div_ceil(32) * 4; // 1 bit per pixel, rows padded to 4 bytes
    let pixels_len = width * height * 4;
    let mask_len = mask_stride * height;

    let mut out = Vec::with_capacity(40 + (pixels_len + mask_len) as usize);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&((height * 2) as i32).to_le_bytes()); // biHeight: image plus mask
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&(pixels_len + mask_len).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..height).rev() {
        for x in 0..width {
            let [r, g, b, a] = image.get_pixel(x, y).0;
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out.resize(out.len() + mask_len as usize, 0);
    out
}
