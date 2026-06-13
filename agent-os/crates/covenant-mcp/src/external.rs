//! Bridge from a remote MCP server to the local [`Tool`] trait.
//!
//! After spawning a JSON-RPC client, [`bootstrap_remote_tools`] performs the
//! MCP `initialize` handshake, fetches the server's tool list via
//! `tools/list`, and wraps each spec in a [`RemoteTool`] that forwards
//! `call(...)` over the same transport. The result drops cleanly into the
//! local [`crate::ToolRegistry`].

use crate::transport::{McpClient, McpClientError};
use crate::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

/// MCP protocol version we advertise during `initialize`. The server may
/// downgrade; we don't enforce a specific version yet (Phase 4+).
pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("transport: {0}")]
    Transport(#[from] McpClientError),
    #[error("malformed initialize response: {0}")]
    BadInitialize(String),
    #[error("malformed tools/list response: {0}")]
    BadList(String),
    #[error("duplicate remote tool name after MCP prefixing: {0}")]
    DuplicateToolName(String),
}

#[derive(Debug, Clone, Default)]
pub struct RemoteToolOptions {
    pub tool_prefix: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

fn initialize_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Run `initialize` → `notifications/initialized` → `tools/list` against
/// `client` and return one [`Tool`] per advertised spec, all sharing the
/// same client.
pub async fn bootstrap_remote_tools(
    client: Arc<dyn McpClient>,
) -> Result<Vec<Arc<dyn Tool>>, BootstrapError> {
    bootstrap_remote_tools_with_options(client, RemoteToolOptions::default()).await
}

/// Like [`bootstrap_remote_tools`], but applies Covenant-side naming hygiene
/// and include/exclude filters before the remote specs enter the registry.
pub async fn bootstrap_remote_tools_with_options(
    client: Arc<dyn McpClient>,
    options: RemoteToolOptions,
) -> Result<Vec<Arc<dyn Tool>>, BootstrapError> {
    let init_params = serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "covenant", "version": env!("CARGO_PKG_VERSION") }
    });
    let init_response = client.request("initialize", init_params).await?;
    // The MCP `initialize` result MUST be a JSON object per the spec.
    // A non-object (null, string, array, number) is a misconfigured or
    // incompatible server; refusing here keeps the broken session from
    // poisoning the downstream tools/list call with a useless error.
    let init_obj = init_response.as_object().ok_or_else(|| {
        BootstrapError::BadInitialize(format!(
            "expected JSON object, got {}",
            initialize_kind(&init_response)
        ))
    })?;
    // protocolVersion mismatch is non-fatal in Phase 0 — the spec
    // explicitly allows the server to negotiate a different version —
    // but operators triaging a broken connection deserve a warn so the
    // log shows what the server actually offered.
    if let Some(advertised) = init_obj.get("protocolVersion").and_then(|v| v.as_str()) {
        if advertised != PROTOCOL_VERSION {
            tracing::warn!(
                client_protocol = PROTOCOL_VERSION,
                server_protocol = advertised,
                "mcp: initialize protocolVersion mismatch"
            );
        }
    }
    client
        .notify("notifications/initialized", Value::Null)
        .await?;

    let list = client.request("tools/list", Value::Null).await?;
    let parsed: ToolsListResponse =
        serde_json::from_value(list).map_err(|e| BootstrapError::BadList(format!("{e}")))?;

    let mut out: Vec<Arc<dyn Tool>> = Vec::with_capacity(parsed.tools.len());
    let mut seen = HashSet::new();
    for spec in parsed.tools {
        if !options.allows(&spec.name) {
            continue;
        }
        let upstream_name = spec.name.clone();
        let advertised_name = options.advertised_name(&upstream_name);
        if !seen.insert(advertised_name.clone()) {
            return Err(BootstrapError::DuplicateToolName(advertised_name));
        }
        out.push(Arc::new(RemoteTool {
            client: client.clone(),
            upstream_name,
            spec: ToolSpec {
                name: advertised_name,
                ..spec
            },
        }));
    }
    Ok(out)
}

impl RemoteToolOptions {
    fn allows(&self, name: &str) -> bool {
        if !self.include.is_empty()
            && !self
                .include
                .iter()
                .any(|pattern| matches_filter(pattern, name))
        {
            return false;
        }
        !self
            .exclude
            .iter()
            .any(|pattern| matches_filter(pattern, name))
    }

    fn advertised_name(&self, upstream_name: &str) -> String {
        let Some(prefix) = self
            .tool_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
        else {
            return upstream_name.to_string();
        };
        format!("mcp_{}_{}", sanitize_prefix(prefix), upstream_name)
    }
}

fn matches_filter(pattern: &str, name: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" || pattern == name {
        return true;
    }
    pattern
        .strip_suffix('*')
        .is_some_and(|prefix| name.starts_with(prefix))
}

fn sanitize_prefix(prefix: &str) -> String {
    let sanitized: String = prefix
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "remote".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Deserialize)]
struct ToolsListResponse {
    tools: Vec<ToolSpec>,
}

pub struct RemoteTool {
    client: Arc<dyn McpClient>,
    upstream_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> &str {
        &self.spec.description
    }
    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let params = serde_json::json!({ "name": self.upstream_name, "arguments": arguments });
        match self.client.request("tools/call", params).await {
            Ok(v) => parse_tool_call_result(v),
            Err(McpClientError::Rpc { code, message }) => {
                Err(ToolError::Failed(format!("rpc {code}: {message}")))
            }
            Err(e) => Err(ToolError::Failed(format!("transport: {e}"))),
        }
    }
}

fn parse_tool_call_result(v: Value) -> Result<ToolCallResult, ToolError> {
    #[derive(Deserialize)]
    struct Wire {
        #[serde(default)]
        content: Vec<Content>,
        #[serde(default, alias = "isError")]
        is_error: bool,
    }
    let w: Wire = serde_json::from_value(v)
        .map_err(|e| ToolError::Failed(format!("bad tools/call result: {e}")))?;
    Ok(ToolCallResult {
        content: w.content,
        is_error: w.is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockMcpClient;

    #[test]
    fn protocol_version_pins_exact_mcp_spec_date_string() {
        // covenant_mcp::external::PROTOCOL_VERSION is the public const
        // every external MCP server initialize handshake quotes via
        // 'protocolVersion' in the initialize request, the initialize
        // response, and the initialized notification. External MCP
        // servers parse the field as a
        // date string and compare it against their supported versions
        // to decide whether to accept the handshake.
        //
        // No direct test today; the value is exercised indirectly via
        // stdio transport handshake tests that capture the field. A
        // refactor that bumped this to a newer date without
        // coordinating the request/response shape changes that newer
        // MCP versions require would silently fail every external
        // server's handshake; a refactor that dropped the const in
        // favor of inline literals at the three call sites would split
        // the source of truth and let drift creep in.
        assert_eq!(
            PROTOCOL_VERSION, "2024-11-05",
            "PROTOCOL_VERSION must remain '2024-11-05' — the MCP \
             spec revision the daemon's external transport implements. \
             A refactor that bumped this without coordinating the wire \
             shape changes would silently break every operator's MCP \
             integration on the next initialize handshake",
        );

        assert_eq!(
            PROTOCOL_VERSION.len(),
            10,
            "PROTOCOL_VERSION must be exactly 10 chars (YYYY-MM-DD) — \
             a refactor that switched to semver ('1.0.0' is 5 chars; \
             'v2024.11.05' is 11 chars) would silently break the \
             documented MCP spec contract that external servers parse \
             this field as a date string and reject anything that does \
             not match the date-string shape",
        );

        let parts: Vec<&str> = PROTOCOL_VERSION.split('-').collect();
        assert_eq!(
            parts.len(),
            3,
            "PROTOCOL_VERSION must contain exactly two '-' separators \
             splitting it into [year, month, day] — a refactor that \
             changed the separator (e.g., to '/' or '.') or dropped a \
             component (e.g., to 'YYYY-MM') would silently break MCP \
             servers that parse the field as date components",
        );
        for (label, part, expected_len) in [
            ("year", parts[0], 4),
            ("month", parts[1], 2),
            ("day", parts[2], 2),
        ] {
            assert_eq!(
                part.len(),
                expected_len,
                "PROTOCOL_VERSION {label} component must be {expected_len} \
                 chars — pinning the YYYY-MM-DD structure (4-2-2) so a \
                 refactor that emitted a 1-digit month or 3-digit year \
                 surfaces here; got {part:?} ({} chars)",
                part.len(),
            );
            assert!(
                part.chars().all(|c| c.is_ascii_digit()),
                "PROTOCOL_VERSION {label} component must be all ASCII \
                 digits — pinning that the date string carries no \
                 letters (e.g., 'v', 'rc1') or punctuation; a refactor \
                 that prefixed with 'v' or suffixed with a milestone \
                 tag would silently break the date-string contract; \
                 got {part:?}",
            );
        }
    }

    #[test]
    fn matches_filter_pins_star_exact_and_prefix_glob_branches() {
        assert!(
            matches_filter("*", "anything"),
            "'*' must pass every tool name; dropping this arm silently blocks every tool when the operator wires '*' in allowed_tools",
        );
        assert!(
            matches_filter("*", ""),
            "'*' must even pass the empty name so the all-pass arm is unconditional",
        );

        assert!(
            matches_filter("foo", "foo"),
            "an exact literal must match itself",
        );
        assert!(
            !matches_filter("foo", "bar"),
            "an exact literal must not match a different name",
        );
        assert!(
            !matches_filter("foo", "foobar"),
            "an exact literal must NOT match a longer name; otherwise the literal silently widens to a prefix",
        );

        assert!(
            matches_filter("foo*", "foo"),
            "the trailing-* glob must match the bare prefix; pattern='foo*' is documented to allow 'foo'",
        );
        assert!(
            matches_filter("foo*", "foobar"),
            "the trailing-* glob must match anything that starts with the prefix",
        );
        assert!(
            !matches_filter("foo*", "fo"),
            "the trailing-* glob must NOT match a name that is shorter than the prefix",
        );
        assert!(
            !matches_filter("foo*", "barfoo"),
            "the trailing-* glob is starts_with, not contains; embedded matches must reject",
        );

        assert!(
            matches_filter("  *  ", "x"),
            "leading/trailing whitespace on '*' must be trimmed before matching",
        );
        assert!(
            matches_filter("  foo  ", "foo"),
            "leading/trailing whitespace on an exact pattern must be trimmed before matching",
        );
        assert!(
            matches_filter(" foo* ", "foobar"),
            "leading/trailing whitespace on a glob pattern must be trimmed before matching",
        );
    }

    #[test]
    fn sanitize_prefix_maps_non_alnum_to_underscore_trims_edges_and_falls_back_to_remote() {
        assert_eq!(
            sanitize_prefix("myserver"),
            "myserver",
            "alphanumeric prefixes must pass through unchanged; otherwise operator-supplied names get rewritten in ways the tool-allowlist can't anticipate",
        );
        assert_eq!(
            sanitize_prefix("123"),
            "123",
            "digit-only prefixes must pass through unchanged so a numeric server label remains a valid tool-name fragment",
        );
        assert_eq!(
            sanitize_prefix("my-server"),
            "my_server",
            "documented character map: dash becomes underscore so the result stays inside the tool-name allowed alphabet",
        );
        assert_eq!(
            sanitize_prefix("my.server"),
            "my_server",
            "documented character map: dot becomes underscore so dotted hostnames don't collide with the mcp_<prefix>_<name> separator convention",
        );
        assert_eq!(
            sanitize_prefix("  my-server  "),
            "my_server",
            "surrounding whitespace must be mapped to underscore then trimmed; otherwise a stray space in operator config leaks into the wire tool name",
        );
        assert_eq!(
            sanitize_prefix(""),
            "remote",
            "an empty prefix must fall back to the literal 'remote' so the generated tool name mcp_remote_<name> never collapses to mcp__<name> and silently merges two unconfigured servers",
        );
        assert_eq!(
            sanitize_prefix("..."),
            "remote",
            "an all-punctuation prefix becomes all-underscore then trims to empty; the fallback must still kick in or the tool-name shape collapses",
        );
        assert_eq!(
            sanitize_prefix("   "),
            "remote",
            "an all-whitespace prefix is equivalent to empty after mapping and trimming; the fallback must apply consistently with the empty-input case",
        );
    }

    #[test]
    fn remote_tool_options_allows_pins_include_then_exclude_precedence_and_default_arms() {
        // RemoteToolOptions::allows is the per-tool admission gate
        // that bootstrap_remote_tools_with_options consults to decide
        // whether each upstream MCP
        // tools/list spec enters the operator's covenantd registry.
        // The function encodes a documented include-then-exclude
        // precedence: if include is non-empty the name must match at
        // least one include pattern via matches_filter — otherwise
        // default-deny on the include side; regardless of include,
        // any matching exclude pattern denies.
        //
        // matches_filter_pins_star_exact_and_prefix_glob_branches
        // (above) pins matches_filter itself. This pin closes the
        // composition: how allows() chains include and exclude, the
        // default-allow when both lists are empty, the include-as-
        // allowlist when include is non-empty, and the exclude-wins
        // arm when both match.
        let default = RemoteToolOptions::default();
        assert!(
            default.allows("anything"),
            "default options (include=[], exclude=[]) must allow every \
             tool name — a refactor that defaulted to deny under a \
             'least-privilege default' rationale would silently drop \
             EVERY upstream MCP tool from EVERY registry on first \
             deploy with no operator-visible signal",
        );

        let include_only = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["exact".into()],
            exclude: vec![],
        };
        assert!(
            include_only.allows("exact"),
            "a tool whose name matches a literal include pattern must \
             be allowed when include is the only filter",
        );
        assert!(
            !include_only.allows("other"),
            "a tool whose name does NOT match any include pattern must \
             be denied when include is non-empty — the include list is \
             an allowlist gate, not an additive list. A refactor that \
             treated non-empty include as informational would silently \
             let unlisted tools through",
        );

        let exclude_only = RemoteToolOptions {
            tool_prefix: None,
            include: vec![],
            exclude: vec!["banned".into()],
        };
        assert!(
            !exclude_only.allows("banned"),
            "exclude must apply even when include is empty — otherwise \
             a deny-only configuration (the simpler 'block these few' \
             setup most operators reach for first) would silently \
             allow every tool",
        );
        assert!(
            exclude_only.allows("other"),
            "a tool whose name does not match any exclude pattern must \
             be allowed when only exclude is set — the empty-include \
             arm must default to allow, otherwise this configuration \
             would deny everything despite having no allowlist",
        );

        let star_include_with_exclude = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["*".into()],
            exclude: vec!["banned".into()],
        };
        assert!(
            !star_include_with_exclude.allows("banned"),
            "exclude must win over include — even when include='*' \
             matches every name, a matching exclude pattern must \
             deny. A refactor that swapped the precedence so include \
             was checked LAST and short-circuited on a match would \
             silently let exclude entries leak through any catch-all \
             include",
        );
        assert!(
            star_include_with_exclude.allows("other"),
            "the catch-all include must still allow non-excluded names \
             — pinning that '*' is honored as a glob via matches_filter, \
             not silently re-treated as a literal '*' that would only \
             match the literal name '*'",
        );

        let glob_include = RemoteToolOptions {
            tool_prefix: None,
            include: vec!["foo*".into()],
            exclude: vec![],
        };
        assert!(
            glob_include.allows("foobar"),
            "an include pattern with a trailing '*' must use the \
             matches_filter glob path — a refactor that 'simplified' \
             allows by replacing matches_filter with Vec::contains \
             would silently drop this branch because Vec::contains \
             does literal equality and 'foobar' is not equal to \
             'foo*'",
        );
        assert!(
            !glob_include.allows("bar"),
            "an include pattern with a trailing '*' must NOT match \
             unrelated names — pins that the glob is starts_with-only, \
             not contains-anywhere, so an unrelated tool that happens \
             to share a substring with the include prefix is still \
             denied",
        );
    }

    #[test]
    fn remote_tool_options_advertised_name_pins_none_empty_trimmed_and_format_arms() {
        // RemoteToolOptions::advertised_name decides the
        // registry-facing name for each remote MCP tool. The
        // function maps tool_prefix Option through three documented
        // arms: None -> verbatim upstream_name; Some(s) where
        // s.trim().is_empty() -> verbatim upstream_name; Some(non-
        // empty after trim) -> "mcp_<sanitize_prefix(trimmed)>_
        // <upstream_name>".
        //
        // sanitize_prefix_maps_non_alnum_to_underscore_trims_edges_
        // and_falls_back_to_remote (above) pins sanitize_prefix
        // itself. This pin closes the composition: the trim-and-
        // fallback short-circuit, the literal "mcp_" prefix on the
        // wrap branch, and the sanitize_prefix wiring on the prefix
        // (but NOT on the upstream name — upstream passes verbatim).
        let none = RemoteToolOptions::default();
        assert_eq!(
            none.advertised_name("foo"),
            "foo",
            "None tool_prefix must return upstream_name verbatim — a \
             refactor that always wrapped (e.g., \
             'format!(\"mcp_{{}}_{{}}\", \
             sanitize_prefix(prefix.unwrap_or_default()), \
             upstream_name)' under a 'tool names should always be \
             qualified' rationale) would silently rename every tool \
             from 'foo' to 'mcp_remote_foo' on operators with no \
             prefix configured (the empty string passed to \
             sanitize_prefix falls back to the literal 'remote'), \
             breaking every existing tools/call wire-name reference",
        );

        let empty = RemoteToolOptions {
            tool_prefix: Some(String::new()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            empty.advertised_name("foo"),
            "foo",
            "Some('') tool_prefix must return upstream_name verbatim \
             — empty after trim must short-circuit to the same path \
             as None, otherwise an operator who set tool_prefix='' \
             expecting it to be a no-op would see every tool renamed \
             to 'mcp_remote_<name>'",
        );

        let whitespace = RemoteToolOptions {
            tool_prefix: Some("   ".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            whitespace.advertised_name("foo"),
            "foo",
            "Some('   ') tool_prefix must return upstream_name \
             verbatim — whitespace-only after trim must short-circuit \
             to the same path as None and empty, pinning that the \
             trim-then-is_empty filter applies before the wrap branch",
        );

        let plain = RemoteToolOptions {
            tool_prefix: Some("myserver".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            plain.advertised_name("foo"),
            "mcp_myserver_foo",
            "Some('myserver') tool_prefix must wrap as \
             'mcp_<prefix>_<upstream>' — pins the literal 'mcp_' \
             prefix on the wrap branch. A refactor that swapped the \
             format string from 'mcp_{{}}_{{}}' to '{{}}_{{}}' under \
             a 'drop the redundant mcp_ prefix' rationale would \
             silently break tool-namespace collision handling, since \
             two MCP servers with the same prefix 'fs' would both \
             produce 'fs_read' instead of distinct 'mcp_fs_read' \
             names",
        );

        let dashed = RemoteToolOptions {
            tool_prefix: Some("my-server".into()),
            include: vec![],
            exclude: vec![],
        };
        assert_eq!(
            dashed.advertised_name("foo"),
            "mcp_my_server_foo",
            "Some('my-server') tool_prefix must wrap with the dash \
             mapped to underscore via sanitize_prefix — pins that \
             the sanitize_prefix composition is wired through \
             advertised_name. A refactor that bypassed sanitize_prefix \
             and called the prefix verbatim under a 'sanitize_prefix \
             is overcautious' rationale would produce \
             'mcp_my-server_foo' and surface here",
        );
    }

    #[test]
    fn parse_tool_call_result_pins_default_content_default_is_error_camel_case_alias_and_error_wrap(
    ) {
        let r = parse_tool_call_result(serde_json::json!({})).expect(
            "an empty object must decode to an Ok ToolCallResult so MCP servers that omit content do not error out",
        );
        assert!(
            r.content.is_empty(),
            "missing content field must default to an empty Vec<Content>; otherwise a spec-compliant server that returns only is_error is silently rejected",
        );
        assert!(
            !r.is_error,
            "missing is_error field must default to false; otherwise every spec-compliant tools/call success would be treated as an error",
        );

        let r = parse_tool_call_result(serde_json::json!({ "isError": true }))
            .expect("the camelCase isError wire field must decode without error");
        assert!(
            r.is_error,
            "the isError serde alias must map onto is_error; otherwise every MCP-spec error result silently looks like a success because the wire form is camelCase",
        );

        let r = parse_tool_call_result(serde_json::json!({
            "content": [ { "type": "text", "text": "hello" } ],
            "isError": false
        }))
        .expect("a payload with text content must decode");
        assert_eq!(
            r.content,
            vec![Content::text("hello")],
            "text content must round-trip through parse_tool_call_result; otherwise tool output is silently dropped or mistyped",
        );
        assert!(
            !r.is_error,
            "isError=false must keep is_error=false; the alias must not invert the value",
        );

        let err = parse_tool_call_result(serde_json::json!("not an object"))
            .expect_err("a non-object payload must not decode as a ToolCallResult");
        match err {
            ToolError::Failed(msg) => assert!(
                msg.starts_with("bad tools/call result: "),
                "the error message must begin with the documented prefix so operators can grep for upstream wire failures; got: {msg}",
            ),
            other => panic!(
                "expected ToolError::Failed for a malformed wire payload, got: {other:?}"
            ),
        }
    }

    #[test]
    fn parse_tool_call_result_pins_multi_block_content_and_snake_case_is_error() {
        // parse_tool_call_result_pins_default_content_default_is_error_camel_case_alias_and_error_wrap
        // covers four cases (empty object, isError camelCase alias,
        // single-text-block content, non-object error wrap), but two
        // forward-compat surfaces of the Wire struct's deserialization
        // are not pinned:
        //
        // 1. Multi-block content with mixed Content variants. Real MCP
        //    tools routinely return both human-readable text and a
        //    structured JSON payload in the same tools/call response;
        //    Vec<Content> must compose through serde with both
        //    variants in one payload preserving order. A custom
        //    Vec<Content> deserializer that only handled single-block
        //    payloads would silently drop the trailing blocks and
        //    Covenant's RemoteTool wrapper would surface an
        //    incomplete result.
        // 2. The canonical snake_case is_error decoding path. The
        //    Wire struct's #[serde(default, alias = "isError")] makes
        //    both names accept on the wire — the existing test only
        //    asserts the camelCase alias path. A refactor that
        //    hardened the field to camelCase-only by removing the
        //    canonical name (or by reversing the alias direction in
        //    a way that drops the snake_case decode path) would
        //    silently break MCP proxies and future server
        //    implementations that emit the Rust-native shape.
        let multi = parse_tool_call_result(serde_json::json!({
            "content": [
                { "type": "text", "text": "first block" },
                { "type": "json", "value": { "k": "v" } }
            ],
            "isError": false
        }))
        .expect("a multi-block payload with mixed Text+Json content variants must decode");
        assert_eq!(
            multi.content,
            vec![
                Content::text("first block"),
                Content::json(serde_json::json!({ "k": "v" })),
            ],
            "Vec<Content> must preserve order and variant identity through \
             parse_tool_call_result; a custom deserializer that only handled \
             single-block payloads would silently drop the trailing block \
             and Covenant's RemoteTool wrapper would surface an incomplete \
             result for every multi-block tools/call response",
        );
        assert!(
            !multi.is_error,
            "multi-block ok results must keep is_error=false; the second-block \
             decode must not silently flip the error flag",
        );

        let snake = parse_tool_call_result(serde_json::json!({ "is_error": true })).expect(
            "the canonical snake_case is_error must decode without error; the \
             Wire struct's alias = \"isError\" attribute opens the camelCase \
             surface but the Rust-native snake_case name is the underlying \
             field and must remain a valid wire form for MCP proxies and \
             future spec versions emitting the canonical shape",
        );
        assert!(
            snake.is_error,
            "snake_case is_error=true must map onto is_error=true; a refactor \
             that hardened the field to camelCase-only (removing the canonical \
             name in favour of #[serde(rename = \"isError\")] without an alias \
             back to is_error) would silently treat every snake_case error \
             result as a success",
        );
    }

    fn happy_handler(method: &str, params: &Value) -> Result<Value, McpClientError> {
        match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            "tools/list" => Ok(serde_json::json!({
                "tools": [
                    {
                        "name": "fs.read",
                        "description": "read a file",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "fs.write",
                        "description": "write a file",
                        "inputSchema": { "type": "object" }
                    }
                ]
            })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": format!("called {name}") }],
                    "isError": false
                }))
            }
            other => Err(McpClientError::Rpc {
                code: -32601,
                message: format!("unknown method {other}"),
            }),
        }
    }

    #[tokio::test]
    async fn bootstrap_lists_remote_tools() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools(client.clone()).await.unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["fs.read".to_string(), "fs.write".to_string()]);
    }

    #[tokio::test]
    async fn bootstrap_fires_initialized_notification() {
        let mock = Arc::new(MockMcpClient::new(happy_handler));
        let _ = bootstrap_remote_tools(mock.clone() as Arc<dyn McpClient>)
            .await
            .unwrap();
        let n = mock.notifications();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "notifications/initialized");
    }

    #[tokio::test]
    async fn remote_tool_call_forwards_arguments() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let r = tools[0]
            .call(serde_json::json!({ "path": "/tmp" }))
            .await
            .unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "called fs.read"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn prefixed_remote_tool_forwards_original_name() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools_with_options(
            client,
            RemoteToolOptions {
                tool_prefix: Some("hermes.agent".into()),
                ..RemoteToolOptions::default()
            },
        )
        .await
        .unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(
            names,
            vec![
                "mcp_hermes_agent_fs.read".to_string(),
                "mcp_hermes_agent_fs.write".to_string()
            ]
        );

        let r = tools[0]
            .call(serde_json::json!({ "path": "/tmp" }))
            .await
            .unwrap();
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "called fs.read"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn filters_remote_tools_before_advertising() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(happy_handler));
        let tools = bootstrap_remote_tools_with_options(
            client,
            RemoteToolOptions {
                include: vec!["fs.*".into()],
                exclude: vec!["fs.write".into()],
                ..RemoteToolOptions::default()
            },
        )
        .await
        .unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["fs.read".to_string()]);
    }

    #[tokio::test]
    async fn duplicate_remote_names_fail_bootstrap() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            "tools/list" => Ok(serde_json::json!({
                "tools": [
                    {
                        "name": "dup",
                        "description": "first",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "dup",
                        "description": "second",
                        "inputSchema": { "type": "object" }
                    }
                ]
            })),
            other => Err(McpClientError::Rpc {
                code: -32601,
                message: format!("unknown method {other}"),
            }),
        }));

        match bootstrap_remote_tools(client).await {
            Err(BootstrapError::DuplicateToolName(name)) => assert_eq!(name, "dup"),
            Err(other) => panic!("unexpected: {other:?}"),
            Ok(_) => panic!("duplicate remote tool names should fail bootstrap"),
        }
    }

    #[tokio::test]
    async fn remote_tool_propagates_rpc_errors() {
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" | "tools/list" => happy_handler(method, &Value::Null),
            "tools/call" => Err(McpClientError::Rpc {
                code: -32602,
                message: "invalid params".into(),
            }),
            _ => unreachable!(),
        }));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let err = tools[0].call(Value::Null).await.unwrap_err();
        match err {
            ToolError::Failed(msg) => assert!(msg.contains("invalid params")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remote_tool_transport_faults_surface_as_transport_prefixed_failed_distinct_from_rpc() {
        // RemoteTool::call routes McpClientError::Rpc to a "rpc {code}: {message}"
        // message and folds every transport fault -- Io, Serde, Closed, Timeout,
        // ServerCrashed -- into "transport: {e}". remote_tool_propagates_rpc_errors
        // pins only the Rpc arm; the catch-all has no coverage. The transport layer
        // keeps Timeout and ServerCrashed as distinct, retry-policy-bearing variants
        // (transport.rs pins their Display and the crash-vs-closed routing so an
        // operator can decide reconnect-vs-abandon). Pin that those diagnostics
        // survive the collapse into the opaque ToolError::Failed AND stay
        // prefix-distinct from a server-side rpc rejection: an operator triaging a
        // failed tool call must tell "the server ran my tool and rejected it" (fix
        // the request) apart from "the call never completed" (restart or wait on a
        // slow/crashed server). A simplify pass that merged the two match arms, or
        // dropped the {e} interpolation for a static string, would converge the
        // prefixes or discard the retry signal and surface here.

        // Timeout: the transport's "timeout after {dur}" diagnostic must reach the
        // tool-call error surface so the operator sees the call never completed.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" | "tools/list" => happy_handler(method, &Value::Null),
            "tools/call" => Err(McpClientError::Timeout(std::time::Duration::from_secs(5))),
            _ => unreachable!(),
        }));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let err = tools[0].call(Value::Null).await.unwrap_err();
        match err {
            ToolError::Failed(msg) => {
                assert!(
                    msg.starts_with("transport: ") && !msg.starts_with("rpc "),
                    "a transport timeout must surface under the 'transport: ' prefix, \
                     distinct from the 'rpc ' prefix RemoteTool::call gives a server-side \
                     rejection; a refactor that merged the two arms would converge them: {msg}"
                );
                assert!(
                    msg.contains("timeout after"),
                    "the McpClientError::Timeout Display detail must survive the collapse \
                     into ToolError::Failed so the operator can tell a slow/unresponsive \
                     server from any other transport fault; a static 'transport error' \
                     string would drop this retry signal: {msg}"
                );
            }
            other => panic!("expected ToolError::Failed for a transport timeout, got: {other:?}"),
        }

        // ServerCrashed: the crash diagnostic must likewise reach the surface so the
        // operator restarts the server rather than retrying a doomed request.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" | "tools/list" => happy_handler(method, &Value::Null),
            "tools/call" => Err(McpClientError::ServerCrashed),
            _ => unreachable!(),
        }));
        let tools = bootstrap_remote_tools(client).await.unwrap();
        let err = tools[0].call(Value::Null).await.unwrap_err();
        match err {
            ToolError::Failed(msg) => {
                assert!(
                    msg.starts_with("transport: ") && !msg.starts_with("rpc "),
                    "a server crash must surface under the 'transport: ' prefix, not the \
                     'rpc ' prefix of a server-side tool rejection: {msg}"
                );
                assert!(
                    msg.contains("server crashed"),
                    "the McpClientError::ServerCrashed diagnostic must survive into \
                     ToolError::Failed so the operator restarts the server instead of \
                     retrying a doomed request: {msg}"
                );
            }
            other => panic!("expected ToolError::Failed for a server crash, got: {other:?}"),
        }
    }

    #[test]
    fn bootstrap_error_display_messages_pin_two_string_variant_format_strings() {
        let bad_list = format!("{}", BootstrapError::BadList("missing field tools".into()));
        assert_eq!(
            bad_list, "malformed tools/list response: missing field tools",
            "BootstrapError::BadList Display drifted (typo or dropped 'tools/list' protocol-stage marker regression class)"
        );

        let duplicate = format!("{}", BootstrapError::DuplicateToolName("fs.read".into()));
        assert_eq!(
            duplicate, "duplicate remote tool name after MCP prefixing: fs.read",
            "BootstrapError::DuplicateToolName Display drifted (typo or dropped 'after MCP prefixing' post-prefix-namespace qualifier regression class)"
        );

        assert_ne!(
            bad_list, duplicate,
            "BootstrapError::BadList must not converge with BootstrapError::DuplicateToolName \
             (prefix-convergence regression class would merge protocol-stage failures with namespace-collision failures)"
        );
    }

    #[test]
    fn bootstrap_error_transport_display_message_pins_prefix_and_inner_mcp_client_error_display_delegation(
    ) {
        let err = BootstrapError::Transport(McpClientError::Closed);
        let message = format!("{err}");
        assert_eq!(
            message, "transport: transport closed",
            "BootstrapError::Transport Display drifted (typo, dropped bootstrap-stage prefix, sibling-variant prefix convergence, or Debug-vs-Display formatting regression class on the {{0}} interpolation)"
        );
        assert!(
            message.starts_with("transport: "),
            "BootstrapError::Transport must surface the 'transport: ' bootstrap-stage prefix so audit-log filters can distinguish MCP-bootstrap-transport-failures from BootstrapError::BadList (protocol-layer) and BootstrapError::DuplicateToolName (namespace-collision) (dropped-bootstrap-stage-prefix regression class): {message}"
        );
        assert!(
            message.contains("transport closed"),
            "BootstrapError::Transport must surface the inner McpClientError via its Display impl ('transport closed' for Closed); a refactor to {{0:?}} would surface the Debug rendering ('Closed' as the bare variant name) instead (Debug-vs-Display formatting regression class on the {{0}} interpolation): {message}"
        );
        assert_ne!(
            message, "transport: Closed",
            "BootstrapError::Transport must NOT surface the bare variant name 'Closed' (the McpClientError Debug rendering); the {{0}} interpolation must use Display, not Debug (Debug-vs-Display formatting regression class on the {{0}} interpolation): {message}"
        );
        assert!(
            !message.starts_with("malformed tools/list response:"),
            "BootstrapError::Transport must not converge with BootstrapError::BadList prefix; the bootstrap-stage prefix discriminator must distinguish transport-layer failures from protocol-layer failures (sibling-variant prefix-convergence regression class): {message}"
        );
        assert!(
            !message.starts_with("duplicate remote tool name after MCP prefixing:"),
            "BootstrapError::Transport must not converge with BootstrapError::DuplicateToolName prefix; the bootstrap-stage prefix discriminator must distinguish transport-layer failures from namespace-collision failures (sibling-variant prefix-convergence regression class): {message}"
        );
    }

    #[test]
    fn bootstrap_error_transport_source_delegation_pin_returns_inner_mcp_client_error_via_std_error_source(
    ) {
        use std::error::Error;
        let inner = McpClientError::Closed;
        let expected_display = format!("{inner}");
        let err = BootstrapError::Transport(inner);
        let source = err.source().expect(
            "BootstrapError::Transport must surface the inner McpClientError via std::error::Error::source so anyhow chain printers, tracing's source-walking emitters, and daemon-side bootstrap audit-log emitters that may downcast source() to McpClientError to classify Closed vs Io vs Serde vs Wire failures can render the wrapper context AND the inner cause; a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        let source_message = format!("{source}");
        assert_eq!(
            source_message, expected_display,
            "BootstrapError::Transport source() Display must match a direct format!() of the same McpClientError variant verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> (under a 'support arbitrary transport error types' rationale) would silently break daemon-side downcasts to McpClientError used to extract the specific Closed/Io/Serde/Wire subkind for audit-log classification (concrete-source-type regression class)"
        );
        assert_eq!(
            source_message, "transport closed",
            "BootstrapError::Transport source() Display must remain the literal 'transport closed' — anchors the McpClientError::Closed Display verbatim through the wrapper so a cross-crate refactor that changed McpClientError::Closed's Display under a 'consistency with sibling variants' rationale would surface as a Transport pin failure (cross-crate-Display-drift regression class)"
        );
    }

    #[tokio::test]
    async fn bootstrap_rejects_non_object_initialize_response() {
        // Misconfigured-server guard: an MCP server that responds to
        // initialize with a non-object payload (string, array, null,
        // number) is incompatible with the spec. Refusing here keeps
        // the broken session from poisoning tools/list with a
        // downstream error that obscures the actual root cause.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!("not an object")),
            other => happy_handler(other, &Value::Null),
        }));
        let outcome = bootstrap_remote_tools(client).await;
        match outcome {
            Err(BootstrapError::BadInitialize(message)) => {
                assert!(
                    message.contains("expected JSON object"),
                    "BadInitialize must explain WHAT was rejected: {message}"
                );
                assert!(
                    message.contains("string"),
                    "BadInitialize must surface the offending kind so operators triaging the audit log see what the server actually returned: {message}"
                );
            }
            Err(other) => panic!("expected BadInitialize, got Err({other:?})"),
            Ok(tools) => panic!(
                "expected BadInitialize, got Ok with {} tools — non-object initialize must surface as a Bootstrap rejection, not a silent success",
                tools.len()
            ),
        }
    }

    #[tokio::test]
    async fn bootstrap_accepts_object_initialize_with_mismatched_protocol_version() {
        // Compatibility guard: protocolVersion drift is non-fatal per
        // the MCP spec — the server may negotiate a different version
        // — so bootstrap must succeed. The mismatch lands in the log
        // (visible to operators via tracing) without breaking the
        // session.
        let client: Arc<dyn McpClient> = Arc::new(MockMcpClient::new(|method, _| match method {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": "1999-01-01",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.1" }
            })),
            other => happy_handler(other, &Value::Null),
        }));
        let tools = bootstrap_remote_tools(client).await.expect(
            "version drift must not abort bootstrap — MCP spec allows server-side negotiation",
        );
        assert_eq!(tools.len(), 2);
    }
}
