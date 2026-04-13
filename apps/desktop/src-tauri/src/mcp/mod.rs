//! MCP (Model Context Protocol) server module.
//!
//! Exposes Prvw functionality to AI agents via a JSON-RPC HTTP server.
//! Agents can navigate images, control zoom/fullscreen, and read app state.

mod config;
mod executor;
mod protocol;
mod resources;
mod server;
mod tools;

pub use config::McpConfig;
pub use server::start_mcp_server;

// Available for future use (for example, frontend querying port, live restart).
#[allow(unused_imports)]
pub use server::{get_mcp_actual_port, is_mcp_running};
