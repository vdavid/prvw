//! Gain-map parameters as the app holds them: one shape whatever the file's metadata flavor
//! (ISO 21496-1, Ultra HDR XMP, or Apple's maker note), in ISO 21496-1's terms.

/// One color channel's gain-map parameters. Gains and headrooms are log2, gamma and offsets
/// linear, as ISO 21496-1 writes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelParams {
    /// log2 of the gain a map value of 0 stands for.
    pub min: f32,
    /// log2 of the gain a map value of 1 stands for.
    pub max: f32,
    /// The map's encoding gamma: a map value `v` is `pow(v, 1 / gamma)` of the way from `min`
    /// to `max`.
    pub gamma: f32,
    /// Added to the base rendition before the gain.
    pub base_offset: f32,
    /// Subtracted from the result after the gain.
    pub alternate_offset: f32,
}

impl Default for ChannelParams {
    /// ISO 21496-1's (and Ultra HDR's) defaults, where a field is optional.
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 0.0,
            gamma: 1.0,
            base_offset: 1.0 / 64.0,
            alternate_offset: 1.0 / 64.0,
        }
    }
}

/// Where a file's gain-map parameters came from. The first one a file carries wins, in this
/// order (`docs/specs/hdr-images.md`, section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GainMapSource {
    /// An ISO 21496-1 APP2 segment on the gain-map image.
    Iso21496,
    /// Ultra HDR v1's `hdrgm:` XMP on the gain-map image.
    UltraHdrXmp,
    /// Apple's maker-note headroom, with the map tagged as Apple's HDR gain map.
    Apple,
}

/// A gain map's parameters, whatever metadata they were read from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainMapParams {
    /// R, G, B. A single-channel map repeats the same parameters three times.
    pub channels: [ChannelParams; 3],
    /// log2 of the base rendition's headroom: 0 for an SDR base.
    pub base_headroom: f32,
    /// log2 of the alternate rendition's headroom.
    pub alternate_headroom: f32,
    /// Apply the gain in the base image's color space (`true`) or the alternate's.
    pub use_base_color_space: bool,
}

impl GainMapParams {
    /// Whether every number is one the shader can use: finite, and a positive gamma. Metadata
    /// that fails this is treated as no gain map at all.
    pub fn is_usable(&self) -> bool {
        self.base_headroom.is_finite()
            && self.alternate_headroom.is_finite()
            && self.channels.iter().all(|c| {
                [c.min, c.max, c.gamma, c.base_offset, c.alternate_offset]
                    .iter()
                    .all(|v| v.is_finite())
                    && c.gamma > 0.0
            })
    }
}
