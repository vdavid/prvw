//! MCP HTTP server (Axum-based, JSON-RPC 2.0).
//!
//! Binds to localhost only for security. Handles MCP protocol messages
//! and routes them to tools and resources.

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Runtime};

use super::config::McpConfig;
use super::executor::execute_tool;
use super::protocol::{
    INVALID_PARAMS, METHOD_NOT_FOUND, McpRequest, McpResponse, ServerCapabilities,
};
use super::resources::{get_all_resources, read_resource};
use super::tools::get_all_tools;

/// Handle to the running MCP server task.
static MCP_HANDLE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// The port the server is actually listening on (0 when not running).
static MCP_ACTUAL_PORT: AtomicU16 = AtomicU16::new(0);

/// Shared state for the MCP server.
pub struct McpState<R: Runtime> {
    pub app: AppHandle<R>,
}

/// Start the MCP server. Binds to the configured port and spawns the server task.
pub async fn start_mcp_server<R: Runtime + 'static>(
    app: AppHandle<R>,
    config: McpConfig,
) -> Result<(), String> {
    if !config.enabled {
        log::info!("MCP server is disabled");
        return Ok(());
    }

    if is_mcp_running() {
        log::debug!("MCP server is already running, ignoring start request");
        return Ok(());
    }

    let state = Arc::new(McpState { app });

    let router = Router::new()
        .route("/mcp", post(handle_mcp_post::<R>))
        .route("/mcp/health", get(health_check))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("MCP server couldn't bind to port {}: {e}", config.port))?;

    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(config.port);

    log::info!("MCP server listening on http://127.0.0.1:{port}");
    MCP_ACTUAL_PORT.store(port, Ordering::Relaxed);

    let handle = tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            log::error!("MCP server crashed: {e}");
        }
        MCP_ACTUAL_PORT.store(0, Ordering::Relaxed);
    });

    if let Ok(mut guard) = MCP_HANDLE.lock() {
        *guard = Some(handle);
    }

    Ok(())
}

/// Returns whether the MCP server is running.
pub fn is_mcp_running() -> bool {
    MCP_ACTUAL_PORT.load(Ordering::Relaxed) != 0
}

/// Returns the actual port the MCP server is listening on, or `None` if not running.
#[allow(dead_code)]
pub fn get_mcp_actual_port() -> Option<u16> {
    let port = MCP_ACTUAL_PORT.load(Ordering::Relaxed);
    if port == 0 { None } else { Some(port) }
}

/// Health check endpoint.
async fn health_check() -> Json<Value> {
    Json(json!({"ok": true}))
}

/// Handle POST /mcp — main JSON-RPC request handler.
async fn handle_mcp_post<R: Runtime>(
    State(state): State<Arc<McpState<R>>>,
    Json(request): Json<McpRequest>,
) -> Json<McpResponse> {
    log::debug!("MCP: {} (id={:?})", request.method, request.id);

    let response = match request.method.as_str() {
        "initialize" => {
            let caps = ServerCapabilities::default();
            McpResponse::success(request.id, serde_json::to_value(caps).unwrap())
        }

        "notifications/initialized" => {
            McpResponse::success(request.id, json!({"acknowledged": true}))
        }

        "tools/list" => {
            let tools = get_all_tools();
            McpResponse::success(request.id, json!({"tools": tools}))
        }

        "tools/call" => {
            let name = match request.params.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => {
                    return Json(McpResponse::error(
                        request.id,
                        INVALID_PARAMS,
                        "Missing 'name' parameter",
                    ));
                }
            };

            let arguments = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));

            log::debug!("MCP: executing tool {name}");
            match execute_tool(&state.app, name, &arguments) {
                Ok(value) => {
                    // If the tool already returned structured MCP content (e.g., screenshot
                    // with image data), pass it through. Otherwise wrap as text.
                    let result = if value.get("content").is_some() {
                        value
                    } else {
                        let text = format_tool_result(&value);
                        json!({"content": [{"type": "text", "text": text}]})
                    };
                    McpResponse::success(request.id, result)
                }
                Err(e) => {
                    log::warn!("MCP: tool {name} failed: {}", e.message);
                    McpResponse::error(request.id, e.code, e.message)
                }
            }
        }

        "resources/list" => {
            let resources = get_all_resources();
            McpResponse::success(request.id, json!({"resources": resources}))
        }

        "resources/read" => {
            let uri = match request.params.get("uri").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    return Json(McpResponse::error(
                        request.id,
                        INVALID_PARAMS,
                        "Missing 'uri' parameter",
                    ));
                }
            };

            match read_resource(&state.app, uri) {
                Ok(content) => McpResponse::success(request.id, json!({"contents": [content]})),
                Err(e) => McpResponse::error(request.id, INVALID_PARAMS, e),
            }
        }

        "ping" => McpResponse::success(request.id, json!({})),

        _ => McpResponse::error(
            request.id,
            METHOD_NOT_FOUND,
            format!("Unknown method: {}", request.method),
        ),
    };

    Json(response)
}

/// Format tool result for MCP content response.
fn format_tool_result(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        s.to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tool_result_string() {
        assert_eq!(format_tool_result(&json!("hello")), "hello");
    }

    #[test]
    fn test_format_tool_result_object() {
        let result = format_tool_result(&json!({"key": "value"}));
        assert!(result.contains("key"));
    }
}
