//! Native Rust tools for the v0 registry. Real value: prove the trait shape
//! works end-to-end (with and without arguments) before wiring external MCP
//! servers.

use crate::{Content, Tool, ToolCallResult, ToolError};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Echoes its `text` argument straight back. Useful as a smoke test for the
/// CLI wiring and as a reference for argument validation.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Returns the provided `text` argument verbatim."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing string `text`".into()))?
            .to_string();
        Ok(ToolCallResult::ok(vec![Content::text(text)]))
    }
}

/// No-arg tool that returns the current epoch ms. Proves the schema-empty
/// path and gives the daemon a non-trivial probe target.
pub struct ClockTool;

#[async_trait]
impl Tool for ClockTool {
    fn name(&self) -> &str {
        "clock"
    }
    fn description(&self) -> &str {
        "Returns the current Unix time in milliseconds."
    }
    async fn call(&self, _arguments: Value) -> Result<ToolCallResult, ToolError> {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(ToolCallResult::ok(vec![Content::json(
            serde_json::json!({ "epoch_ms": ms }),
        )]))
    }
}

/// Builds an unsigned Solana transaction proposal in the shape the
/// `@covenant/sdk` instruction builders emit (`PreparedSolanaBundle` in
/// `packages/sdk/src/solana/instructions.ts`):
///
/// ```json
/// { "chain": "solana", "cluster": "...", "rpcUrl": "...",
///   "instructions": [{ "programId": "...", "instruction": "...",
///     "accounts": [{ "name": "...", "address": "...",
///                    "signer": false, "writable": true }],
///     "data": { "...": "..." } }] }
/// ```
///
/// The tool ONLY structures and validates the proposal. It never signs,
/// never sends, and holds no keypair, RPC client, or filesystem handle —
/// so there is no code path by which it can mutate chain state. Devnet is
/// the default cluster. Simulation, capability-gating, and signing are the
/// daemon broker's responsibility downstream; the program/instruction
/// account layout is knowledge the skill supplies in the call arguments.
pub struct SolanaProposeTxTool;

const PROPOSE_ARG_KEYS: [&str; 6] = [
    "programId",
    "instruction",
    "accounts",
    "data",
    "cluster",
    "rpcUrl",
];
const ACCOUNT_META_KEYS: [&str; 4] = ["name", "address", "signer", "writable"];

/// Mirror of `@covenant/config` `SOLANA_ADDRESS_REGEX`
/// (`/^[1-9A-HJ-NP-Za-km-z]{32,44}$/`) and the SDK's `assertSolanaAddress`:
/// 32–44 base58 characters (the bitcoin alphabet excludes `0`, `O`, `I`, `l`).
fn is_base58_address(value: &str) -> bool {
    let len = value.len();
    (32..=44).contains(&len)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l'))
}

/// Resolve `(cluster, default_rpc_url)` for a cluster key, mirroring
/// `resolveCovenantNetwork`: an absent or unrecognised key falls back to
/// devnet (`covenantSolanaNetworks[selected] ?? devnet`), and the `mainnet`
/// key resolves to the `mainnet-beta` cluster label.
fn cluster_network(cluster: Option<&str>) -> (&'static str, &'static str) {
    match cluster {
        Some("localnet") => ("localnet", "http://127.0.0.1:8899"),
        Some("mainnet") => ("mainnet-beta", "https://api.mainnet-beta.solana.com"),
        _ => ("devnet", "https://api.devnet.solana.com"),
    }
}

#[async_trait]
impl Tool for SolanaProposeTxTool {
    fn name(&self) -> &str {
        "solana_propose_tx"
    }
    fn description(&self) -> &str {
        "Builds an unsigned Solana transaction proposal (`programId`, \
         `instruction`, `accounts`, `data`) in the @covenant/sdk bundle \
         shape. Never signs or sends."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "programId": { "type": "string" },
                "instruction": { "type": "string" },
                "accounts": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "address": { "type": "string" },
                            "signer": { "type": "boolean" },
                            "writable": { "type": "boolean" }
                        },
                        "required": ["name", "address", "signer", "writable"],
                        "additionalProperties": false
                    }
                },
                "data": {
                    "type": "object",
                    "additionalProperties": { "type": ["string", "number", "boolean", "null"] }
                },
                "cluster": { "type": "string" },
                "rpcUrl": { "type": "string" }
            },
            "required": ["programId", "instruction", "accounts", "data"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let obj = arguments
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
        for key in obj.keys() {
            if !PROPOSE_ARG_KEYS.contains(&key.as_str()) {
                return Err(ToolError::InvalidArguments(format!(
                    "unknown argument `{key}`"
                )));
            }
        }

        let program_id = obj
            .get("programId")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArguments("missing string `programId`".into()))?;
        if !is_base58_address(program_id) {
            return Err(ToolError::InvalidArguments(
                "`programId` must be a Solana base58 address".into(),
            ));
        }

        let instruction = obj
            .get("instruction")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArguments("missing string `instruction`".into()))?;
        if instruction.is_empty() {
            return Err(ToolError::InvalidArguments(
                "`instruction` must not be empty".into(),
            ));
        }

        let accounts_in = obj
            .get("accounts")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::InvalidArguments("missing array `accounts`".into()))?;
        if accounts_in.is_empty() {
            return Err(ToolError::InvalidArguments(
                "`accounts` must not be empty".into(),
            ));
        }
        let mut accounts = Vec::with_capacity(accounts_in.len());
        for (i, entry) in accounts_in.iter().enumerate() {
            let meta = entry.as_object().ok_or_else(|| {
                ToolError::InvalidArguments(format!("accounts[{i}] must be an object"))
            })?;
            for key in meta.keys() {
                if !ACCOUNT_META_KEYS.contains(&key.as_str()) {
                    return Err(ToolError::InvalidArguments(format!(
                        "accounts[{i}] has unknown field `{key}`"
                    )));
                }
            }
            let name = meta.get("name").and_then(Value::as_str).ok_or_else(|| {
                ToolError::InvalidArguments(format!("accounts[{i}].name must be a string"))
            })?;
            if name.is_empty() {
                return Err(ToolError::InvalidArguments(format!(
                    "accounts[{i}].name must not be empty"
                )));
            }
            let address = meta.get("address").and_then(Value::as_str).ok_or_else(|| {
                ToolError::InvalidArguments(format!("accounts[{i}].address must be a string"))
            })?;
            if !is_base58_address(address) {
                return Err(ToolError::InvalidArguments(format!(
                    "accounts[{i}].address must be a Solana base58 address"
                )));
            }
            let signer = meta.get("signer").and_then(Value::as_bool).ok_or_else(|| {
                ToolError::InvalidArguments(format!("accounts[{i}].signer must be a boolean"))
            })?;
            let writable = meta
                .get("writable")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    ToolError::InvalidArguments(format!("accounts[{i}].writable must be a boolean"))
                })?;
            accounts.push(serde_json::json!({
                "name": name,
                "address": address,
                "signer": signer,
                "writable": writable,
            }));
        }

        let data = obj
            .get("data")
            .ok_or_else(|| ToolError::InvalidArguments("missing object `data`".into()))?;
        let data_map = data
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("`data` must be a JSON object".into()))?;
        for (key, value) in data_map {
            if !(value.is_string() || value.is_number() || value.is_boolean() || value.is_null()) {
                return Err(ToolError::InvalidArguments(format!(
                    "data.{key} must be a string, number, boolean, or null"
                )));
            }
        }

        let cluster_key = match obj.get("cluster") {
            None => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => {
                return Err(ToolError::InvalidArguments(
                    "`cluster` must be a string".into(),
                ))
            }
        };
        let (cluster, default_rpc) = cluster_network(cluster_key);

        let rpc_url = match obj.get("rpcUrl") {
            None => default_rpc.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::String(_)) => {
                return Err(ToolError::InvalidArguments(
                    "`rpcUrl` must not be empty".into(),
                ))
            }
            Some(_) => {
                return Err(ToolError::InvalidArguments(
                    "`rpcUrl` must be a string".into(),
                ))
            }
        };

        Ok(ToolCallResult::ok(vec![Content::json(serde_json::json!({
            "chain": "solana",
            "cluster": cluster,
            "rpcUrl": rpc_url,
            "instructions": [{
                "programId": program_id,
                "instruction": instruction,
                "accounts": accounts,
                "data": data.clone(),
            }],
        }))]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_returns_text_argument() {
        let t = EchoTool;
        let r = t.call(serde_json::json!({ "text": "ping" })).await.unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Text { text } => assert_eq!(text, "ping"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn echo_rejects_missing_text() {
        let t = EchoTool;
        let err = t.call(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn clock_returns_recent_epoch_ms() {
        let t = ClockTool;
        let r = t.call(Value::Null).await.unwrap();
        assert!(!r.is_error);
        let ms = match &r.content[0] {
            Content::Json { value } => value
                .get("epoch_ms")
                .and_then(|v| v.as_u64())
                .expect("epoch_ms u64"),
            other => panic!("unexpected: {other:?}"),
        };
        // A sanity floor: any time after 2024-01-01.
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn echo_input_schema_pins_required_text_string_property_and_additional_properties_false() {
        // EchoTool::input_schema overrides Tool::input_schema's
        // empty-object default with the documented MCP-tools/list schema:
        //
        //   {
        //     "type": "object",
        //     "properties": { "text": { "type": "string" } },
        //     "required": ["text"],
        //     "additionalProperties": false
        //   }
        //
        // The schema is what every MCP client (Claude Desktop, downstream
        // wrappers, generated SDK clients) reads to validate operator
        // inputs before sending them to the daemon. echo_returns_text_argument
        // and echo_rejects_missing_text cover the call() runtime path but
        // no test pins the schema shape itself. A refactor that flipped
        // additionalProperties to true would silently let MCP clients send
        // arbitrary extra fields; a refactor that renamed properties.text
        // to message without updating call()'s arguments.get('text') would
        // silently break every operator-facing echo invocation.
        let schema = EchoTool.input_schema();
        let obj = schema
            .as_object()
            .expect("EchoTool::input_schema must emit a JSON object");

        assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "EchoTool::input_schema must declare type=object — a refactor \
             that changed the top-level type to anything else (e.g., array) \
             would break every MCP client's argument-validation entry path",
        );

        let properties = obj.get("properties").and_then(|v| v.as_object()).expect(
            "schema.properties must be a JSON object — MCP clients \
                     traverse it to enumerate accepted arguments",
        );
        let text_spec = properties.get("text").and_then(|v| v.as_object()).expect(
            "schema.properties.text must be an object with a type field — \
                 a refactor that renamed properties.text (e.g., to message) \
                 without updating call()'s arguments.get('text') would silently \
                 break every operator-facing echo invocation because the daemon \
                 keeps reading the old key",
        );
        assert_eq!(
            text_spec.get("type").and_then(|v| v.as_str()),
            Some("string"),
            "EchoTool::input_schema.properties.text.type must declare 'string' \
             — call() uses Value::as_str on arguments.get('text'), so a schema \
             that declared anything else (e.g., 'integer' or 'array') would \
             let MCP clients pass values that the daemon path treats as \
             missing-text",
        );

        let required = obj.get("required").and_then(|v| v.as_array()).expect(
            "schema.required must be a JSON array — a refactor that swapped \
                 the array for a string 'text' under a 'simpler schema authoring' \
                 rationale would silently break MCP client validation that reads \
                 required as an array, and operator-facing inputs that omitted \
                 the text field would either fail with a confusing schema error \
                 or succeed unexpectedly through a permissive fallback",
        );
        assert_eq!(
            required.len(),
            1,
            "schema.required must be exactly one element — a refactor that \
             added a second required field (e.g., metadata) without updating \
             call()'s extraction path would silently let every echo call \
             surface a confusing missing-arg error on inputs that historically \
             validated",
        );
        assert_eq!(
            required[0].as_str(),
            Some("text"),
            "schema.required[0] must be the string 'text' — pinning the \
             exact field name so a refactor that renamed properties.text \
             and the required entry together (but not call()'s arguments.get) \
             still surfaces here, and the cross-binding with the call()-side \
             literal stays anchored",
        );

        let additional = obj.get("additionalProperties").unwrap_or_else(|| {
            panic!(
                "schema.additionalProperties must be present — a refactor \
                 that dropped the field under a 'JSON Schema default is \
                 false anyway' rationale would silently let some MCP \
                 clients (whose defaults are true) accept extra fields the \
                 daemon ignores"
            )
        });
        assert_eq!(
            additional,
            &Value::Bool(false),
            "schema.additionalProperties must be the JSON bool false (not the \
             string 'false', not the number 0, not null) — a refactor that \
             flipped this to true to support a future optional field without \
             bumping the schema would silently let MCP clients send arbitrary \
             extra fields, and SDK clients generated from the schema would \
             lose the closed-world invariant the daemon's call() path relies \
             on for argument shape stability",
        );

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["additionalProperties", "properties", "required", "type"],
            "EchoTool::input_schema must surface EXACTLY four top-level keys \
             (type, properties, required, additionalProperties) — a refactor \
             that added a fifth field (e.g., 'title' or '$schema') without \
             coordinating the MCP wire-form expectations would silently shift \
             the schema shape across every client; pinning the exact key set \
             catches both additions and removals in one assertion",
        );
    }

    #[test]
    fn echo_and_clock_tool_name_and_description_pin_operator_facing_strings() {
        // EchoTool::name and description, plus ClockTool::name and
        // description, are the four operator-facing strings every MCP
        // client (Claude
        // Desktop, downstream SDK wrappers, the Covenant TUI) reads
        // out of tools/list to render the tool catalog. The names
        // double as selectors operators type into commands; the
        // descriptions are the tooltip operators consult to decide
        // when to invoke the tool.
        //
        // registry_lists_tools_sorted_by_name (in lib.rs) pins the
        // names indirectly via the sorted-output assertion
        // ['clock', 'echo'], so a rename of either would surface there.
        // But no test reads .description() and compares the literal
        // string. A refactor that rewrote either description —
        // 'Returns the provided text argument verbatim.' → 'Echo the
        // input back.' for terseness, or 'Returns the current Unix
        // time in milliseconds.' → 'Get current time.' for
        // accessibility — would silently shift the operator-facing
        // copy. echo_returns_text_argument and
        // clock_returns_recent_epoch_ms probe call() behavior, not the
        // description string.

        assert_eq!(
            EchoTool.name(),
            "echo",
            "EchoTool::name must remain 'echo' — the operator-typed \
             selector and the registry sort key. A rename would break \
             every operator command and the sort-order pin in \
             registry_lists_tools_sorted_by_name",
        );
        assert_eq!(
            EchoTool.description(),
            "Returns the provided `text` argument verbatim.",
            "EchoTool::description must remain the literal documented \
             string — the backtick-wrapped 'text' identifier names the \
             argument operators must pass, and a rewrite (e.g., 'Echo \
             the input back.') would silently drop that pointer. The \
             behavior tests probe call() return value, not the \
             description, so they pass under any rewrite",
        );
        assert_eq!(
            ClockTool.name(),
            "clock",
            "ClockTool::name must remain 'clock' — the operator-typed \
             selector and the registry sort key paired with EchoTool::name",
        );
        assert_eq!(
            ClockTool.description(),
            "Returns the current Unix time in milliseconds.",
            "ClockTool::description must remain the literal documented \
             string — the 'milliseconds' unit specifier is load-bearing \
             because operators consuming the epoch_ms return value need \
             to know whether to convert to seconds, minutes, or use the \
             value verbatim. A rewrite that dropped the unit (e.g., \
             'Get current time.' or 'Return Unix time.') would silently \
             let operators consume the value as seconds and parse the \
             returned epoch as a date ~1000x earlier than intended",
        );
    }

    fn valid_propose_args() -> Value {
        // A register_agent-shaped proposal: the SDK's
        // prepareRegisterAgentInstruction account layout, with addresses
        // the caller (the skill) supplies. Used as the happy-path fixture
        // the rejection tests mutate one field at a time.
        serde_json::json!({
            "programId": "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y",
            "instruction": "register_agent",
            "accounts": [
                { "name": "config", "address": "11111111111111111111111111111111", "signer": false, "writable": false },
                { "name": "agent", "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "signer": false, "writable": true },
                { "name": "operator", "address": "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y", "signer": true, "writable": true }
            ],
            "data": {
                "agent_key": "0000000000000000000000000000000000000000000000000000000000000001",
                "metadata_hash": "0000000000000000000000000000000000000000000000000000000000000002"
            }
        })
    }

    fn json_keys_recursively<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    out.push(k.as_str());
                    json_keys_recursively(v, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| json_keys_recursively(v, out)),
            _ => {}
        }
    }

    async fn call_bundle(args: Value) -> Value {
        let r = SolanaProposeTxTool.call(args).await.unwrap();
        assert!(!r.is_error);
        match &r.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn propose_returns_sdk_shaped_unsigned_bundle() {
        let r = SolanaProposeTxTool
            .call(valid_propose_args())
            .await
            .unwrap();
        assert!(!r.is_error);
        let bundle = match &r.content[0] {
            Content::Json { value } => value,
            other => panic!("expected json content, got {other:?}"),
        };

        assert_eq!(bundle["chain"], "solana");
        assert_eq!(bundle["cluster"], "devnet");
        assert_eq!(bundle["rpcUrl"], "https://api.devnet.solana.com");

        let ix = &bundle["instructions"][0];
        assert_eq!(
            ix["programId"],
            "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"
        );
        assert_eq!(ix["instruction"], "register_agent");
        assert_eq!(
            ix["data"]["agent_key"],
            valid_propose_args()["data"]["agent_key"]
        );

        let accounts = ix["accounts"].as_array().expect("accounts array");
        assert_eq!(accounts.len(), 3);
        let operator = &accounts[2];
        assert_eq!(operator["name"], "operator");
        assert_eq!(operator["signer"], true);
        assert_eq!(operator["writable"], true);
        let meta_keys: std::collections::BTreeSet<&str> = operator
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            meta_keys,
            ["address", "name", "signer", "writable"]
                .into_iter()
                .collect(),
            "each account meta must expose exactly the PreparedAccountMeta keys; \
             a divergence breaks shape parity with packages/sdk",
        );
    }

    #[tokio::test]
    async fn propose_never_emits_a_signature_field() {
        // The whole point of skills-30 is propose-only: the broker signs
        // later, never this tool. The tool holds no keypair and no RPC
        // handle, so the only way a signature could appear is a refactor
        // that started signing here. Pin that no emitted key resembles a
        // produced signature or secret.
        let bundle = call_bundle(valid_propose_args()).await;
        let mut keys = Vec::new();
        json_keys_recursively(&bundle, &mut keys);
        for forbidden in [
            "signature",
            "signatures",
            "signedTx",
            "sig",
            "secretKey",
            "keypair",
        ] {
            assert!(
                !keys.contains(&forbidden),
                "propose output must never carry `{forbidden}` — this tool \
                 proposes, it must not sign or send",
            );
        }
    }

    #[tokio::test]
    async fn propose_defaults_cluster_and_rpc_to_devnet() {
        let bundle = call_bundle(valid_propose_args()).await;
        assert_eq!(bundle["cluster"], "devnet");
        assert_eq!(bundle["rpcUrl"], "https://api.devnet.solana.com");
    }

    #[tokio::test]
    async fn propose_resolves_known_cluster_rpc_defaults() {
        let mut args = valid_propose_args();
        args["cluster"] = "localnet".into();
        let bundle = call_bundle(args).await;
        assert_eq!(bundle["cluster"], "localnet");
        assert_eq!(bundle["rpcUrl"], "http://127.0.0.1:8899");

        let mut args = valid_propose_args();
        args["cluster"] = "mainnet".into();
        let bundle = call_bundle(args).await;
        assert_eq!(
            bundle["cluster"], "mainnet-beta",
            "the `mainnet` key must resolve to the mainnet-beta cluster label, mirroring resolveCovenantNetwork",
        );
        assert_eq!(bundle["rpcUrl"], "https://api.mainnet-beta.solana.com");
    }

    #[tokio::test]
    async fn propose_unknown_cluster_falls_back_to_devnet() {
        let mut args = valid_propose_args();
        args["cluster"] = "staging".into();
        let bundle = call_bundle(args).await;
        assert_eq!(
            bundle["cluster"], "devnet",
            "an unrecognised cluster key must fall back to devnet, mirroring \
             `covenantSolanaNetworks[selected] ?? devnet`",
        );
        assert_eq!(bundle["rpcUrl"], "https://api.devnet.solana.com");
    }

    #[tokio::test]
    async fn propose_respects_explicit_rpc_url() {
        let mut args = valid_propose_args();
        args["rpcUrl"] = "https://devnet.helius-rpc.com/?api-key=x".into();
        let bundle = call_bundle(args).await;
        assert_eq!(bundle["rpcUrl"], "https://devnet.helius-rpc.com/?api-key=x");
    }

    #[tokio::test]
    async fn propose_rejects_non_base58_program_id() {
        let mut args = valid_propose_args();
        args["programId"] = "not a base58 address".into();
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));

        // 31 chars (one under the floor) and a base58-excluded char ('0').
        let mut args = valid_propose_args();
        args["programId"] = "1111111111111111111111111111111".into();
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
        let mut args = valid_propose_args();
        args["programId"] = "cov0UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y".into();
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn propose_rejects_non_base58_account_address() {
        let mut args = valid_propose_args();
        args["accounts"][0]["address"] = "bad!".into();
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn propose_rejects_empty_accounts() {
        let mut args = valid_propose_args();
        args["accounts"] = serde_json::json!([]);
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn propose_rejects_non_scalar_data_value() {
        // SDK data is Record<string, string|number|boolean|null>. A nested
        // object or array means the skill skipped serialising the field to
        // its instruction-data scalar form; reject it at the boundary.
        let mut args = valid_propose_args();
        args["data"]["nested"] = serde_json::json!({ "x": 1 });
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
        let mut args = valid_propose_args();
        args["data"]["list"] = serde_json::json!([1, 2]);
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn propose_accepts_mixed_scalar_data_values() {
        let mut args = valid_propose_args();
        args["data"] = serde_json::json!({
            "amount_covnt": "1000",
            "receipt_count": 3,
            "dry_run": true,
            "memo": Value::Null,
        });
        let bundle = call_bundle(args).await;
        let data = &bundle["instructions"][0]["data"];
        assert_eq!(data["amount_covnt"], "1000");
        assert_eq!(data["receipt_count"], 3);
        assert_eq!(data["dry_run"], true);
        assert!(data["memo"].is_null());
    }

    #[tokio::test]
    async fn propose_rejects_missing_required_args() {
        for missing in ["programId", "instruction", "accounts", "data"] {
            let mut args = valid_propose_args();
            args.as_object_mut().unwrap().remove(missing);
            let err = SolanaProposeTxTool.call(args).await.unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidArguments(_)),
                "omitting required `{missing}` must be rejected, got {err:?}",
            );
        }
    }

    #[tokio::test]
    async fn propose_rejects_unknown_fields() {
        let mut args = valid_propose_args();
        args["sign"] = true.into();
        assert!(
            matches!(
                SolanaProposeTxTool.call(args).await.unwrap_err(),
                ToolError::InvalidArguments(_)
            ),
            "an unknown top-level argument (e.g. a `sign` flag) must be \
             rejected so the closed-world schema cannot be widened silently",
        );
        let mut args = valid_propose_args();
        args["accounts"][0]["pubkey"] = "x".into();
        assert!(matches!(
            SolanaProposeTxTool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn propose_tool_registers_and_dispatches_via_registry() {
        // Defends the "not registered in ToolRegistry" failure mode at the
        // unit level: the tool must list under its name and be callable
        // through ToolRegistry::call, the same path covenantd's daemon uses.
        use crate::ToolRegistry;
        use std::sync::Arc;
        let reg = ToolRegistry::from_tools(vec![Arc::new(SolanaProposeTxTool)]);
        assert!(reg.names().contains(&"solana_propose_tx".to_string()));
        let r = reg
            .call("solana_propose_tx", valid_propose_args())
            .await
            .unwrap();
        assert!(!r.is_error);
    }

    #[test]
    fn propose_input_schema_pins_required_set_and_closed_world() {
        let schema = SolanaProposeTxTool.input_schema();
        assert_eq!(schema["type"], "object");
        let required: std::collections::BTreeSet<&str> = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            required,
            ["programId", "instruction", "accounts", "data"]
                .into_iter()
                .collect(),
            "the four required args are the proposal's load-bearing fields; \
             cluster and rpcUrl stay optional with devnet defaults",
        );
        assert_eq!(
            schema["additionalProperties"],
            Value::Bool(false),
            "the proposal schema must stay closed-world so MCP clients \
             cannot smuggle a sign/send flag past the daemon",
        );
        assert_eq!(
            schema["properties"]["accounts"]["items"]["additionalProperties"],
            Value::Bool(false),
            "account metas must stay closed-world to preserve PreparedAccountMeta parity",
        );
        // The schema must advertise the same non-empty/scalar-only contracts
        // call() enforces, so MCP clients reject bad input before dispatch.
        assert_eq!(
            schema["properties"]["accounts"]["minItems"], 1,
            "accounts must declare minItems:1 to match the runtime non-empty guard",
        );
        let data_values: std::collections::BTreeSet<&str> = schema["properties"]["data"]
            ["additionalProperties"]["type"]
            .as_array()
            .expect("data.additionalProperties.type array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            data_values,
            ["boolean", "null", "number", "string"]
                .into_iter()
                .collect(),
            "data values must be schema-constrained to scalars, mirroring the \
             SDK Record<string, string|number|boolean|null> and the runtime check",
        );
    }

    #[test]
    fn propose_tool_name_and_description_pin() {
        assert_eq!(
            SolanaProposeTxTool.name(),
            "solana_propose_tx",
            "the name is the selector the broker keys on and the registry sort key",
        );
        let desc = SolanaProposeTxTool.description();
        assert!(
            desc.contains("unsigned") && desc.contains("Never signs"),
            "the description must keep advertising the propose-only contract: {desc:?}",
        );
    }
}
