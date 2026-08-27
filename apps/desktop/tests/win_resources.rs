//! Tests for the `.res` encoders `build.rs` uses.
//!
//! The build script writes these bytes on Windows only, but they're pure byte assembly, so the
//! tests run everywhere. That's deliberate: the icon and manifest are invisible on the machine
//! that produces them, and a malformed blob shows up as a linker error or, worse, an executable
//! that silently drops its icon.

#[path = "../build-support/win_resources.rs"]
mod win_resources;

use win_resources::{VersionInfo, build_res};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;

const MANIFEST: &str = "<assembly manifestVersion=\"1.0\" />";

fn sample_version() -> VersionInfo {
    VersionInfo {
        version: [0, 15, 1, 0],
        company: "Rymdskottkärra AB".to_string(),
        description: "Prvw".to_string(),
        product: "Prvw".to_string(),
        copyright: "© 2026 Rymdskottkärra AB".to_string(),
        file_name: "prvw.exe".to_string(),
    }
}

/// The committed artwork, which is what the build script actually embeds.
fn app_icon() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/AppIcon.ico");
    std::fs::read(&path).unwrap_or_else(|e| panic!("couldn't read {}: {e}", path.display()))
}

/// A minimal two-image `.ico` with recognizable payloads, for tests that need to trace bytes.
fn tiny_ico() -> Vec<u8> {
    let images: [&[u8]; 2] = [b"first-image-bytes", b"second"];
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + images.len() * 16;
    for (index, image) in images.iter().enumerate() {
        out.push(if index == 0 { 16 } else { 0 }); // 0 means 256
        out.push(if index == 0 { 16 } else { 0 });
        out.push(0); // no palette
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(image);
    }
    out
}

struct Entry {
    type_id: u16,
    name_id: u16,
    language: u16,
    data: Vec<u8>,
}

/// Walk a `.res` the way a linker does, so a header that lies about its own size fails here.
fn parse_res(res: &[u8]) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < res.len() {
        assert_eq!(offset % 4, 0, "every entry starts four-byte aligned");
        assert!(offset + 16 <= res.len(), "a header runs past the end");
        let data_size = u32::from_le_bytes(res[offset..offset + 4].try_into().unwrap()) as usize;
        let header_size =
            u32::from_le_bytes(res[offset + 4..offset + 8].try_into().unwrap()) as usize;
        assert!(
            header_size >= 32,
            "a header claims to be {header_size} bytes"
        );
        let read_u16 = |at: usize| u16::from_le_bytes(res[at..at + 2].try_into().unwrap());
        assert_eq!(
            read_u16(offset + 8),
            0xFFFF,
            "types are written as ordinals"
        );
        assert_eq!(
            read_u16(offset + 12),
            0xFFFF,
            "names are written as ordinals"
        );
        let data_start = offset + header_size;
        assert!(
            data_start + data_size <= res.len(),
            "data runs past the end"
        );
        entries.push(Entry {
            type_id: read_u16(offset + 10),
            name_id: read_u16(offset + 14),
            // The header ends with DataVersion, MemoryFlags, LanguageId, Version, Characteristics.
            language: read_u16(offset + header_size - 10),
            data: res[data_start..data_start + data_size].to_vec(),
        });
        offset = data_start + data_size.next_multiple_of(4);
    }
    entries
}

#[test]
fn res_starts_with_the_empty_marker_entry() {
    let res = build_res(&tiny_ico(), MANIFEST, &sample_version()).expect("the .ico is valid");
    // These 16 bytes are the magic `link.exe` and `lld-link` match to recognize a `.res` at all.
    // Get them wrong and the linker silently treats the file as something it doesn't understand,
    // leaving the executable with no resources and no warning.
    assert_eq!(
        res[..16],
        [
            0, 0, 0, 0, 0x20, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0
        ]
    );
    let entries = parse_res(&res);
    assert_eq!(entries[0].type_id, 0);
    assert_eq!(entries[0].name_id, 0);
    assert!(
        entries[0].data.is_empty(),
        "the marker entry carries no data"
    );
}

#[test]
fn every_icon_image_becomes_its_own_resource() {
    let res = build_res(&tiny_ico(), MANIFEST, &sample_version()).expect("the .ico is valid");
    let entries = parse_res(&res);
    let icons: Vec<&Entry> = entries.iter().filter(|e| e.type_id == RT_ICON).collect();

    assert_eq!(icons.len(), 2);
    assert_eq!(icons[0].name_id, 1, "ids start at 1, not 0");
    assert_eq!(icons[0].data, b"first-image-bytes");
    assert_eq!(icons[1].name_id, 2);
    assert_eq!(icons[1].data, b"second");
    assert!(icons.iter().all(|e| e.language == 0x0409));
}

#[test]
fn the_icon_group_points_at_those_resources() {
    let res = build_res(&tiny_ico(), MANIFEST, &sample_version()).expect("the .ico is valid");
    let entries = parse_res(&res);
    let group = entries
        .iter()
        .find(|e| e.type_id == RT_GROUP_ICON)
        .expect("there's an icon group");

    assert_eq!(
        group.name_id, 1,
        "Windows picks the lowest-numbered group as the app icon"
    );
    assert_eq!(&group.data[0..2], &0u16.to_le_bytes(), "reserved");
    assert_eq!(
        &group.data[2..4],
        &1u16.to_le_bytes(),
        "1 means icons, not cursors"
    );
    assert_eq!(&group.data[4..6], &2u16.to_le_bytes(), "two images");
    assert_eq!(
        group.data.len(),
        6 + 2 * 14,
        "a group entry is 14 bytes, two shorter than a file's"
    );

    // First entry: 16x16, sized like the image it points at, resource id 1.
    assert_eq!(group.data[6], 16);
    assert_eq!(group.data[7], 16);
    assert_eq!(
        u32::from_le_bytes(group.data[14..18].try_into().unwrap()),
        "first-image-bytes".len() as u32
    );
    assert_eq!(
        u16::from_le_bytes(group.data[18..20].try_into().unwrap()),
        1
    );
    // Second entry: 256x256 is written as 0, and it points at resource id 2.
    assert_eq!(group.data[20], 0);
    assert_eq!(
        u16::from_le_bytes(group.data[32..34].try_into().unwrap()),
        2
    );
}

#[test]
fn the_manifest_goes_in_verbatim_as_resource_one() {
    let res = build_res(&tiny_ico(), MANIFEST, &sample_version()).expect("the .ico is valid");
    let entries = parse_res(&res);
    let manifest = entries
        .iter()
        .find(|e| e.type_id == RT_MANIFEST)
        .expect("there's a manifest");

    assert_eq!(
        manifest.name_id, 1,
        "1 is CREATEPROCESS_MANIFEST_RESOURCE_ID"
    );
    assert_eq!(String::from_utf8(manifest.data.clone()).unwrap(), MANIFEST);
}

/// Walk the `VS_VERSIONINFO` tree the way Windows does: every node declares its own length, and a
/// node that miscounts leaves the reader in the middle of the next one.
fn version_children(node: &[u8]) -> Vec<&[u8]> {
    let value_len = u16::from_le_bytes(node[2..4].try_into().unwrap()) as usize;
    let is_text = u16::from_le_bytes(node[4..6].try_into().unwrap()) == 1;
    let value_bytes = if is_text { value_len * 2 } else { value_len };

    let mut offset = 6;
    while offset + 1 < node.len() && node[offset..offset + 2] != [0, 0] {
        offset += 2; // the UTF-16 key
    }
    offset = (offset + 2).next_multiple_of(4); // past the key's terminator, then aligned
    offset = (offset + value_bytes).next_multiple_of(4);

    let mut children = Vec::new();
    while offset < node.len() {
        let length = u16::from_le_bytes(node[offset..offset + 2].try_into().unwrap()) as usize;
        assert!(length >= 6, "a version node claims to be {length} bytes");
        assert!(
            offset + length <= node.len(),
            "a version node overruns its parent"
        );
        children.push(&node[offset..offset + length]);
        offset += length.next_multiple_of(4);
    }
    children
}

fn version_key(node: &[u8]) -> String {
    let units: Vec<u16> = node[6..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16(&units).expect("keys are valid UTF-16")
}

fn version_text_value(node: &[u8]) -> String {
    let value_len = u16::from_le_bytes(node[2..4].try_into().unwrap()) as usize;
    let key_end = 6 + (version_key(node).encode_utf16().count() + 1) * 2;
    let start = key_end.next_multiple_of(4);
    let units: Vec<u16> = node[start..start + value_len * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16(&units).expect("values are valid UTF-16")
}

#[test]
fn version_info_describes_the_build() {
    let res = build_res(&tiny_ico(), MANIFEST, &sample_version()).expect("the .ico is valid");
    let entries = parse_res(&res);
    let version = entries
        .iter()
        .find(|e| e.type_id == RT_VERSION)
        .expect("there's a version block");
    let root = &version.data;

    assert_eq!(
        u16::from_le_bytes(root[0..2].try_into().unwrap()) as usize,
        root.len(),
        "the root node's declared length covers the whole block"
    );
    assert_eq!(version_key(root), "VS_VERSION_INFO");

    // VS_FIXEDFILEINFO: signature, then 0.15.1.0 split across two DWORDs.
    let fixed_start = 6usize + ("VS_VERSION_INFO".len() + 1) * 2;
    let fixed = &root[fixed_start.next_multiple_of(4)..];
    assert_eq!(
        u32::from_le_bytes(fixed[0..4].try_into().unwrap()),
        0xFEEF_04BD
    );
    assert_eq!(u32::from_le_bytes(fixed[8..12].try_into().unwrap()), 15);
    assert_eq!(
        u32::from_le_bytes(fixed[12..16].try_into().unwrap()),
        1 << 16
    );

    let children = version_children(root);
    let keys: Vec<String> = children.iter().map(|c| version_key(c)).collect();
    assert_eq!(keys, ["StringFileInfo", "VarFileInfo"]);

    let table = version_children(children[0]);
    assert_eq!(version_key(table[0]), "040904B0", "US English in UTF-16");

    let strings: Vec<(String, String)> = version_children(table[0])
        .iter()
        .map(|node| (version_key(node), version_text_value(node)))
        .collect();
    assert!(strings.contains(&("FileVersion".to_string(), "0.15.1.0".to_string())));
    assert!(strings.contains(&("ProductName".to_string(), "Prvw".to_string())));
    assert!(strings.contains(&("OriginalFilename".to_string(), "prvw.exe".to_string())));
    assert!(
        strings.contains(&("InternalName".to_string(), "prvw".to_string())),
        "InternalName drops the extension"
    );
    assert!(
        strings.contains(&("CompanyName".to_string(), "Rymdskottkärra AB".to_string())),
        "non-ASCII survives the UTF-16 encoding: {strings:?}"
    );
}

#[test]
fn the_committed_app_icon_encodes() {
    let ico = app_icon();
    let res = build_res(&ico, MANIFEST, &sample_version()).expect("AppIcon.ico is valid");
    let entries = parse_res(&res);
    let icons: Vec<&Entry> = entries.iter().filter(|e| e.type_id == RT_ICON).collect();

    assert_eq!(icons.len(), 7, "16, 24, 32, 48, 64, 128, and 256");
    let total: usize = icons.iter().map(|e| e.data.len()).sum();
    assert!(
        total > 100_000,
        "the images should carry real pixels, got {total} bytes"
    );

    let group = entries.iter().find(|e| e.type_id == RT_GROUP_ICON).unwrap();
    let sizes: Vec<u8> = (0..icons.len()).map(|i| group.data[6 + i * 14]).collect();
    assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 0]);
}

#[test]
fn a_broken_ico_stops_the_build_with_a_readable_message() {
    let err = build_res(b"not an icon", MANIFEST, &sample_version()).unwrap_err();
    assert!(err.contains("isn't an .ico"), "got {err:?}");

    let mut truncated = tiny_ico();
    truncated.truncate(20);
    let err = build_res(&truncated, MANIFEST, &sample_version()).unwrap_err();
    assert!(
        err.contains("stops short") || err.contains("past the end"),
        "got {err:?}"
    );
}

/// The manifest is read by Windows before any of our code runs, so nothing at runtime can check
/// it and nothing on a Mac can either. These four declarations are the reason it exists, and
/// `longPathAware` in particular is what M1 step 10's path handling is built on.
#[test]
fn the_manifest_declares_what_windows_reads_before_startup() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/prvw.manifest");
    let manifest = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("couldn't read {}: {e}", path.display()));

    for needle in [
        "name=\"Microsoft.Windows.Common-Controls\" version=\"6.0.0.0\"",
        "<dpiAwareness xmlns=\"http://schemas.microsoft.com/SMI/2016/WindowsSettings\">PerMonitorV2<",
        "<longPathAware xmlns=\"http://schemas.microsoft.com/SMI/2016/WindowsSettings\">true<",
        "level=\"asInvoker\"",
    ] {
        assert!(
            manifest.contains(needle),
            "the manifest is missing {needle:?}"
        );
    }
    assert!(
        manifest.contains("version=\"__VERSION__\""),
        "the build script substitutes the app version into the manifest's assemblyIdentity"
    );
}
