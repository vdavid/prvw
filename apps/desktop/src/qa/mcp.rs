//! MCP JSON-RPC handler. Exposes viewer state and commands as MCP tools and resources
//! over streamable HTTP (`POST /mcp`).

use base64::Engine;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

use super::server::{format_state_json, write_response};
use crate::app::SharedAppState;
use crate::commands::AppCommand;
use crate::settings;

const SYNC_TIMEOUT: Duration = Duration::from_secs(2);

/// Send an `AppCommand` and block until the event loop confirms it via a `Sync` barrier.
/// Returns the updated state snapshot as JSON, or a JSON-RPC error on timeout / closed loop.
fn send_and_wait(
    proxy: &EventLoopProxy<AppCommand>,
    command: AppCommand,
    state: &Arc<Mutex<SharedAppState>>,
    timeout: Duration,
) -> Result<Value, Value> {
    proxy
        .send_event(command)
        .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
    let (tx, rx) = mpsc::channel();
    proxy
        .send_event(AppCommand::Sync(tx))
        .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
    rx.recv_timeout(timeout)
        .map_err(|_| json_rpc_error(-32603, "Command timeout"))?;
    Ok(format_state_json(state))
}

pub(super) fn handle_mcp(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    session_id: &str,
) -> Result<(), String> {
    let req: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {e}"))?;

    let method = req["method"].as_str().unwrap_or("");
    let id = req.get("id"); // None for notifications

    let result = match method {
        "initialize" => Some(mcp_initialize(session_id)),
        "notifications/initialized" => None, // Notification, no response
        "tools/list" => Some(mcp_tools_list()),
        "tools/call" => Some(mcp_tools_call(&req["params"], state, proxy)),
        "resources/list" => Some(mcp_resources_list()),
        "resources/read" => Some(mcp_resources_read(&req["params"], state)),
        _ => Some(Err(json_rpc_error(
            -32601,
            &format!("Method not found: {method}"),
        ))),
    };

    // Notifications (no `id`) get a 202 Accepted with no body per MCP spec.
    let Some(result) = result else {
        return write_response(stream, 202, "application/json", b"", &[]);
    };

    let id_val = id.cloned().unwrap_or(Value::Null);

    let response = match result {
        Ok(val) => json!({ "jsonrpc": "2.0", "id": id_val, "result": val }),
        Err(err) => json!({ "jsonrpc": "2.0", "id": id_val, "error": err }),
    };

    let response_bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    let session_header = format!("Mcp-Session-Id: {session_id}");
    write_response(
        stream,
        200,
        "application/json",
        &response_bytes,
        &[session_header.as_str()],
    )
}

fn json_rpc_error(code: i32, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

fn mcp_initialize(session_id: &str) -> Result<Value, Value> {
    Ok(json!({
        "protocolVersion": "2025-03-26",
        "capabilities": { "tools": {}, "resources": {} },
        "serverInfo": { "name": "prvw", "version": "0.1.0" },
        "sessionId": session_id,
    }))
}

fn mcp_tools_list() -> Result<Value, Value> {
    #[allow(unused_mut)]
    let mut tools = json!([
        {
            "name": "navigate",
            "description": "Navigate to the next or previous image in the directory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["next", "prev"],
                        "description": "Direction to navigate."
                    }
                },
                "required": ["direction"]
            }
        },
        {
            "name": "key",
            "description": "Simulate a key press. Key names follow web conventions (ArrowLeft, Escape, f, etc.).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key name to simulate."
                    }
                },
                "required": ["key"]
            }
        },
        {
            "name": "zoom",
            "description": "Set zoom level. Use a float for absolute zoom, 'fit' for fit-to-window, or 'actual' for 1:1 pixels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "level": {
                        "type": "string",
                        "description": "Zoom level: a positive float, 'fit', or 'actual'."
                    }
                },
                "required": ["level"]
            }
        },
        {
            "name": "fullscreen",
            "description": "Control fullscreen mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["on", "off", "toggle"],
                        "description": "Fullscreen mode to set."
                    }
                },
                "required": ["mode"]
            }
        },
        {
            "name": "open",
            "description": "Open a specific image file by path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the image file."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "auto_fit_window",
            "description": "Control the auto-fit window setting. When enabled, the window resizes to match each loaded image.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable, false to disable."
                    }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "title_bar",
            "description": "Control the title bar area. When enabled, a strip at the top is reserved so the title bar doesn't cover the image.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable, false to disable."
                    }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "scroll_to_zoom",
            "description": "Control whether scroll zooms the image (true) or navigates between images (false).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable, false to disable."
                    }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "enlarge_small_images",
            "description": "Control whether small images are enlarged to fill the window. Ignored when auto-fit window is on.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable, false to disable."
                    }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_window_geometry",
            "description": "Set the window position and/or size. All parameters are optional — only provided values are changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "Window left edge in screen pixels." },
                    "y": { "type": "integer", "description": "Window top edge in screen pixels." },
                    "width": { "type": "integer", "description": "Window content width in logical pixels." },
                    "height": { "type": "integer", "description": "Window content height in logical pixels." }
                }
            }
        },
        {
            "name": "scroll_zoom",
            "description": "Simulate a scroll-wheel zoom at a specific cursor position. Positive delta zooms in, negative zooms out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "delta": { "type": "number", "description": "Scroll delta. Positive = zoom in, negative = zoom out." },
                    "cursor_x": { "type": "number", "description": "Cursor X position in window pixels." },
                    "cursor_y": { "type": "number", "description": "Cursor Y position in window pixels." }
                },
                "required": ["delta", "cursor_x", "cursor_y"]
            }
        },
        {
            "name": "zoom_in",
            "description": "Zoom in by one keyboard step (25%), centered on the window.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "zoom_out",
            "description": "Zoom out by one keyboard step (25%), centered on the window.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "refresh",
            "description": "Re-display the current image, re-applying zoom and settings.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "screenshot",
            "description": "Capture a screenshot of the current view. Returns a base64-encoded PNG image.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "show_settings",
            "description": "Open the Settings window, optionally switching to a specific section.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "section": {
                        "type": "string",
                        "description": "Section to show: 'general' or 'file_associations'. Omit to show the current/default section."
                    }
                }
            }
        },
        {
            "name": "close_settings",
            "description": "Close the Settings window.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "thumbnails_status",
            "description": "Report the thumbnail scheduler's state: folder size, in-flight indices, queue length, cached/failed indices, paused flag, parallelism cap.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "histogram",
            "description": "Toggle the histogram overlay (top-right of the window).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "exif_info",
            "description": "Toggle the EXIF info overlay (below the histogram, top-right). The overlay only renders when the current image actually has EXIF metadata; otherwise toggling on is a no-op visually.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_cursor_position",
            "description": "Move the cached cursor position. Used by tests to drive the histogram hover readout without a real mouse.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "number", "description": "Cursor X in logical pixels." },
                    "y": { "type": "number", "description": "Cursor Y in logical pixels." }
                },
                "required": ["x", "y"]
            }
        }
    ]);

    // Debug-only: full-window screenshot via `/usr/sbin/screencapture`. Compile-time
    // gated so release builds neither expose the tool nor link the dispatch arm.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Some(arr) = tools.as_array_mut() {
        arr.push(json!({
            "name": "screenshot_window",
            "description": "Debug builds only. Capture the entire native window as the user sees it (overlays, title bar, vibrancy, modal panels). Requires Screen Recording permission — macOS will prompt on first use, and the first call may return a black frame until the user grants it. Returns a base64-encoded PNG image.",
            "inputSchema": { "type": "object", "properties": {} }
        }));
    }

    Ok(json!({ "tools": tools }))
}

fn mcp_tools_call(
    params: &Value,
    state: &Arc<Mutex<SharedAppState>>,
    proxy: &EventLoopProxy<AppCommand>,
) -> Result<Value, Value> {
    let tool_name = params["name"].as_str().unwrap_or("");
    let args = &params["arguments"];

    match tool_name {
        "navigate" => {
            let direction = args["direction"].as_str().unwrap_or("");
            let forward = match direction {
                "next" | "forward" => true,
                "prev" | "previous" | "backward" => false,
                _ => return Err(json_rpc_error(-32602, "direction must be 'next' or 'prev'")),
            };
            let state_json =
                send_and_wait(proxy, AppCommand::Navigate(forward), state, SYNC_TIMEOUT)?;
            let label = if forward { "next" } else { "prev" };
            let mut content = mcp_text_content(&format!("Navigated {label}."));
            content["state"] = state_json;
            Ok(content)
        }
        "key" => {
            let key = args["key"].as_str().unwrap_or("").to_string();
            if key.is_empty() {
                return Err(json_rpc_error(-32602, "key is required"));
            }
            let state_json =
                send_and_wait(proxy, AppCommand::SendKey(key.clone()), state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content(&format!("Sent key: {key}"));
            content["state"] = state_json;
            Ok(content)
        }
        "zoom" => {
            let level = args["level"].as_str().unwrap_or("");
            let cmd = match level {
                "fit" => AppCommand::FitToWindow,
                "actual" => AppCommand::ActualSize,
                _ => match level.parse::<f32>() {
                    Ok(v) if v > 0.0 => AppCommand::SetZoom(v),
                    _ => {
                        return Err(json_rpc_error(
                            -32602,
                            "level must be 'fit', 'actual', or a positive float",
                        ));
                    }
                },
            };
            let state_json = send_and_wait(proxy, cmd, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content(&format!("Zoom set to: {level}"));
            content["state"] = state_json;
            Ok(content)
        }
        "fullscreen" => {
            let mode = args["mode"].as_str().unwrap_or("");
            let cmd = match mode {
                "toggle" => AppCommand::ToggleFullscreen,
                "on" => AppCommand::SetFullscreen(true),
                "off" => AppCommand::SetFullscreen(false),
                _ => {
                    return Err(json_rpc_error(
                        -32602,
                        "mode must be 'on', 'off', or 'toggle'",
                    ));
                }
            };
            let state_json = send_and_wait(proxy, cmd, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content(&format!("Fullscreen: {mode}"));
            content["state"] = state_json;
            Ok(content)
        }
        "open" => {
            let path_str = args["path"].as_str().unwrap_or("");
            if path_str.is_empty() {
                return Err(json_rpc_error(-32602, "path is required"));
            }
            let state_json = send_and_wait(
                proxy,
                AppCommand::OpenFile(PathBuf::from(path_str)),
                state,
                SYNC_TIMEOUT,
            )?;
            let mut content = mcp_text_content(&format!("Opened: {path_str}"));
            content["state"] = state_json;
            Ok(content)
        }
        "auto_fit_window" => {
            let enabled = args["enabled"]
                .as_bool()
                .ok_or_else(|| json_rpc_error(-32602, "enabled must be a boolean"))?;
            let state_json = send_and_wait(
                proxy,
                AppCommand::SetAutoFitWindow(enabled),
                state,
                SYNC_TIMEOUT,
            )?;
            let label = if enabled { "enabled" } else { "disabled" };
            let mut content = mcp_text_content(&format!("Auto-fit window: {label}"));
            content["state"] = state_json;
            Ok(content)
        }
        "title_bar" => {
            let enabled = args["enabled"]
                .as_bool()
                .ok_or_else(|| json_rpc_error(-32602, "enabled must be a boolean"))?;
            let state_json =
                send_and_wait(proxy, AppCommand::SetTitleBar(enabled), state, SYNC_TIMEOUT)?;
            let label = if enabled { "enabled" } else { "disabled" };
            let mut content = mcp_text_content(&format!("Title bar: {label}"));
            content["state"] = state_json;
            Ok(content)
        }
        "scroll_to_zoom" => {
            let enabled = args["enabled"]
                .as_bool()
                .ok_or_else(|| json_rpc_error(-32602, "enabled must be a boolean"))?;
            let state_json = send_and_wait(
                proxy,
                AppCommand::SetScrollToZoom(enabled),
                state,
                SYNC_TIMEOUT,
            )?;
            let label = if enabled { "enabled" } else { "disabled" };
            let mut content = mcp_text_content(&format!("Scroll to zoom: {label}"));
            content["state"] = state_json;
            Ok(content)
        }
        "enlarge_small_images" => {
            let enabled = args["enabled"]
                .as_bool()
                .ok_or_else(|| json_rpc_error(-32602, "enabled must be a boolean"))?;
            let state_json = send_and_wait(
                proxy,
                AppCommand::SetEnlargeSmallImages(enabled),
                state,
                SYNC_TIMEOUT,
            )?;
            let label = if enabled { "enabled" } else { "disabled" };
            let mut content = mcp_text_content(&format!("Enlarge small images: {label}"));
            content["state"] = state_json;
            Ok(content)
        }
        "set_window_geometry" => {
            let x = args["x"].as_i64().map(|v| v as i32);
            let y = args["y"].as_i64().map(|v| v as i32);
            let width = args["width"].as_u64().map(|v| v as u32);
            let height = args["height"].as_u64().map(|v| v as u32);
            let state_json = send_and_wait(
                proxy,
                AppCommand::SetWindowGeometry {
                    x,
                    y,
                    width,
                    height,
                },
                state,
                SYNC_TIMEOUT,
            )?;
            let mut content = mcp_text_content("Window geometry updated.");
            content["state"] = state_json;
            Ok(content)
        }
        "scroll_zoom" => {
            let delta = args["delta"]
                .as_f64()
                .ok_or_else(|| json_rpc_error(-32602, "delta is required"))?
                as f32;
            let cx = args["cursor_x"]
                .as_f64()
                .ok_or_else(|| json_rpc_error(-32602, "cursor_x is required"))?
                as f32;
            let cy = args["cursor_y"]
                .as_f64()
                .ok_or_else(|| json_rpc_error(-32602, "cursor_y is required"))?
                as f32;
            let state_json = send_and_wait(
                proxy,
                AppCommand::ScrollZoom {
                    delta,
                    cursor_x: cx,
                    cursor_y: cy,
                },
                state,
                SYNC_TIMEOUT,
            )?;
            let mut content = mcp_text_content("Scroll zoom applied.");
            content["state"] = state_json;
            Ok(content)
        }
        "zoom_in" => {
            let state_json = send_and_wait(proxy, AppCommand::ZoomIn, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Zoomed in.");
            content["state"] = state_json;
            Ok(content)
        }
        "zoom_out" => {
            let state_json = send_and_wait(proxy, AppCommand::ZoomOut, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Zoomed out.");
            content["state"] = state_json;
            Ok(content)
        }
        "refresh" => {
            let state_json = send_and_wait(proxy, AppCommand::Refresh, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Refreshed.");
            content["state"] = state_json;
            Ok(content)
        }
        "screenshot" => {
            let (tx, rx) = mpsc::channel();
            proxy
                .send_event(AppCommand::TakeScreenshot(tx))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            let png_bytes = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| json_rpc_error(-32603, "Screenshot timeout"))?;
            if png_bytes.is_empty() {
                return Err(json_rpc_error(-32603, "No image loaded"));
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(json!({
                "content": [{
                    "type": "image",
                    "data": b64,
                    "mimeType": "image/png"
                }]
            }))
        }
        #[cfg(all(debug_assertions, target_os = "macos"))]
        "screenshot_window" => screenshot_window(proxy),
        "show_settings" => {
            let section = args["section"].as_str().unwrap_or("").to_string();
            if section.is_empty() {
                let state_json =
                    send_and_wait(proxy, AppCommand::ShowSettings, state, SYNC_TIMEOUT)?;
                let mut content = mcp_text_content("Settings window opened.");
                content["state"] = state_json;
                Ok(content)
            } else {
                // Open settings then switch to section
                proxy
                    .send_event(AppCommand::ShowSettings)
                    .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
                let state_json = send_and_wait(
                    proxy,
                    AppCommand::ShowSettingsSection(section.clone()),
                    state,
                    SYNC_TIMEOUT,
                )?;
                let mut content =
                    mcp_text_content(&format!("Settings opened to section: {section}"));
                content["state"] = state_json;
                Ok(content)
            }
        }
        "close_settings" => {
            let state_json = send_and_wait(proxy, AppCommand::CloseSettings, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Settings window closed.");
            content["state"] = state_json;
            Ok(content)
        }
        "histogram" => {
            let state_json =
                send_and_wait(proxy, AppCommand::ToggleHistogram, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Toggled histogram.");
            content["state"] = state_json;
            Ok(content)
        }
        "exif_info" => {
            let state_json = send_and_wait(proxy, AppCommand::ToggleExifInfo, state, SYNC_TIMEOUT)?;
            let mut content = mcp_text_content("Toggled exif info.");
            content["state"] = state_json;
            Ok(content)
        }
        "set_cursor_position" => {
            let x = args["x"]
                .as_f64()
                .ok_or_else(|| json_rpc_error(-32602, "x is required"))?;
            let y = args["y"]
                .as_f64()
                .ok_or_else(|| json_rpc_error(-32602, "y is required"))?;
            let state_json = send_and_wait(
                proxy,
                AppCommand::SetCursorPosition { x, y },
                state,
                SYNC_TIMEOUT,
            )?;
            let mut content = mcp_text_content(&format!("Cursor moved to ({x}, {y})."));
            content["state"] = state_json;
            Ok(content)
        }
        "thumbnails_status" => {
            // Round-trip a Sync so the returned state reflects any
            // in-flight mutations from just-processed commands.
            let (tx, rx) = mpsc::channel();
            proxy
                .send_event(AppCommand::Sync(tx))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            rx.recv_timeout(SYNC_TIMEOUT)
                .map_err(|_| json_rpc_error(-32603, "Command timeout"))?;
            let snap = {
                let guard = state
                    .lock()
                    .map_err(|_| json_rpc_error(-32603, "Lock poisoned"))?;
                guard.thumbnails.clone()
            };
            let events: Vec<Value> = snap
                .events
                .iter()
                .map(|e| {
                    json!({
                        "ts_ms": e.ts_ms,
                        "kind": e.kind,
                        "detail": e.detail,
                    })
                })
                .collect();
            let payload = json!({
                "folder_len": snap.folder_len,
                "current": snap.current,
                "pending_current": snap.pending_current,
                "placeholder_active": snap.placeholder_active,
                "in_flight": snap.in_flight,
                "queue_len": snap.queue_len,
                "cached": snap.cached,
                "failed": snap.failed,
                "paused": snap.paused,
                "max_parallel": snap.max_parallel,
                "events": events,
            });
            let mut content = mcp_text_content(&format!(
                "Thumbnails: {} cached, {} in flight, {} queued, paused={}, placeholder_active={}",
                snap.cached.len(),
                snap.in_flight.len(),
                snap.queue_len,
                snap.paused,
                snap.placeholder_active
            ));
            content["thumbnails"] = payload;
            Ok(content)
        }
        _ => Err(json_rpc_error(
            -32602,
            &format!("Unknown tool: {tool_name}"),
        )),
    }
}

fn mcp_resources_list() -> Result<Value, Value> {
    Ok(json!({
        "resources": [
            {
                "uri": "prvw://state",
                "name": "App state",
                "description": "Current file, zoom, pan, fullscreen, window size, image layout.",
                "mimeType": "application/json"
            },
            {
                "uri": "prvw://settings",
                "name": "Settings",
                "description": "Current settings (auto-update, scroll to zoom, title bar, auto-fit window, enlarge small images).",
                "mimeType": "application/json"
            },
            {
                "uri": "prvw://menu",
                "name": "Menu layout",
                "description": "The app's menu bar structure.",
                "mimeType": "text/plain"
            },
            {
                "uri": "prvw://diagnostics",
                "name": "Performance diagnostics",
                "description": "Cache state, navigation timing, and memory usage.",
                "mimeType": "text/plain"
            }
        ]
    }))
}

fn mcp_resources_read(params: &Value, state: &Arc<Mutex<SharedAppState>>) -> Result<Value, Value> {
    let uri = params["uri"].as_str().unwrap_or("");
    match uri {
        "prvw://state" => {
            let state_json = format_state_json(state);
            let text = serde_json::to_string_pretty(&state_json).unwrap_or_default();
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": text
                }]
            }))
        }
        "prvw://settings" => {
            let s = settings::Settings::load();
            let settings_json = json!({
                "auto_update": s.auto_update,
                "scroll_to_zoom": s.scroll_to_zoom,
                "title_bar": s.title_bar,
                "auto_fit_window": s.auto_fit_window,
                "enlarge_small_images": s.enlarge_small_images,
            });
            let text = serde_json::to_string_pretty(&settings_json).unwrap_or_default();
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": text
                }]
            }))
        }
        "prvw://menu" => Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/plain",
                "text": super::http::MENU_TEXT
            }]
        })),
        "prvw://diagnostics" => {
            let text = state
                .lock()
                .map(|s| s.diagnostics_text.clone())
                .unwrap_or_else(|_| "(lock error)".to_string());
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": text
                }]
            }))
        }
        _ => Err(json_rpc_error(-32602, &format!("Unknown resource: {uri}"))),
    }
}

/// Wrap a string in an MCP text content response.
fn mcp_text_content(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

/// Debug-only: capture the entire native window as the user sees it via
/// `/usr/sbin/screencapture -l <windowNumber>`. Cheaper to maintain than the
/// `CGWindowListCreateImage` FFI dance (which is also deprecated as of macOS
/// 14.4). The ~300–500 ms spawn cost is fine for a QA tool.
///
/// Requires macOS Screen Recording permission. macOS prompts the user on first
/// use; the first call may return a black/empty frame until they grant it.
#[cfg(all(debug_assertions, target_os = "macos"))]
fn screenshot_window(proxy: &EventLoopProxy<AppCommand>) -> Result<Value, Value> {
    use std::process::Command;

    // 1. Ask the main thread for the NSWindow number.
    let (tx, rx) = mpsc::channel();
    proxy
        .send_event(AppCommand::GetWindowNumber(tx))
        .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
    let window_number = rx
        .recv_timeout(SYNC_TIMEOUT)
        .map_err(|_| json_rpc_error(-32603, "Window number timeout"))?;
    if window_number == 0 {
        return Err(json_rpc_error(-32603, "Main window not yet ready"));
    }

    // 2. Shell out. `-o` no shadow, `-x` silent (no shutter sound), `-t png`,
    //    file path last. Skip stdout capture — `screencapture` writes a fixed
    //    string we don't need, and ignoring it dodges interleaving.
    let tmp = std::env::temp_dir().join(format!(
        "prvw-screenshot-window-{}-{}.png",
        std::process::id(),
        window_number
    ));
    let output = Command::new("/usr/sbin/screencapture")
        .args(["-l", &window_number.to_string(), "-o", "-x", "-t", "png"])
        .arg(&tmp)
        .output()
        .map_err(|e| json_rpc_error(-32603, &format!("Couldn't run screencapture: {e}")))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(json_rpc_error(
            -32603,
            &format!("screencapture exited with {}: {stderr}", output.status),
        ));
    }

    // 3. Read the PNG and clean up.
    let png_bytes = std::fs::read(&tmp)
        .map_err(|e| json_rpc_error(-32603, &format!("Couldn't read screenshot: {e}")))?;
    let _ = std::fs::remove_file(&tmp);

    if png_bytes.is_empty() {
        return Err(json_rpc_error(
            -32603,
            "Screenshot is empty. Grant Screen Recording permission to prvw and try again.",
        ));
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok(json!({
        "content": [{
            "type": "image",
            "data": b64,
            "mimeType": "image/png"
        }]
    }))
}
