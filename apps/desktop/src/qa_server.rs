//! Embedded HTTP server for QA/E2E testing. Lets agents and tests interact with the running app
//! via simple HTTP endpoints: query state, send keys, navigate, zoom, take screenshots.
//!
//! The server runs on a background thread using a raw `TcpListener` (no external HTTP crate).
//! Port is controlled by `PRVW_QA_PORT` env var (default 19447, set to 0 to disable).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use winit::event_loop::EventLoopProxy;

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

    eprintln!("QA server listening on http://127.0.0.1:{port}");

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

        if let Err(e) = handle_request(&mut stream, &state, &proxy) {
            log::debug!("QA server request error: {e}");
            let _ = write_response(
                &mut stream,
                500,
                "text/plain",
                format!("Internal error: {e}").as_bytes(),
            );
        }
    }
}

/// Parse an HTTP request and dispatch to the appropriate handler.
fn handle_request(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
    proxy: &EventLoopProxy<AppCommand>,
) -> Result<(), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    // Read the request line
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| e.to_string())?;
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 {
        return write_response(stream, 400, "text/plain", b"Bad request");
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

    match (method, path) {
        ("GET", "/state") => handle_get_state(stream, state),
        ("GET", "/menu") => handle_get_menu(stream),
        ("GET", "/screenshot") => handle_get_screenshot(stream, proxy),
        ("POST", "/key") => handle_post_key(stream, proxy, &body),
        ("POST", "/navigate") => handle_post_navigate(stream, proxy, &body),
        ("POST", "/zoom") => handle_post_zoom(stream, proxy, &body),
        ("POST", "/fullscreen") => handle_post_fullscreen(stream, proxy, &body),
        ("POST", "/open") => handle_post_open(stream, proxy, &body),
        _ => write_response(stream, 404, "text/plain", b"Not found"),
    }
}

fn handle_get_state(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let s = state.lock().map_err(|e| e.to_string())?;

    let file_display = s
        .current_file
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(none)".to_string());

    let zoom_label = if (s.zoom - 1.0).abs() < 0.005 {
        " (fit to window)".to_string()
    } else {
        String::new()
    };

    let fullscreen_str = if s.fullscreen { "yes" } else { "no" };

    let body = format!(
        "file: {file_display}\n\
         index: {} of {}\n\
         zoom: {:.2}{zoom_label}\n\
         pan: ({:.2}, {:.2})\n\
         fullscreen: {fullscreen_str}\n\
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
    );

    write_response(stream, 200, "text/plain", body.as_bytes())
}

fn handle_get_menu(stream: &mut std::net::TcpStream) -> Result<(), String> {
    // Menu structure is static, so we hardcode it here.
    let body = "\
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
  Zoom in      Cmd+=
  Zoom out     Cmd+-
  ---
  Actual size  Cmd+1
  Fit to window Cmd+0
  ---
  Fullscreen   Cmd+F
Navigate
  Previous
  Next
";

    write_response(stream, 200, "text/plain", body.as_bytes())
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
        );
    }

    write_response(stream, 200, "image/png", &png_bytes)
}

fn handle_post_key(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
) -> Result<(), String> {
    let key = body.trim().to_string();
    if key.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing key name in body");
    }
    proxy
        .send_event(AppCommand::SendKey(key))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok")
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
        _ => return write_response(stream, 400, "text/plain", b"Body must be 'next' or 'prev'"),
    };
    proxy
        .send_event(AppCommand::Navigate(forward))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok")
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
                );
            }
        },
    };
    proxy
        .send_event(cmd)
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok")
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
            );
        }
    };
    proxy
        .send_event(cmd)
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok")
}

fn handle_post_open(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
) -> Result<(), String> {
    let path_str = body.trim();
    if path_str.is_empty() {
        return write_response(stream, 400, "text/plain", b"Missing file path in body");
    }
    let path = PathBuf::from(path_str);
    proxy
        .send_event(AppCommand::OpenFile(path))
        .map_err(|e| format!("Event loop closed: {e}"))?;
    write_response(stream, 200, "text/plain", b"ok")
}

fn write_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };

    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );

    stream
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    Ok(())
}
