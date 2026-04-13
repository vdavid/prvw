//! MCP server configuration.

use std::env;

const DEFAULT_PORT: u16 = 19447;

/// Configuration for the MCP server.
#[derive(Debug, Clone)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: u16,
}

impl McpConfig {
    /// Load configuration from environment variables.
    /// `PRVW_MCP_PORT` overrides the default port. Set to 0 to disable.
    pub fn from_env() -> Self {
        let port_str = env::var("PRVW_MCP_PORT").unwrap_or_default();
        if port_str.is_empty() {
            return Self {
                enabled: true,
                port: DEFAULT_PORT,
            };
        }

        match port_str.parse::<u16>() {
            Ok(0) => Self {
                enabled: false,
                port: 0,
            },
            Ok(p) => Self {
                enabled: true,
                port: p,
            },
            Err(e) => {
                log::warn!(
                    "Invalid PRVW_MCP_PORT value '{port_str}': {e}, using default {DEFAULT_PORT}"
                );
                Self {
                    enabled: true,
                    port: DEFAULT_PORT,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direct_construction() {
        let config = McpConfig {
            enabled: true,
            port: 19447,
        };
        assert!(config.enabled);
        assert_eq!(config.port, 19447);
    }

    #[test]
    fn test_from_env_returns_config() {
        let config = McpConfig::from_env();
        assert!(config.port > 0 || !config.enabled);
    }
}
