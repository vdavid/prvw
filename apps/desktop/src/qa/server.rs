//! Listener loop and shared HTTP utilities for the QA server.
//!
//! - `start` / `server_loop` — accept connections on `PRVW_QA_PORT`
//!   (default 19447, `0` disables).
//! - `handle_request` — parse the request line + headers + body, dispatch to
//!   `http::handle_*` or `mcp::handle_mcp`.
//! - `write_response` + `format_state_json` — shared utilities for both dispatchers.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use winit::event_loop::EventLoopProxy;

use super::{http, mcp};
use crate::app::SharedAppState;
use crate::commands::AppCommand;

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
        ("POST", "/mcp") => mcp::handle_mcp(stream, state, proxy, &body, session_id),
        // Simple HTTP endpoints
        ("GET", "/state") => http::handle_get_state(stream, state),
        ("GET", "/menu") => http::handle_get_menu(stream),
        ("GET", "/screenshot") => http::handle_get_screenshot(stream, proxy),
        ("GET", "/diagnostics") => http::handle_get_diagnostics(stream, state),
        ("POST", "/key") => http::handle_post_key(stream, proxy, &body, state),
        ("POST", "/navigate") => http::handle_post_navigate(stream, proxy, &body, state),
        ("POST", "/zoom") => http::handle_post_zoom(stream, proxy, &body, state),
        ("POST", "/fullscreen") => http::handle_post_fullscreen(stream, proxy, &body, state),
        ("POST", "/auto-fit") => http::handle_post_auto_fit(stream, proxy, &body, state),
        ("POST", "/enlarge-small") => http::handle_post_enlarge_small(stream, proxy, &body, state),
        ("POST", "/scroll-to-zoom") => {
            http::handle_post_scroll_to_zoom(stream, proxy, &body, state)
        }
        ("POST", "/title-bar") => http::handle_post_title_bar(stream, proxy, &body, state),
        ("POST", "/open") => http::handle_post_open(stream, proxy, &body, state),
        ("POST", "/window-geometry") => {
            http::handle_post_window_geometry(stream, proxy, &body, state)
        }
        ("POST", "/scroll-zoom") => http::handle_post_scroll_zoom(stream, proxy, &body, state),
        ("POST", "/zoom-in") => http::handle_post_zoom_in(stream, proxy, state),
        ("POST", "/zoom-out") => http::handle_post_zoom_out(stream, proxy, state),
        ("POST", "/refresh") => http::handle_post_refresh(stream, proxy, state),
        ("POST", "/show-settings") => http::handle_post_show_settings(stream, proxy, &body, state),
        ("POST", "/close-settings") => http::handle_post_close_settings(stream, proxy, state),
        ("GET", "/settings") => http::handle_get_settings(stream),
        _ => write_response(stream, 404, "text/plain", b"Not found", &[]),
    }
}

/// Format app state as a JSON value. Shared by HTTP and MCP handlers.
pub(super) fn format_state_json(state: &Arc<Mutex<SharedAppState>>) -> Value {
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
        "scroll_to_zoom": s.scroll_to_zoom,
        "title_bar": s.title_bar,
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

/// Write an HTTP response with the given status, content type, body, and extra headers.
pub(super) fn write_response(
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
