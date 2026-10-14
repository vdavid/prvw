//! The bits of a gain-map JPEG's XMP that Prvw reads: Ultra HDR's `hdrgm:` parameters, Google's
//! `GContainer` directory, and Apple's auxiliary-image tag.
//!
//! Not an XML parser. The schema is a dozen attributes, each either a scalar or a per-channel
//! `rdf:Seq`, written as an attribute or as an element. A scanner that finds them by name,
//! resolving the namespace prefix the packet actually binds, reads every writer we've seen and
//! can't be tricked into anything worse than "no gain map". `ultrahdr-core` no longer parses
//! XMP, and the crate that does is AGPL (spec Findings, 2026-10-13).

use crate::color::hdr::gain_map::{ChannelParams, GainMapParams, GainMapSource};

const HDRGM_NS: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
const CONTAINER_NS: &str = "http://ns.google.com/photos/1.0/container/";
const ITEM_NS: &str = "http://ns.google.com/photos/1.0/container/item/";
/// Apple's tag for the MPF image that holds its HDR gain map (`apdi:AuxiliaryImageType`).
const APPLE_GAIN_MAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

/// Whether the packet declares the `hdrgm` namespace at all: the primary's "this is Ultra HDR"
/// signal, which carries only `hdrgm:Version`.
pub fn mentions_hdrgm(xmp: &str) -> bool {
    xmp.contains(HDRGM_NS)
}

/// Whether this is the XMP of Apple's HDR gain-map image.
pub fn is_apple_gain_map(xmp: &str) -> bool {
    xmp.contains(APPLE_GAIN_MAP_URN)
}

/// Read gain-map parameters from the gain-map image's `hdrgm` XMP, in either of its two
/// dialects:
///
/// - **Ultra HDR v1** (Pixel, Samsung, libultrahdr): headrooms as `HDRCapacityMin` and
///   `HDRCapacityMax`. Reported as [`GainMapSource::UltraHdrXmp`].
/// - **Apple's ISO 21496-1 rendering** (iOS 18 and later): the same namespace under another
///   prefix, ISO's `BaseHeadroom` and `AlternateHeadroom`, and `0`/`1` booleans. ImageIO calls it
///   the ISO gain map, so it's reported as [`GainMapSource::Iso21496`].
///
/// `None` when the packet has no parameters, when a required one (`GainMapMax`, the alternate
/// headroom) is missing or unreadable, or when the base rendition is the HDR one, which
/// libultrahdr (the reference implementation) rejects too: Ultra HDR doesn't pin down the
/// direction of the gain for it.
pub fn parse_hdrgm(xmp: &str) -> Option<(GainMapParams, GainMapSource)> {
    let prefix = prefix_for(xmp, HDRGM_NS).unwrap_or("hdrgm");
    let field = |name: &str| find_field(xmp, prefix, name);

    if let Some(Field::Scalar(base_is_hdr)) = field("BaseRenditionIsHDR")
        && matches!(
            base_is_hdr.trim().to_ascii_lowercase().as_str(),
            "true" | "1"
        )
    {
        log::debug!("Gain-map XMP with an HDR base rendition: not supported, showing it as SDR");
        return None;
    }
    let (source, base_name, alternate_name) = if field("AlternateHeadroom").is_some() {
        (GainMapSource::Iso21496, "BaseHeadroom", "AlternateHeadroom")
    } else {
        (
            GainMapSource::UltraHdrXmp,
            "HDRCapacityMin",
            "HDRCapacityMax",
        )
    };

    let per_channel = |name: &str, default: Option<f32>| -> Option<[f32; 3]> {
        match field(name) {
            Some(f) => f.per_channel(),
            None => default.map(|d| [d; 3]),
        }
    };
    let scalar = |name: &str, default: Option<f32>| -> Option<f32> {
        match field(name) {
            Some(Field::Scalar(s)) => parse_number(&s),
            Some(Field::Seq(_)) => None,
            None => default,
        }
    };

    let min = per_channel("GainMapMin", Some(0.0))?;
    let max = per_channel("GainMapMax", None)?;
    let gamma = per_channel("Gamma", Some(1.0))?;
    let offset_sdr = per_channel("OffsetSDR", Some(1.0 / 64.0))?;
    let offset_hdr = per_channel("OffsetHDR", Some(1.0 / 64.0))?;
    let capacity_min = scalar(base_name, Some(0.0))?;
    let capacity_max = scalar(alternate_name, None)?;

    let channels = std::array::from_fn(|c| ChannelParams {
        min: min[c],
        max: max[c],
        gamma: gamma[c],
        base_offset: offset_sdr[c],
        alternate_offset: offset_hdr[c],
    });
    let params = GainMapParams {
        channels,
        base_headroom: capacity_min,
        alternate_headroom: capacity_max,
        use_base_color_space: true,
        curve: crate::color::hdr::gain_map::GainCurve::Log2,
    };
    params.is_usable().then_some((params, source))
}

/// One `Container:Item` of a `GContainer` directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerItem {
    pub semantic: String,
    /// The item's length in bytes. The primary's is usually absent.
    pub length: Option<usize>,
}

/// The most `GContainer` items read, and the most `Item:Semantic` names looked at (valid or
/// not) before the scan gives up. Real files list two or three items.
const MAX_CONTAINER_ITEMS: usize = 16;
const MAX_SEMANTIC_HITS: usize = 64;

/// The `GContainer` directory in the primary's XMP: the media items the file carries, in file
/// order. Empty when there's none.
pub fn container_items(xmp: &str) -> Vec<ContainerItem> {
    if prefix_for(xmp, CONTAINER_NS).is_none() {
        return Vec::new();
    }
    let item = prefix_for(xmp, ITEM_NS).unwrap_or("Item");
    let semantic_attr = format!("{item}:Semantic");
    let mut items = Vec::new();
    let mut from = 0;
    let mut hits = 0;
    while let Some(found) = find_name(xmp, &semantic_attr, from) {
        hits += 1;
        if hits > MAX_SEMANTIC_HITS || items.len() == MAX_CONTAINER_ITEMS {
            log::debug!("GContainer directory past {MAX_CONTAINER_ITEMS} items; reading no more");
            break;
        }
        from = found + semantic_attr.len();
        // The attributes of the element this one sits on.
        let Some(open) = xmp[..found].rfind('<') else {
            continue;
        };
        let close = xmp[found..].find('>').map_or(xmp.len(), |i| found + i);
        let tag = &xmp[open..close];
        let Some(semantic) = attribute(tag, &semantic_attr) else {
            continue;
        };
        let length = attribute(tag, &format!("{item}:Length")).and_then(|l| l.trim().parse().ok());
        items.push(ContainerItem {
            semantic: semantic.to_string(),
            length,
        });
    }
    items
}

#[derive(Debug, PartialEq)]
enum Field {
    Scalar(String),
    Seq(Vec<String>),
}

impl Field {
    /// Three per-channel values: a scalar or a one-item sequence repeats, a three-item sequence
    /// is R, G, B, and anything else (or anything that isn't a number) is unreadable.
    fn per_channel(&self) -> Option<[f32; 3]> {
        match self {
            Field::Scalar(s) => parse_number(s).map(|v| [v; 3]),
            Field::Seq(items) => match items.as_slice() {
                [one] => parse_number(one).map(|v| [v; 3]),
                [r, g, b] => Some([parse_number(r)?, parse_number(g)?, parse_number(b)?]),
                _ => None,
            },
        }
    }
}

fn parse_number(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

/// The prefix the packet binds `namespace` to (`xmlns:hdrgm="…"`), so a writer that picked
/// another prefix is still read.
fn prefix_for<'a>(xmp: &'a str, namespace: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = xmp[from..].find("xmlns:") {
        let start = from + i + "xmlns:".len();
        from = start;
        let rest = &xmp[start..];
        let eq = rest.find('=')?;
        let prefix = rest[..eq].trim();
        let value = rest[eq + 1..].trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let value = &value[1..];
        if let Some(end) = value.find(quote)
            && &value[..end] == namespace
            && !prefix.is_empty()
        {
            return Some(prefix);
        }
    }
    None
}

/// The next place `qname` appears as a whole name (not inside a longer one, not a closing tag),
/// at or after `from`.
fn find_name(xmp: &str, qname: &str, from: usize) -> Option<usize> {
    let mut from = from;
    while let Some(i) = xmp.get(from..)?.find(qname) {
        let at = from + i;
        from = at + qname.len();
        let before = xmp[..at].chars().next_back();
        let after = xmp[at + qname.len()..].chars().next();
        let starts_name = matches!(before, Some(c) if c.is_whitespace() || c == '<');
        let ends_name =
            matches!(after, Some(c) if c.is_whitespace() || c == '=' || c == '>' || c == '/');
        if starts_name && ends_name {
            return Some(at);
        }
    }
    None
}

/// `prefix:name` as an attribute (`hdrgm:Gamma="1"`) or an element (`<hdrgm:Gamma>1</…>`,
/// possibly holding an `rdf:Seq`). The first occurrence wins.
fn find_field(xmp: &str, prefix: &str, name: &str) -> Option<Field> {
    let qname = format!("{prefix}:{name}");
    let at = find_name(xmp, &qname, 0)?;
    let after = &xmp[at + qname.len()..];
    let is_element = xmp[..at].ends_with('<');
    if !is_element {
        let value = after.trim_start().strip_prefix('=')?.trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = &value[1..];
        return Some(Field::Scalar(value[..value.find(quote)?].to_string()));
    }
    // An element: skip its own attributes, then read up to the matching close tag.
    let open_end = after.find('>')?;
    if after[..open_end].ends_with('/') {
        return None;
    }
    let body = &after[open_end + 1..];
    let body = &body[..body.find(&format!("</{qname}"))?];
    if !body.contains("<") {
        return Some(Field::Scalar(body.to_string()));
    }
    let mut items = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(":li") {
        let li = &rest[i..];
        let start = li.find('>')? + 1;
        let end = li[start..].find('<')? + start;
        items.push(li[start..end].to_string());
        rest = &li[end..];
        // Step past this item's close tag, which also contains ":li".
        rest = &rest[rest.find('>')? + 1..];
    }
    Some(Field::Seq(items))
}

/// The value of attribute `qname` inside one start tag.
fn attribute<'a>(tag: &'a str, qname: &str) -> Option<&'a str> {
    let at = find_name(tag, qname, 0)?;
    let value = tag[at + qname.len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &value[1..];
    Some(&value[..value.find(quote)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(xmp: &str) -> Option<GainMapParams> {
        parse_hdrgm(xmp).map(|(p, _)| p)
    }

    const ATTRIBUTE_FORM: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
      <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
        <rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0" hdrgm:GainMapMin="-0.5" hdrgm:GainMapMax="2.25"
          hdrgm:Gamma="1.1" hdrgm:OffsetSDR="0.01" hdrgm:OffsetHDR="0.02"
          hdrgm:HDRCapacityMin="0.1" hdrgm:HDRCapacityMax="2.5"
          hdrgm:BaseRenditionIsHDR="False"/>
      </rdf:RDF>
    </x:xmpmeta>"#;

    #[test]
    fn scalar_attributes_fill_every_channel() {
        let p = params(ATTRIBUTE_FORM).expect("valid hdrgm");
        for c in p.channels {
            assert_eq!(
                c,
                ChannelParams {
                    min: -0.5,
                    max: 2.25,
                    gamma: 1.1,
                    base_offset: 0.01,
                    alternate_offset: 0.02
                }
            );
        }
        assert_eq!(p.base_headroom, 0.1);
        assert_eq!(p.alternate_headroom, 2.5);
        assert!(p.use_base_color_space);
    }

    #[test]
    fn per_channel_sequences_are_r_g_b() {
        let xmp = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:Version="1.0" hdrgm:HDRCapacityMax="3">
          <hdrgm:GainMapMax><rdf:Seq><rdf:li>1.5</rdf:li><rdf:li>2</rdf:li><rdf:li>2.5</rdf:li></rdf:Seq></hdrgm:GainMapMax>
          <hdrgm:Gamma>
            <rdf:Seq>
              <rdf:li>1</rdf:li>
              <rdf:li>0.9</rdf:li>
              <rdf:li>0.8</rdf:li>
            </rdf:Seq>
          </hdrgm:Gamma>
          <hdrgm:GainMapMin>0.25</hdrgm:GainMapMin>
        </rdf:Description>"#;
        let p = params(xmp).expect("valid hdrgm");
        assert_eq!(p.channels.map(|c| c.max), [1.5, 2.0, 2.5]);
        assert_eq!(p.channels.map(|c| c.gamma), [1.0, 0.9, 0.8]);
        assert_eq!(p.channels.map(|c| c.min), [0.25; 3]);
        assert_eq!(p.alternate_headroom, 3.0);
    }

    /// Ultra HDR's defaults: min 0, gamma 1, offsets 1/64, capacity min 0.
    #[test]
    fn missing_optional_fields_take_the_spec_defaults() {
        let xmp = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:GainMapMax="3" hdrgm:HDRCapacityMax="3"/>"#;
        let p = params(xmp).expect("valid hdrgm");
        assert_eq!(
            p.channels[0],
            ChannelParams {
                max: 3.0,
                ..ChannelParams::default()
            }
        );
        assert_eq!(p.base_headroom, 0.0);
    }

    #[test]
    fn a_missing_required_field_is_no_gain_map() {
        let no_max = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:Version="1.0" hdrgm:HDRCapacityMax="3"/>"#;
        assert_eq!(parse_hdrgm(no_max), None);
        let no_capacity = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:Version="1.0" hdrgm:GainMapMax="3"/>"#;
        assert_eq!(parse_hdrgm(no_capacity), None);
        // The primary's XMP: only the version, which says "look for the map", not "here it is".
        let primary = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:Version="1.0"/>"#;
        assert!(mentions_hdrgm(primary));
        assert_eq!(parse_hdrgm(primary), None);
    }

    #[test]
    fn an_hdr_base_rendition_is_rejected_like_libultrahdr() {
        let xmp = ATTRIBUTE_FORM.replace(
            r#"BaseRenditionIsHDR="False""#,
            r#"BaseRenditionIsHDR="True""#,
        );
        assert_eq!(parse_hdrgm(&xmp), None);
    }

    #[test]
    fn unreadable_values_are_no_gain_map() {
        for (from, to) in [
            (r#"GainMapMax="2.25""#, r#"GainMapMax="lots""#),
            (r#"Gamma="1.1""#, r#"Gamma="0""#),
            (r#"Gamma="1.1""#, r#"Gamma="-1""#),
            (r#"GainMapMax="2.25""#, r#"GainMapMax="inf""#),
        ] {
            let xmp = ATTRIBUTE_FORM.replace(from, to);
            assert_eq!(parse_hdrgm(&xmp), None, "{to}");
        }
    }

    /// The namespace, not the literal "hdrgm", is what identifies the schema.
    #[test]
    fn another_prefix_bound_to_the_namespace_is_read() {
        // The namespace URI says "hdr-gain-map", so only the prefix changes.
        let xmp = ATTRIBUTE_FORM.replace("hdrgm", "gm");
        assert!(!xmp.contains("hdrgm:"));
        assert_eq!(params(&xmp).map(|p| p.alternate_headroom), Some(2.5));
    }

    /// `hdrgm:GainMapMaxima` is not `hdrgm:GainMapMax`.
    #[test]
    fn a_longer_name_does_not_match_a_shorter_one() {
        let xmp = r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
            hdrgm:GainMapMaxima="9" hdrgm:GainMapMax="3" hdrgm:HDRCapacityMax="3"/>"#;
        assert_eq!(params(xmp).map(|p| p.channels[0].max), Some(3.0));
    }

    const CONTAINER: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
      <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
        <rdf:Description xmlns:Container="http://ns.google.com/photos/1.0/container/"
            xmlns:Item="http://ns.google.com/photos/1.0/container/item/"
            xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0">
          <Container:Directory>
            <rdf:Seq>
              <rdf:li rdf:parseType="Resource">
                <Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/>
              </rdf:li>
              <rdf:li rdf:parseType="Resource">
                <Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="12345"/>
              </rdf:li>
            </rdf:Seq>
          </Container:Directory>
        </rdf:Description>
      </rdf:RDF>
    </x:xmpmeta>"#;

    #[test]
    fn a_gcontainer_directory_lists_its_items_in_order() {
        assert_eq!(
            container_items(CONTAINER),
            vec![
                ContainerItem {
                    semantic: "Primary".into(),
                    length: None
                },
                ContainerItem {
                    semantic: "GainMap".into(),
                    length: Some(12345)
                },
            ]
        );
        assert!(container_items(ATTRIBUTE_FORM).is_empty());
    }

    /// A hostile packet can't make the directory scan quadratic: each `Item:Semantic` hit looks
    /// back for its tag's `<`, so the scan stops after a few times what real files carry (2–3).
    #[test]
    fn a_gcontainer_directory_is_read_up_to_a_cap() {
        let item = r#"<Container:Item Item:Semantic="GainMap" Item:Length="1"/>"#;
        let many = CONTAINER.replace(
            r#"<Container:Directory>"#,
            &format!("<Container:Directory>{}", item.repeat(500)),
        );
        assert_eq!(container_items(&many).len(), MAX_CONTAINER_ITEMS);
        let bare = CONTAINER.replace(
            r#"<Container:Directory>"#,
            &format!("<Container:Directory>{}", " Item:Semantic".repeat(50_000)),
        );
        assert!(container_items(&bare).is_empty());
    }

    /// What iOS 18 writes on its gain-map image (this is the iPhone Air fixture's packet): ISO
    /// 21496-1's fields, named `BaseHeadroom` and `AlternateHeadroom`, in Adobe's `hdrgm`
    /// namespace under another prefix, with `0`/`1` for booleans. ImageIO reports it as the ISO
    /// gain map, and so do we.
    #[test]
    fn apples_iso_fields_in_xmp_read_as_iso_21496() {
        let xmp = r#"<rdf:Description rdf:about=""
            xmlns:apdi="http://ns.apple.com/pixeldatainfo/1.0/"
            xmlns:HDRToneMap="http://ns.adobe.com/hdr-gain-map/1.0/"
            xmlns:HDRGainMap="http://ns.apple.com/HDRGainMap/1.0/">
         <apdi:AuxiliaryImageType>urn:com:apple:photo:2020:aux:hdrgainmap</apdi:AuxiliaryImageType>
         <HDRToneMap:Version>1</HDRToneMap:Version>
         <HDRToneMap:GainMapMax>1.767799</HDRToneMap:GainMapMax>
         <HDRToneMap:OffsetHDR>0.000010</HDRToneMap:OffsetHDR>
         <HDRToneMap:Gamma>0.608887</HDRToneMap:Gamma>
         <HDRToneMap:OffsetSDR>0.000010</HDRToneMap:OffsetSDR>
         <HDRToneMap:BaseRenditionIsHDR>0</HDRToneMap:BaseRenditionIsHDR>
         <HDRToneMap:BaseHeadroom>0.000000</HDRToneMap:BaseHeadroom>
         <HDRToneMap:AlternateHeadroom>1.767799</HDRToneMap:AlternateHeadroom>
         <HDRToneMap:GainMapMin>0.000178</HDRToneMap:GainMapMin>
         <HDRGainMap:HDRGainMapHeadroom>3.405341</HDRGainMap:HDRGainMapHeadroom>
        </rdf:Description>"#;
        let (p, source) = parse_hdrgm(xmp).expect("Apple's ISO gain map");
        assert_eq!(source, GainMapSource::Iso21496);
        assert_eq!(p.channels[0].max, 1.767799);
        assert_eq!(p.channels[2].gamma, 0.608887);
        assert_eq!(p.channels[1].base_offset, 0.00001);
        assert_eq!((p.base_headroom, p.alternate_headroom), (0.0, 1.767799));

        let base_is_hdr = xmp.replace(
            "<HDRToneMap:BaseRenditionIsHDR>0<",
            "<HDRToneMap:BaseRenditionIsHDR>1<",
        );
        assert_eq!(parse_hdrgm(&base_is_hdr), None);
    }

    #[test]
    fn the_apple_gain_map_tag_is_recognized() {
        let xmp = r#"<rdf:Description xmlns:apdi="http://ns.apple.com/pixeldatainfo/1.0/"
            apdi:AuxiliaryImageType="urn:com:apple:photo:2020:aux:hdrgainmap"/>"#;
        assert!(is_apple_gain_map(xmp));
        assert!(!is_apple_gain_map(ATTRIBUTE_FORM));
    }

    /// The packet is file bytes: every truncation must answer, never panic.
    #[test]
    fn truncated_packets_never_panic() {
        for packet in [ATTRIBUTE_FORM, CONTAINER] {
            for (end, _) in packet.char_indices() {
                let cut = &packet[..end];
                let _ = parse_hdrgm(cut);
                let _ = container_items(cut);
            }
        }
    }
}
