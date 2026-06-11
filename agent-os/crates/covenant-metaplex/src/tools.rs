//! MCP tool surface for the Metaplex profile.
//!
//! Two families, both named `metaplex.*` and gated by the daemon's
//! `tool.call.metaplex.*` capabilities:
//!
//! - read tools (`metaplex.das.*`) run DAS queries through a
//!   [`crate::das::DasClient`]. No keys, no spend.
//! - write tools (`metaplex.attest.*`, `metaplex.identity.*`) hand a
//!   [`SignerRequest`] to a [`MetaplexSigner`] (the daemon drives the
//!   signer sidecar). The minting key never enters this crate.
//!
//! [`metaplex_specs`] lists the enabled tools for `tools/list`;
//! [`metaplex_tool`] resolves one by name for dispatch. Both honour
//! `config.reads_enabled()` / `config.writes_enabled()` and the
//! allowlist, mirroring the Hyre profile's `hyre_specs` / `hyre_tool`.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{json, Value};

use crate::config::MetaplexConfig;
use crate::das::DasClient;
use crate::request::{AttestationPayload, SignerRequest, SignerResponse};

/// Prefix every Metaplex tool name carries.
pub const TOOL_PREFIX: &str = "metaplex.";

const READ_SLUGS: &[&str] = &[
    "das.get_asset",
    "das.assets_by_owner",
    "das.search",
    "das.get_asset_proof",
];
const WRITE_SLUGS: &[&str] = &["attest.audit_root", "identity.register"];

/// The write side of the profile: hand a built request to whatever holds
/// the minting key. The daemon's implementation drives the
/// `covenant-metaplex-signer` subprocess; tests can stand in.
#[async_trait]
pub trait MetaplexSigner: Send + Sync {
    async fn sign(&self, request: SignerRequest) -> Result<SignerResponse, String>;
}

fn slug_of(name: &str) -> &str {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name)
}

/// Map a borrowed slug back to its `'static` table entry, so a tool can
/// hold a `&'static str` slug for its name/description without owning it.
fn static_read_slug(slug: &str) -> Option<&'static str> {
    READ_SLUGS.iter().copied().find(|s| *s == slug)
}
fn static_write_slug(slug: &str) -> Option<&'static str> {
    WRITE_SLUGS.iter().copied().find(|s| *s == slug)
}

fn description(slug: &str) -> &'static str {
    match slug {
        "das.get_asset" => "Fetch one Solana digital asset (NFT, cNFT, or token) by id via the Metaplex DAS API.",
        "das.assets_by_owner" => "Page through the digital assets owned by a wallet via the Metaplex DAS API.",
        "das.search" => "Search Solana digital assets with a structured DAS searchAssets query.",
        "das.get_asset_proof" => "Fetch the merkle proof for a compressed asset (cNFT) via the Metaplex DAS API.",
        "attest.audit_root" => "Anchor a Covenant audit-root attestation into an MPL Core asset's AppData plugin (DAS-indexed).",
        "identity.register" => "Bind the daemon's agent identity to an MPL Agent Identity record on an MPL Core asset.",
        _ => "Metaplex tool.",
    }
}

fn input_schema(slug: &str) -> Value {
    match slug {
        "das.get_asset" | "das.get_asset_proof" => json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "Asset id (mint or compressed-asset id)" } },
            "required": ["id"],
            "additionalProperties": false,
        }),
        "das.assets_by_owner" => json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Owner wallet address" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000 },
                "page": { "type": "integer", "minimum": 1 }
            },
            "required": ["owner"],
            "additionalProperties": false,
        }),
        "das.search" => json!({
            "type": "object",
            "description": "Pass-through of a DAS searchAssets params object.",
            "additionalProperties": true,
        }),
        "attest.audit_root" => json!({
            "type": "object",
            "properties": {
                "rootHashHex": { "type": "string", "description": "32-byte audit/merkle root, lowercase hex (64 chars)" },
                "releaseTarget": { "type": "string" },
                "releaseSubject": { "type": "string" },
                "releaseScope": { "type": "string" },
                "recordedAt": { "type": "integer", "minimum": 0 },
                "collection": { "type": "string", "description": "MPL Core collection to mint into (optional; overridden by COVENANT_METAPLEX_COLLECTION)" }
            },
            "required": ["rootHashHex", "releaseTarget", "releaseSubject", "releaseScope", "recordedAt"],
            "additionalProperties": false,
        }),
        "identity.register" => json!({
            "type": "object",
            "properties": {
                "agentLabel": { "type": "string" },
                "agentPubkey": { "type": "string" },
                "registrationUri": { "type": "string", "description": "ERC-8004 registration document URI (https:// or ar://); omit to record the covenant://agent/<pubkey> identifier form" }
            },
            "required": ["agentLabel", "agentPubkey"],
            "additionalProperties": false,
        }),
        _ => json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    }
}

fn spec_for(slug: &str) -> ToolSpec {
    ToolSpec {
        name: format!("{TOOL_PREFIX}{slug}"),
        description: description(slug).to_string(),
        input_schema: input_schema(slug),
    }
}

/// Specs for the tools this config enables, for `tools/list`.
pub fn metaplex_specs(config: &MetaplexConfig) -> Vec<ToolSpec> {
    let mut specs = Vec::new();
    if config.reads_enabled() {
        for slug in READ_SLUGS {
            if config.allows(slug) {
                specs.push(spec_for(slug));
            }
        }
    }
    if config.writes_enabled() {
        for slug in WRITE_SLUGS {
            if config.allows(slug) {
                specs.push(spec_for(slug));
            }
        }
    }
    specs
}

/// Resolve a Metaplex tool by full name for dispatch. Returns `None`
/// when the tool is unknown, disabled by config/allowlist, or a write
/// tool with no signer wired.
pub fn metaplex_tool(
    config: &MetaplexConfig,
    name: &str,
    das: Arc<dyn DasClient>,
    signer: Option<Arc<dyn MetaplexSigner>>,
) -> Option<Arc<dyn Tool>> {
    let slug = slug_of(name);
    if !config.allows(slug) {
        return None;
    }
    if let Some(slug) = static_read_slug(slug) {
        if !config.reads_enabled() {
            return None;
        }
        return Some(Arc::new(ReadTool {
            slug,
            name: format!("{TOOL_PREFIX}{slug}"),
            das,
        }) as Arc<dyn Tool>);
    }
    if let Some(slug) = static_write_slug(slug) {
        if !config.writes_enabled() {
            return None;
        }
        let signer = signer?;
        return Some(Arc::new(WriteTool {
            slug,
            name: format!("{TOOL_PREFIX}{slug}"),
            collection: config.collection.clone(),
            signer,
        }) as Arc<dyn Tool>);
    }
    None
}

// --- read tools -----------------------------------------------------------

struct ReadTool {
    slug: &'static str,
    name: String,
    das: Arc<dyn DasClient>,
}

impl ReadTool {
    async fn run(&self, args: Value) -> Result<Value, ToolError> {
        match self.slug {
            "das.get_asset" => {
                let id = str_arg(&args, "id")?;
                self.das.get_asset(id).await.map_err(das_err)
            }
            "das.get_asset_proof" => {
                let id = str_arg(&args, "id")?;
                self.das.get_asset_proof(id).await.map_err(das_err)
            }
            "das.assets_by_owner" => {
                let owner = str_arg(&args, "owner")?;
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
                let page = args.get("page").and_then(Value::as_u64).unwrap_or(1) as u32;
                self.das
                    .get_assets_by_owner(owner, limit, page)
                    .await
                    .map_err(das_err)
            }
            "das.search" => self.das.search_assets(args).await.map_err(das_err),
            other => Err(ToolError::NotFound(format!("{TOOL_PREFIX}{other}"))),
        }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        description(self.slug)
    }
    fn input_schema(&self) -> Value {
        input_schema(self.slug)
    }
    fn spec(&self) -> ToolSpec {
        spec_for(self.slug)
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        match self.run(arguments).await {
            Ok(v) => Ok(ToolCallResult::ok(vec![Content::json(v)])),
            Err(ToolError::InvalidArguments(m)) => Err(ToolError::InvalidArguments(m)),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

// --- write tools ----------------------------------------------------------

struct WriteTool {
    slug: &'static str,
    name: String,
    collection: String,
    signer: Arc<dyn MetaplexSigner>,
}

impl WriteTool {
    fn build_request(&self, args: &Value) -> Result<SignerRequest, ToolError> {
        match self.slug {
            "attest.audit_root" => {
                let root = str_arg(args, "rootHashHex")?;
                crate::request::validate_root_hash_hex(root)
                    .map_err(ToolError::InvalidArguments)?;
                let payload = AttestationPayload::new(
                    root,
                    str_arg(args, "releaseTarget")?,
                    str_arg(args, "releaseSubject")?,
                    str_arg(args, "releaseScope")?,
                    args.get("recordedAt")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| {
                            ToolError::InvalidArguments("recordedAt (integer) is required".into())
                        })?,
                );
                // `asset` (append to an existing attestation asset) is not
                // wired yet — v1 always mints a fresh asset, so we never
                // pass one and never advertise the arg.
                Ok(SignerRequest::AttestAuditRoot {
                    payload,
                    asset: None,
                    collection: if self.collection.is_empty() {
                        opt_str_arg(args, "collection")
                    } else {
                        Some(self.collection.clone())
                    },
                })
            }
            "identity.register" => {
                let registration_uri = opt_str_arg(args, "registrationUri");
                if let Some(uri) = &registration_uri {
                    crate::request::validate_registration_uri(uri)
                        .map_err(ToolError::InvalidArguments)?;
                }
                Ok(SignerRequest::RegisterIdentity {
                    agent_label: str_arg(args, "agentLabel")?.to_string(),
                    agent_pubkey: str_arg(args, "agentPubkey")?.to_string(),
                    asset: None,
                    registration_uri,
                })
            }
            other => Err(ToolError::NotFound(format!("{TOOL_PREFIX}{other}"))),
        }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        description(self.slug)
    }
    fn input_schema(&self) -> Value {
        input_schema(self.slug)
    }
    fn spec(&self) -> ToolSpec {
        spec_for(self.slug)
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let request = self.build_request(&arguments)?;
        match self.signer.sign(request).await {
            Ok(resp) => Ok(ToolCallResult::ok(vec![Content::json(
                serde_json::to_value(resp).unwrap_or(Value::Null),
            )])),
            Err(e) => Ok(ToolCallResult::error(e)),
        }
    }
}

// --- helpers --------------------------------------------------------------

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} (non-empty string) is required")))
}

fn opt_str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn das_err(e: crate::das::DasError) -> ToolError {
    ToolError::Failed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_reads() -> MetaplexConfig {
        MetaplexConfig {
            enabled: true,
            das_url: "https://das.example".into(),
            ..Default::default()
        }
    }

    fn enabled_all() -> MetaplexConfig {
        MetaplexConfig {
            enabled: true,
            das_url: "https://das.example".into(),
            rpc_url: "https://api.devnet.solana.com".into(),
            signer_binary: "/bin/covenant-metaplex-signer".into(),
            ..Default::default()
        }
    }

    struct NullDas;
    #[async_trait]
    impl DasClient for NullDas {
        async fn get_asset(&self, _id: &str) -> Result<Value, crate::das::DasError> {
            Ok(json!({ "ok": true }))
        }
        async fn get_asset_proof(&self, _id: &str) -> Result<Value, crate::das::DasError> {
            Ok(Value::Null)
        }
        async fn get_assets_by_owner(
            &self,
            _o: &str,
            _l: u32,
            _p: u32,
        ) -> Result<Value, crate::das::DasError> {
            Ok(Value::Null)
        }
        async fn search_assets(&self, p: Value) -> Result<Value, crate::das::DasError> {
            Ok(p)
        }
    }

    struct CapturingSigner;
    #[async_trait]
    impl MetaplexSigner for CapturingSigner {
        async fn sign(&self, _r: SignerRequest) -> Result<SignerResponse, String> {
            Ok(SignerResponse {
                signature: "sig".into(),
                asset: "asset".into(),
                cluster: "devnet".into(),
            })
        }
    }

    #[test]
    fn read_only_config_lists_only_read_tools() {
        let specs = metaplex_specs(&enabled_reads());
        let names: Vec<_> = specs.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.iter().all(|n| n.starts_with("metaplex.das.")));
    }

    #[test]
    fn full_config_lists_read_and_write_tools() {
        let specs = metaplex_specs(&enabled_all());
        assert_eq!(specs.len(), 6);
        assert!(specs.iter().any(|s| s.name == "metaplex.attest.audit_root"));
    }

    #[test]
    fn disabled_config_lists_nothing() {
        assert!(metaplex_specs(&MetaplexConfig::default()).is_empty());
    }

    #[test]
    fn write_tool_unavailable_without_signer() {
        let cfg = enabled_all();
        let got = metaplex_tool(&cfg, "metaplex.attest.audit_root", Arc::new(NullDas), None);
        assert!(got.is_none(), "write tool must be None without a signer");
    }

    #[tokio::test]
    async fn read_tool_requires_id() {
        let cfg = enabled_reads();
        let tool = metaplex_tool(&cfg, "metaplex.das.get_asset", Arc::new(NullDas), None).unwrap();
        let err = tool.call(json!({})).await.expect_err("missing id");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        let ok = tool.call(json!({ "id": "abc" })).await.unwrap();
        assert!(!ok.is_error);
    }

    #[tokio::test]
    async fn write_tool_builds_and_signs() {
        let cfg = enabled_all();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.attest.audit_root",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();
        // Short hash is rejected before any subprocess/on-chain round-trip.
        let err = tool
            .call(json!({
                "rootHashHex": "deadbeef",
                "releaseTarget": "v0.1.0",
                "releaseSubject": "covenant",
                "releaseScope": "audit",
                "recordedAt": 1_700_000_000u64,
            }))
            .await
            .expect_err("short root hash");
        assert!(matches!(err, ToolError::InvalidArguments(m) if m.contains("64 hex")));
        let ok = tool
            .call(json!({
                "rootHashHex": "a".repeat(64),
                "releaseTarget": "v0.1.0",
                "releaseSubject": "covenant",
                "releaseScope": "audit",
                "recordedAt": 1_700_000_000u64,
            }))
            .await
            .unwrap();
        assert!(!ok.is_error);
    }

    struct EchoDas;
    #[async_trait]
    impl DasClient for EchoDas {
        async fn get_asset(&self, id: &str) -> Result<Value, crate::das::DasError> {
            Ok(json!({ "id": id }))
        }
        async fn get_asset_proof(&self, id: &str) -> Result<Value, crate::das::DasError> {
            Ok(json!({ "id": id }))
        }
        async fn get_assets_by_owner(
            &self,
            o: &str,
            l: u32,
            p: u32,
        ) -> Result<Value, crate::das::DasError> {
            Ok(json!({ "owner": o, "limit": l, "page": p }))
        }
        async fn search_assets(&self, p: Value) -> Result<Value, crate::das::DasError> {
            Ok(p)
        }
    }

    struct ErrDas;
    #[async_trait]
    impl DasClient for ErrDas {
        async fn get_asset(&self, _id: &str) -> Result<Value, crate::das::DasError> {
            Err(crate::das::DasError::Rpc("boom".into()))
        }
        async fn get_asset_proof(&self, _id: &str) -> Result<Value, crate::das::DasError> {
            Err(crate::das::DasError::Rpc("boom".into()))
        }
        async fn get_assets_by_owner(
            &self,
            _o: &str,
            _l: u32,
            _p: u32,
        ) -> Result<Value, crate::das::DasError> {
            Err(crate::das::DasError::Rpc("boom".into()))
        }
        async fn search_assets(&self, _p: Value) -> Result<Value, crate::das::DasError> {
            Err(crate::das::DasError::Rpc("boom".into()))
        }
    }

    #[test]
    fn allowlist_filters_advertised_specs() {
        let mut cfg = enabled_all();
        cfg.allow = Some(vec!["das.get_asset".into(), "attest.audit_root".into()]);
        let names: Vec<_> = metaplex_specs(&cfg).into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec!["metaplex.das.get_asset", "metaplex.attest.audit_root"]
        );
    }

    #[test]
    fn allowlist_gates_per_name_dispatch() {
        let mut cfg = enabled_all();
        cfg.allow = Some(vec!["das.get_asset".into()]);
        assert!(
            metaplex_tool(&cfg, "metaplex.das.get_asset", Arc::new(NullDas), None).is_some(),
            "an allowed read resolves"
        );
        assert!(
            metaplex_tool(&cfg, "metaplex.das.search", Arc::new(NullDas), None).is_none(),
            "a disallowed read does not resolve even with reads enabled"
        );
        assert!(
            metaplex_tool(
                &cfg,
                "metaplex.attest.audit_root",
                Arc::new(NullDas),
                Some(Arc::new(CapturingSigner)),
            )
            .is_none(),
            "a disallowed write does not resolve even with a signer wired"
        );
    }

    #[test]
    fn unknown_tool_name_resolves_to_none() {
        let cfg = enabled_all();
        assert!(metaplex_tool(
            &cfg,
            "metaplex.does.not_exist",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .is_none());
    }

    #[test]
    fn read_tool_unavailable_when_reads_disabled() {
        let cfg = MetaplexConfig {
            enabled: true,
            das_url: String::new(),
            ..Default::default()
        };
        assert!(!cfg.reads_enabled());
        assert!(metaplex_tool(&cfg, "metaplex.das.get_asset", Arc::new(NullDas), None).is_none());
    }

    #[test]
    fn write_tool_unavailable_when_writes_disabled() {
        // Reads on, writes off (no rpc/signer), but a signer is supplied: the
        // writes_enabled config gate must refuse before the signer is consulted.
        let cfg = enabled_reads();
        assert!(!cfg.writes_enabled());
        assert!(metaplex_tool(
            &cfg,
            "metaplex.attest.audit_root",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .is_none());
    }

    #[tokio::test]
    async fn identity_register_rejects_malformed_uri() {
        let cfg = enabled_all();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.identity.register",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();
        let err = tool
            .call(json!({
                "agentLabel": "agent@local",
                "agentPubkey": "Agent11111111111111111111111111111111111111",
                "registrationUri": "http://insecure.example/a.json",
            }))
            .await
            .expect_err("plain http must be rejected before any signer round-trip");
        assert!(matches!(err, ToolError::InvalidArguments(m) if m.contains("registrationUri")));
    }

    #[tokio::test]
    async fn identity_register_builds_and_signs_with_valid_args() {
        let cfg = enabled_all();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.identity.register",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();
        let ok = tool
            .call(json!({
                "agentLabel": "agent@local",
                "agentPubkey": "Agent11111111111111111111111111111111111111",
                "registrationUri": "https://opencovenant.org/agents/covenant.json",
            }))
            .await
            .unwrap();
        assert!(!ok.is_error);
    }

    #[tokio::test]
    async fn identity_register_requires_agent_pubkey() {
        let cfg = enabled_all();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.identity.register",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();
        let err = tool
            .call(json!({ "agentLabel": "agent@local" }))
            .await
            .expect_err("missing agentPubkey");
        assert!(matches!(err, ToolError::InvalidArguments(m) if m.contains("agentPubkey")));
    }

    #[tokio::test]
    async fn search_passes_args_through_to_das() {
        let cfg = enabled_reads();
        let tool = metaplex_tool(&cfg, "metaplex.das.search", Arc::new(EchoDas), None).unwrap();
        let res = tool
            .call(json!({ "ownerAddress": "owner", "page": 2 }))
            .await
            .unwrap();
        assert!(!res.is_error);
        assert_eq!(
            res.content,
            vec![Content::json(json!({ "ownerAddress": "owner", "page": 2 }))]
        );
    }

    #[tokio::test]
    async fn assets_by_owner_defaults_limit_and_page() {
        let cfg = enabled_reads();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.das.assets_by_owner",
            Arc::new(EchoDas),
            None,
        )
        .unwrap();
        let res = tool.call(json!({ "owner": "wallet" })).await.unwrap();
        assert_eq!(
            res.content,
            vec![Content::json(
                json!({ "owner": "wallet", "limit": 100, "page": 1 })
            )]
        );
        let res = tool
            .call(json!({ "owner": "wallet", "limit": 5, "page": 3 }))
            .await
            .unwrap();
        assert_eq!(
            res.content,
            vec![Content::json(
                json!({ "owner": "wallet", "limit": 5, "page": 3 })
            )]
        );
    }

    #[tokio::test]
    async fn das_error_surfaces_as_non_throwing_error_result() {
        let cfg = enabled_reads();
        let tool = metaplex_tool(&cfg, "metaplex.das.get_asset", Arc::new(ErrDas), None).unwrap();
        let res = tool
            .call(json!({ "id": "abc" }))
            .await
            .expect("a DAS rpc error is a tool-error result, not a protocol error");
        assert!(res.is_error);
    }

    struct RecordingSigner {
        seen: std::sync::Mutex<Option<SignerRequest>>,
    }
    #[async_trait]
    impl MetaplexSigner for RecordingSigner {
        async fn sign(&self, request: SignerRequest) -> Result<SignerResponse, String> {
            *self.seen.lock().unwrap() = Some(request);
            Ok(SignerResponse {
                signature: "sig".into(),
                asset: "asset".into(),
                cluster: "devnet".into(),
            })
        }
    }

    /// Drive a write tool through the real `tool.call()` path and return the
    /// `SignerRequest` `build_request` handed to the signer.
    async fn captured_request(cfg: &MetaplexConfig, name: &str, args: Value) -> SignerRequest {
        let rec = Arc::new(RecordingSigner {
            seen: std::sync::Mutex::new(None),
        });
        let signer: Arc<dyn MetaplexSigner> = rec.clone();
        let tool = metaplex_tool(cfg, name, Arc::new(NullDas), Some(signer)).unwrap();
        tool.call(args).await.unwrap();
        let req = rec.seen.lock().unwrap().clone();
        req.expect("signer received a request")
    }

    fn attest_args() -> Value {
        json!({
            "rootHashHex": "a".repeat(64),
            "releaseTarget": "v0.1.0",
            "releaseSubject": "covenant",
            "releaseScope": "audit",
            "recordedAt": 1_700_000_000u64,
        })
    }

    #[tokio::test]
    async fn attest_build_request_pins_asset_none_and_collection_precedence() {
        // config.collection empty + a caller collection arg: the arg is used,
        // and asset stays None (v1 mints fresh, never appends to a caller asset).
        let mut args = attest_args();
        args["collection"] = json!("ArgColl1111111111111111111111111111111111111");
        // A caller-supplied asset must be dropped: appending to an existing
        // asset is not wired, so a passed `asset` must never reach the request.
        args["asset"] = json!("CallerAsset111111111111111111111111111111111");
        match captured_request(&enabled_all(), "metaplex.attest.audit_root", args).await {
            SignerRequest::AttestAuditRoot {
                payload,
                asset,
                collection,
            } => {
                assert_eq!(
                    asset, None,
                    "a caller-supplied asset must never reach the request"
                );
                assert_eq!(
                    collection.as_deref(),
                    Some("ArgColl1111111111111111111111111111111111111")
                );
                assert_eq!(payload.root_hash_hex, "a".repeat(64));
            }
            other => panic!("unexpected request: {other:?}"),
        }

        // config.collection set: it overrides the caller's collection arg.
        let mut cfg = enabled_all();
        cfg.collection = "ConfigColl11111111111111111111111111111111".into();
        let mut args = attest_args();
        args["collection"] = json!("ArgColl1111111111111111111111111111111111111");
        match captured_request(&cfg, "metaplex.attest.audit_root", args).await {
            SignerRequest::AttestAuditRoot { collection, .. } => assert_eq!(
                collection.as_deref(),
                Some("ConfigColl11111111111111111111111111111111"),
                "operator config collection overrides the caller arg"
            ),
            other => panic!("unexpected request: {other:?}"),
        }

        // config empty + no arg: no collection pin, asset still None.
        match captured_request(&enabled_all(), "metaplex.attest.audit_root", attest_args()).await {
            SignerRequest::AttestAuditRoot {
                collection, asset, ..
            } => {
                assert_eq!(collection, None);
                assert_eq!(asset, None);
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[tokio::test]
    async fn attest_rejects_missing_or_non_integer_recorded_at() {
        // recordedAt must be an integer; build_request refuses before any
        // signer round-trip so a missing or non-integer timestamp never lands
        // on-chain. Value::as_u64 yields None for a JSON string or a float.
        let tool = metaplex_tool(
            &enabled_all(),
            "metaplex.attest.audit_root",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();

        let mut absent = attest_args();
        absent.as_object_mut().unwrap().remove("recordedAt");
        let mut as_string = attest_args();
        as_string["recordedAt"] = json!("1700000000");
        let mut as_float = attest_args();
        as_float["recordedAt"] = json!(1.5);

        for (label, args) in [
            ("absent", absent),
            ("string", as_string),
            ("float", as_float),
        ] {
            let err = tool
                .call(args)
                .await
                .expect_err("a non-integer recordedAt must be refused before signing");
            assert!(
                matches!(err, ToolError::InvalidArguments(ref m) if m.contains("recordedAt")),
                "{label}: got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn attest_rejects_an_empty_required_string_field() {
        // str_arg treats an empty string as missing, so an empty releaseTarget
        // is refused before it can be signed into an attestation's provenance
        // fields — a distinct branch from an absent key.
        let tool = metaplex_tool(
            &enabled_all(),
            "metaplex.attest.audit_root",
            Arc::new(NullDas),
            Some(Arc::new(CapturingSigner)),
        )
        .unwrap();
        let mut args = attest_args();
        args["releaseTarget"] = json!("");
        let err = tool
            .call(args)
            .await
            .expect_err("an empty releaseTarget must be rejected");
        assert!(
            matches!(err, ToolError::InvalidArguments(ref m) if m.contains("releaseTarget")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn identity_build_request_pins_asset_none_and_uri_fallback() {
        // A caller-supplied asset must be dropped here too.
        let args = json!({
            "agentLabel": "agent@local",
            "agentPubkey": "Agent11111111111111111111111111111111111111",
            "asset": "CallerAsset111111111111111111111111111111111",
        });
        match captured_request(&enabled_all(), "metaplex.identity.register", args).await {
            SignerRequest::RegisterIdentity {
                agent_label,
                agent_pubkey,
                asset,
                registration_uri,
            } => {
                assert_eq!(
                    asset, None,
                    "a caller-supplied asset must never reach the request"
                );
                assert_eq!(
                    registration_uri, None,
                    "an omitted uri stays None here; the covenant:// fallback is derived downstream, not an empty string"
                );
                assert_eq!(agent_label, "agent@local");
                assert_eq!(agent_pubkey, "Agent11111111111111111111111111111111111111");
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_asset_proof_dispatches_through_read_tool() {
        let cfg = enabled_reads();
        let tool = metaplex_tool(
            &cfg,
            "metaplex.das.get_asset_proof",
            Arc::new(EchoDas),
            None,
        )
        .unwrap();
        let res = tool.call(json!({ "id": "cnft-1" })).await.unwrap();
        assert!(!res.is_error);
        assert_eq!(res.content, vec![Content::json(json!({ "id": "cnft-1" }))]);
    }
}
