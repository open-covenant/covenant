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
    fn mcp_server_serde_pins_two_required_and_six_default_bearing_fields() {
        // McpServer is the [[mcp.server]] block operators write to
        // secrets.toml to register an external MCP tool server. Two
        // strictly required fields (name, command) with no serde
        // attributes, and six default-bearing fields:
        //   * args: Vec<String>             #[serde(default)] -> []
        //   * enabled: bool                 #[serde(default = "default_enabled")] -> true
        //   * tool_prefix: Option<String>   #[serde(default)] -> None
        //   * include: Vec<String>          #[serde(default)] -> []
        //   * exclude: Vec<String>          #[serde(default)] -> []
        //   * env: BTreeMap<String, String> #[serde(default)] -> empty
        //
        // default_enabled returning true is the most load-bearing default
        // in the file. A refactor that flipped it to false would silently
        // disable every operator's MCP server block that does not write
        // an explicit `enabled = true` line; the daemon's tool registry
        // would collapse to the native EchoTool/ClockTool floor on next
        // restart with no parse-time signal and agents would lose every
        // registered external tool. The existing parses_server_hygiene_
        // fields test exercises a happy path with enabled = false but
        // does NOT pin the default = true contract for an omitted field.
        let full = r#"
[[mcp.server]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/work"]
enabled = false
tool_prefix = "fs"
include = ["read_file"]
exclude = ["write_file"]
env = { FOO = "bar", BAZ = "qux" }
"#;
        let cfg: McpConfigFile = toml::from_str(full).unwrap();
        let srv = &cfg.servers()[0];
        assert_eq!(srv.name, "filesystem");
        assert_eq!(srv.command, "npx");
        assert_eq!(
            srv.args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                "/work".to_string()
            ],
        );
        assert!(!srv.enabled);
        assert_eq!(srv.tool_prefix.as_deref(), Some("fs"));
        assert_eq!(srv.include, vec!["read_file".to_string()]);
        assert_eq!(srv.exclude, vec!["write_file".to_string()]);
        assert_eq!(srv.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(srv.env.get("BAZ").map(String::as_str), Some("qux"));

        // Minimal block — only the two required fields. Every default-
        // bearing field must fall through to its documented default.
        let minimal = r#"
[[mcp.server]]
name = "tiny"
command = "node"
"#;
        let cfg: McpConfigFile = toml::from_str(minimal).unwrap();
        let srv = &cfg.servers()[0];
        assert_eq!(srv.name, "tiny");
        assert_eq!(srv.command, "node");
        assert!(
            srv.args.is_empty(),
            "McpServer::args default must be the empty Vec",
        );
        assert!(
            srv.enabled,
            "McpServer::enabled default MUST be true — the default_enabled \
             function returning true is the load-bearing default for every \
             [[mcp.server]] block that omits an explicit enabled key; a \
             refactor that flipped default_enabled to false silently \
             disables every operator's MCP server on next daemon restart \
             with no parse-time signal and the daemon's tool registry \
             collapses to the native EchoTool/ClockTool floor",
        );
        assert!(
            srv.tool_prefix.is_none(),
            "McpServer::tool_prefix default must be None",
        );
        assert!(
            srv.include.is_empty(),
            "McpServer::include default must be the empty Vec",
        );
        assert!(
            srv.exclude.is_empty(),
            "McpServer::exclude default must be the empty Vec",
        );
        assert!(
            srv.env.is_empty(),
            "McpServer::env default must be the empty BTreeMap",
        );

        // Each strictly-required field must reject omission so a future
        // #[serde(default)] regression on name or command does not let
        // operator deployments boot with empty-string identifiers — the
        // daemon's MCP audit rows correlate on (server name, tool
        // invocation) and an empty name silently collapses every server
        // into one audit bucket.
        for (label, toml_src) in [
            (
                "name",
                r#"
[[mcp.server]]
command = "node"
"#,
            ),
            (
                "command",
                r#"
[[mcp.server]]
name = "tiny"
"#,
            ),
        ] {
            assert!(
                toml::from_str::<McpConfigFile>(toml_src).is_err(),
                "McpServer must reject a block missing {label:?}; a \
                 refactor that gave the field a #[serde(default)] would \
                 silently let the block parse with an empty-string \
                 default and operator MCP audit correlation would break",
            );
        }

        // env BTreeMap deterministic ordering — operators writing env
        // keys in arbitrary order must see them surface in BTreeMap-
        // sorted order on iteration. A refactor that swapped BTreeMap
        // for HashMap silently breaks fixture-based audit/JSON output
        // tests that depend on stable key ordering.
        let ordered = r#"
[[mcp.server]]
name = "ordered"
command = "node"
env = { ZULU = "z", ALPHA = "a", MIKE = "m" }
"#;
        let cfg: McpConfigFile = toml::from_str(ordered).unwrap();
        let keys: Vec<&str> = cfg.servers()[0].env.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["ALPHA", "MIKE", "ZULU"],
            "McpServer::env must iterate in BTreeMap-sorted key order \
             — a refactor swapping BTreeMap for HashMap silently breaks \
             fixture-based audit and JSON output tests that depend on \
             stable key ordering",
        );
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
