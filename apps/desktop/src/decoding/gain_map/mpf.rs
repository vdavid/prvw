//! The CIPA Multi-Picture Format index (DC-007): the small TIFF IFD in a JPEG's `MPF\0` APP2
//! segment that lists every image the file carries, with its size and where it starts. A
//! gain-map JPEG uses it to point from the primary image at the gain map appended after it.
//!
//! Every byte here comes from the file, so every offset and count is checked before it's used,
//! and a malformed index answers "no images" rather than panicking. `ultrahdr-core` no longer
//! parses MPF (spec Findings, 2026-10-13), and the crate that does is AGPL, hence our own.

use std::ops::Range;

/// One entry of the MP index: an image somewhere in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MpImage {
    /// The image's bytes, as a range into the whole file, already checked to lie inside it.
    pub range: Range<usize>,
    /// The entry's "individual image attribute" word (type code in the low 24 bits).
    pub attribute: u32,
}

/// MP Index IFD tags (DC-007 section 5.2.3).
const TAG_NUMBER_OF_IMAGES: u16 = 0xB001;
const TAG_MP_ENTRY: u16 = 0xB002;
/// Each MP entry is 16 bytes: attribute, size, offset (u32 each), two dependent-image u16s.
const MP_ENTRY_LEN: usize = 16;
/// More than any real file carries (cameras write two to four), and it bounds the work a
/// hostile count can ask for.
const MAX_IMAGES: usize = 64;

/// Parse the MP index.
///
/// - `mpf` is the APP2 payload after the `MPF\0` signature, so it starts at the TIFF header.
/// - `mpf_offset` is where that TIFF header sits in the file: entry offsets are relative to it,
///   except the first image's, which is 0 and means the start of the file.
/// - `file_len` bounds every range.
///
/// Entries whose bytes fall outside the file are dropped. An index that can't be read at all
/// gives an empty list.
pub fn parse(mpf: &[u8], mpf_offset: usize, file_len: usize) -> Vec<MpImage> {
    parse_entries(mpf, mpf_offset, file_len).unwrap_or_default()
}

fn parse_entries(mpf: &[u8], mpf_offset: usize, file_len: usize) -> Option<Vec<MpImage>> {
    let endian = match mpf.get(..4)? {
        b"II\x2a\x00" => Endian::Little,
        b"MM\x00\x2a" => Endian::Big,
        _ => return None,
    };
    let ifd = endian.u32(mpf, 4)? as usize;
    let count = usize::from(endian.u16(mpf, ifd)?);

    let mut number_of_images = None;
    let mut entries_at = None;
    for i in 0..count {
        let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        let tag = endian.u16(mpf, entry)?;
        let value_count = endian.u32(mpf, entry + 4)? as usize;
        match tag {
            TAG_NUMBER_OF_IMAGES => number_of_images = Some(endian.u32(mpf, entry + 8)? as usize),
            // UNDEFINED, `count` bytes. Always longer than four bytes, so the value field is an
            // offset from the TIFF header.
            TAG_MP_ENTRY => entries_at = Some((endian.u32(mpf, entry + 8)? as usize, value_count)),
            _ => {}
        }
    }

    let (entries_at, entries_len) = entries_at?;
    let listed = entries_len / MP_ENTRY_LEN;
    let images = number_of_images
        .unwrap_or(listed)
        .min(listed)
        .min(MAX_IMAGES);

    let mut out = Vec::with_capacity(images);
    for i in 0..images {
        let at = entries_at.checked_add(i * MP_ENTRY_LEN)?;
        let attribute = endian.u32(mpf, at)?;
        let size = endian.u32(mpf, at + 4)? as usize;
        let offset = endian.u32(mpf, at + 8)? as usize;
        let start = if offset == 0 {
            0
        } else {
            match mpf_offset.checked_add(offset) {
                Some(start) => start,
                None => continue,
            }
        };
        let Some(end) = start.checked_add(size) else {
            continue;
        };
        if size == 0 || end > file_len {
            log::debug!("MPF entry {i} ({start}+{size}) lies outside the {file_len}-byte file");
            continue;
        }
        out.push(MpImage {
            range: start..end,
            attribute,
        });
    }
    Some(out)
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, b: &[u8], at: usize) -> Option<u16> {
        let bytes: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(match self {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        })
    }

    fn u32(self, b: &[u8], at: usize) -> Option<u32> {
        let bytes: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(match self {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }
}

/// Build an MP index the way libultrahdr writes one (big-endian, version, count, entries), for
/// tests here and in the gain-map locator. `images` is `(attribute, size, offset)` per entry.
#[cfg(test)]
pub fn build_index(images: &[(u32, u32, u32)], little_endian: bool) -> Vec<u8> {
    let u16b = |v: u16| {
        if little_endian {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let u32b = |v: u32| {
        if little_endian {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let mut out = Vec::new();
    out.extend_from_slice(if little_endian {
        b"II\x2a\x00"
    } else {
        b"MM\x00\x2a"
    });
    out.extend_from_slice(&u32b(8));
    let tags = 3u16;
    out.extend_from_slice(&u16b(tags));
    let entries_at = 8 + 2 + 12 * u32::from(tags) + 4;
    // MPFVersion, UNDEFINED, 4, "0100"
    out.extend_from_slice(&u16b(0xB000));
    out.extend_from_slice(&u16b(7));
    out.extend_from_slice(&u32b(4));
    out.extend_from_slice(b"0100");
    out.extend_from_slice(&u16b(TAG_NUMBER_OF_IMAGES));
    out.extend_from_slice(&u16b(4));
    out.extend_from_slice(&u32b(1));
    out.extend_from_slice(&u32b(images.len() as u32));
    out.extend_from_slice(&u16b(TAG_MP_ENTRY));
    out.extend_from_slice(&u16b(7));
    out.extend_from_slice(&u32b((images.len() * MP_ENTRY_LEN) as u32));
    out.extend_from_slice(&u32b(entries_at));
    out.extend_from_slice(&u32b(0)); // next IFD
    for &(attribute, size, offset) in images {
        out.extend_from_slice(&u32b(attribute));
        out.extend_from_slice(&u32b(size));
        out.extend_from_slice(&u32b(offset));
        out.extend_from_slice(&u16b(0));
        out.extend_from_slice(&u16b(0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: u32 = 0x0003_0000;

    #[test]
    fn a_valid_index_lists_both_images_with_absolute_ranges() {
        for little_endian in [false, true] {
            let index = build_index(&[(PRIMARY, 1000, 0), (0, 200, 900)], little_endian);
            let images = parse(&index, 50, 2000);
            assert_eq!(
                images,
                vec![
                    MpImage {
                        range: 0..1000,
                        attribute: PRIMARY
                    },
                    // Relative to the TIFF header at 50, not the file start.
                    MpImage {
                        range: 950..1150,
                        attribute: 0
                    },
                ],
                "little endian: {little_endian}"
            );
        }
    }

    #[test]
    fn an_entry_past_the_end_of_the_file_is_dropped() {
        let index = build_index(&[(PRIMARY, 1000, 0), (0, 200, 900)], false);
        let images = parse(&index, 50, 1100);
        assert_eq!(images.len(), 1, "{images:?}");
        assert_eq!(images[0].range, 0..1000);
    }

    #[test]
    fn offsets_that_overflow_are_dropped_not_wrapped() {
        let index = build_index(&[(PRIMARY, 10, 0), (0, u32::MAX, u32::MAX)], false);
        let images = parse(&index, usize::MAX - 2, usize::MAX);
        assert_eq!(images.len(), 1, "{images:?}");
    }

    #[test]
    fn zero_images_is_an_empty_list() {
        assert!(parse(&build_index(&[], false), 0, 100).is_empty());
    }

    #[test]
    fn a_count_larger_than_the_entry_table_reads_only_the_table() {
        let mut index = build_index(&[(PRIMARY, 10, 0)], false);
        // NumberOfImages' value sits at 8 + 2 + 12 + 8.
        index[30..34].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(parse(&index, 0, 100).len(), 1);
    }

    /// Every truncation of a valid index, and every single-byte corruption of it, must answer
    /// with a list (possibly empty) and never panic. These bytes come straight from the file.
    #[test]
    fn truncated_or_corrupt_indexes_never_panic() {
        let index = build_index(&[(PRIMARY, 1000, 0), (0, 200, 900)], false);
        for len in 0..index.len() {
            let _ = parse(&index[..len], 50, 2000);
        }
        for at in 0..index.len() {
            for value in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
                let mut corrupt = index.clone();
                corrupt[at] = value;
                for images in parse(&corrupt, 50, 2000) {
                    assert!(images.range.end <= 2000);
                }
            }
        }
    }

    #[test]
    fn a_missing_signature_is_no_index() {
        assert!(parse(b"not a tiff header at all", 0, 100).is_empty());
    }
}
