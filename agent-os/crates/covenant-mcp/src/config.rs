//! Operator-facing config for the MCP server registry.
//!
//! Section in `~/.covenant/secrets.toml`:
//!
//! ```toml
//! [[mcp.server]]
//! name    = "filesystem"
//! command = "npx"
//! args    = ["-y", "@modelcontextprotocol/server-filesystem", "/work"]
//! ```
//!
//! The `name` is informational (logging + audit); the live tool names come
//! from the server's own `tools/list`.

use serde::Deserialize;
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
"#;
        let cfg: McpConfigFile = toml::from_str(s).unwrap();
        let servers = cfg.servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "filesystem");
        assert_eq!(servers[0].command, "npx");
        assert_eq!(servers[0].args.len(), 3);
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
}
