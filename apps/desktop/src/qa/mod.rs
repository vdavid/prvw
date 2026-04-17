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

pub use server::start;
