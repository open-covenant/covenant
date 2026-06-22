//! MCP tool surface for the SNS profile.
//!
//! Three read tools, all named `sns.*` and gated by the daemon's
//! `tool.call.sns.*` capabilities. No keys, no spend. [`sns_specs`] lists the
//! enabled tools for `tools/list`; [`sns_tool`] resolves one by name for
//! dispatch. Mirrors the Metaplex profile's `metaplex_specs` / `metaplex_tool`.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{json, Value};

use crate::config::SnsConfig;
use crate::resolve::SnsResolver;

pub const TOOL_PREFIX: &str = "sns.";

const READ_SLUGS: &[&str] = &["resolve", "reverse", "record"];

fn slug_of(name: &str) -> &str {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name)
}

fn static_slug(slug: &str) -> Option<&'static str> {
    READ_SLUGS.iter().copied().find(|s| *s == slug)
}

fn description(slug: &str) -> &'static str {
    match slug {
        "resolve" => "Resolve a .sol name to its owner wallet via SNS (Solana Name Service).",
        "reverse" => "List the .sol domains a wallet owns via SNS.",
        "record" => "Read a typed SNS record (url, ipfs, email, ...) from a .sol domain.",
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
    specs
}

/// Resolve an SNS tool by full name for dispatch. `None` when the tool is
/// unknown or disabled by config/allowlist.
pub fn sns_tool(
    config: &SnsConfig,
    name: &str,
    resolver: Arc<dyn SnsResolver>,
) -> Option<Arc<dyn Tool>> {
    let slug = slug_of(name);
    if !config.reads_enabled() || !config.allows(slug) {
        return None;
    }
    let slug = static_slug(slug)?;
    Some(Arc::new(ReadTool {
        slug,
        name: format!("{TOOL_PREFIX}{slug}"),
        resolver,
    }) as Arc<dyn Tool>)
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
        assert!(sns_tool(&cfg, "sns.resolve", Arc::new(StubResolver)).is_none());
    }

    #[test]
    fn allowlist_filters_specs_and_dispatch() {
        let cfg = SnsConfig {
            enabled: true,
            allow: Some(vec!["resolve".into()]),
            ..Default::default()
        };
        assert_eq!(sns_specs(&cfg).len(), 1);
        assert!(sns_tool(&cfg, "sns.resolve", Arc::new(StubResolver)).is_some());
        assert!(sns_tool(&cfg, "sns.reverse", Arc::new(StubResolver)).is_none());
    }

    #[tokio::test]
    async fn resolve_tool_requires_name_then_returns_owner() {
        let tool = sns_tool(&enabled(), "sns.resolve", Arc::new(StubResolver)).unwrap();
        let err = tool.call(json!({})).await.expect_err("missing name");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        let ok = tool.call(json!({ "name": "bonfida.sol" })).await.unwrap();
        assert!(!ok.is_error);
    }

    #[tokio::test]
    async fn record_tool_requires_both_args() {
        let tool = sns_tool(&enabled(), "sns.record", Arc::new(StubResolver)).unwrap();
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
}
