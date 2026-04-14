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
use winit::event_loop::EventLoopProxy;

/// Global event loop proxy, set once in `resumed()`. Allows non-main-loop code (like the
/// native Settings window delegate) to send commands into the event loop.
static EVENT_LOOP_PROXY: OnceLock<EventLoopProxy<AppCommand>> = OnceLock::new();

/// Store the event loop proxy so it's accessible from native UI delegates.
pub fn set_event_loop_proxy(proxy: EventLoopProxy<AppCommand>) {
    let _ = EVENT_LOOP_PROXY.set(proxy);
}

/// Send a command through the global event loop proxy. Returns false if the proxy
/// hasn't been set or the event loop is closed.
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
    pub window_width: u32,
    pub window_height: u32,
    pub window_title: String,
    /// Whether auto-fit window is enabled.
    pub auto_fit_window: bool,
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
            window_width: 0,
            window_height: 0,
            window_title: String::new(),
            auto_fit_window: true,
            diagnostics_text: String::new(),
        }
    }
}

/// Commands sent from the HTTP server to the main event loop.
pub enum AppCommand {
    /// Simulate a key press. Key name follows web conventions: "ArrowLeft", "Escape", "f", etc.
    SendKey(String),
    /// Navigate forward (true) or backward (false).
    Navigate(bool),
    /// Set absolute zoom level.
    SetZoom(f32),
    /// Reset zoom to fit the image in the window.
    FitToWindow,
    /// Set zoom to 1:1 pixel mapping.
    ActualSize,
    /// Toggle fullscreen mode.
    ToggleFullscreen,
    /// Set fullscreen on or off explicitly.
    SetFullscreen(bool),
    /// Open a specific file.
    OpenFile(PathBuf),
    /// Set auto-fit window mode.
    SetAutoFitWindow(bool),
    /// Capture a screenshot. The sender receives PNG bytes.
    TakeScreenshot(mpsc::Sender<Vec<u8>>),
}

const DEFAULT_PORT: u16 = 19447;

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
        ("POST", "/key") => handle_post_key(stream, proxy, &body),
        ("POST", "/navigate") => handle_post_navigate(stream, proxy, &body),
        ("POST", "/zoom") => handle_post_zoom(stream, proxy, &body),
        ("POST", "/fullscreen") => handle_post_fullscreen(stream, proxy, &body),
        ("POST", "/auto-fit") => handle_post_auto_fit(stream, proxy, &body),
        ("POST", "/open") => handle_post_open(stream, proxy, &body),
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
                "next" => true,
                "prev" | "previous" => false,
                _ => return Err(json_rpc_error(-32602, "direction must be 'next' or 'prev'")),
            };
            proxy
                .send_event(AppCommand::Navigate(forward))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            // Brief pause to let the event loop process the navigation.
            std::thread::sleep(std::time::Duration::from_millis(50));
            let state_text = format_state_text(state);
            Ok(mcp_text_content(&format!(
                "Navigated {direction}.\n\n{state_text}"
            )))
        }
        "key" => {
            let key = args["key"].as_str().unwrap_or("").to_string();
            if key.is_empty() {
                return Err(json_rpc_error(-32602, "key is required"));
            }
            proxy
                .send_event(AppCommand::SendKey(key.clone()))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            Ok(mcp_text_content(&format!("Sent key: {key}")))
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
            proxy
                .send_event(cmd)
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            Ok(mcp_text_content(&format!("Zoom set to: {level}")))
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
            proxy
                .send_event(cmd)
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            Ok(mcp_text_content(&format!("Fullscreen: {mode}")))
        }
        "open" => {
            let path_str = args["path"].as_str().unwrap_or("");
            if path_str.is_empty() {
                return Err(json_rpc_error(-32602, "path is required"));
            }
            proxy
                .send_event(AppCommand::OpenFile(PathBuf::from(path_str)))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            Ok(mcp_text_content(&format!("Opened: {path_str}")))
        }
        "auto_fit_window" => {
            let enabled = args["enabled"]
                .as_bool()
                .ok_or_else(|| json_rpc_error(-32602, "enabled must be a boolean"))?;
            proxy
                .send_event(AppCommand::SetAutoFitWindow(enabled))
                .map_err(|_| json_rpc_error(-32603, "Event loop closed"))?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            let label = if enabled { "enabled" } else { "disabled" };
            Ok(mcp_text_content(&format!("Auto-fit window: {label}")))
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
                "description": "Current file, zoom, pan, fullscreen, window size.",
                "mimeType": "text/plain"
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
            let text = format_state_text(state);
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
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

/// Format app state as human-readable text. Shared between simple HTTP and MCP.
fn format_state_text(state: &Arc<Mutex<SharedAppState>>) -> String {
    let s = match state.lock() {
        Ok(s) => s,
        Err(_) => return "(lock error)".to_string(),
    };

    let file_display = s
        .current_file
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(none)".to_string());

    let zoom_label = if (s.zoom - 1.0).abs() < 0.005 {
        " (fit to window)"
    } else {
        ""
    };

    let fullscreen_str = if s.fullscreen { "yes" } else { "no" };
    let auto_fit_str = if s.auto_fit_window { "yes" } else { "no" };

    format!(
        "file: {file_display}\n\
         index: {} of {}\n\
         zoom: {:.2}{zoom_label}\n\
         pan: ({:.2}, {:.2})\n\
         fullscreen: {fullscreen_str}\n\
         auto_fit_window: {auto_fit_str}\n\
         window: {}x{}\n\
         title: {}\n",
        s.current_index + 1,
        s.total_files,
        s.zoom,
        s.pan_x,
        s.pan_y,
        s.window_width,
        s.window_height,
        s.window_title,
    )
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
  ---
  Fullscreen   F/Enter/F11/Cmd+F
Navigate
  Previous ←   ←/[/Backspace
  Next →        →/]/Space
";

fn handle_get_state(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let body = format_state_text(state);
    write_response(stream, 200, "text/plain", body.as_bytes(), &[])
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
) -> Result<(), String> {
    let key = body.trim().to_string();
    if key.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing key name in body", &[]);
    }
    proxy
        .send_event(AppCommand::SendKey(key))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
}

fn handle_post_navigate(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
) -> Result<(), String> {
    let direction = body.trim().to_lowercase();
    let forward = match direction.as_str() {
        "next" => true,
        "prev" | "previous" => false,
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
    proxy
        .send_event(AppCommand::Navigate(forward))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
}

fn handle_post_zoom(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
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
    proxy
        .send_event(cmd)
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
}

fn handle_post_fullscreen(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
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
    proxy
        .send_event(cmd)
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
}

fn handle_post_auto_fit(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
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
    proxy
        .send_event(AppCommand::SetAutoFitWindow(enabled))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
}

fn handle_post_open(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
) -> Result<(), String> {
    let path_str = body.trim();
    if path_str.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing file path in body", &[]);
    }
    let path = PathBuf::from(path_str);
    proxy
        .send_event(AppCommand::OpenFile(path))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok", &[])
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
