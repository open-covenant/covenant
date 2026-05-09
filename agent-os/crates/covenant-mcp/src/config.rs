//! Operator-facing config for the MCP server registry.
//!
//! Section in `~/.covenant/secrets.toml`:
//!
//! ```toml
//! [[mcp.server]]
//! name    = "filesystem"
//! command = "npx"
//! args    = ["-y", "@modelcontextprotocol/server-filesystem", "/work"]
//! tool_prefix = "fs"
//! ```
//!
//! The `name` is informational (logging + audit). `tool_prefix` makes remote
//! tool names stable inside Covenant as `mcp_<prefix>_<upstream_tool>`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpConfigFile {
    #[serde(default)]
    pub mcp: Option<McpSection>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpSection {
    #[serde(default)]
    pub server: Vec<McpServer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tool_prefix: Option<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
}

impl McpConfigFile {
    pub fn from_path(p: &Path) -> Result<Self, ConfigError> {
        if !p.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(p)?;
        Ok(toml::from_str(&s)?)
    }

    pub fn servers(&self) -> &[McpServer] {
        self.mcp
            .as_ref()
            .map(|m| m.server.as_slice())
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_server_block() {
        let s = r#"
[[mcp.server]]
name    = "filesystem"
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "/work"]
tool_prefix = "fs"
"#;
        let cfg: McpConfigFile = toml::from_str(s).unwrap();
        let servers = cfg.servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "filesystem");
        assert_eq!(servers[0].command, "npx");
        assert_eq!(servers[0].args.len(), 3);
        assert!(servers[0].enabled);
        assert_eq!(servers[0].tool_prefix.as_deref(), Some("fs"));
    }

    #[test]
    fn empty_file_yields_zero_servers() {
        let cfg: McpConfigFile = toml::from_str("").unwrap();
        assert_eq!(cfg.servers().len(), 0);
    }

    #[test]
    fn parses_multiple_server_blocks() {
        let s = r#"
[[mcp.server]]
name = "fs"
command = "node"
args = ["fs.js"]

[[mcp.server]]
name = "git"
command = "node"
args = ["git.js"]
"#;
        let cfg: McpConfigFile = toml::from_str(s).unwrap();
        assert_eq!(cfg.servers().len(), 2);
        assert_eq!(cfg.servers()[1].name, "git");
    }

    #[test]
    fn ignores_unrelated_sections() {
        let s = r#"
[llm]
provider = "ollama"
model = "qwen2.5:7b"

[[mcp.server]]
name = "fs"
command = "node"
args = []
"#;
        let cfg: McpConfigFile = toml::from_str(s).unwrap();
        assert_eq!(cfg.servers().len(), 1);
    }

    #[test]
    fn parses_server_hygiene_fields() {
        let s = r#"
[[mcp.server]]
name = "hermes-agent"
command = "pnpm"
args = ["--filter", "@covenant/hermes-mcp-bridge", "start"]
enabled = false
tool_prefix = "hermes_agent"
include = ["hermes_run", "hermes_run_status"]
exclude = ["hermes_run_events"]
env = { HERMES_API_BASE_URL = "http://127.0.0.1:8642/v1" }
"#;
        let cfg: McpConfigFile = toml::from_str(s).unwrap();
        let srv = &cfg.servers()[0];
        assert!(!srv.enabled);
        assert_eq!(srv.tool_prefix.as_deref(), Some("hermes_agent"));
        assert_eq!(srv.include, vec!["hermes_run", "hermes_run_status"]);
        assert_eq!(srv.exclude, vec!["hermes_run_events"]);
        assert_eq!(
            srv.env.get("HERMES_API_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:8642/v1")
        );
    }
}
