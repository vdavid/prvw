//! MCP tool definitions for Prvw.
//!
//! Each tool maps to an action the user can perform in the image viewer.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A tool definition for MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl Tool {
    fn no_params(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }
}

/// Get all available tools.
pub fn get_all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "navigate".to_string(),
            description: "Navigate to the next or previous image in the directory".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["forward", "backward"],
                        "description": "Direction to navigate"
                    }
                },
                "required": ["direction"]
            }),
        },
        Tool {
            name: "open".to_string(),
            description: "Open a specific image file by absolute path".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the image file"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "zoom".to_string(),
            description: "Set zoom level. 1.0 = fit to window".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "level": {
                        "type": "number",
                        "description": "Zoom level as a positive number (1.0 = fit to window)"
                    }
                },
                "required": ["level"]
            }),
        },
        Tool::no_params("fit_to_window", "Reset zoom to fit the image in the window"),
        Tool::no_params("actual_size", "Set zoom to 1:1 pixel mapping"),
        Tool::no_params("toggle_fullscreen", "Toggle fullscreen mode"),
        Tool {
            name: "screenshot".to_string(),
            description: "Capture the current image as a base64-encoded PNG".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "send_key".to_string(),
            description: "Simulate a key press. Key names follow web conventions (ArrowLeft, Escape, f, etc.)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key name to simulate (for example, ArrowLeft, ArrowRight, Escape, f, 0, 1)"
                    }
                },
                "required": ["key"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_count() {
        let tools = get_all_tools();
        // navigate, open, zoom, fit_to_window, actual_size, toggle_fullscreen, screenshot, send_key
        assert_eq!(tools.len(), 8);
    }

    #[test]
    fn test_all_tools_have_schemas() {
        for tool in get_all_tools() {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }
}
