//! MCP tool surface for the SNS profile.
//!
//! Read tools (`resolve`/`reverse`/`record`, keyless) and write tools
//! (`register_subdomain`/`set_record`, behind the signer sidecar), all named
//! `sns.*` and gated by the daemon's `tool.call.sns.*` capabilities.
//! [`sns_specs`] lists the enabled tools for `tools/list`; [`sns_tool`]
//! resolves one by name for dispatch. Mirrors the Metaplex profile's
//! `metaplex_specs` / `metaplex_tool`.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{json, Value};

use crate::config::SnsConfig;
use crate::resolve::SnsResolver;
use crate::sign::{validate_label, SignerRequest, SnsSigner};

pub const TOOL_PREFIX: &str = "sns.";

const READ_SLUGS: &[&str] = &["resolve", "reverse", "record"];
const WRITE_SLUGS: &[&str] = &["register_subdomain", "set_record"];

fn slug_of(name: &str) -> &str {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name)
}

fn static_read_slug(slug: &str) -> Option<&'static str> {
    READ_SLUGS.iter().copied().find(|s| *s == slug)
}

fn static_write_slug(slug: &str) -> Option<&'static str> {
    WRITE_SLUGS.iter().copied().find(|s| *s == slug)
}

fn description(slug: &str) -> &'static str {
    match slug {
        "resolve" => "Resolve a .sol name to its owner wallet via SNS (Solana Name Service).",
        "reverse" => "List the .sol domains a wallet owns via SNS.",
        "record" => "Read a typed SNS record (url, ipfs, email, ...) from a .sol domain.",
        "register_subdomain" => {
            "Issue a subdomain under Covenant's parent .sol, bound to an agent wallet."
        }
        "set_record" => "Write a typed SNS record on a .sol domain Covenant controls.",
        _ => "SNS tool.",
    }
}

fn input_schema(slug: &str) -> Value {
    match slug {
        "resolve" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "A .sol name, with or without the .sol suffix" }
            },
            "required": ["name"],
            "additionalProperties": false,
        }),
        "reverse" => json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Owner wallet address (base58)" }
            },
            "required": ["owner"],
            "additionalProperties": false,
        }),
        "record" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "A .sol name, with or without the .sol suffix" },
                "record": { "type": "string", "description": "Record key, e.g. url, ipfs, email, github" }
            },
            "required": ["name", "record"],
            "additionalProperties": false,
        }),
        "register_subdomain" => json!({
            "type": "object",
            "properties": {
                "subdomain": { "type": "string", "description": "The label to issue (e.g. \"foundation\" for foundation.covenant.sol)" },
                "owner": { "type": "string", "description": "Agent wallet (base58) the subdomain is bound to" }
            },
            "required": ["subdomain", "owner"],
            "additionalProperties": false,
        }),
        "set_record" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "A .sol name Covenant controls, with or without the .sol suffix" },
                "record": { "type": "string", "description": "Record key, e.g. url, ipfs, github" },
                "value": { "type": "string", "description": "Record value to write" }
            },
            "required": ["name", "record", "value"],
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
pub fn sns_specs(config: &SnsConfig) -> Vec<ToolSpec> {
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

/// Resolve an SNS tool by full name for dispatch. `None` when the tool is
/// unknown, disabled by config/allowlist, or a write tool with no signer wired.
pub fn sns_tool(
    config: &SnsConfig,
    name: &str,
    resolver: Arc<dyn SnsResolver>,
    signer: Option<Arc<dyn SnsSigner>>,
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
            resolver,
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
            parent_domain: config.parent_domain.clone(),
            signer,
        }) as Arc<dyn Tool>);
    }
    None
}

struct ReadTool {
    slug: &'static str,
    name: String,
    resolver: Arc<dyn SnsResolver>,
}

impl ReadTool {
    async fn run(&self, args: Value) -> Result<Value, ToolError> {
        match self.slug {
            "resolve" => {
                let name = str_arg(&args, "name")?;
                let owner = self.resolver.resolve(name).await.map_err(sns_err)?;
                Ok(json!({ "name": name, "owner": owner }))
            }
            "reverse" => {
                let owner = str_arg(&args, "owner")?;
                let domains = self.resolver.reverse(owner).await.map_err(sns_err)?;
                Ok(json!({ "owner": owner, "domains": domains }))
            }
            "record" => {
                let name = str_arg(&args, "name")?;
                let record = str_arg(&args, "record")?;
                let value = self.resolver.record(name, record).await.map_err(sns_err)?;
                Ok(json!({ "name": name, "record": record, "value": value }))
            }
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

struct WriteTool {
    slug: &'static str,
    name: String,
    parent_domain: String,
    signer: Arc<dyn SnsSigner>,
}

impl WriteTool {
    fn build_request(&self, args: &Value) -> Result<SignerRequest, ToolError> {
        match self.slug {
            "register_subdomain" => {
                if self.parent_domain.is_empty() {
                    return Err(ToolError::InvalidArguments(
                        "subdomain issuance needs COVENANT_SNS_PARENT_DOMAIN".into(),
                    ));
                }
                let subdomain = str_arg(args, "subdomain")?;
                validate_label(subdomain).map_err(ToolError::InvalidArguments)?;
                Ok(SignerRequest::RegisterSubdomain {
                    parent: self.parent_domain.clone(),
                    subdomain: subdomain.to_string(),
                    owner: str_arg(args, "owner")?.to_string(),
                })
            }
            "set_record" => Ok(SignerRequest::SetRecord {
                domain: crate::normalize_domain(str_arg(args, "name")?).to_string(),
                record: str_arg(args, "record")?.to_string(),
                value: str_arg(args, "value")?.to_string(),
            }),
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

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} (non-empty string) is required")))
}

fn sns_err(e: crate::resolve::SnsError) -> ToolError {
    ToolError::Failed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{ResolvedDomain, SnsError};
    use crate::sign::SignerResponse;

    fn enabled() -> SnsConfig {
        SnsConfig {
            enabled: true,
            ..Default::default()
        }
    }

    struct StubResolver;
    #[async_trait]
    impl SnsResolver for StubResolver {
        async fn resolve(&self, name: &str) -> Result<String, SnsError> {
            assert_eq!(name, "bonfida.sol");
            Ok("OwnerWallet".into())
        }
        async fn reverse(&self, _owner: &str) -> Result<Vec<ResolvedDomain>, SnsError> {
            Ok(vec![ResolvedDomain {
                domain: "bonfida.sol".into(),
                key: "Key1".into(),
            }])
        }
        async fn record(&self, _name: &str, _record: &str) -> Result<String, SnsError> {
            Ok("https://opencovenant.org".into())
        }
    }

    fn enabled_writes() -> SnsConfig {
        SnsConfig {
            enabled: true,
            rpc_url: "https://api.devnet.solana.com".into(),
            signer_binary: "/bin/covenant-sns-signer".into(),
            parent_domain: "covenant.sol".into(),
            ..Default::default()
        }
    }

    struct StubSigner;
    #[async_trait]
    impl SnsSigner for StubSigner {
        async fn sign(&self, _r: SignerRequest) -> Result<SignerResponse, String> {
            Ok(SignerResponse {
                signature: "sig".into(),
                domain: "foundation.covenant.sol".into(),
                cluster: "devnet".into(),
            })
        }
    }

    #[test]
    fn enabled_config_lists_three_read_tools() {
        let specs = sns_specs(&enabled());
        let names: Vec<_> = specs.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"sns.resolve".to_string()));
        assert!(names.contains(&"sns.reverse".to_string()));
        assert!(names.contains(&"sns.record".to_string()));
    }

    #[test]
    fn disabled_config_lists_nothing_and_resolves_nothing() {
        let cfg = SnsConfig::default();
        assert!(sns_specs(&cfg).is_empty());
        assert!(sns_tool(&cfg, "sns.resolve", Arc::new(StubResolver), None).is_none());
    }

    #[test]
    fn allowlist_filters_specs_and_dispatch() {
        let cfg = SnsConfig {
            enabled: true,
            allow: Some(vec!["resolve".into()]),
            ..Default::default()
        };
        assert_eq!(sns_specs(&cfg).len(), 1);
        assert!(sns_tool(&cfg, "sns.resolve", Arc::new(StubResolver), None).is_some());
        assert!(sns_tool(&cfg, "sns.reverse", Arc::new(StubResolver), None).is_none());
    }

    #[test]
    fn writes_enabled_config_lists_read_and_write_tools() {
        let specs = sns_specs(&enabled_writes());
        let names: Vec<_> = specs.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"sns.register_subdomain".to_string()));
        assert!(names.contains(&"sns.set_record".to_string()));
    }

    #[test]
    fn write_tool_unavailable_without_signer() {
        let cfg = enabled_writes();
        assert!(
            sns_tool(&cfg, "sns.register_subdomain", Arc::new(StubResolver), None).is_none(),
            "write tool must be None without a signer"
        );
    }

    #[tokio::test]
    async fn resolve_tool_requires_name_then_returns_owner() {
        let tool = sns_tool(&enabled(), "sns.resolve", Arc::new(StubResolver), None).unwrap();
        let err = tool.call(json!({})).await.expect_err("missing name");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        let ok = tool.call(json!({ "name": "bonfida.sol" })).await.unwrap();
        assert!(!ok.is_error);
    }

    #[tokio::test]
    async fn record_tool_requires_both_args() {
        let tool = sns_tool(&enabled(), "sns.record", Arc::new(StubResolver), None).unwrap();
        let err = tool
            .call(json!({ "name": "bonfida.sol" }))
            .await
            .expect_err("missing record");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        let ok = tool
            .call(json!({ "name": "bonfida.sol", "record": "url" }))
            .await
            .unwrap();
        assert!(!ok.is_error);
    }

    #[tokio::test]
    async fn register_subdomain_validates_then_signs() {
        let tool = sns_tool(
            &enabled_writes(),
            "sns.register_subdomain",
            Arc::new(StubResolver),
            Some(Arc::new(StubSigner)),
        )
        .unwrap();
        let err = tool
            .call(json!({ "subdomain": "a.b", "owner": "W" }))
            .await
            .expect_err("dotted label");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        let ok = tool
            .call(json!({ "subdomain": "foundation", "owner": "OwnerWallet" }))
            .await
            .unwrap();
        assert!(!ok.is_error);
    }
}
