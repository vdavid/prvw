//! Every persisted setting, and what each platform's UI owes it.
//!
//! `settings::Settings` stays the source of truth for what a setting *is*: its type, its
//! default, how it serializes. [`SettingKey`] is the source of truth for what a UI *owes* it,
//! and the two are held together by `settings::persistence`'s `every_settings_field_has_a_key`
//! test, which walks the serialized `Settings` and fails when a field has no key or a key names
//! a field that's gone. So the chain is: add a field, that test fails; add a key, every
//! platform's coverage match below stops compiling.
//!
//! Keys are declared in the order the UI presents them, panel by panel, so the table reads
//! like the settings window.

use super::{Coverage, Platform};

/// Which settings-window panel a key belongs to.
///
/// This is the product's intended grouping, shared by every platform, because the settings
/// window means the same thing everywhere. A platform that has to place something differently
/// for native reasons says so in its coverage arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    General,
    Zoom,
    Color,
    Raw,
    Slideshow,
    FileAssociations,
    /// Not a settings-window row at all: the menu and the keyboard are its only surfaces.
    None,
}

impl Panel {
    pub const fn name(self) -> &'static str {
        match self {
            Panel::General => "General",
            Panel::Zoom => "Zoom",
            Panel::Color => "Color",
            Panel::Raw => "RAW",
            Panel::Slideshow => "Slideshow",
            Panel::FileAssociations => "File associations",
            Panel::None => "Menu only",
        }
    }
}

/// The kind of control a platform has to put on screen. Native toolkits spell these
/// differently (`NSSwitch` and a Win32 checkbox, `NSSlider` and a trackbar), which is the
/// point: the registry names the job, each platform picks its own widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Toggle,
    Slider,
    /// One of a fixed set, like the sort column.
    Choice,
    /// A filesystem path, with whatever picker the platform uses.
    Path,
    /// A bespoke surface, described by the key's docs.
    Custom,
}

impl Control {
    pub const fn name(self) -> &'static str {
        match self {
            Control::Toggle => "toggle",
            Control::Slider => "slider",
            Control::Choice => "choice",
            Control::Path => "path",
            Control::Custom => "custom",
        }
    }
}

/// Declares the whole registry from one table: the enum, `ALL`, and every accessor.
///
/// One table means `ALL` can't drift from the variants, which matters because layer 2 and the
/// audits enumerate through it.
macro_rules! setting_keys {
    ($(
        $(#[$doc:meta])*
        $variant:ident {
            label: $label:literal,
            panel: $panel:ident,
            control: $control:ident,
            field: $field:literal,
        }
    )*) => {
        /// One variant per persisted setting. See the module docs for the guarantee.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum SettingKey {
            $( $(#[$doc])* $variant, )*
        }

        impl SettingKey {
            /// Every key, in the order the UI presents them.
            pub const ALL: &'static [SettingKey] = &[ $( SettingKey::$variant, )* ];

            /// Stable identifier for the parity table and tests: the variant's own name.
            pub const fn name(self) -> &'static str {
                match self { $( SettingKey::$variant => stringify!($variant), )* }
            }

            /// The user-facing label. Every platform's control wears this exact string, so the
            /// two builds can't drift into calling one setting two names.
            pub const fn label(self) -> &'static str {
                match self { $( SettingKey::$variant => $label, )* }
            }

            /// Which settings panel it belongs to.
            pub const fn panel(self) -> Panel {
                match self { $( SettingKey::$variant => Panel::$panel, )* }
            }

            /// The kind of control a platform owes it.
            pub const fn control(self) -> Control {
                match self { $( SettingKey::$variant => Control::$control, )* }
            }

            /// The `Settings` field it drives, as a dotted path into the settings JSON
            /// (`raw.clarity` for a nested one). Checked against the real struct by
            /// `settings::persistence`'s tests.
            pub const fn field(self) -> &'static str {
                match self { $( SettingKey::$variant => $field, )* }
            }
        }
    };
}

setting_keys! {
    // ── General panel ────────────────────────────────────────────────
    /// Check for updates at startup. macOS installs one; Windows opens the download page.
    AutoUpdate { label: "Auto-update", panel: General, control: Toggle, field: "auto_update", }
    /// Scroll wheel and trackpad zoom instead of navigating.
    ScrollToZoom { label: "Scroll to zoom", panel: General, control: Toggle, field: "scroll_to_zoom", }
    /// Decode adjacent images in the background so navigation is instant.
    PreloadNeighbors { label: "Preload next/prev images", panel: General, control: Toggle, field: "preload_neighbors", }
    /// Reserve a strip at the top of the window so the title bar doesn't cover the image.
    TitleBar { label: "Title bar", panel: General, control: Toggle, field: "title_bar", }

    // ── Zoom panel ───────────────────────────────────────────────────
    AutoFitWindow { label: "Auto-fit window", panel: Zoom, control: Toggle, field: "auto_fit_window", }
    EnlargeSmallImages { label: "Enlarge small images", panel: Zoom, control: Toggle, field: "enlarge_small_images", }

    // ── Color panel ──────────────────────────────────────────────────
    IccColorManagement { label: "ICC color management", panel: Color, control: Toggle, field: "icc_color_management", }
    ColorMatchDisplay { label: "Color match display", panel: Color, control: Toggle, field: "color_match_display", }
    RelativeColorimetric { label: "Relative colorimetric", panel: Color, control: Toggle, field: "use_relative_colorimetric", }

    // ── RAW panel ────────────────────────────────────────────────────
    RawDngOpcodeList1 { label: "DNG OpcodeList 1", panel: Raw, control: Toggle, field: "raw.dng_opcode_list_1", }
    RawDngOpcodeList2 { label: "DNG OpcodeList 2", panel: Raw, control: Toggle, field: "raw.dng_opcode_list_2", }
    RawDngOpcodeList3 { label: "DNG OpcodeList 3", panel: Raw, control: Toggle, field: "raw.dng_opcode_list_3", }
    RawBaselineExposure { label: "Baseline exposure", panel: Raw, control: Toggle, field: "raw.baseline_exposure", }
    RawBaselineExposureOffset { label: "Baseline exposure offset", panel: Raw, control: Slider, field: "raw.baseline_exposure_offset", }
    RawDcpHueSatMap { label: "DCP HueSatMap", panel: Raw, control: Toggle, field: "raw.dcp_hue_sat_map", }
    RawDcpLookTable { label: "DCP LookTable", panel: Raw, control: Toggle, field: "raw.dcp_look_table", }
    RawSaturationBoost { label: "Saturation boost", panel: Raw, control: Toggle, field: "raw.saturation_boost", }
    RawSaturationAmount { label: "Saturation amount", panel: Raw, control: Slider, field: "raw.saturation_boost_amount", }
    RawHighlightRecovery { label: "Highlight recovery", panel: Raw, control: Toggle, field: "raw.highlight_recovery", }
    RawDefaultToneCurve { label: "Default tone curve", panel: Raw, control: Toggle, field: "raw.default_tone_curve", }
    RawToneMidtoneAnchor { label: "Tone midtone anchor", panel: Raw, control: Slider, field: "raw.midtone_anchor", }
    RawDcpToneCurve { label: "DCP tone curve", panel: Raw, control: Toggle, field: "raw.dcp_tone_curve", }
    RawClarity { label: "Clarity (local contrast)", panel: Raw, control: Toggle, field: "raw.clarity", }
    RawClarityRadius { label: "Clarity radius", panel: Raw, control: Slider, field: "raw.clarity_radius", }
    RawClarityAmount { label: "Clarity amount", panel: Raw, control: Slider, field: "raw.clarity_amount", }
    RawCaptureSharpening { label: "Capture sharpening", panel: Raw, control: Toggle, field: "raw.capture_sharpening", }
    RawSharpenAmount { label: "Sharpening amount", panel: Raw, control: Slider, field: "raw.sharpen_amount", }
    RawChromaDenoise { label: "Chroma noise reduction", panel: Raw, control: Toggle, field: "raw.chroma_denoise", }
    RawLensCorrection { label: "Lens correction", panel: Raw, control: Toggle, field: "raw.lens_correction", }
    RawHdrOutput { label: "HDR / EDR output", panel: Raw, control: Toggle, field: "raw.hdr_output", }
    RawHdrGain { label: "HDR brightness gain", panel: Raw, control: Slider, field: "raw.hdr_gain", }
    /// Directory of `.dcp` profiles that wins over the bundled collection.
    CustomDcpDir { label: "Custom DCP directory", panel: Raw, control: Path, field: "custom_dcp_dir", }

    // ── Slideshow panel ──────────────────────────────────────────────
    SlideshowSeconds { label: "Time per image", panel: Slideshow, control: Slider, field: "slideshow_seconds", }
    SlideshowCrossfade { label: "Crossfade", panel: Slideshow, control: Toggle, field: "slideshow_crossfade", }
    SlideshowLoop { label: "Loop", panel: Slideshow, control: Toggle, field: "slideshow_loop", }

    // ── File associations panel ──────────────────────────────────────
    /// The list of image types Prvw opens by default. Backed by `previous_handlers`, which
    /// remembers what handled each type before, so turning a type off can restore it.
    FileAssociations { label: "File associations", panel: FileAssociations, control: Custom, field: "previous_handlers", }

    // ── Persisted, but the menu and keyboard are their only surfaces ──
    HistogramVisible { label: "Histogram", panel: None, control: Toggle, field: "histogram_visible", }
    ExifVisible { label: "Exif info", panel: None, control: Toggle, field: "exif_visible", }
    LoopNavigation { label: "Loop navigation", panel: None, control: Toggle, field: "loop_navigation", }
    SortBy { label: "Sort by", panel: None, control: Choice, field: "sort_by", }
}

impl SettingKey {
    /// What `platform`'s UI does with this setting.
    ///
    /// Each arm below is an exhaustive `match` with no `_`, so a new key breaks all three at
    /// once and every platform has to answer for it.
    pub const fn coverage(self, platform: Platform) -> Coverage {
        match platform {
            Platform::MacOs => self.macos_coverage(),
            Platform::Windows => self.windows_coverage(),
            Platform::Linux => self.linux_coverage(),
        }
    }

    /// macOS builds every setting: the panel rows in `settings/panels` and the feature panels,
    /// the menu-only ones in `menu/native.rs`.
    const fn macos_coverage(self) -> Coverage {
        match self {
            SettingKey::AutoUpdate
            | SettingKey::ScrollToZoom
            | SettingKey::PreloadNeighbors
            | SettingKey::TitleBar
            | SettingKey::AutoFitWindow
            | SettingKey::EnlargeSmallImages
            | SettingKey::IccColorManagement
            | SettingKey::ColorMatchDisplay
            | SettingKey::RelativeColorimetric
            | SettingKey::RawDngOpcodeList1
            | SettingKey::RawDngOpcodeList2
            | SettingKey::RawDngOpcodeList3
            | SettingKey::RawBaselineExposure
            | SettingKey::RawBaselineExposureOffset
            | SettingKey::RawDcpHueSatMap
            | SettingKey::RawDcpLookTable
            | SettingKey::RawSaturationBoost
            | SettingKey::RawSaturationAmount
            | SettingKey::RawHighlightRecovery
            | SettingKey::RawDefaultToneCurve
            | SettingKey::RawToneMidtoneAnchor
            | SettingKey::RawDcpToneCurve
            | SettingKey::RawClarity
            | SettingKey::RawClarityRadius
            | SettingKey::RawClarityAmount
            | SettingKey::RawCaptureSharpening
            | SettingKey::RawSharpenAmount
            | SettingKey::RawChromaDenoise
            | SettingKey::RawLensCorrection
            | SettingKey::RawHdrOutput
            | SettingKey::RawHdrGain
            | SettingKey::CustomDcpDir
            | SettingKey::SlideshowSeconds
            | SettingKey::SlideshowCrossfade
            | SettingKey::SlideshowLoop
            | SettingKey::FileAssociations
            | SettingKey::HistogramVisible
            | SettingKey::ExifVisible
            | SettingKey::LoopNavigation
            | SettingKey::SortBy => Coverage::Present,
        }
    }

    /// Windows builds every setting: the six tabs of `settings::windows` (whose `model` is the
    /// single list of what each page holds), and the menu-only ones through the menu bar. The
    /// keys are listed rather than caught by a `_` arm so this match keeps failing on every new
    /// setting, which is what makes each one an answered question here.
    const fn windows_coverage(self) -> Coverage {
        match self {
            SettingKey::TitleBar => Coverage::NotApplicable {
                reason: "A Win32 client area starts below the caption, so there's no title bar \
                         overlapping the image and nothing to reserve space for. macOS needs it \
                         because the window draws content behind a transparent title bar.",
            },
            SettingKey::AutoUpdate
            | SettingKey::ScrollToZoom
            | SettingKey::PreloadNeighbors
            | SettingKey::AutoFitWindow
            | SettingKey::EnlargeSmallImages
            | SettingKey::IccColorManagement
            | SettingKey::ColorMatchDisplay
            | SettingKey::RelativeColorimetric
            | SettingKey::RawDngOpcodeList1
            | SettingKey::RawDngOpcodeList2
            | SettingKey::RawDngOpcodeList3
            | SettingKey::RawBaselineExposure
            | SettingKey::RawBaselineExposureOffset
            | SettingKey::RawDcpHueSatMap
            | SettingKey::RawDcpLookTable
            | SettingKey::RawSaturationBoost
            | SettingKey::RawSaturationAmount
            | SettingKey::RawHighlightRecovery
            | SettingKey::RawDefaultToneCurve
            | SettingKey::RawToneMidtoneAnchor
            | SettingKey::RawDcpToneCurve
            | SettingKey::RawClarity
            | SettingKey::RawClarityRadius
            | SettingKey::RawClarityAmount
            | SettingKey::RawCaptureSharpening
            | SettingKey::RawSharpenAmount
            | SettingKey::RawChromaDenoise
            | SettingKey::RawLensCorrection
            | SettingKey::RawHdrOutput
            | SettingKey::RawHdrGain
            | SettingKey::CustomDcpDir
            | SettingKey::SlideshowSeconds
            | SettingKey::SlideshowCrossfade
            | SettingKey::SlideshowLoop
            | SettingKey::FileAssociations => Coverage::Present,
            // The menu and the keyboard are these four's only surfaces on every platform, so
            // no dialog tab claims them here either.
            SettingKey::HistogramVisible
            | SettingKey::ExifVisible
            | SettingKey::LoopNavigation
            | SettingKey::SortBy => Coverage::Present,
        }
    }

    /// Linux has neither a settings window nor a menu bar to reach these from, and gets no
    /// parity work in this effort (decision 4). The gaps are real, so they're `Missing` rather
    /// than waved away, and a Linux spec later is what closes them.
    const fn linux_coverage(self) -> Coverage {
        match self {
            SettingKey::TitleBar => Coverage::NotApplicable {
                reason: "Linux windows carry their decorations outside the surface Prvw draws \
                         into, so nothing covers the image and there's no strip to reserve.",
            },
            SettingKey::AutoUpdate
            | SettingKey::ScrollToZoom
            | SettingKey::PreloadNeighbors
            | SettingKey::AutoFitWindow
            | SettingKey::EnlargeSmallImages
            | SettingKey::IccColorManagement
            | SettingKey::ColorMatchDisplay
            | SettingKey::RelativeColorimetric
            | SettingKey::RawDngOpcodeList1
            | SettingKey::RawDngOpcodeList2
            | SettingKey::RawDngOpcodeList3
            | SettingKey::RawBaselineExposure
            | SettingKey::RawBaselineExposureOffset
            | SettingKey::RawDcpHueSatMap
            | SettingKey::RawDcpLookTable
            | SettingKey::RawSaturationBoost
            | SettingKey::RawSaturationAmount
            | SettingKey::RawHighlightRecovery
            | SettingKey::RawDefaultToneCurve
            | SettingKey::RawToneMidtoneAnchor
            | SettingKey::RawDcpToneCurve
            | SettingKey::RawClarity
            | SettingKey::RawClarityRadius
            | SettingKey::RawClarityAmount
            | SettingKey::RawCaptureSharpening
            | SettingKey::RawSharpenAmount
            | SettingKey::RawChromaDenoise
            | SettingKey::RawLensCorrection
            | SettingKey::RawHdrOutput
            | SettingKey::RawHdrGain
            | SettingKey::CustomDcpDir
            | SettingKey::SlideshowSeconds
            | SettingKey::SlideshowCrossfade
            | SettingKey::SlideshowLoop
            | SettingKey::FileAssociations
            | SettingKey::HistogramVisible
            | SettingKey::ExifVisible
            | SettingKey::LoopNavigation
            | SettingKey::SortBy => Coverage::Missing,
        }
    }

    /// What a platform's settings window owes, for [`super::Audit::mismatches`]. Menu-only
    /// keys are left out: they're the menu registry's business, not a panel's.
    pub fn panel_coverage(platform: Platform) -> impl Iterator<Item = (SettingKey, Coverage)> {
        SettingKey::ALL
            .iter()
            .filter(|key| !matches!(key.panel(), Panel::None))
            .map(move |key| (*key, key.coverage(platform)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique() {
        for (index, key) in SettingKey::ALL.iter().enumerate() {
            let duplicate = SettingKey::ALL[index + 1..]
                .iter()
                .find(|other| other.name() == key.name() || other.field() == key.field());
            assert!(duplicate.is_none(), "{} is declared twice", key.name());
        }
    }

    #[test]
    fn panel_coverage_skips_menu_only_keys() {
        let panel_keys: Vec<_> = SettingKey::panel_coverage(Platform::MacOs)
            .map(|(key, _)| key)
            .collect();
        assert!(panel_keys.contains(&SettingKey::AutoUpdate));
        assert!(!panel_keys.contains(&SettingKey::SortBy));
    }

    /// Sliders and choices need more than an on/off control, and a platform that renders one
    /// as a checkbox has silently lost the setting. Pin the ones that aren't toggles.
    #[test]
    fn non_toggle_controls_stay_non_toggles() {
        assert_eq!(SettingKey::SlideshowSeconds.control(), Control::Slider);
        assert_eq!(SettingKey::SortBy.control(), Control::Choice);
        assert_eq!(SettingKey::CustomDcpDir.control(), Control::Path);
        assert_eq!(SettingKey::FileAssociations.control(), Control::Custom);
    }
}
