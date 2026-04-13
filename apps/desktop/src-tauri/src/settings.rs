use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub updates_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            updates_enabled: true,
        }
    }
}

/// Load settings from the tauri-plugin-store JSON file on disk.
/// Returns defaults if the file doesn't exist or can't be parsed.
/// This reads the store directly (no Tauri runtime needed) so it works at startup.
pub fn load_from_store(app_data_dir: &Path) -> Settings {
    let store_path = app_data_dir.join("settings.json");
    let mut settings = Settings::default();

    let content = match std::fs::read_to_string(&store_path) {
        Ok(c) => c,
        Err(_) => return settings,
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Couldn't parse settings.json: {e}");
            return settings;
        }
    };

    if let Some(v) = json.get("updatesEnabled").and_then(|v| v.as_bool()) {
        settings.updates_enabled = v;
    }

    settings
}
