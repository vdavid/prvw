//! The macOS side of Finder tags: the `com.apple.metadata:_kMDItemUserTags` extended attribute.
//!
//! The attribute is a binary plist holding an array of strings, each `"Name\nN"`, where `N` is
//! the Finder color index ([`finder_index`], `0` for colorless). A colorless tag may leave the
//! `\nN` suffix off entirely.
//!
//! Only that one attribute is ever touched. `com.apple.FinderInfo` in particular stays as it is:
//! it carries a custom icon's flag and the old type and creator codes, and modern Finder reads
//! tags from `_kMDItemUserTags` alone.

use std::io;
use std::path::Path;

use super::{Tag, TagColor, has_color};

/// The extended attribute Finder keeps a file's tags in.
pub const TAGS_XATTR: &str = "com.apple.metadata:_kMDItemUserTags";

/// The index Finder stores after a tag's name: `1` gray, `2` green, `3` purple, `4` blue,
/// `5` yellow, `6` red, `7` orange. `0` means colorless, which has no [`TagColor`].
const fn finder_index(color: TagColor) -> u8 {
    match color {
        TagColor::Gray => 1,
        TagColor::Green => 2,
        TagColor::Purple => 3,
        TagColor::Blue => 4,
        TagColor::Yellow => 5,
        TagColor::Red => 6,
        TagColor::Orange => 7,
    }
}

/// The color behind a Finder index, or `None` for `0` (colorless) and anything past `7`.
fn color_of_index(index: u8) -> Option<TagColor> {
    TagColor::ALL
        .into_iter()
        .find(|color| finder_index(*color) == index)
}

/// Finder's built-in tag of `color`, which is what toggling adds. Written under the name Finder
/// gives it, a Prvw tag is indistinguishable from one Finder wrote.
fn system_tag(color: TagColor) -> Tag {
    Tag {
        name: color.name().to_string(),
        color: Some(color),
    }
}

/// A file's tags. A file with no attribute has none.
///
/// An attribute that's there but isn't a tag list is an error, never "no tags": the write path
/// reads through here, and treating an unreadable list as empty would overwrite it.
pub fn read_tags(path: &Path) -> io::Result<Vec<Tag>> {
    let Some(bytes) = xattr::get(path, TAGS_XATTR)? else {
        return Ok(Vec::new());
    };
    parse_tags_plist(&bytes).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the tag attribute isn't a list Finder would recognize",
        )
    })
}

/// Decode a raw attribute value, or `None` when it isn't a plist holding an array. Entries that
/// aren't strings are skipped: Finder writes none, and one can't be a tag.
pub fn parse_tags_plist(bytes: &[u8]) -> Option<Vec<Tag>> {
    let value = plist::Value::from_reader(io::Cursor::new(bytes)).ok()?;
    let array = value.as_array()?;
    Some(
        array
            .iter()
            .filter_map(plist::Value::as_string)
            .map(parse_tag_string)
            .collect(),
    )
}

/// Split one `"Name\nN"` string. The split is on the last newline, so a name may contain
/// newlines of its own; a string whose tail isn't a color index `0..=7` is all name, colorless.
fn parse_tag_string(text: &str) -> Tag {
    if let Some((name, index)) = text.rsplit_once('\n')
        && let Ok(index) = index.parse::<u8>()
        && index <= 7
    {
        return Tag {
            name: name.to_string(),
            color: color_of_index(index),
        };
    }
    Tag {
        name: text.to_string(),
        color: None,
    }
}

/// Encode tags the way Finder writes them: a **binary** plist (`plist` defaults to XML, which
/// Finder doesn't read) of `"Name\nN"` strings, the color suffix always present, `0` included.
pub fn encode_tags_plist(tags: &[Tag]) -> io::Result<Vec<u8>> {
    let array = tags
        .iter()
        .map(|tag| {
            let index = tag.color.map_or(0, finder_index);
            plist::Value::String(format!("{}\n{index}", tag.name))
        })
        .collect();
    let mut bytes = Vec::new();
    plist::Value::Array(array)
        .to_writer_binary(&mut bytes)
        .map_err(io::Error::other)?;
    Ok(bytes)
}

/// Replace a file's tags with `tags`. An empty set removes the attribute, the way Finder clears
/// a file's last tag, so no empty list lingers behind. `setxattr` replaces an attribute
/// atomically, so a failed write leaves the old list whole.
pub fn set_tags(path: &Path, tags: &[Tag]) -> io::Result<()> {
    if tags.is_empty() {
        // Only remove what's there, so clearing an untagged file doesn't surface ENOATTR.
        if xattr::get(path, TAGS_XATTR)?.is_some() {
            xattr::remove(path, TAGS_XATTR)?;
        }
        return Ok(());
    }
    xattr::set(path, TAGS_XATTR, &encode_tags_plist(tags)?)
}

/// Toggle `color` on one file, keeping every other tag: remove all tags of that color when the
/// file has one (a custom tag of that color counts), append Finder's built-in tag otherwise.
/// Returns the set written.
///
/// Reads strictly first. A tag list that can't be read or decoded stops the toggle, because
/// writing on top of it would throw the file's other tags away.
pub fn toggle_color(path: &Path, color: TagColor) -> io::Result<Vec<Tag>> {
    let mut tags = read_tags(path)?;
    if has_color(&tags, color) {
        tags.retain(|tag| tag.color != Some(color));
    } else {
        tags.push(system_tag(color));
    }
    set_tags(path, &tags)?;
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes a hex string, as `xattr -px` prints one.
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn tag(name: &str, color: Option<TagColor>) -> Tag {
        Tag {
            name: name.to_string(),
            color,
        }
    }

    fn red() -> Tag {
        tag("Red", Some(TagColor::Red))
    }

    // ── Fixtures Finder wrote, captured with `xattr -px` ─────────────────────────────────────

    /// `["Red\n6"]`.
    const RED_FIXTURE: &str = "62706C6973743030A101555265640A36080A0000000000000101000000000000000200000000000000000000000000000010";

    #[test]
    fn a_single_color_tag() {
        assert_eq!(parse_tags_plist(&hex(RED_FIXTURE)), Some(vec![red()]));
    }

    #[test]
    fn five_color_tags_keep_their_order() {
        let bytes = hex(
            "62706C6973743030A50102030405555265640A36584F72616E67650A375859656C6C6F770A3557477265656E0A3256426C75650A34080E141D262E0000000000000101000000000000000600000000000000000000000000000035",
        );
        assert_eq!(
            parse_tags_plist(&bytes),
            Some(vec![
                red(),
                tag("Orange", Some(TagColor::Orange)),
                tag("Yellow", Some(TagColor::Yellow)),
                tag("Green", Some(TagColor::Green)),
                tag("Blue", Some(TagColor::Blue)),
            ])
        );
    }

    #[test]
    fn a_colorless_named_tag() {
        // `["Work\n0"]`
        let bytes = hex(
            "62706C6973743030A10156576F726B0A30080A0000000000000101000000000000000200000000000000000000000000000011",
        );
        assert_eq!(parse_tags_plist(&bytes), Some(vec![tag("Work", None)]));
    }

    #[test]
    fn colorless_and_colored_together() {
        // `["Important\n0", "Red\n6"]`
        let bytes = hex(
            "62706C6973743030A201025B496D706F7274616E740A30555265640A36080B17000000000000010100000000000000030000000000000000000000000000001D",
        );
        assert_eq!(
            parse_tags_plist(&bytes),
            Some(vec![tag("Important", None), red()])
        );
    }

    #[test]
    fn a_buffer_that_isnt_a_tag_list_decodes_to_nothing() {
        assert_eq!(parse_tags_plist(&[]), None);
        assert_eq!(parse_tags_plist(&[0xDE, 0xAD, 0xBE, 0xEF]), None);
        assert_eq!(parse_tags_plist(b"not a plist at all"), None);
    }

    /// Finder's on-disk indices, which aren't in menu order: 1 gray through 7 orange.
    #[test]
    fn indices_match_what_finder_writes() {
        assert_eq!(finder_index(TagColor::Gray), 1);
        assert_eq!(finder_index(TagColor::Green), 2);
        assert_eq!(finder_index(TagColor::Purple), 3);
        assert_eq!(finder_index(TagColor::Blue), 4);
        assert_eq!(finder_index(TagColor::Yellow), 5);
        assert_eq!(finder_index(TagColor::Red), 6);
        assert_eq!(finder_index(TagColor::Orange), 7);
        for color in TagColor::ALL {
            assert_eq!(color_of_index(finder_index(color)), Some(color));
        }
        assert_eq!(color_of_index(0), None);
        assert_eq!(color_of_index(8), None);
    }

    // ── One tag string ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_string_without_a_newline_is_colorless() {
        assert_eq!(parse_tag_string("Plain"), tag("Plain", None));
    }

    #[test]
    fn an_out_of_range_index_is_part_of_the_name() {
        assert_eq!(parse_tag_string("Weird\n9"), tag("Weird\n9", None));
    }

    #[test]
    fn a_name_may_contain_newlines() {
        assert_eq!(
            parse_tag_string("two\nline\n4"),
            tag("two\nline", Some(TagColor::Blue))
        );
    }

    // ── Encode, then decode. Semantic round trips: valid binary plists differ in object-table
    //    order, so byte equality with a Finder fixture isn't the claim. ────────────────────────

    #[test]
    fn encoding_writes_a_binary_plist() {
        let bytes = encode_tags_plist(&[red()]).expect("encode");
        assert!(bytes.starts_with(b"bplist00"), "{bytes:?}");
    }

    #[test]
    fn round_trips() {
        for tags in [
            vec![red()],
            vec![
                tag("Important", None),
                red(),
                tag("Blue", Some(TagColor::Blue)),
            ],
            vec![tag("two\nline", Some(TagColor::Blue))],
            Vec::new(),
        ] {
            let bytes = encode_tags_plist(&tags).expect("encode");
            assert_eq!(parse_tags_plist(&bytes), Some(tags));
        }
    }

    /// A colorless tag is written with its `\n0`, the way Finder writes one.
    #[test]
    fn a_colorless_tag_is_written_with_its_zero() {
        let bytes = encode_tags_plist(&[tag("Work", None)]).expect("encode");
        assert!(
            bytes.windows(6).any(|w| w == b"Work\n0"),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    // ── Against real files ───────────────────────────────────────────────────────────────────

    fn temp_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"x").expect("write");
        (dir, file)
    }

    #[test]
    fn reads_a_tag_finder_wrote() {
        let (_dir, file) = temp_file();
        xattr::set(&file, TAGS_XATTR, &hex(RED_FIXTURE)).expect("set");
        assert_eq!(read_tags(&file).expect("read"), vec![red()]);
    }

    #[test]
    fn an_untagged_file_has_no_tags() {
        let (_dir, file) = temp_file();
        assert_eq!(read_tags(&file).expect("read"), Vec::new());
    }

    #[test]
    fn an_undecodable_attribute_is_an_error_not_an_empty_list() {
        let (_dir, file) = temp_file();
        xattr::set(&file, TAGS_XATTR, b"garbage").expect("set");
        assert!(read_tags(&file).is_err());
    }

    #[test]
    fn set_tags_round_trips_and_an_empty_set_removes_the_attribute() {
        let (_dir, file) = temp_file();

        let many = vec![
            tag("Important", None),
            red(),
            tag("Blue", Some(TagColor::Blue)),
        ];
        set_tags(&file, &many).expect("set");
        assert_eq!(read_tags(&file).expect("read"), many);
        let raw = xattr::get(&file, TAGS_XATTR)
            .expect("get")
            .expect("present");
        assert!(raw.starts_with(b"bplist00"));

        set_tags(&file, &[]).expect("clear");
        assert!(xattr::get(&file, TAGS_XATTR).expect("get").is_none());
        // Clearing a file that has no tags is a quiet no-op, not an ENOATTR.
        set_tags(&file, &[]).expect("clear again");
    }

    #[test]
    fn toggling_adds_the_color_and_keeps_the_other_tags() {
        let (_dir, file) = temp_file();
        set_tags(&file, &[tag("Important", None)]).expect("set");

        let written = toggle_color(&file, TagColor::Red).expect("toggle");
        let expected = vec![tag("Important", None), red()];
        assert_eq!(written, expected);
        assert_eq!(read_tags(&file).expect("read"), expected);
    }

    #[test]
    fn toggling_removes_only_that_color() {
        let (_dir, file) = temp_file();
        set_tags(&file, &[tag("Blue", Some(TagColor::Blue)), red()]).expect("set");

        toggle_color(&file, TagColor::Red).expect("toggle");
        assert_eq!(
            read_tags(&file).expect("read"),
            vec![tag("Blue", Some(TagColor::Blue))]
        );
    }

    /// Finder's rule: a custom tag of the color counts as having it, and toggling takes every
    /// tag of that color away rather than adding a duplicate.
    #[test]
    fn a_custom_tag_of_the_color_is_what_toggling_removes() {
        let (_dir, file) = temp_file();
        set_tags(&file, &[tag("Important", Some(TagColor::Red)), red()]).expect("set");

        toggle_color(&file, TagColor::Red).expect("toggle");
        assert!(xattr::get(&file, TAGS_XATTR).expect("get").is_none());
    }

    #[test]
    fn toggling_twice_leaves_an_untagged_file_untagged() {
        let (_dir, file) = temp_file();
        toggle_color(&file, TagColor::Green).expect("add");
        assert_eq!(
            read_tags(&file).expect("read"),
            vec![tag("Green", Some(TagColor::Green))]
        );
        toggle_color(&file, TagColor::Green).expect("remove");
        assert!(xattr::get(&file, TAGS_XATTR).expect("get").is_none());
    }

    /// Writing on top of a tag list we couldn't decode would destroy it.
    #[test]
    fn toggling_refuses_to_overwrite_a_list_it_cant_read() {
        let (_dir, file) = temp_file();
        xattr::set(&file, TAGS_XATTR, b"garbage").expect("set");

        assert!(toggle_color(&file, TagColor::Red).is_err());
        assert_eq!(
            xattr::get(&file, TAGS_XATTR).expect("get").as_deref(),
            Some(&b"garbage"[..])
        );
    }

    /// Tagging never touches `com.apple.FinderInfo`, where a folder's custom-icon flag lives
    /// (`kHasCustomIcon`, `0x0400` at byte 8).
    #[test]
    fn tagging_leaves_finder_info_alone() {
        let (_dir, file) = temp_file();
        let mut finder_info = vec![0u8; 32];
        finder_info[8] = 0x04;
        xattr::set(&file, "com.apple.FinderInfo", &finder_info).expect("set");

        toggle_color(&file, TagColor::Red).expect("toggle");

        assert_eq!(read_tags(&file).expect("read"), vec![red()]);
        assert_eq!(
            xattr::get(&file, "com.apple.FinderInfo").expect("get"),
            Some(finder_info)
        );
    }

    /// The proof a Prvw tag is a real Finder tag: macOS's own resource API, the one Finder and
    /// Spotlight read through, sees the names we wrote.
    #[test]
    fn macos_reads_the_tags_we_write() {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2_foundation::{NSArray, NSString, NSURL};

        let (_dir, file) = temp_file();
        set_tags(&file, &[red(), tag("Work", None)]).expect("set");

        let url = NSURL::fileURLWithPath(&NSString::from_str(file.to_str().unwrap()));
        let key = NSString::from_str("NSURLTagNamesKey");
        let mut value: Option<Retained<AnyObject>> = None;
        // SAFETY: `url` and `key` are valid objects, and `&mut value` is the out-parameter shape
        // objc2 stores an already-retained object into on success.
        let result = unsafe { url.getResourceValue_forKey_error(&mut value, &key) };
        assert!(result.is_ok(), "the resource read should succeed");

        let array = value
            .expect("tag names present")
            .downcast::<NSArray<AnyObject>>()
            .expect("NSURLTagNamesKey is an NSArray");
        let names: Vec<String> = (0..array.count())
            .map(|i| {
                array
                    .objectAtIndex(i)
                    .downcast::<NSString>()
                    .expect("a tag name is an NSString")
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["Red".to_string(), "Work".to_string()]);
    }
}
