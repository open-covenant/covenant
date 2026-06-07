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
//!
//! The hosted Solana MCP server is wired through a dedicated, off-by-default
//! bridge rather than a hand-written server block:
//!
//! ```toml
//! [mcp.solana]
//! enabled = true          # default false — nothing connects unless set
//! # url = "https://mcp.solana.com/mcp"   # optional override
//! # tool_prefix = "solana"               # tools land as mcp_solana_*
//! ```
//!
//! When enabled it materializes into an ordinary stdio server (the
//! `mcp-remote` shim), so it runs behind the same subprocess sandbox as
//! every other MCP server.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::external::{hosted_bridge_command, SOLANA_MCP_URL};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpConfigFile {
    #[serde(default)]
    pub mcp: Option<McpSection>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpSection {
    #[serde(default)]
    pub server: Vec<McpServer>,
    #[serde(default)]
    pub solana: Option<SolanaBridge>,
}

/// Config-gated bridge to the hosted Solana MCP server. Off unless
/// `enabled = true`; when on it resolves to a stdio `mcp-remote` server so
/// skills get live Solana docs inside the governed runtime without a new
/// in-process transport.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SolanaBridge {
    pub enabled: bool,
    pub url: String,
    pub tool_prefix: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for SolanaBridge {
    fn default() -> Self {
        Self {
            enabled: false,
            url: SOLANA_MCP_URL.to_string(),
            tool_prefix: "solana".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

impl SolanaBridge {
    /// Materialize into a stdio server entry the daemon spawns like any
    /// other, or `None` when disabled. `NO_DNA=1` mirrors the hosted
    /// server's non-human-CLI signal.
    fn to_server(&self) -> Option<McpServer> {
        if !self.enabled {
            return None;
        }
        let (command, args) = hosted_bridge_command(&self.url);
        let mut env = BTreeMap::new();
        env.insert("NO_DNA".to_string(), "1".to_string());
        Some(McpServer {
            name: "solana".to_string(),
            command,
            args,
            enabled: true,
            tool_prefix: Some(self.tool_prefix.clone()),
            include: self.include.clone(),
            exclude: self.exclude.clone(),
            env,
        })
    }
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
        let mut cfg: Self = toml::from_str(&s)?;
        cfg.materialize_hosted_bridges();
        Ok(cfg)
    }

    /// Fold any enabled hosted bridges (`[mcp.solana]`) into the server list
    /// so `servers()` is the effective set the daemon spawns. Disabled or
    /// absent bridges add nothing.
    fn materialize_hosted_bridges(&mut self) {
        if let Some(section) = self.mcp.as_mut() {
            if let Some(server) = section.solana.as_ref().and_then(SolanaBridge::to_server) {
                section.server.push(server);
            }
        }
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
    fn mcp_section_default_server() {
        let empty_section = r#"
[mcp]
"#;
        let cfg: McpConfigFile = toml::from_str(empty_section).unwrap();
        let section = cfg
            .mcp
            .as_ref()
            .expect("[mcp] header surfaces McpSection even with no [[mcp.server]] entries");
        assert!(
            section.server.is_empty(),
            "an empty [mcp] section parses to an empty server Vec, not an error",
        );

        // Implicit-section: [[mcp.server]] arrays without an explicit [mcp] header still populate.
        let two_servers = r#"
[[mcp.server]]
name = "fs"
command = "node"

[[mcp.server]]
name = "git"
command = "node"
"#;
        let cfg: McpConfigFile = toml::from_str(two_servers).unwrap();
        let section = cfg.mcp.as_ref().unwrap();
        assert_eq!(section.server.len(), 2);
        assert_eq!(section.server[0].name, "fs");
        assert_eq!(section.server[1].name, "git");

        let default_section = McpSection::default();
        assert!(
            default_section.server.is_empty(),
            "McpSection::default() has an empty server Vec",
        );

        let bare: McpSection = toml::from_str("").unwrap();
        assert!(
            bare.server.is_empty(),
            "an empty TOML body parses to an empty server Vec",
        );
    }

    #[test]
    fn mcp_config_file_optional_section() {
        let empty: McpConfigFile = toml::from_str("").unwrap();
        assert!(
            empty.mcp.is_none(),
            "an empty TOML payload decodes with mcp == None",
        );
        assert!(
            empty.servers().is_empty(),
            "servers() on the empty decode returns the empty slice",
        );

        // Unknown root sections (operators co-locate [llm]/[embed]/[search]) must not reject the file.
        let other_root = r#"
[llm]
provider = "anthropic"
api_key = "sk"

[embed]
provider = "ollama"
"#;
        let cfg: McpConfigFile = toml::from_str(other_root).unwrap();
        assert!(
            cfg.mcp.is_none(),
            "unknown root sections parse without mcp being set",
        );
        assert!(cfg.servers().is_empty());

        let full = r#"
[[mcp.server]]
name = "filesystem"
command = "npx"

[[mcp.server]]
name = "git"
command = "node"
"#;
        let cfg: McpConfigFile = toml::from_str(full).unwrap();
        assert!(
            cfg.mcp.is_some(),
            "mcp is Some when [[mcp.server]] entries exist",
        );
        assert_eq!(cfg.servers().len(), 2);
        assert_eq!(cfg.servers()[0].name, "filesystem");
        assert_eq!(cfg.servers()[1].name, "git");

        let default_cfg = McpConfigFile::default();
        assert!(
            default_cfg.mcp.is_none(),
            "McpConfigFile::default() has mcp == None",
        );
        assert!(default_cfg.servers().is_empty());
    }

    #[test]
    fn mcp_server_field_defaults() {
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

        // Minimal block: only the two required fields; everything else falls through to defaults.
        let minimal = r#"
[[mcp.server]]
name = "tiny"
command = "node"
"#;
        let cfg: McpConfigFile = toml::from_str(minimal).unwrap();
        let srv = &cfg.servers()[0];
        assert_eq!(srv.name, "tiny");
        assert_eq!(srv.command, "node");
        assert!(srv.args.is_empty(), "args defaults to the empty Vec");
        assert!(srv.enabled, "enabled defaults to true");
        assert!(srv.tool_prefix.is_none(), "tool_prefix defaults to None");
        assert!(srv.include.is_empty(), "include defaults to the empty Vec");
        assert!(srv.exclude.is_empty(), "exclude defaults to the empty Vec");
        assert!(srv.env.is_empty(), "env defaults to the empty BTreeMap");

        // name and command have no serde default, so a block missing either must fail to parse.
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
                "a block missing {label:?} must fail to parse",
            );
        }

        // env iterates in sorted key order (BTreeMap, not HashMap).
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
            "env iterates in sorted key order",
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

    #[test]
    fn solana_bridge_default_is_disabled_with_well_known_url_and_prefix() {
        // Off by default: an enabled default would open an outbound MCP subprocess on crate upgrade.
        let b = SolanaBridge::default();
        assert!(
            !b.enabled,
            "SolanaBridge::default() is disabled — nothing connects unless the operator opts in",
        );
        assert_eq!(b.url, SOLANA_MCP_URL);
        assert_eq!(b.tool_prefix, "solana");
        assert!(b.include.is_empty());
        assert!(b.exclude.is_empty());
        assert!(
            b.to_server().is_none(),
            "a disabled bridge must materialize no server",
        );
    }

    #[test]
    fn solana_bridge_parses_operator_overrides() {
        let cfg: McpConfigFile = toml::from_str(
            r#"
[mcp.solana]
enabled = true
url = "https://mcp.example.test/mcp"
tool_prefix = "sol"
include = ["getBalance"]
exclude = ["airdrop"]
"#,
        )
        .unwrap();
        let b = cfg
            .mcp
            .as_ref()
            .and_then(|m| m.solana.as_ref())
            .expect("[mcp.solana] must surface a SolanaBridge");
        assert!(b.enabled);
        assert_eq!(b.url, "https://mcp.example.test/mcp");
        assert_eq!(b.tool_prefix, "sol");
        assert_eq!(b.include, vec!["getBalance".to_string()]);
        assert_eq!(b.exclude, vec!["airdrop".to_string()]);

        // Omitted fields fall back to the container-level serde default —
        // an operator who writes only `enabled = true` still gets the
        // well-known endpoint and prefix.
        let minimal: McpConfigFile = toml::from_str(
            r#"
[mcp.solana]
enabled = true
"#,
        )
        .unwrap();
        let b = minimal.mcp.unwrap().solana.unwrap();
        assert_eq!(b.url, SOLANA_MCP_URL);
        assert_eq!(b.tool_prefix, "solana");
    }

    #[test]
    fn materialize_hosted_bridges_appends_enabled_solana_stdio_server() {
        let mut cfg: McpConfigFile = toml::from_str(
            r#"
[[mcp.server]]
name = "fs"
command = "node"

[mcp.solana]
enabled = true
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.servers().len(),
            1,
            "before materialization only the configured server is present",
        );

        cfg.materialize_hosted_bridges();

        let names: Vec<&str> = cfg.servers().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["fs", "solana"],
            "the enabled bridge appends a 'solana' server to the effective list",
        );
        let solana = cfg
            .servers()
            .iter()
            .find(|s| s.name == "solana")
            .expect("materialized solana server");
        assert!(solana.enabled);
        assert_eq!(solana.command, "npx");
        assert_eq!(
            solana.args,
            vec![
                "-y".to_string(),
                "mcp-remote".to_string(),
                SOLANA_MCP_URL.to_string(),
            ],
            "the bridge must route through the documented mcp-remote stdio shim",
        );
        assert_eq!(solana.tool_prefix.as_deref(), Some("solana"));
        assert_eq!(
            solana.env.get("NO_DNA").map(String::as_str),
            Some("1"),
            "NO_DNA=1 mirrors the hosted server's non-human-CLI signal",
        );
    }

    #[test]
    fn materialize_hosted_bridges_noop_when_solana_absent_or_disabled() {
        // Absent section: nothing to add.
        let mut absent: McpConfigFile = toml::from_str(
            r#"
[[mcp.server]]
name = "fs"
command = "node"
"#,
        )
        .unwrap();
        absent.materialize_hosted_bridges();
        assert_eq!(absent.servers().len(), 1);

        // Present but disabled (the off-by-default state, written
        // explicitly): the section parses but must materialize nothing.
        let mut disabled: McpConfigFile = toml::from_str(
            r#"
[mcp.solana]
url = "https://mcp.solana.com/mcp"
"#,
        )
        .unwrap();
        disabled.materialize_hosted_bridges();
        assert!(
            disabled.servers().is_empty(),
            "a disabled [mcp.solana] block must not spawn a server",
        );
    }

    #[test]
    fn config_error_display_and_source() {
        let io_err = ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "mcp.toml missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "ConfigError::Io carries the 'io: ' prefix: {io_message}"
        );
        assert!(
            io_message.contains("mcp.toml missing"),
            "ConfigError::Io surfaces the inner io::Error Display, not Debug: {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "ConfigError::Io must not surface the io::Error Debug rendering: {io_message}"
        );

        let toml_source =
            toml::from_str::<toml::Value>("not valid toml = =").expect_err("toml parse must fail");
        let toml_err = ConfigError::Toml(toml_source);
        let toml_message = format!("{toml_err}");
        assert!(
            toml_message.starts_with("toml: "),
            "ConfigError::Toml carries the 'toml: ' prefix: {toml_message}"
        );
        assert!(
            !toml_message.starts_with("toml parse:"),
            "ConfigError::Toml uses 'toml: ', not the manifest crate's 'toml parse: ': {toml_message}"
        );
        assert!(
            toml_message.contains("TOML parse error"),
            "ConfigError::Toml surfaces the inner toml::de::Error Display, not Debug: {toml_message}"
        );
        assert!(
            !toml_message.contains("TomlError {"),
            "ConfigError::Toml must not surface the toml::de::Error Debug rendering: {toml_message}"
        );

        assert_ne!(
            io_message, toml_message,
            "Io and Toml Display must not converge: io={io_message} toml={toml_message}"
        );
        assert!(
            !io_message.starts_with("toml:") && !toml_message.starts_with("io:"),
            "the two prefixes must not be swapped: io={io_message} toml={toml_message}"
        );
    }

    #[test]
    fn config_error_io_source() {
        use std::error::Error;

        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "mcp.toml denied");
        let expected_display = format!("{inner}");
        let err = ConfigError::Io(inner);
        let source = err
            .source()
            .expect("ConfigError::Io exposes the inner io::Error via source()");
        assert_eq!(
            format!("{source}"),
            expected_display,
            "source() Display matches the inner io::Error verbatim"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "source() downcasts to io::Error so callers can read ErrorKind"
        );
    }

    #[test]
    fn config_error_toml_source() {
        use std::error::Error;

        let inner =
            toml::from_str::<toml::Value>("not valid toml = =").expect_err("toml parse must fail");
        let expected_display = format!("{inner}");
        let err = ConfigError::Toml(inner);
        let source = err
            .source()
            .expect("ConfigError::Toml exposes the inner toml::de::Error via source()");
        assert_eq!(
            format!("{source}"),
            expected_display,
            "source() Display matches the inner toml::de::Error verbatim"
        );
        assert!(
            source.downcast_ref::<toml::de::Error>().is_some(),
            "source() downcasts to toml::de::Error so callers can read span/message"
        );
    }
}
