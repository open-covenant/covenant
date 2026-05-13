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
    fn mcp_section_serde_pins_default_server_array() {
        // McpSection is the [mcp] block in secrets.toml, sitting
        // between McpConfigFile.mcp (Option<McpSection>) and the
        // [[mcp.server]] arrays. The struct carries a single field —
        // server: Vec<McpServer> — with field-level #[serde(default)]
        // and a derived Default impl, so a [mcp] block that opens the
        // section but writes no [[mcp.server]] entries decodes to an
        // empty server Vec rather than failing parse.
        //
        // A refactor that renamed McpSection::server (or applied
        // #[serde(rename)]) silently lets every operator's
        // secrets.toml decode into a section with an empty server Vec
        // — McpConfigFile::servers() returns an empty slice with no
        // parse signal, and configured MCP tool servers disappear on
        // next daemon restart. A refactor that dropped the field-level
        // #[serde(default)] silently turns every [mcp] header with no
        // [[mcp.server]] entries into a parse error.
        let empty_section = r#"
[mcp]
"#;
        let cfg: McpConfigFile = toml::from_str(empty_section).unwrap();
        let section = cfg.mcp.as_ref().expect(
            "[mcp] header must surface McpSection even when no [[mcp.server]] entries follow",
        );
        assert!(
            section.server.is_empty(),
            "McpSection::server default must be the empty Vec — a \
             refactor that dropped the field-level #[serde(default)] \
             would silently turn every [mcp] header with no \
             [[mcp.server]] entries into a parse error",
        );

        // [[mcp.server]] arrays without an explicit [mcp] header — TOML
        // treats this as the implicit-section pattern and still
        // populates mcp.server. Pin so a refactor that required an
        // explicit [mcp] header would surface here.
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

        // McpSection::default() must produce an empty server Vec — pin
        // this so a refactor that diverged the derived Default impl
        // from the empty-section parse path would fail loud.
        let default_section = McpSection::default();
        assert!(
            default_section.server.is_empty(),
            "McpSection::default() must have server == empty Vec",
        );

        // Direct toml::from_str<McpSection> of an empty body — pins
        // the field-level #[serde(default)] independently of the
        // outer wrapper.
        let bare: McpSection = toml::from_str("").unwrap();
        assert!(
            bare.server.is_empty(),
            "McpSection deserialized from an empty TOML body must \
             produce an empty server Vec via field-level \
             #[serde(default)]; a refactor that dropped the default \
             would turn this into a parse error and break every test \
             that constructs McpSection from a {{}} body",
        );
    }

    #[test]
    fn mcp_config_file_serde_pins_optional_mcp_section_default() {
        // McpConfigFile is the top-level secrets.toml decoder for the
        // [mcp] section. The struct carries a single field — mcp:
        // Option<McpSection> — with field-level #[serde(default)] and a
        // derived Default impl. McpConfigFile::from_path returns
        // Self::default() when the file is missing, and
        // McpConfigFile::servers() treats mcp.is_none() as the empty-
        // slice fall-through.
        //
        // Mirror of the wrapper-pin contract pinned for ProviderConfig
        // (covenant-llm), EmbedderConfig (covenant-llm), and
        // SearchConfig (covenant-tools). No test pins the wrapper's
        // field name, the None default on an empty TOML payload, or
        // the parse contract when [mcp] is omitted but other root
        // sections exist. A refactor that renamed McpConfigFile::mcp
        // silently makes every operator's secrets.toml decode into
        // McpConfigFile::default(), servers() returns an empty slice
        // with no parse signal, and configured MCP tool servers
        // silently disappear on next daemon restart — agents lose
        // every registered external tool.
        let empty: McpConfigFile = toml::from_str("").unwrap();
        assert!(
            empty.mcp.is_none(),
            "McpConfigFile must decode an empty TOML payload as \
             McpConfigFile::default() with mcp == None — \
             McpConfigFile::from_path relies on this path for missing \
             secrets.toml, and McpConfigFile::servers() relies on \
             mcp.is_none() to return the empty slice",
        );
        assert!(
            empty.servers().is_empty(),
            "McpConfigFile::servers() on the empty-TOML decode must \
             return the empty slice",
        );

        // No [mcp] section but other unknown root keys present —
        // operators co-locate [llm]/[embed]/[search] blocks in the same
        // secrets.toml. The wrapper does not carry
        // #[serde(deny_unknown_fields)] and unrelated sections must
        // parse without rejecting the file.
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
            "McpConfigFile must tolerate unknown root sections without \
             refusing to parse — operators co-locate [llm]/[embed]/\
             [search] blocks in the same secrets.toml and a refactor \
             that added #[serde(deny_unknown_fields)] would break \
             every co-resident secrets.toml at daemon start",
        );
        assert!(cfg.servers().is_empty());

        // [[mcp.server]] populated — wrapper round-trips into the
        // McpSection. Pin Some(McpSection) presence and a populated
        // server list so a refactor that broke the wrapper's
        // deserialization path would fail this assertion rather than
        // landing on a misleading inner-field error.
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
            "McpConfigFile::mcp must surface as Some(McpSection) when \
             [[mcp.server]] entries exist",
        );
        assert_eq!(cfg.servers().len(), 2);
        assert_eq!(cfg.servers()[0].name, "filesystem");
        assert_eq!(cfg.servers()[1].name, "git");

        // McpConfigFile::default() must match the empty-TOML decode —
        // pin this so a refactor that diverged the derived Default
        // impl from the empty-TOML path would fail loud.
        let default_cfg = McpConfigFile::default();
        assert!(
            default_cfg.mcp.is_none(),
            "McpConfigFile::default() must have mcp == None to match \
             the empty-TOML decode path that McpConfigFile::from_path \
             relies on for missing secrets.toml",
        );
        assert!(default_cfg.servers().is_empty());
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

    #[test]
    fn config_error_io_and_toml_display_messages_pin_prefix_and_external_source_display_delegation()
    {
        let io_err = ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "mcp.toml missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "ConfigError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish mcp.toml disk faults from TOML syntax faults during MCP config load (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("mcp.toml missing"),
            "ConfigError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render 'Custom {{ kind: NotFound, error: ... }}' instead of the message payload (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "ConfigError::Io must NOT surface the std::io::Error Debug rendering; a Debug refactor on {{0}} would expose internal struct fields like 'Custom {{ kind: ..., error: ... }}' or 'Os {{ code: ..., kind: ..., message: ... }}' (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let toml_source = toml::from_str::<toml::Value>("not valid toml = =")
            .expect_err("toml parse must fail");
        let toml_err = ConfigError::Toml(toml_source);
        let toml_message = format!("{toml_err}");
        assert!(
            toml_message.starts_with("toml: "),
            "ConfigError::Toml must surface the literal 'toml: ' bootstrap-stage prefix so audit-log filters can distinguish mcp.toml syntax faults from disk faults during MCP config load (dropped-prefix regression class): {toml_message}"
        );
        assert!(
            !toml_message.starts_with("toml parse:"),
            "ConfigError::Toml must surface the 'toml: ' prefix (not 'toml parse: '); covenant_manifest::ManifestError::Parse intentionally uses 'toml parse: ' as a cross-crate discriminator between manifest-parse failures and MCP-config-parse failures. A 'harmonize TOML error prefixes' pass that aligned the two would silently break audit-log filters that grep exactly 'toml: ' for MCP config faults and exactly 'toml parse: ' for manifest faults (cross-crate-prefix-alignment regression class): {toml_message}"
        );
        assert!(
            toml_message.contains("TOML parse error"),
            "ConfigError::Toml must surface the inner toml::de::Error Display rendering after the colon ({{0}}, not {{0:?}}); toml v0.8 Display renders parse failures starting with 'TOML parse error at line N, column M', a Debug refactor on {{0}} would render 'TomlError {{ message: ..., raw: ..., keys: ..., span: ... }}' instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {toml_message}"
        );
        assert!(
            !toml_message.contains("TomlError {"),
            "ConfigError::Toml must NOT surface the toml::de::Error Debug rendering; a Debug refactor on {{0}} would expose 'TomlError {{ message: ..., raw: ..., keys: ..., span: ... }}' internal struct fields (Debug-vs-Display formatting regression class on the {{0}} interpolation): {toml_message}"
        );

        assert_ne!(
            io_message, toml_message,
            "ConfigError::Io and ConfigError::Toml Display must not converge; merging the two prefixes would lose the disk-fault vs TOML-syntax-fault discriminator (prefix-convergence regression class): io={io_message} toml={toml_message}"
        );
        assert!(
            !io_message.starts_with("toml:") && !toml_message.starts_with("io:"),
            "ConfigError::Io must not start with 'toml:' and ConfigError::Toml must not start with 'io:'; a sibling-prefix swap would silently mis-route incident triage (sibling-prefix-swap regression class): io={io_message} toml={toml_message}"
        );
    }

    #[test]
    fn config_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "mcp.toml denied");
        let expected_display = format!("{inner}");
        let err = ConfigError::Io(inner);
        let source = err.source().expect(
            "covenant_mcp::config::ConfigError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side MCP config-load classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct decisions on mcp.toml disk faults (NotFound proceeds with default config because mcp.toml is optional, PermissionDenied escalates as operator-attention on a config file present but unreadable, other ErrorKinds surface with the underlying OS context); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_mcp::config::ConfigError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "covenant_mcp::config::ConfigError::Io source() must downcast_ref to std::io::Error so daemon-side MCP config-load classifiers can extract io::ErrorKind for distinct decisions on mcp.toml disk faults; a refactor that wrapped the inner in a project-local newtype (e.g., McpConfigIoError(std::io::Error) under a 'tag MCP config IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies mcp.toml-load IO faults (concrete-source-type downcast regression class)"
        );
    }
}
