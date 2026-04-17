//! Simple HTTP endpoint handlers. Used by E2E tests and cURL debugging.
//!
//! Each handler reads `SharedAppState` or sends an `AppCommand`, then writes a JSON
//! (or text/PNG) response via `super::server::write_response`.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

use super::server::{format_state_json, write_response};
use crate::app::SharedAppState;
use crate::commands::AppCommand;
use crate::settings;

/// Send an `AppCommand`, wait for the event loop to acknowledge via a `Sync` barrier,
/// then write the updated state snapshot as an HTTP JSON response.
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

pub(super) fn handle_post_close_settings(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::CloseSettings, state)
}

pub(super) const MENU_TEXT: &str = "\
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

pub(super) fn handle_get_state(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let state_json = format_state_json(state);
    let body = serde_json::to_string_pretty(&state_json).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", body.as_bytes(), &[])
}

pub(super) fn handle_get_menu(stream: &mut std::net::TcpStream) -> Result<(), String> {
    write_response(stream, 200, "text/plain", MENU_TEXT.as_bytes(), &[])
}

pub(super) fn handle_get_screenshot(
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

pub(super) fn handle_get_diagnostics(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let text = state
        .lock()
        .map(|s| s.diagnostics_text.clone())
        .unwrap_or_else(|_| "(lock error)".to_string());
    write_response(stream, 200, "text/plain", text.as_bytes(), &[])
}

pub(super) fn handle_post_key(
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

pub(super) fn handle_post_navigate(
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

pub(super) fn handle_post_zoom(
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

pub(super) fn handle_post_fullscreen(
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

pub(super) fn handle_post_auto_fit(
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

pub(super) fn handle_post_enlarge_small(
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

pub(super) fn handle_post_scroll_to_zoom(
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
    send_and_wait_http(stream, proxy, AppCommand::SetScrollToZoom(enabled), state)
}

pub(super) fn handle_post_title_bar(
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
    send_and_wait_http(stream, proxy, AppCommand::SetTitleBar(enabled), state)
}

pub(super) fn handle_post_open(
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

pub(super) fn handle_post_window_geometry(
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

pub(super) fn handle_post_scroll_zoom(
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

pub(super) fn handle_post_zoom_in(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::ZoomIn, state)
}

pub(super) fn handle_post_zoom_out(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::ZoomOut, state)
}

pub(super) fn handle_post_refresh(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    send_and_wait_http(stream, proxy, AppCommand::Refresh, state)
}

pub(super) fn handle_post_show_settings(
    stream: &mut std::net::TcpStream,
    proxy: &EventLoopProxy<AppCommand>,
    body: &str,
    state: &Arc<Mutex<SharedAppState>>,
) -> Result<(), String> {
    let section = body.trim().to_string();
    if section.is_empty() {
        send_and_wait_http(stream, proxy, AppCommand::ShowSettings, state)
    } else {
        proxy
            .send_event(AppCommand::ShowSettings)
            .map_err(|e| format!("Event loop closed: {e}"))?;
        send_and_wait_http(
            stream,
            proxy,
            AppCommand::ShowSettingsSection(section),
            state,
        )
    }
}

pub(super) fn handle_get_settings(stream: &mut std::net::TcpStream) -> Result<(), String> {
    let s = settings::Settings::load();
    let settings_json = json!({
        "auto_update": s.auto_update,
        "auto_fit_window": s.auto_fit_window,
        "enlarge_small_images": s.enlarge_small_images,
    });
    let body = serde_json::to_string_pretty(&settings_json).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", body.as_bytes(), &[])
}
