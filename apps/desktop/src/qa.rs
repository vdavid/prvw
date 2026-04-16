//! QA and MCP surface: embedded HTTP server for automated tests and AI-agent control.
//!
//! - `server` — raw `TcpListener` HTTP server + MCP JSON-RPC. Reads `SharedAppState`
//!   via an `Arc<Mutex<_>>`, sends commands via `EventLoopProxy<AppCommand>`.
//!
//! The server is the outside-world's view of the viewer. It MUST NOT hold app-core
//! types directly — commands flow through `crate::commands::AppCommand`.

pub mod server;

pub use server::{SharedAppState, start};
