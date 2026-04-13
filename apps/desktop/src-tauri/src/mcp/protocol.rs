//! MCP protocol message types (JSON-RPC 2.0).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP JSON-RPC request.
#[derive(Debug, Clone, Deserialize)]
pub struct McpRequest {
    #[allow(dead_code, reason = "Required by JSON-RPC spec, validated in handler")]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// MCP JSON-RPC response.
#[derive(Debug, Clone, Serialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

/// MCP error object.
#[derive(Debug, Clone, Serialize)]
pub struct McpError {
    pub code: i32,
    pub message: String,
}

// JSON-RPC 2.0 standard error codes
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
pub const METHOD_NOT_FOUND: i32 = -32601;

impl McpResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(McpError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Server capabilities returned during initialization.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub protocol_version: String,
    pub capabilities: Capabilities,
    pub server_info: ServerInfo,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub tools: ToolCapabilities,
    pub resources: ResourceCapabilities,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCapabilities {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceCapabilities {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self {
            protocol_version: "2025-03-26".to_string(),
            capabilities: Capabilities {
                tools: ToolCapabilities {
                    list_changed: false,
                },
                resources: ResourceCapabilities {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: "prvw".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: concat!(
                "Controls a running Prvw image viewer instance. Use these tools to navigate ",
                "between images, control zoom and fullscreen, open specific files, and take ",
                "screenshots.\n\n",
                "Start by reading the prvw://state resource to understand which image is ",
                "currently displayed, zoom level, and window state before taking actions.",
            )
            .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_request() {
        let json_str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let request: McpRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(request.method, "tools/list");
        assert_eq!(request.id, Some(json!(1)));
    }

    #[test]
    fn test_success_response() {
        let response = McpResponse::success(Some(json!(1)), json!({"status": "ok"}));
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["result"]["status"], "ok");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_error_response() {
        let response = McpResponse::error(Some(json!(1)), METHOD_NOT_FOUND, "Not found");
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(json["error"]["message"], "Not found");
        assert!(json.get("result").is_none());
    }

    #[test]
    fn test_server_capabilities() {
        let caps = ServerCapabilities::default();
        assert_eq!(caps.server_info.name, "prvw");
    }
}
