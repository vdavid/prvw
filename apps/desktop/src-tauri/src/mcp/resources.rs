//! MCP resource definitions for Prvw.
//!
//! Resources expose read-only app state to agents.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Manager, Runtime};

use crate::AppState;

/// A resource definition for MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

/// Resource content returned by resources/read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: String,
    pub text: String,
}

/// Get all available resources.
pub fn get_all_resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: "prvw://state".to_string(),
            name: "App state".to_string(),
            description: "Current file, index, total, zoom, pan, fullscreen, and window size"
                .to_string(),
            mime_type: "text/yaml".to_string(),
        },
        Resource {
            uri: "prvw://diagnostics".to_string(),
            name: "Diagnostics".to_string(),
            description: "Cache info, preloader status, and navigation history".to_string(),
            mime_type: "text/plain".to_string(),
        },
    ]
}

/// Read a resource by URI.
pub fn read_resource<R: Runtime>(
    app: &tauri::AppHandle<R>,
    uri: &str,
) -> Result<ResourceContent, String> {
    match uri {
        "prvw://state" => {
            let state = app
                .try_state::<Mutex<AppState>>()
                .ok_or("App state not available")?;
            let state = state
                .lock()
                .map_err(|_| "State lock poisoned".to_string())?;
            let shared = state
                .shared_state
                .lock()
                .map_err(|_| "Shared state lock poisoned".to_string())?;

            let file_display = shared
                .current_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string());

            let yaml = format!(
                "file: {file_display}\n\
                 index: {}\n\
                 total: {}\n\
                 zoom: {:.2}\n\
                 pan: [{:.2}, {:.2}]\n\
                 fullscreen: {}\n\
                 window: [{}x{}]\n\
                 title: {}\n",
                shared.current_index,
                shared.total_files,
                shared.zoom,
                shared.pan_x,
                shared.pan_y,
                shared.fullscreen,
                shared.window_width,
                shared.window_height,
                shared.window_title,
            );

            Ok(ResourceContent {
                uri: uri.to_string(),
                mime_type: "text/yaml".to_string(),
                text: yaml,
            })
        }
        "prvw://diagnostics" => {
            let state = app
                .try_state::<Mutex<AppState>>()
                .ok_or("App state not available")?;
            let state = state
                .lock()
                .map_err(|_| "State lock poisoned".to_string())?;
            let text = state
                .shared_state
                .lock()
                .map(|s| s.diagnostics_text.clone())
                .unwrap_or_else(|_| "(lock error)".to_string());

            Ok(ResourceContent {
                uri: uri.to_string(),
                mime_type: "text/plain".to_string(),
                text,
            })
        }
        _ => Err(format!("Unknown resource URI: {uri}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_count() {
        let resources = get_all_resources();
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn test_resource_uris() {
        let resources = get_all_resources();
        for resource in resources {
            assert!(resource.uri.starts_with("prvw://"));
            assert!(!resource.name.is_empty());
        }
    }
}
