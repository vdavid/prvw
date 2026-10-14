//! Backend selection: which extensions Prvw opens, and which decoder a file goes to.
//!
//! The extension decides whether a file is an image at all (folder listings, the file picker),
//! and the file's first bytes decide which decoder reads it (`pick_backend_for`), with the
//! extension as the tiebreaker where the bytes can't say. The three extension tables are the
//! single source of truth for the first half: the predicates, `pick_backend`, and the file
//! picker's filter (`supported_extensions`) all read them.

/// The decoder that will handle a given file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    /// Fast SIMD JPEG path via `zune-jpeg`.
    Jpeg,
    /// Camera RAW via `rawler`.
    Raw,
    /// Fallback via the `image` crate (PNG, GIF, WebP, BMP, TIFF).
    Generic,
}

/// Pick the decoder for a file extension. Unknown extensions fall through to
/// `Generic`; callers gate on [`is_supported_extension`] first.
pub(super) fn pick_backend(ext: &str) -> Backend {
    if is_jpeg_extension(ext) {
        Backend::Jpeg
    } else if is_raw_extension(ext) {
        Backend::Raw
    } else {
        Backend::Generic
    }
}

/// Pick the decoder for a file from its first bytes, with the extension as the tiebreaker
/// where the bytes can't decide. Cameras and copy tools do produce mislabeled files (a Sony
/// JPEG saved as `.arw`, a PNG saved as `.jpg`), and the extension's decoder refuses them.
///
/// - A JPEG, PNG, GIF, WebP, or BMP signature picks that format's decoder.
/// - A RAW format with a signature of its own (RAF, CR3, ORF, RW2, CR2) is RAW.
/// - A TIFF signature is RAW when IFD0 is built like a camera RAW (`tiff_is_camera_raw`).
///   Otherwise the extension breaks the tie: most RAW formats are TIFF containers, so a RAW
///   extension stays RAW, and anything else (`.tif`, `.tiff`) is a TIFF.
/// - Bytes that match nothing leave it to the extension (`pick_backend`).
///
/// `bytes` only needs the file's head (IFD0 has to be in it for the TIFF check), so callers
/// with a header prefix get the same answer as `load_image` with the whole file.
pub(super) fn pick_backend_for(ext: &str, bytes: &[u8]) -> Backend {
    match sniff(bytes) {
        Some(Sniffed::Jpeg) => Backend::Jpeg,
        Some(Sniffed::Generic) => Backend::Generic,
        Some(Sniffed::Raw) => Backend::Raw,
        Some(Sniffed::Tiff) if is_raw_extension(ext) => Backend::Raw,
        Some(Sniffed::Tiff) => Backend::Generic,
        None => pick_backend(ext),
    }
}

/// What a file's first bytes say it is.
enum Sniffed {
    Jpeg,
    /// PNG, GIF, WebP, or BMP: the `image` crate guesses the exact format from the bytes too.
    Generic,
    /// A camera RAW, from its own signature or a TIFF laid out like one.
    Raw,
    /// A TIFF that doesn't show which kind it is.
    Tiff,
}

fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    let at = |offset: usize, magic: &[u8]| bytes.get(offset..offset + magic.len()) == Some(magic);
    if at(0, &[0xFF, 0xD8, 0xFF]) {
        return Some(Sniffed::Jpeg);
    }
    if at(0, b"\x89PNG\r\n\x1a\n")
        || at(0, b"GIF87a")
        || at(0, b"GIF89a")
        || (at(0, b"RIFF") && at(8, b"WEBP"))
        || at(0, b"BM")
    {
        return Some(Sniffed::Generic);
    }
    if at(0, b"FUJIFILMCCD-RAW")
        || at(4, b"ftypcrx ")
        || at(0, b"IIRO")
        || at(0, b"IIRS")
        || at(0, b"MMOR")
        || at(0, b"IIU\x00")
    {
        return Some(Sniffed::Raw);
    }
    let little_endian = at(0, b"II*\x00");
    if little_endian || at(0, b"MM\x00*") {
        // CR2 is a TIFF with Canon's own marker right after the header.
        if at(8, b"CR\x02") || tiff_is_camera_raw(bytes, little_endian) {
            return Some(Sniffed::Raw);
        }
        return Some(Sniffed::Tiff);
    }
    None
}

/// Whether a TIFF's IFD0 carries a camera RAW's structure: `DNGVersion`, a CFA or LinearRaw
/// photometric interpretation (Pentax keeps its mosaic in IFD0), or a camera maker's `Make`
/// next to `SubIFDs` (NEF and ARW keep the raw data in a sub-IFD). A scanner's or an editor's
/// TIFF has none of these; sub-IFDs alone also turn up in pyramidal and scientific TIFFs, so
/// they need the `Make` beside them. False when IFD0 lies past `bytes`.
fn tiff_is_camera_raw(bytes: &[u8], little_endian: bool) -> bool {
    const MAKE: u16 = 271;
    const PHOTOMETRIC: u16 = 262;
    const SUB_IFDS: u16 = 330;
    const DNG_VERSION: u16 = 50706;
    const CFA: u16 = 32803;
    const LINEAR_RAW: u16 = 34892;

    let u16_at = |offset: usize| {
        let pair: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        })
    };
    let u32_at = |offset: usize| {
        let quad: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(quad)
        } else {
            u32::from_be_bytes(quad)
        })
    };
    let Some(ifd) = u32_at(4).map(|offset| offset as usize) else {
        return false;
    };
    let Some(count) = u16_at(ifd) else {
        return false;
    };
    let (mut make, mut sub_ifds) = (false, false);
    for i in 0..usize::from(count) {
        let entry = ifd + 2 + i * 12;
        let Some(tag) = u16_at(entry) else {
            return false;
        };
        match tag {
            DNG_VERSION => return true,
            // A SHORT value sits left-justified in the entry's value field.
            PHOTOMETRIC if matches!(u16_at(entry + 8), Some(CFA | LINEAR_RAW)) => return true,
            MAKE => make = true,
            SUB_IFDS => sub_ifds = true,
            _ => {}
        }
    }
    make && sub_ifds
}

/// JPEG extensions eligible for the fast zune-jpeg decode path.
const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif"];

/// Camera RAW extensions handled by the `rawler` backend.
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
];

/// Extensions the generic `image` crate backend handles.
const GENERIC_EXTENSIONS: &[&str] = &["png", "gif", "webp", "bmp", "tiff", "tif"];

pub(super) fn is_jpeg_extension(ext: &str) -> bool {
    contains(JPEG_EXTENSIONS, ext)
}

pub(super) fn is_raw_extension(ext: &str) -> bool {
    contains(RAW_EXTENSIONS, ext)
}

pub(super) fn is_generic_extension(ext: &str) -> bool {
    contains(GENERIC_EXTENSIONS, ext)
}

/// Whether any backend claims this extension.
pub(super) fn is_supported_extension(ext: &str) -> bool {
    is_jpeg_extension(ext) || is_raw_extension(ext) || is_generic_extension(ext)
}

/// Every extension the app opens, for the file picker's filter. Built from the same three
/// tables the predicates read, so a format can't be openable and yet invisible in the picker.
pub(super) fn supported_extensions() -> Vec<&'static str> {
    JPEG_EXTENSIONS
        .iter()
        .chain(RAW_EXTENSIONS)
        .chain(GENERIC_EXTENSIONS)
        .copied()
        .collect()
}

/// Extensions arrive from `Path::extension`, so they carry whatever case the file has.
fn contains(table: &[&str], ext: &str) -> bool {
    table.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_extensions_match_case_insensitively() {
        for ext in [
            "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
        ] {
            assert!(is_raw_extension(ext), "{ext} should be RAW");
            assert!(
                is_raw_extension(&ext.to_ascii_uppercase()),
                "{} should be RAW",
                ext.to_ascii_uppercase()
            );
        }
    }

    #[test]
    fn non_raw_extensions_are_rejected() {
        for ext in [
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif", "",
        ] {
            assert!(!is_raw_extension(ext), "{ext} should not be RAW");
        }
    }

    #[test]
    fn pick_backend_routes_correctly() {
        assert_eq!(pick_backend("jpg"), Backend::Jpeg);
        assert_eq!(pick_backend("JPEG"), Backend::Jpeg);
        assert_eq!(pick_backend("dng"), Backend::Raw);
        assert_eq!(pick_backend("ARW"), Backend::Raw);
        assert_eq!(pick_backend("cr3"), Backend::Raw);
        assert_eq!(pick_backend("png"), Backend::Generic);
        assert_eq!(pick_backend("tif"), Backend::Generic);
        // Unknown extensions fall through to Generic; the supported-extension
        // gate is what filters them out upstream.
        assert_eq!(pick_backend("xyz"), Backend::Generic);
    }

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x10];
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";
    const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
    const BMP: &[u8] = b"BM\x36\x00\x00\x00\x00\x00";
    const RAF: &[u8] = b"FUJIFILMCCD-RAW 0201FF383501";
    const CR3: &[u8] = b"\x00\x00\x00\x18ftypcrx \x00\x00\x00\x01";
    const ORF: &[u8] = b"IIRO\x08\x00\x00\x00\x18\x00";
    const RW2: &[u8] = b"IIU\x00\x18\x00\x00\x00\x88\xe7";
    const CR2: &[u8] = b"II*\x00\x10\x00\x00\x00CR\x02\x00\x00\x00\x00\x00";

    /// A little-endian TIFF whose IFD0 (at offset 8) holds these `(tag, short value)` entries.
    fn tiff_with(entries: &[(u16, u16)]) -> Vec<u8> {
        let mut bytes = b"II*\x00\x08\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for &(tag, value) in entries {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&3u16.to_le_bytes()); // SHORT
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&u32::from(value).to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes
    }

    /// A plain RGB TIFF: width, height, photometric RGB.
    fn plain_tiff() -> Vec<u8> {
        tiff_with(&[(256, 4), (257, 4), (262, 2)])
    }

    #[test]
    fn the_first_bytes_pick_the_decoder_whatever_the_extension_says() {
        for ext in ["jpg", "png", "gif", "webp", "bmp", "tif", "arw", "NEF"] {
            assert_eq!(pick_backend_for(ext, JPEG), Backend::Jpeg, "JPEG as .{ext}");
            for (name, bytes) in [("PNG", PNG), ("GIF", GIF), ("WebP", WEBP), ("BMP", BMP)] {
                assert_eq!(
                    pick_backend_for(ext, bytes),
                    Backend::Generic,
                    "{name} as .{ext}"
                );
            }
        }
    }

    #[test]
    fn a_raw_format_with_its_own_signature_is_raw_under_any_extension() {
        for ext in ["jpg", "png", "tif", "tiff", "webp", "raf", "cr3"] {
            for (name, bytes) in [
                ("RAF", RAF),
                ("CR3", CR3),
                ("ORF", ORF),
                ("RW2", RW2),
                ("CR2", CR2),
            ] {
                assert_eq!(
                    pick_backend_for(ext, bytes),
                    Backend::Raw,
                    "{name} as .{ext}"
                );
            }
        }
    }

    #[test]
    fn a_plain_tiff_signature_leaves_the_tiebreak_to_the_extension() {
        let tiff = plain_tiff();
        // Most RAW formats are TIFF containers, so a RAW extension stays RAW.
        assert_eq!(pick_backend_for("arw", &tiff), Backend::Raw);
        assert_eq!(pick_backend_for("DNG", &tiff), Backend::Raw);
        assert_eq!(pick_backend_for("tif", &tiff), Backend::Generic);
        assert_eq!(pick_backend_for("TIFF", &tiff), Backend::Generic);
        assert_eq!(pick_backend_for("jpg", &tiff), Backend::Generic);
    }

    #[test]
    fn a_tiff_built_like_a_camera_raw_is_raw_even_named_tif() {
        // DNGVersion (50706), a CFA photometric interpretation (32803), or a camera maker's IFD0
        // that points at sub-IFDs (Make 271 + SubIFDs 330, which is how NEF and ARW are laid out).
        for (what, tiff) in [
            ("DNGVersion", tiff_with(&[(256, 4), (50706, 1)])),
            ("CFA photometric", tiff_with(&[(256, 4), (262, 32803)])),
            (
                "LinearRaw photometric",
                tiff_with(&[(256, 4), (262, 34892)]),
            ),
            ("Make + SubIFDs", tiff_with(&[(256, 4), (271, 0), (330, 0)])),
        ] {
            assert_eq!(pick_backend_for("tif", &tiff), Backend::Raw, "{what}");
            assert_eq!(pick_backend_for("jpg", &tiff), Backend::Raw, "{what}");
        }
        // Sub-IFDs alone are how pyramidal and some scientific TIFFs keep their extra pages.
        let pyramid = tiff_with(&[(256, 4), (330, 0)]);
        assert_eq!(pick_backend_for("tif", &pyramid), Backend::Generic);
    }

    #[test]
    fn a_big_endian_tiff_reads_its_ifd_the_same_way() {
        // MM, version 42, IFD0 at 8, one entry: DNGVersion.
        let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08\x00\x01".to_vec();
        tiff.extend_from_slice(&[0xC6, 0x12, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 1, 4, 0, 0]);
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(pick_backend_for("tif", &tiff), Backend::Raw);
    }

    #[test]
    fn bytes_that_say_nothing_leave_it_to_the_extension() {
        let unknown = b"\x00\x01\x02\x03 nothing we know";
        assert_eq!(pick_backend_for("jpg", unknown), Backend::Jpeg);
        assert_eq!(pick_backend_for("nef", unknown), Backend::Raw);
        assert_eq!(pick_backend_for("png", unknown), Backend::Generic);
        assert_eq!(pick_backend_for("jpg", &[]), Backend::Jpeg);
        // A TIFF whose IFD0 lies past the bytes in hand can't show its structure.
        let truncated = b"II*\x00\x00\x10\x00\x00";
        assert_eq!(pick_backend_for("tif", truncated), Backend::Generic);
        assert_eq!(pick_backend_for("nef", truncated), Backend::Raw);
    }

    #[test]
    fn the_picker_filter_lists_exactly_what_we_open() {
        for ext in supported_extensions() {
            assert!(
                is_supported_extension(ext),
                "{ext} is offered but not opened"
            );
        }
        assert!(!supported_extensions().contains(&"txt"));
    }

    #[test]
    fn is_supported_extension_covers_all_raw_formats() {
        for ext in [
            "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
        ] {
            assert!(is_supported_extension(ext), "{ext} should be supported");
        }
        assert!(is_supported_extension("jpg"));
        assert!(is_supported_extension("png"));
        assert!(!is_supported_extension("txt"));
        assert!(!is_supported_extension("mov"));
    }
}
