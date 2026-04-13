//! Tool execution logic.
//!
//! Routes MCP tool calls to app actions via Tauri events.

use std::sync::Mutex;

use base64::Engine;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::protocol::{INTERNAL_ERROR, INVALID_PARAMS};
use crate::AppState;

/// Result of tool execution.
pub type ToolResult = Result<Value, ToolError>;

/// Error from tool execution.
#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl ToolError {
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: msg.into(),
        }
    }
}

impl From<tauri::Error> for ToolError {
    fn from(e: tauri::Error) -> Self {
        Self::internal(e.to_string())
    }
}

/// Execute an MCP tool by name.
pub fn execute_tool<R: Runtime>(app: &AppHandle<R>, name: &str, params: &Value) -> ToolResult {
    match name {
        "navigate" => {
            let direction = params
                .get("direction")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("Missing 'direction' parameter"))?;
            let forward = match direction {
                "forward" => true,
                "backward" => false,
                _ => {
                    return Err(ToolError::invalid_params(
                        "direction must be 'forward' or 'backward'",
                    ));
                }
            };
            app.emit("qa-navigate", forward)?;
            Ok(json!(format!("OK: Navigated {direction}")))
        }

        "open" => {
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("Missing 'path' parameter"))?;
            if path.is_empty() {
                return Err(ToolError::invalid_params("path must not be empty"));
            }
            app.emit("qa-open-file", path.to_string())?;
            Ok(json!(format!("OK: Opening {path}")))
        }

        "zoom" => {
            let level = params
                .get("level")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| ToolError::invalid_params("Missing or invalid 'level' parameter"))?;
            if level <= 0.0 {
                return Err(ToolError::invalid_params("level must be positive"));
            }
            app.emit("qa-set-zoom", level as f32)?;
            Ok(json!(format!("OK: Zoom set to {level:.2}")))
        }

        "fit_to_window" => {
            app.emit("qa-fit-to-window", ())?;
            Ok(json!("OK: Fit to window"))
        }

        "actual_size" => {
            app.emit("qa-actual-size", ())?;
            Ok(json!("OK: Actual size (1:1)"))
        }

        "toggle_fullscreen" => {
            app.emit("qa-toggle-fullscreen", ())?;
            Ok(json!("OK: Toggled fullscreen"))
        }

        "screenshot" => {
            // Read current file from shared state, load and encode to PNG
            let current_file = {
                let state = app
                    .try_state::<Mutex<AppState>>()
                    .ok_or_else(|| ToolError::internal("App state not available"))?;
                let state = state
                    .lock()
                    .map_err(|_| ToolError::internal("State lock poisoned"))?;
                state
                    .shared_state
                    .lock()
                    .map_err(|_| ToolError::internal("Shared state lock poisoned"))?
                    .current_file
                    .clone()
            };

            let path = current_file.ok_or_else(|| ToolError::internal("No image loaded"))?;

            let decoded = crate::image_loader::load_image(&path)
                .map_err(|e| ToolError::internal(format!("Failed to decode image: {e}")))?;

            // Encode to PNG
            let mut png_bytes = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
            image::ImageEncoder::write_image(
                encoder,
                &decoded.rgba_data,
                decoded.width,
                decoded.height,
                image::ColorType::Rgba8.into(),
            )
            .map_err(|e| ToolError::internal(format!("PNG encoding failed: {e}")))?;

            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(json!({
                "content": [{
                    "type": "image",
                    "data": b64,
                    "mimeType": "image/png"
                }]
            }))
        }

        "send_key" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("Missing 'key' parameter"))?;
            if key.is_empty() {
                return Err(ToolError::invalid_params("key must not be empty"));
            }
            app.emit("qa-send-key", key.to_string())?;
            Ok(json!(format!("OK: Sent key '{key}'")))
        }

        _ => Err(ToolError::invalid_params(format!("Unknown tool: {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_error_codes() {
        let err = ToolError::invalid_params("test");
        assert_eq!(err.code, INVALID_PARAMS);

        let err = ToolError::internal("test");
        assert_eq!(err.code, INTERNAL_ERROR);
    }
}
