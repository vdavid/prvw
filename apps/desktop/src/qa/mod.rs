//! QA and MCP surface: embedded HTTP server for automated tests and AI-agent control.
//!
//! - `server` — raw `TcpListener` HTTP server + MCP JSON-RPC. Reads `SharedAppState`
//!   (defined in `crate::app`) via an `Arc<Mutex<_>>`, sends commands via
//!   `EventLoopProxy<AppCommand>`.
//!
//! Commands flow through `crate::commands::AppCommand`. This module doesn't hold any
//! app-core types directly.

mod http;
mod mcp;
pub mod server;
/// The debug-only `screenshot_window` tool. macOS and Windows have a way to photograph a
/// window; nothing on Linux does yet, so the tool isn't registered there.
#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "windows")))]
mod window_capture;

pub use server::start;
