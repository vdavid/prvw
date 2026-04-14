//! Embedded HTTP server for QA/E2E testing and MCP (Model Context Protocol) integration.
//!
//! Provides two interfaces:
//! - **Simple HTTP** (`GET /state`, `POST /key`, etc.) for cURL debugging and E2E tests.
//! - **MCP JSON-RPC** (`POST /mcp`) for AI agent integration via streamable HTTP transport.
//!
//! The server runs on a background thread using a raw `TcpListener` (no external HTTP crate).
//! Port is controlled by `PRVW_QA_PORT` env var (default 19447, set to 0 to disable).

use base64::Engine;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

use crate::settings;

/// Global event loop proxy, set once in `resumed()`. Allows non-main-loop code (like the
/// native Settings window delegate) to send commands into the event loop.
static EVENT_LOOP_PROXY: OnceLock<EventLoopProxy<AppCommand>> = OnceLock::new();

/// Store the event loop proxy so it's accessible from native UI delegates.
pub fn set_event_loop_proxy(proxy: EventLoopProxy<AppCommand>) {
    let _ = EVENT_LOOP_PROXY.set(proxy);
}

/// Send a command through the global event loop proxy. Returns false if the proxy
/// hasn't been set or the event loop is closed.
#[cfg(target_os = "macos")] // Called from native_ui.rs (macOS-only Settings delegate)
pub fn send_command(command: AppCommand) -> bool {
    EVENT_LOOP_PROXY
        .get()
        .and_then(|p| p.send_event(command).ok())
        .is_some()
}

/// Snapshot of app state, updated by the main thread on every state change.
#[derive(Clone, Debug)]
pub struct SharedAppState {
    pub current_file: Option<PathBuf>,
    pub current_index: usize,
    pub total_files: usize,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub fullscreen: bool,
    pub window_x: f64,
    pub window_y: f64,
    pub window_width: u32,
    pub window_height: u32,
    pub window_title: String,
    pub image_width: u32,
    pub image_height: u32,
    pub image_render_x: f32,
    pub image_render_y: f32,
    pub image_render_width: f32,
    pub image_render_height: f32,
    pub min_zoom: f32,
    /// Whether auto-fit window is enabled.
    pub auto_fit_window: bool,
    /// Whether small images are enlarged to fill the window.
    pub enlarge_small_images: bool,
    /// Pre-formatted diagnostics text, updated by the main thread.
    pub diagnostics_text: String,
}

impl Default for SharedAppState {
    fn default() -> Self {
        Self {
            current_file: None,
            current_index: 0,
            total_files: 0,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            fullscreen: false,
            window_x: 0.0,
            window_y: 0.0,
            window_width: 0,
            window_height: 0,
            window_title: String::new(),
            image_width: 0,
            image_height: 0,
            image_render_x: 0.0,
            image_render_y: 0.0,
            image_render_width: 0.0,
            image_render_height: 0.0,
            min_zoom: 1.0,
            auto_fit_window: true,
            enlarge_small_images: false,
            diagnostics_text: String::new(),
        }
    }
}

/// Commands that drive all app behavior. Keyboard, mouse, menu, QA server, and MCP all
/// map their inputs to these commands. The `user_event` handler in main.rs is the single
/// place where each command's effect is implemented.
pub enum AppCommand {
    // ── Navigation ───────────────────────────────────────────────────
    /// Navigate forward (true) or backward (false).
    Navigate(bool),
    /// Open a specific file.
    OpenFile(PathBuf),

    // ── View ─────────────────────────────────────────────────────────
    /// Zoom in one step (keyboard shortcut).
    ZoomIn,
    /// Zoom out one step (keyboard shortcut).
    ZoomOut,
    /// Set absolute zoom level.
    SetZoom(f32),
    /// Reset zoom to fit the image in the window.
    FitToWindow,
    /// Set zoom to 1:1 pixel mapping.
    ActualSize,
    /// Toggle between fit-to-window and actual size.
    ToggleFit,
    /// Toggle fullscreen mode.
    ToggleFullscreen,
    /// Set fullscreen on or off explicitly.
    SetFullscreen(bool),
    /// Set auto-fit window mode.
    SetAutoFitWindow(bool),
    /// Set enlarge-small-images mode.
    SetEnlargeSmallImages(bool),

    // ── App ──────────────────────────────────────────────────────────
    /// Show the About window.
    ShowAbout,
    /// Show the Settings window.
    ShowSettings,
    /// Exit the application.
    Exit,

    // ── Window ───────────────────────────────────────────────────────
    /// Reposition and/or resize the window. All fields optional.
    SetWindowGeometry {
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
    },

    // ── QA / MCP ─────────────────────────────────────────────────────
    /// Scroll-wheel zoom at a specific cursor position.
    ScrollZoom {
        delta: f32,
        cursor_x: f32,
        cursor_y: f32,
    },
    /// Re-display the current image (re-applies zoom, re-reads from cache/disk).
    Refresh,

    /// Simulate a key press. Key name follows web conventions: "ArrowLeft", "Escape", "f", etc.
    SendKey(String),
    /// Capture a screenshot. The sender receives PNG bytes.
    TakeScreenshot(mpsc::Sender<Vec<u8>>),
    /// Synchronization barrier — sends () back to confirm all prior commands were processed.
    Sync(mpsc::Sender<()>),
}

const DEFAULT_PORT: u16 = 19447;
const SYNC_TIMEOUT: Duration = Duration::from_secs(2);

/// Start the QA HTTP server on a background thread. Returns `None` if disabled (port=0).
pub fn start(
    state: Arc<Mutex<SharedAppState>>,
    proxy: EventLoopProxy<AppCommand>,
) -> Option<std::thread::JoinHandle<()>> {
    let port_str = std::env::var("PRVW_QA_PORT").unwrap_or_default();
    let port: u16 = if port_str.is_empty() {
        DEFAULT_PORT
    } else {
        match port_str.parse::<u16>() {
            Ok(0) => return None,
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "Invalid PRVW_QA_PORT value '{port_str}': {e}, using default {DEFAULT_PORT}"
                );
                DEFAULT_PORT
            }
        }
    };

    let listener = match TcpListener::bind(format!("127.0.0.1:{port}")) {
        Ok(l) => l,
        Err(e) => {
            log::error!("QA server couldn't bind to port {port}: {e}");
            return None;
        }
    };

    log::info!("QA server listening on http://127.0.0.1:{port}");

    let handle = std::thread::Builder::new()
        .name("prvw-qa-server".to_string())
        .spawn(move || {
            server_loop(listener, state, proxy);
        })
        .expect("Failed to spawn QA server thread");

    Some(handle)
}

fn server_loop(
    listener: TcpListener,
    state: Arc<Mutex<SharedAppState>>,
    proxy: EventLoopProxy<AppCommand>,
) {
    // Generate a session ID for MCP (single-session server).
    let session_id = format!("prvw-{}", std::process::id());

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("QA server accept error: {e}");
                continue;
            }
        };

        // Set a read timeout so malformed/stalled connections don't block the thread forever.
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));

        if let Err(e) = handle_request(&mut stream, &state, &proxy, &session_id) {
            log::debug!("QA server request error: {e}");
            let _ = write_response(
                &mut stream,
                500,
                "text/plain",
                format!("Internal error: {e}").as_bytes(),
                &[],
            );
        }
    }
}

/// Parse an HTTP request and dispatch to the appropriate handler.
fn handle_request(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
    proxy: &EventLoopProxy<AppCommand>,
    session_id: &str,
) -> Result<(), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    // Read the request line
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| e.to_string())?;
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 {
        return write_response(stream, 400, "text/plain", b"Bad request", &[]);
    }
    let method = parts[0];
    let path = parts[1];

    // Read headers to find Content-Length
    let mut content_length: usize = 0;
    loop {
        let mut header_line = String::new();
        reader
            .read_line(&mut header_line)
            .map_err(|e| e.to_string())?;
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Read body for POST requests
    let body = if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };

    log::debug!("QA: {method} {path}");

    match (method, path) {
        // MCP JSON-RPC endpoint
        ("POST", "/mcp") => handle_mcp(stream, state, proxy, &body, session_id),
        // Simple HTTP endpoints
        ("GET", "/state") => handle_get_state(stream, state),
        ("GET", "/menu") => handle_get_menu(stream),
        ("GET", "/screenshot") => handle_get_screenshot(stream, proxy),
        ("GET", "/diagnostics") => handle_get_diagnostics(stream, state),
        ("POST", "/key") => handle_post_key(stream, proxy, &body, state),
        ("POST", "/navigate") => handle_post_navigate(stream, proxy, &body, state),
        ("POST", "/zoom") => handle_post_zoom(stream, proxy, &body, state),
        ("POST", "/fullscreen") => handle_post_fullscreen(stream, proxy, &body, state),
        ("POST", "/auto-fit") => handle_post_auto_fit(stream, proxy, &body, state),
        ("POST", "/enlarge-small") => handle_post_enlarge_small(stream, proxy, &body, state),
        ("POST", "/open") => handle_post_open(stream, proxy, &body, state),
        ("POST", "/window-geometry") => handle_post_window_geometry(stream, proxy, &body, state),
        ("POST", "/scroll-zoom") => handle_post_scroll_zoom(stream, proxy, &body, state),
        ("POST", "/zoom-in") => handle_post_zoom_in(stream, proxy, state),
        ("POST", "/zoom-out") => handle_post_zoom_out(stream, proxy, state),
        ("POST", "/refresh") => handle_post_refresh(stream, proxy, state),
        ("GET", "/settings") => handle_get_settings(stream),
        _ => write_response(stream, 404, "text/plain", b"Not found", &[]),
    }
}

// ---------------------------------------------------------------------------
// MCP JSON-RPC dispatch
// ---------------------------------------------------------------------------

fn handle_mcp(
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
    Ok(json!({
        "tools": [
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
            }
        ]
    }))
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
                "description": "Current settings (auto-update, auto-fit window, enlarge small images).",
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
                "text": MENU_TEXT
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

// ---------------------------------------------------------------------------
// Simple HTTP handlers (unchanged)
// ---------------------------------------------------------------------------

/// Format app state as a JSON value.
fn format_state_json(state: &Arc<Mutex<SharedAppState>>) -> Value {
    let s = match state.lock() {
        Ok(s) => s,
        Err(_) => return json!({"error": "lock error"}),
    };

    let file = s
        .current_file
        .as_ref()
        .map(|p| Value::String(p.display().to_string()))
        .unwrap_or(Value::Null);

    json!({
        "file": file,
        "index": s.current_index + 1,
        "total_files": s.total_files,
        "zoom": s.zoom,
        "pan_x": s.pan_x,
        "pan_y": s.pan_y,
        "fullscreen": s.fullscreen,
        "auto_fit_window": s.auto_fit_window,
        "enlarge_small_images": s.enlarge_small_images,
        "window_x": s.window_x,
        "window_y": s.window_y,
        "window_width": s.window_width,
        "window_height": s.window_height,
        "image_width": s.image_width,
        "image_height": s.image_height,
        "image_render_x": s.image_render_x,
        "image_render_y": s.image_render_y,
        "image_render_width": s.image_render_width,
        "image_render_height": s.image_render_height,
        "min_zoom": s.min_zoom,
        "title": s.window_title,
    })
}

/// Send a command and wait for it to be processed by the event loop.
/// Returns the updated state as JSON.
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

/// Send a command via the event loop proxy and wait for processing, then return
/// the state JSON as an HTTP response. Used by simple HTTP POST handlers.
fn send_and_wait_http(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    command: AppCommand,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    proxy
        .send_event(command)
        .map_err(|e| format!("Event loop closed: {e}"))?;
    let (tx, rx) = mpsc::channel();
    proxy
        .send_event(AppCommand::Sync(tx))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    rx.recv_timeout(Duration::from_secs(2))
        .map_err(|e| format!("Command timeout: {e}"))?;
    let state_json = format_state_json(state);
    let body = serde_json::to_string_pretty(&state_json).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", body.as_bytes(), &[])
}

const MENU_TEXT: &str = "\
Prvw
  About Prvw
  ---
  Hide Prvw
  Hide others
  Show all
  ---
  Quit Prvw    Cmd+Q
File
  Close        Cmd+W
View
  Zoom in      +/Cmd+=
  Zoom out     -/Cmd+-
  ---
  Actual size  1/Cmd+1
  Fit to window 0/Cmd+0
  Auto-fit window (toggle)
  Enlarge small images (toggle, disabled when auto-fit is on)
  ---
  Fullscreen   F/Enter/F11/Cmd+F
  ---
  Refresh
Navigate
  Previous ←   ←/[/Backspace
  Next →        →/]/Space
";

fn handle_get_state(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let state_json = format_state_json(state);
    let body = serde_json::to_string_pretty(&state_json).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", body.as_bytes(), &[])
}

fn handle_get_menu(stream: &mut std::net::TcpStream) -> Result<(), String> {
    write_response(stream, 200, "text/plain", MENU_TEXT.as_bytes(), &[])
}

fn handle_get_screenshot(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    proxy
        .send_event(AppCommand::TakeScreenshot(tx))
        .map_err(|e| format!("Event loop closed: {e}"))?;

    // Wait up to 5 seconds for the screenshot
    let png_bytes = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Screenshot timeout: {e}"))?;

    if png_bytes.is_empty() {
        return write_response(
            stream,
            500,
            "text/plain",
            b"Screenshot capture failed (no image loaded)",
            &[],
        );
    }

    write_response(stream, 200, "image/png", &png_bytes, &[])
}

fn handle_get_diagnostics(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let text = state
        .lock()
        .map(|s| s.diagnostics_text.clone())
        .unwrap_or_else(|_| "(lock error)".to_string());
    write_response(stream, 200, "text/plain", text.as_bytes(), &[])
}

fn handle_post_key(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let key = body.trim().to_string();
    if key.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing key name in body", &[]);
    }
    send_and_wait_http(stream, proxy, AppCommand::SendKey(key), state)
}

fn handle_post_navigate(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let direction = body.trim().to_lowercase();
    let forward = match direction.as_str() {
        "next" | "forward" => true,
        "prev" | "previous" | "backward" => false,
        _ => {
            return write_response(
                stream,
                400,
                "text/plain",
                b"Body must be 'next' or 'prev'",
                &[],
            );
        }
    };
    send_and_wait_http(stream, proxy, AppCommand::Navigate(forward), state)
}

fn handle_post_zoom(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let value = body.trim().to_lowercase();
    let cmd = match value.as_str() {
        "fit" => AppCommand::FitToWindow,
        "actual" => AppCommand::ActualSize,
        _ => match value.parse::<f32>() {
            Ok(level) if level > 0.0 => AppCommand::SetZoom(level),
            _ => {
                return write_response(
                    stream,
                    400,
                    "text/plain",
                    b"Body must be 'fit', 'actual', or a positive float",
                    &[],
                );
            }
        },
    };
    send_and_wait_http(stream, proxy, cmd, state)
}

fn handle_post_fullscreen(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let value = body.trim().to_lowercase();
    let cmd = match value.as_str() {
        "toggle" => AppCommand::ToggleFullscreen,
        "on" => AppCommand::SetFullscreen(true),
        "off" => AppCommand::SetFullscreen(false),
        _ => {
            return write_response(
                stream,
                400,
                "text/plain",
                b"Body must be 'on', 'off', or 'toggle'",
                &[],
            );
        }
    };
    send_and_wait_http(stream, proxy, cmd, state)
}

fn handle_post_auto_fit(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let value = body.trim().to_lowercase();
    let enabled = match value.as_str() {
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        _ => {
            return write_response(
                stream,
                400,
                "text/plain",
                b"Body must be 'on'/'off', 'true'/'false', or '1'/'0'",
                &[],
            );
        }
    };
    send_and_wait_http(stream, proxy, AppCommand::SetAutoFitWindow(enabled), state)
}

fn handle_post_enlarge_small(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let value = body.trim().to_lowercase();
    let enabled = match value.as_str() {
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        _ => {
            return write_response(
                stream,
                400,
                "text/plain",
                b"Body must be 'on'/'off', 'true'/'false', or '1'/'0'",
                &[],
            );
        }
    };
    send_and_wait_http(
        stream,
        proxy,
        AppCommand::SetEnlargeSmallImages(enabled),
        state,
    )
}

fn handle_post_open(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let path_str = body.trim();
    if path_str.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing file path in body", &[]);
    }
    let path = PathBuf::from(path_str);
    send_and_wait_http(stream, proxy, AppCommand::OpenFile(path), state)
}

fn handle_post_window_geometry(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let x = parsed["x"].as_i64().map(|v| v as i32);
    let y = parsed["y"].as_i64().map(|v| v as i32);
    let width = parsed["width"].as_u64().map(|v| v as u32);
    let height = parsed["height"].as_u64().map(|v| v as u32);
    send_and_wait_http(
        stream,
        proxy,
        AppCommand::SetWindowGeometry {
            x,
            y,
            width,
            height,
        },
        state,
    )
}

fn handle_post_scroll_zoom(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let delta = parsed["delta"].as_f64().ok_or("missing 'delta'")? as f32;
    let cursor_x = parsed["cursor_x"].as_f64().ok_or("missing 'cursor_x'")? as f32;
    let cursor_y = parsed["cursor_y"].as_f64().ok_or("missing 'cursor_y'")? as f32;
    send_and_wait_http(
        stream,
        proxy,
        AppCommand::ScrollZoom {
            delta,
            cursor_x,
            cursor_y,
        },
        state,
    )
}

fn handle_post_zoom_in(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::ZoomIn, state)
}

fn handle_post_zoom_out(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::ZoomOut, state)
}

fn handle_post_refresh(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::Refresh, state)
}

fn handle_get_settings(stream: &mut std::net::TcpStream) -> Result<(), String> {
    let s = settings::Settings::load();
    let settings_json = json!({
        "auto_update": s.auto_update,
        "auto_fit_window": s.auto_fit_window,
        "enlarge_small_images": s.enlarge_small_images,
    });
    let body = serde_json::to_string_pretty(&settings_json).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", body.as_bytes(), &[])
}

// ---------------------------------------------------------------------------
// HTTP response writer
// ---------------------------------------------------------------------------

fn write_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[&str],
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };

    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for h in extra_headers {
        header.push_str(h);
        header.push_str("\r\n");
    }
    header.push_str("Connection: close\r\n\r\n");

    stream
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    Ok(())
}
