//! Settings persistence.
//!
//! Loads/saves user preferences from the app data directory:
//! - Production: `~/Library/Application Support/com.veszelovszki.prvw/settings.json`
//! - Dev/test: override with `PRVW_DATA_DIR` env var
//!
//! The settings file is the source of truth — no in-memory cache or Arc/Mutex needed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::decoding::RawPipelineFlags;

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub auto_update: bool,

    #[serde(default = "default_true")]
    pub auto_fit_window: bool,

    #[serde(default)]
    pub enlarge_small_images: bool,

    #[serde(default = "default_true")]
    pub icc_color_management: bool,

    #[serde(default = "default_true")]
    pub color_match_display: bool,

    #[serde(default)]
    pub use_relative_colorimetric: bool,

    /// When true, scroll wheel/touchpad zooms the image. When false, scroll navigates images.
    #[serde(default)]
    pub scroll_to_zoom: bool,

    /// When true, the preloader eagerly decodes neighbouring images into the
    /// cache so navigation is instant. When false, only the currently
    /// displayed image is loaded — useful for benchmarking a cold-start
    /// single-image load without concurrent work interfering with the
    /// pipeline stage timings.
    #[serde(default = "default_true")]
    pub preload_neighbors: bool,

    /// When true, reserve 59px at the top so the title bar doesn't cover the image.
    #[serde(default = "default_true")]
    pub title_bar: bool,

    /// When true, show the histogram overlay in the top-right corner.
    /// Toggled with View → Histogram or the bare H key.
    #[serde(default)]
    pub histogram_visible: bool,

    /// When true, show the EXIF info overlay below the histogram (or in
    /// its place when the histogram is hidden). Toggled with View → Exif
    /// info or the bare E key. Hidden anyway when the current image has
    /// no EXIF data.
    #[serde(default)]
    pub exif_visible: bool,

    /// When true, navigation wraps around at the directory boundary:
    /// Next at the last image jumps to the first, Previous at the first
    /// jumps to the last. Toggled with Navigate → Loop navigation or the
    /// bare L key.
    #[serde(default)]
    pub loop_navigation: bool,

    /// Column to sort the directory list by. All comparators are ascending.
    /// Toggled via View → Sort by → {Name | Date | File type}.
    #[serde(default)]
    pub sort_by: crate::navigation::SortBy,

    /// Seconds each image stays on screen during a slideshow. Clamped to
    /// 1..=30 (`slideshow::MIN_SECONDS`..=`slideshow::MAX_SECONDS`). Driven by
    /// Settings → Slideshow → "Time per image" and the `[` / `]` speed keys.
    #[serde(default = "default_slideshow_seconds")]
    pub slideshow_seconds: u32,

    /// When true, slideshow advances cross-fade between images (300 ms)
    /// instead of cutting. Settings → Slideshow → "Crossfade".
    #[serde(default = "default_true")]
    pub slideshow_crossfade: bool,

    /// When true, a slideshow wraps from the last image back to the first
    /// instead of stopping. Settings → Slideshow → "Loop". Independent of
    /// `loop_navigation` (which governs manual Next/Previous wrapping).
    #[serde(default = "default_true")]
    pub slideshow_loop: bool,

    /// Previous default handler for each UTI before Prvw claimed it.
    /// Used to restore associations when the user turns off a file type toggle.
    /// Keys are UTIs (e.g., "public.jpeg"), values are bundle IDs (e.g., "com.apple.Preview").
    #[serde(default)]
    pub previous_handlers: HashMap<String, String>,

    /// Per-stage toggles for the RAW decode pipeline. Defaults match today's
    /// production behavior; flipping any flag off short-circuits that stage
    /// (see `decoding::RawPipelineFlags` and `decoding::raw::decode`). The
    /// Settings → RAW panel drives these interactively.
    #[serde(default)]
    pub raw: RawPipelineFlags,

    /// Optional user-provided directory of `.dcp` profiles. When set and
    /// non-empty, wins over the bundled collection and Adobe Camera Raw's
    /// directory. Exposed in Settings → RAW → "Custom DCP directory".
    /// Stored as a string (not a `PathBuf`) because the settings JSON is
    /// user-editable and consistent serde string handling is clearest.
    #[serde(default)]
    pub custom_dcp_dir: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_slideshow_seconds() -> u32 {
    crate::slideshow::DEFAULT_SECONDS
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_update: true,
            auto_fit_window: true,
            enlarge_small_images: false,
            icc_color_management: true,
            color_match_display: true,
            use_relative_colorimetric: false,
            scroll_to_zoom: false,
            preload_neighbors: true,
            title_bar: true,
            histogram_visible: false,
            exif_visible: false,
            loop_navigation: false,
            sort_by: crate::navigation::SortBy::Name,
            slideshow_seconds: crate::slideshow::DEFAULT_SECONDS,
            slideshow_crossfade: true,
            slideshow_loop: true,
            previous_handlers: HashMap::new(),
            raw: RawPipelineFlags::default(),
            custom_dcp_dir: None,
        }
    }
}

impl Settings {
    /// Load settings from disk, returning defaults if the file is missing or corrupt.
    pub fn load() -> Self {
        let path = settings_path();
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                log::warn!("Couldn't parse settings file, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to disk, creating the directory if needed.
    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            log::warn!("Couldn't create settings directory: {e}");
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    log::warn!("Couldn't write settings file: {e}");
                }
            }
            Err(e) => log::warn!("Couldn't serialize settings: {e}"),
        }
    }
}

/// The app data directory. Controlled by `PRVW_DATA_DIR` env var (for dev/test isolation),
/// falling back to `~/Library/Application Support/com.veszelovszki.prvw/`.
pub fn data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("PRVW_DATA_DIR") {
        return PathBuf::from(custom);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join("Library/Application Support/com.veszelovszki.prvw")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_correct() {
        let settings = Settings::default();
        assert!(settings.auto_update);
        assert!(settings.auto_fit_window);
        assert!(!settings.enlarge_small_images);
        assert!(settings.icc_color_management);
        assert!(settings.color_match_display);
        assert!(!settings.scroll_to_zoom);
        assert!(settings.title_bar);
        assert_eq!(settings.sort_by, crate::navigation::SortBy::Name);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let raw = RawPipelineFlags {
            default_tone_curve: false,
            capture_sharpening: false,
            ..RawPipelineFlags::default()
        };

        let settings = Settings {
            auto_update: false,
            auto_fit_window: false,
            enlarge_small_images: true,
            icc_color_management: false,
            color_match_display: false,
            use_relative_colorimetric: true,
            scroll_to_zoom: true,
            preload_neighbors: false,
            title_bar: false,
            histogram_visible: true,
            exif_visible: true,
            loop_navigation: true,
            sort_by: crate::navigation::SortBy::Date,
            slideshow_seconds: 12,
            slideshow_crossfade: false,
            slideshow_loop: false,
            previous_handlers: HashMap::from([(
                "public.jpeg".to_string(),
                "com.apple.Preview".to_string(),
            )]),
            raw,
            custom_dcp_dir: Some("/tmp/my-dcps".to_string()),
        };
        fs::write(&path, serde_json::to_string(&settings).unwrap()).unwrap();

        let loaded: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!loaded.auto_update);
        assert!(!loaded.auto_fit_window);
        assert!(loaded.enlarge_small_images);
        assert!(loaded.histogram_visible);
        assert!(loaded.exif_visible);
        assert!(loaded.loop_navigation);
        assert_eq!(loaded.sort_by, crate::navigation::SortBy::Date);
        assert_eq!(loaded.slideshow_seconds, 12);
        assert!(!loaded.slideshow_crossfade);
        assert!(!loaded.slideshow_loop);
        assert!(!loaded.raw.default_tone_curve);
        assert!(!loaded.raw.capture_sharpening);
        assert!(loaded.raw.highlight_recovery); // untouched flag stays true
        assert_eq!(loaded.custom_dcp_dir.as_deref(), Some("/tmp/my-dcps"));
    }

    #[test]
    fn round_trip_preserves_raw_tuning_knobs() {
        // Phase 6.0: the Tuning sliders (sharpening amount, saturation
        // boost, midtone anchor) persist alongside the flag toggles. The
        // round-trip test in `raw_flags.rs` covers the struct; this one
        // pins down the full `Settings` path through `serde_json::to_string`
        // and back, matching how `Settings::load`/`save` actually runs on
        // disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let raw = RawPipelineFlags {
            sharpen_amount: 0.55,
            saturation_boost_amount: 0.17,
            midtone_anchor: 0.28,
            ..RawPipelineFlags::default()
        };
        let settings = Settings {
            raw,
            ..Settings::default()
        };
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let loaded: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.raw.sharpen_amount, 0.55);
        assert_eq!(loaded.raw.saturation_boost_amount, 0.17);
        assert_eq!(loaded.raw.midtone_anchor, 0.28);
        // Untouched flags stay at their defaults.
        assert!(loaded.raw.highlight_recovery);
        assert!(loaded.raw.default_tone_curve);
    }

    #[test]
    fn missing_field_gets_default() {
        let json = r#"{"auto_update": false}"#;
        let loaded: Settings = serde_json::from_str(json).unwrap();
        assert!(!loaded.auto_update);
        assert!(loaded.auto_fit_window);
        assert!(!loaded.enlarge_small_images);
        // Histogram defaults to off.
        assert!(!loaded.histogram_visible);
        // EXIF info defaults to off.
        assert!(!loaded.exif_visible);
        // Loop navigation defaults to off.
        assert!(!loaded.loop_navigation);
        // Sort defaults to Name.
        assert_eq!(loaded.sort_by, crate::navigation::SortBy::Name);
        // Slideshow defaults: 4 s per image, crossfade on, loop on.
        assert_eq!(loaded.slideshow_seconds, crate::slideshow::DEFAULT_SECONDS);
        assert!(loaded.slideshow_crossfade);
        assert!(loaded.slideshow_loop);
        // Missing `raw` → all RAW flags default to true.
        assert!(loaded.raw.is_default());
        assert!(loaded.custom_dcp_dir.is_none());
    }
}
