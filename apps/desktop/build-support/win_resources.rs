//! Builds the Windows `.res` blob that `build.rs` hands to the linker.
//!
//! A Windows executable carries its icon, its application manifest, and its version info as
//! linker input, the way a macOS app carries them in its bundle. The usual way to produce that
//! input is a `.rc` script compiled by `rc.exe` or `llvm-rc`, neither of which exists on a Mac,
//! and both of which would make the icon quietly vanish from a cross-compiled build. The `.res`
//! container is simple enough to write directly, so we do: `link.exe` and `lld-link` both take a
//! `.res` as a positional input and convert it themselves.
//!
//! `build.rs` and `tests/win_resources.rs` both `#[path]`-include this file, so the encoders get
//! real test coverage on every platform even though only a Windows build links the result.
//!
//! Formats: the `.res` container and `RT_GROUP_ICON` are documented under "Resource file format"
//! and `GRPICONDIR` in the Windows SDK; the version blob is `VS_VERSIONINFO`.

/// Resource type ordinals, from `winuser.h`.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;

/// The one resource id Windows looks for by convention: the lowest-numbered `RT_GROUP_ICON` is
/// the executable's icon, and `CREATEPROCESS_MANIFEST_RESOURCE_ID` is 1 for an `.exe`.
const PRIMARY_ID: u16 = 1;

/// US English, the language every string in here is written in.
const LANG_EN_US: u16 = 0x0409;

/// The Unicode "code page" a `VS_VERSIONINFO` string table names: 1200, meaning UTF-16.
const CODEPAGE_UNICODE: u16 = 0x04B0;

/// `MOVEABLE | PURE | DISCARDABLE`, the flags `rc.exe` gives an icon or a version block. Modern
/// Windows ignores them; linkers still expect the field to look sane.
const MEMORY_FLAGS: u16 = 0x1030;

/// The version fields shown in Explorer's Properties → Details, and read by signing and installer
/// tooling later on.
pub struct VersionInfo {
    pub version: [u16; 4],
    pub company: String,
    pub description: String,
    pub product: String,
    pub copyright: String,
    pub file_name: String,
}

/// Assemble the whole `.res`: every icon image, the icon group that indexes them, the manifest,
/// and the version block.
///
/// `ico` is the contents of an `.ico` file. Returns an error only when that file is malformed,
/// which means the committed artwork is broken and the build should stop.
pub fn build_res(ico: &[u8], manifest: &str, version: &VersionInfo) -> Result<Vec<u8>, String> {
    let images = parse_ico(ico)?;

    // A `.res` opens with an empty entry that marks the file as 32-bit rather than 16-bit. Its
    // first 16 bytes are the magic a linker matches on, so type and name are both ordinal 0.
    let mut out = res_entry(0, 0, 0, 0, &[]);

    for (index, image) in images.iter().enumerate() {
        let id = icon_image_id(index);
        out.extend_from_slice(&res_entry(
            RT_ICON,
            id,
            MEMORY_FLAGS,
            LANG_EN_US,
            image.data,
        ));
    }
    out.extend_from_slice(&res_entry(
        RT_GROUP_ICON,
        PRIMARY_ID,
        MEMORY_FLAGS,
        LANG_EN_US,
        &group_icon_directory(&images),
    ));
    out.extend_from_slice(&res_entry(
        RT_MANIFEST,
        PRIMARY_ID,
        MEMORY_FLAGS,
        LANG_EN_US,
        manifest.as_bytes(),
    ));
    out.extend_from_slice(&res_entry(
        RT_VERSION,
        PRIMARY_ID,
        MEMORY_FLAGS,
        LANG_EN_US,
        &version_block(version),
    ));
    Ok(out)
}

/// The `RT_ICON` id of the `index`-th image in the `.ico`. Ids start at 1 because 0 is not a valid
/// resource ordinal, and the group directory refers to images by exactly these ids.
fn icon_image_id(index: usize) -> u16 {
    index as u16 + 1
}

/// One image inside an `.ico`, as its directory describes it. Width and height are the on-disk
/// bytes, where 0 means 256.
struct IconImage<'a> {
    width: u8,
    height: u8,
    colors: u8,
    planes: u16,
    bits_per_pixel: u16,
    data: &'a [u8],
}

fn parse_ico(ico: &[u8]) -> Result<Vec<IconImage<'_>>, String> {
    if ico.len() < 6 {
        return Err("the .ico is too short to hold a directory".to_string());
    }
    if read_u16(ico, 0) != 0 || read_u16(ico, 2) != 1 {
        return Err("that file isn't an .ico (bad reserved field or image type)".to_string());
    }
    let count = read_u16(ico, 4) as usize;
    if count == 0 {
        return Err("the .ico holds no images".to_string());
    }

    let mut images = Vec::with_capacity(count);
    for index in 0..count {
        let entry = 6 + index * 16;
        if entry + 16 > ico.len() {
            return Err(format!("the .ico directory stops short at image {index}"));
        }
        let size = read_u32(ico, entry + 8) as usize;
        let offset = read_u32(ico, entry + 12) as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| format!("image {index} of the .ico overflows the file"))?;
        if end > ico.len() {
            return Err(format!(
                "image {index} of the .ico runs past the end of the file"
            ));
        }
        images.push(IconImage {
            width: ico[entry],
            height: ico[entry + 1],
            colors: ico[entry + 2],
            planes: read_u16(ico, entry + 4),
            bits_per_pixel: read_u16(ico, entry + 6),
            data: &ico[offset..end],
        });
    }
    Ok(images)
}

/// The `GRPICONDIR` Windows reads to pick a size: the same directory the `.ico` file carries, with
/// each entry's file offset replaced by the `RT_ICON` resource id holding those bytes.
fn group_icon_directory(images: &[IconImage<'_>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + images.len() * 14);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon, 2 = cursor
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (index, image) in images.iter().enumerate() {
        out.push(image.width);
        out.push(image.height);
        out.push(image.colors);
        out.push(0); // reserved
        out.extend_from_slice(&image.planes.to_le_bytes());
        out.extend_from_slice(&image.bits_per_pixel.to_le_bytes());
        out.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&icon_image_id(index).to_le_bytes());
    }
    out
}

/// One `.res` entry: a header naming the resource by two ordinals, then its bytes. Both the header
/// and the data are padded to a four-byte boundary, and `HeaderSize` counts that padding.
fn res_entry(type_id: u16, name_id: u16, memory_flags: u16, language: u16, data: &[u8]) -> Vec<u8> {
    let mut header = Vec::with_capacity(32);
    header.extend_from_slice(&(data.len() as u32).to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes()); // header size, filled in below
    header.extend_from_slice(&0xFFFFu16.to_le_bytes()); // an ordinal type follows, not a name
    header.extend_from_slice(&type_id.to_le_bytes());
    header.extend_from_slice(&0xFFFFu16.to_le_bytes());
    header.extend_from_slice(&name_id.to_le_bytes());
    pad_to_four(&mut header);
    header.extend_from_slice(&0u32.to_le_bytes()); // data version
    header.extend_from_slice(&memory_flags.to_le_bytes());
    header.extend_from_slice(&language.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes()); // version
    header.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    let header_size = header.len() as u32;
    header[4..8].copy_from_slice(&header_size.to_le_bytes());

    header.extend_from_slice(data);
    pad_to_four(&mut header);
    header
}

/// The value a version block carries, if any.
enum BlockValue<'a> {
    None,
    /// Counted in UTF-16 code units, including the terminator.
    Text(&'a str),
    /// Counted in bytes.
    Binary(&'a [u8]),
}

/// One node of the `VS_VERSIONINFO` tree: a length, a typed value, a UTF-16 key, then children.
///
/// Every node ends four-byte aligned, which is why no child needs padding in front of it and why
/// the declared length never has to exclude trailing padding.
fn version_node(key: &str, value: BlockValue<'_>, children: &[Vec<u8>]) -> Vec<u8> {
    let (value_bytes, value_len, is_text) = match value {
        BlockValue::None => (Vec::new(), 0u16, true),
        BlockValue::Text(text) => {
            let bytes = utf16_with_null(text);
            let units = (bytes.len() / 2) as u16;
            (bytes, units, true)
        }
        BlockValue::Binary(bytes) => (bytes.to_vec(), bytes.len() as u16, false),
    };

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // total length, filled in below
    out.extend_from_slice(&value_len.to_le_bytes());
    out.extend_from_slice(&u16::from(is_text).to_le_bytes());
    out.extend_from_slice(&utf16_with_null(key));
    pad_to_four(&mut out);
    out.extend_from_slice(&value_bytes);
    pad_to_four(&mut out);
    for child in children {
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out
}

/// `VS_FIXEDFILEINFO`: the numeric version Windows compares, as opposed to the strings it shows.
fn fixed_file_info(version: [u16; 4]) -> Vec<u8> {
    let most = (u32::from(version[0]) << 16) | u32::from(version[1]);
    let least = (u32::from(version[2]) << 16) | u32::from(version[3]);
    let fields: [u32; 13] = [
        0xFEEF04BD,  // signature
        0x0001_0000, // struct version 1.0
        most,        // file version, high and low halves
        least,
        most, // product version, the same for us
        least,
        0x3F, // file flags mask: every flag below is meaningful
        0,    // file flags: not a debug, patched, or prerelease build
        0x04, // VOS__WINDOWS32
        0x01, // VFT_APP
        0,    // no subtype
        0,    // file date, which nobody sets
        0,
    ];
    fields.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn version_block(info: &VersionInfo) -> Vec<u8> {
    let version_text = format!(
        "{}.{}.{}.{}",
        info.version[0], info.version[1], info.version[2], info.version[3]
    );
    // InternalName is the name without its extension, OriginalFilename keeps it.
    let internal_name = info.file_name.trim_end_matches(".exe");
    // Alphabetical, as every Windows tool writes them.
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", info.company.as_str()),
        ("FileDescription", info.description.as_str()),
        ("FileVersion", version_text.as_str()),
        ("InternalName", internal_name),
        ("LegalCopyright", info.copyright.as_str()),
        ("OriginalFilename", info.file_name.as_str()),
        ("ProductName", info.product.as_str()),
        ("ProductVersion", version_text.as_str()),
    ]
    .iter()
    .map(|(key, value)| version_node(key, BlockValue::Text(value), &[]))
    .collect();

    let table_key = format!("{LANG_EN_US:04X}{CODEPAGE_UNICODE:04X}");
    let string_table = version_node(&table_key, BlockValue::None, &strings);
    let string_file_info = version_node("StringFileInfo", BlockValue::None, &[string_table]);

    let mut translation = Vec::with_capacity(4);
    translation.extend_from_slice(&LANG_EN_US.to_le_bytes());
    translation.extend_from_slice(&CODEPAGE_UNICODE.to_le_bytes());
    let var = version_node("Translation", BlockValue::Binary(&translation), &[]);
    let var_file_info = version_node("VarFileInfo", BlockValue::None, &[var]);

    version_node(
        "VS_VERSION_INFO",
        BlockValue::Binary(&fixed_file_info(info.version)),
        &[string_file_info, var_file_info],
    )
}

fn utf16_with_null(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

fn pad_to_four(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
