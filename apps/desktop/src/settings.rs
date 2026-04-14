//! Settings persistence.
//!
//! Loads/saves user preferences from the app data directory:
//! - Production: `~/Library/Application Support/com.veszelovszki.prvw/settings.json`
//! - Dev/test: override with `PRVW_DATA_DIR` env var
//!
//! The settings file is the source of truth — no in-memory cache or Arc/Mutex needed.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub auto_update: bool,

    #[serde(default = "default_true")]
    pub auto_fit_window: bool,

    #[serde(default)]
    pub enlarge_small_images: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_update: true,
            auto_fit_window: true,
            enlarge_small_images: false,
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
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            auto_update: false,
            auto_fit_window: false,
            enlarge_small_images: true,
        };
        fs::write(&path, serde_json::to_string(&settings).unwrap()).unwrap();

        let loaded: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!loaded.auto_update);
        assert!(!loaded.auto_fit_window);
        assert!(loaded.enlarge_small_images);
    }

    #[test]
    fn missing_field_gets_default() {
        let json = r#"{"auto_update": false}"#;
        let loaded: Settings = serde_json::from_str(json).unwrap();
        assert!(!loaded.auto_update);
        assert!(loaded.auto_fit_window);
        assert!(!loaded.enlarge_small_images);
    }
}
