//! MCP tool surface for the ClawVille profile: the bounty-verification flow
//! exposed as capability-gated `clawville.*` tools.
//!
//! All four are pure compute — no keys, no network. They turn the typed
//! [`crate::bounty`] engine into a tool surface an agent (poster, worker, or
//! verifier) can drive:
//!
//! - `clawville.bounty.open`    — pin criteria → [`BountyOpened`]
//! - `clawville.bounty.scope`   — issue a worker a scoped [`BountyGrant`]
//! - `clawville.bounty.verify`  — grant + criteria + submission → [`Verdict`]
//! - `clawville.bounty.release` — verdict → [`ReleaseDecision`] (PayAI)
//!
//! [`clawville_specs`] lists enabled tools for `tools/list`;
//! [`clawville_tool`] resolves one by name for dispatch. Both honour
//! `config.enabled()` and the allowlist, mirroring `metaplex_specs` /
//! `metaplex_tool`.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{json, Value};

use crate::bounty::{
    verify, AcceptanceCriteria, BountyGrant, BountyOpened, ReleaseDecision, Submission, Verdict,
};
use crate::config::ClawvilleConfig;

pub const TOOL_PREFIX: &str = "clawville.";

const SLUGS: &[&str] = &[
    "bounty.open",
    "bounty.scope",
    "bounty.verify",
    "bounty.release",
];

fn static_slug(slug: &str) -> Option<&'static str> {
    SLUGS.iter().copied().find(|s| *s == slug)
}

fn description(slug: &str) -> &'static str {
    match slug {
        "bounty.open" => "Pin a bounty's acceptance criteria at open time, returning the criteria hash bound to the escrow.",
        "bounty.scope" => "Issue a worker agent a capability grant scoped to one bounty (the actions it may exercise).",
        "bounty.verify" => "Verify a worker's submission against the criteria and its hash-chained action-log evidence; returns a pass/fail verdict.",
        "bounty.release" => "Turn a verdict into a PayAI release decision (release_payment on pass, refund_buyer on fail). Names the instruction and signer role; never moves funds.",
        _ => "ClawVille bounty tool.",
    }
}

fn input_schema(slug: &str) -> Value {
    match slug {
        "bounty.open" => json!({
            "type": "object",
            "properties": {
                "bountyId": { "type": "string" },
                "poster": { "type": "string", "description": "Poster agent pubkey (base58)" },
                "escrowRef": { "type": "string", "description": "PayAI escrow contract reference (PDA / cid)" },
                "criteria": { "type": "object", "description": "AcceptanceCriteria: { criteria: [...] }" }
            },
            "required": ["bountyId", "poster", "escrowRef", "criteria"],
            "additionalProperties": false,
        }),
        "bounty.scope" => json!({
            "type": "object",
            "properties": {
                "bountyId": { "type": "string" },
                "worker": { "type": "string", "description": "Worker agent pubkey (base58)" },
                "allowedActions": { "type": "array", "items": { "type": "string" }, "description": "Action labels/namespaces the worker may exercise" },
                "expiresAtMs": { "type": "integer", "minimum": 0 }
            },
            "required": ["bountyId", "worker", "allowedActions"],
            "additionalProperties": false,
        }),
        "bounty.verify" => json!({
            "type": "object",
            "properties": {
                "grant": { "type": "object", "description": "BountyGrant from bounty.scope" },
                "criteria": { "type": "object", "description": "AcceptanceCriteria (same set pinned at open)" },
                "expectedCriteriaHash": { "type": "string", "description": "criteria_hash from bounty.open" },
                "submission": { "type": "object", "description": "Submission: { bountyId, worker, result, auditRoot, trail }" }
            },
            "required": ["grant", "criteria", "expectedCriteriaHash", "submission"],
            "additionalProperties": false,
        }),
        "bounty.release" => json!({
            "type": "object",
            "properties": { "verdict": { "type": "object", "description": "Verdict from bounty.verify" } },
            "required": ["verdict"],
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
pub fn clawville_specs(config: &ClawvilleConfig) -> Vec<ToolSpec> {
    if !config.enabled() {
        return Vec::new();
    }
    SLUGS
        .iter()
        .filter(|s| config.allows(s))
        .map(|s| spec_for(s))
        .collect()
}

/// Resolve a ClawVille tool by full name for dispatch. `None` when unknown
/// or disabled by config/allowlist.
pub fn clawville_tool(config: &ClawvilleConfig, name: &str) -> Option<Arc<dyn Tool>> {
    let slug = name.strip_prefix(TOOL_PREFIX).unwrap_or(name);
    if !config.enabled() || !config.allows(slug) {
        return None;
    }
    let slug = static_slug(slug)?;
    Some(Arc::new(BountyTool {
        slug,
        name: format!("{TOOL_PREFIX}{slug}"),
    }) as Arc<dyn Tool>)
}

struct BountyTool {
    slug: &'static str,
    name: String,
}

impl BountyTool {
    fn run(&self, args: Value) -> Result<Value, ToolError> {
        let bad = |e: String| ToolError::InvalidArguments(e);
        match self.slug {
            "bounty.open" => {
                let criteria: AcceptanceCriteria = field(&args, "criteria")?;
                let opened = BountyOpened::new(
                    str_arg(&args, "bountyId")?,
                    str_arg(&args, "poster")?,
                    str_arg(&args, "escrowRef")?,
                    &criteria,
                )
                .map_err(bad)?;
                Ok(to_value(&opened))
            }
            "bounty.scope" => {
                let actions: Vec<String> = field(&args, "allowedActions")?;
                let expires = args.get("expiresAtMs").and_then(Value::as_u64);
                let grant = BountyGrant::new(
                    str_arg(&args, "bountyId")?,
                    str_arg(&args, "worker")?,
                    actions,
                    expires,
                )
                .map_err(bad)?;
                Ok(to_value(&grant))
            }
            "bounty.verify" => {
                let grant: BountyGrant = field(&args, "grant")?;
                let criteria: AcceptanceCriteria = field(&args, "criteria")?;
                let expected = str_arg(&args, "expectedCriteriaHash")?;
                let submission: Submission = field(&args, "submission")?;
                let verdict = verify(&grant, &criteria, expected, &submission).map_err(bad)?;
                Ok(to_value(&verdict))
            }
            "bounty.release" => {
                let verdict: Verdict = field(&args, "verdict")?;
                Ok(to_value(&ReleaseDecision::from_verdict(&verdict)))
            }
            other => Err(ToolError::NotFound(format!("{TOOL_PREFIX}{other}"))),
        }
    }
}

#[async_trait]
impl Tool for BountyTool {
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
        match self.run(arguments) {
            Ok(v) => Ok(ToolCallResult::ok(vec![Content::json(v)])),
            Err(ToolError::InvalidArguments(m)) => Err(ToolError::InvalidArguments(m)),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} (non-empty string) is required")))
}

fn field<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, ToolError> {
    let v = args
        .get(key)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} is required")))?;
    serde_json::from_value(v.clone())
        .map_err(|e| ToolError::InvalidArguments(format!("{key}: {e}")))
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ClawvilleConfig {
        ClawvilleConfig {
            enabled: true,
            allow: None,
        }
    }

    const WORKER: &str = "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH";
    const POSTER: &str = "96GsGo69kVfPZffudCexfnsSi5EuhAyd278MuJPwzGdu";

    #[test]
    fn disabled_lists_nothing_enabled_lists_four() {
        assert!(clawville_specs(&ClawvilleConfig::default()).is_empty());
        assert_eq!(clawville_specs(&enabled()).len(), 4);
    }

    #[test]
    fn allowlist_filters_specs_and_dispatch() {
        let cfg = ClawvilleConfig {
            enabled: true,
            allow: Some(vec!["bounty.verify".into()]),
        };
        let names: Vec<_> = clawville_specs(&cfg)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, vec!["clawville.bounty.verify"]);
        assert!(clawville_tool(&cfg, "clawville.bounty.open").is_none());
        assert!(clawville_tool(&cfg, "clawville.bounty.verify").is_some());
    }

    #[tokio::test]
    async fn full_flow_open_scope_verify_release_over_tools() {
        let cfg = enabled();
        // open
        let open = clawville_tool(&cfg, "clawville.bounty.open").unwrap();
        let opened = open
            .call(json!({
                "bountyId": "b1", "poster": POSTER, "escrowRef": "escrow-cid-1",
                "criteria": { "criteria": [ { "kind": "result_contains", "needle": "done" } ] }
            }))
            .await
            .unwrap();
        let opened_json = match &opened.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!(),
        };
        let criteria_hash = opened_json["criteriaHash"].as_str().unwrap().to_string();
        assert_eq!(criteria_hash.len(), 64);

        // scope
        let scope = clawville_tool(&cfg, "clawville.bounty.scope").unwrap();
        let grant = scope
            .call(json!({ "bountyId": "b1", "worker": WORKER, "allowedActions": ["tool.call.fs"] }))
            .await
            .unwrap();
        let grant_json = match &grant.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!(),
        };

        // build a submission whose trail root we compute via the same engine
        let trail = json!({ "entries": [ { "seq": 0, "action": "tool.call.fs.read", "detailHash": "c".repeat(64) } ] });
        let t: crate::trail::AuditTrail = serde_json::from_value(trail.clone()).unwrap();
        let submission = json!({
            "bountyId": "b1", "worker": WORKER, "result": "task done",
            "auditRoot": t.root(), "trail": trail
        });

        // verify
        let verify_t = clawville_tool(&cfg, "clawville.bounty.verify").unwrap();
        let verdict = verify_t
            .call(json!({ "grant": grant_json, "criteria": { "criteria": [ { "kind": "result_contains", "needle": "done" } ] }, "expectedCriteriaHash": criteria_hash, "submission": submission }))
            .await
            .unwrap();
        let verdict_json = match &verdict.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!(),
        };
        assert_eq!(verdict_json["pass"], true);

        // release
        let release = clawville_tool(&cfg, "clawville.bounty.release").unwrap();
        let decision = release
            .call(json!({ "verdict": verdict_json }))
            .await
            .unwrap();
        let dj = match &decision.content[0] {
            Content::Json { value } => value.clone(),
            _ => panic!(),
        };
        assert_eq!(dj["decision"], "release");
        assert_eq!(dj["instruction"], "release_payment");
        assert_eq!(dj["signerRole"], "buyer");
    }

    #[tokio::test]
    async fn verify_rejects_bad_pubkey_in_submission() {
        let cfg = enabled();
        let t = clawville_tool(&cfg, "clawville.bounty.scope").unwrap();
        let err = t
            .call(json!({ "bountyId": "b1", "worker": "not-base58!!", "allowedActions": ["x"] }))
            .await
            .expect_err("bad worker pubkey");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
