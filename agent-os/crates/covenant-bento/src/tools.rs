//! Bento MCP tools.
//!
//! `bento.screen` runs an intent through Bento's firewall and returns the
//! verdict; it fails closed (blocks) if the guard is unreachable or times out.
//! The result is the firewall's call, labeled as a third-party verdict, not a
//! Covenant decision. `bento.reputation` reads an agent's on-chain Bento
//! standing as a labeled soft signal; it stays layout-gated until Bento
//! publishes the account layout, and says so rather than guessing.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::config::BentoConfig;
use crate::guard::{gate_decision, is_strike, Guard, ScreenRequest, SubprocessGuard};
use crate::reader::BentoReader;
use crate::reputation::clean_signal;
use crate::types::GateOutcome;
use crate::BentoError;

pub const PROVIDER: &str = "bento";
pub const SCREEN_TOOL: &str = "bento.screen";
pub const REPUTATION_TOOL: &str = "bento.reputation";

/// Stamped on every screen result: this is Bento's firewall verdict, not a
/// Covenant decision.
pub const TRUST_LABEL_SCREEN: &str = "bento-screened (third-party firewall verdict)";
/// Stamped on every reputation result: a third-party on-chain signal to weigh
/// alongside Covenant's own audit-derived reputation, not a verified fact.
pub const TRUST_LABEL_REPUTATION: &str = "bento-attested (third-party on-chain), soft signal";

/// An intent is a short natural-language description. Bound it so an
/// agent-supplied string can't balloon the sidecar payload (and risk a pipe
/// stall on a large stdin write).
const MAX_INTENT_LEN: usize = 8 * 1024;

/// Build the Bento tool set from config. Empty unless enabled; each tool also
/// needs its own input (a guard binary to screen, a program id to read). The
/// guard is built here from config, so config is the single source of truth.
pub fn bento_tools(reader: Arc<BentoReader>, cfg: &BentoConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.screening_ready() {
        if let Some(guard) = SubprocessGuard::from_config(cfg) {
            tools.push(Arc::new(ScreenTool {
                guard: Arc::new(guard),
            }));
        }
    }
    if let Some(program_id) = &cfg.program_id {
        tools.push(Arc::new(ReputationTool {
            reader,
            program_id: program_id.clone(),
        }));
    }
    tools
}

struct ScreenTool {
    guard: Arc<dyn Guard>,
}

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str {
        SCREEN_TOOL
    }

    fn description(&self) -> &str {
        "Screen an intended action through Bento's pre-execution firewall. Returns Bento's \
         verdict (ALLOW, BLOCKED, or ESCALATED) with a risk score and reasoning. A third-party \
         firewall call, not a Covenant decision. Fails closed (blocks) if the guard is \
         unreachable or times out."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "description": "Natural-language description of the action to screen."
                },
                "agentAddress": {
                    "type": "string",
                    "description": "Optional base58 pubkey to screen as, overriding the guard's default."
                }
            },
            "required": ["intent"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let intent = arguments
            .get("intent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("missing non-empty \"intent\"".into()))?;
        if intent.len() > MAX_INTENT_LEN {
            return Err(ToolError::InvalidArguments(format!(
                "\"intent\" too long ({} > {MAX_INTENT_LEN} bytes)",
                intent.len()
            )));
        }

        let mut req = ScreenRequest::new(intent);
        if let Some(addr) = arguments.get("agentAddress").and_then(Value::as_str) {
            if !is_base58_pubkey(addr) {
                return Err(ToolError::InvalidArguments(
                    "\"agentAddress\" must be a base58 Solana address".into(),
                ));
            }
            req.agent_address = Some(addr.to_string());
        }

        match self.guard.screen(&req).await {
            Ok(r) => Ok(ToolCallResult::ok(vec![Content::json(json!({
                "provider": PROVIDER,
                "trust": TRUST_LABEL_SCREEN,
                "recommendation": r.recommendation,
                "gate": gate_str(gate_decision(&r)),
                "riskScore": r.risk_score,
                "strike": is_strike(&r),
                "reasoning": r.reasoning,
                "actionId": r.action_id,
                "approveUrl": r.approve_url,
                "blockUrl": r.block_url,
                "reviewUrl": r.review_url,
                "timestamp": r.timestamp,
            }))])),
            // Fail closed: an unreachable or slow firewall blocks the action.
            // This is Covenant's safety default, flagged as such, not Bento's
            // verdict. The internal error is logged for the operator but not
            // returned to the model.
            Err(e) => {
                tracing::warn!(error = %e, "bento screen failed, failing closed");
                Ok(ToolCallResult::ok(vec![Content::json(json!({
                    "provider": PROVIDER,
                    "trust": TRUST_LABEL_SCREEN,
                    "recommendation": "BLOCKED",
                    "gate": "block",
                    "failClosed": true,
                    "reasoning": "Bento guard unavailable; failing closed",
                }))]))
            }
        }
    }
}

struct ReputationTool {
    reader: Arc<BentoReader>,
    program_id: String,
}

#[async_trait]
impl Tool for ReputationTool {
    fn name(&self) -> &str {
        REPUTATION_TOOL
    }

    fn description(&self) -> &str {
        "Read an agent's on-chain Bento standing (strikes, lock state) as a labeled soft signal \
         to weigh alongside Covenant's own audit-derived reputation. Read-only, no funds move. \
         Returns a pending status until Bento publishes its on-chain account layout."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pubkey": { "type": "string", "description": "The agent's Solana pubkey (base58)." }
            },
            "required": ["pubkey"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let pubkey = arguments
            .get("pubkey")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("missing non-empty \"pubkey\"".into()))?;
        if !is_base58_pubkey(pubkey) {
            return Err(ToolError::InvalidArguments(
                "\"pubkey\" must be a base58 Solana address".into(),
            ));
        }

        match self.reader.standing(pubkey, &self.program_id).await {
            Ok(s) => {
                let sig = clean_signal(s);
                Ok(ToolCallResult::ok(vec![Content::json(json!({
                    "provider": PROVIDER,
                    "trust": TRUST_LABEL_REPUTATION,
                    "pubkey": pubkey,
                    "clean": sig.clean,
                    "strikes": sig.strikes,
                    "locked": sig.locked,
                    "standing": sig.standing,
                }))]))
            }
            // The agent has never interacted with Bento: a real answer (no
            // record), distinct from the layout gate below.
            Err(BentoError::NotFound) => Ok(ToolCallResult::ok(vec![Content::json(json!({
                "provider": PROVIDER,
                "trust": TRUST_LABEL_REPUTATION,
                "pubkey": pubkey,
                "status": "no-record",
                "reasoning": "Agent has no Bento record on chain",
            }))])),
            // Expected until Bento ships the layout: an honest pending status,
            // not a fabricated standing and not a hard error.
            Err(BentoError::LayoutPending) => Ok(ToolCallResult::ok(vec![Content::json(json!({
                "provider": PROVIDER,
                "trust": TRUST_LABEL_REPUTATION,
                "pubkey": pubkey,
                "status": "pending",
                "reasoning": "Bento on-chain account layout not yet published; reputation read is wired but inert",
            }))])),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

fn gate_str(outcome: GateOutcome) -> &'static str {
    match outcome {
        GateOutcome::Proceed => "proceed",
        GateOutcome::Block => "block",
        GateOutcome::Escalate => "escalate",
    }
}

/// A Solana address is base58 (Bitcoin alphabet, no `0` `O` `I` `l`) and
/// 32-44 chars. Input hygiene at the boundary: reject a malformed
/// agent-supplied string before it reaches a JSON-RPC param or the sidecar.
fn is_base58_pubkey(s: &str) -> bool {
    (32..=44).contains(&s.len())
        && s.bytes().all(|b| {
            matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AnalysisResult, Verdict};
    use crate::Result as BentoResult;

    struct StubGuard(AnalysisResult);
    #[async_trait]
    impl Guard for StubGuard {
        async fn screen(&self, _req: &ScreenRequest) -> BentoResult<AnalysisResult> {
            Ok(self.0.clone())
        }
    }

    struct FailingGuard;
    #[async_trait]
    impl Guard for FailingGuard {
        async fn screen(&self, _req: &ScreenRequest) -> BentoResult<AnalysisResult> {
            Err(BentoError::Sidecar("down".into()))
        }
    }

    fn verdict(recommendation: Verdict, risk_score: u8) -> AnalysisResult {
        AnalysisResult {
            recommendation,
            risk_score,
            reasoning: "because".into(),
            action_id: Some("act_1".into()),
            approve_url: None,
            block_url: None,
            review_url: None,
            timestamp: None,
        }
    }

    fn screen_tool(guard: impl Guard + 'static) -> ScreenTool {
        ScreenTool {
            guard: Arc::new(guard),
        }
    }

    fn body(res: &ToolCallResult) -> Value {
        match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        }
    }

    #[test]
    fn disabled_registers_nothing() {
        let reader = Arc::new(BentoReader::new("http://x"));
        assert!(bento_tools(reader, &BentoConfig::default()).is_empty());
    }

    #[test]
    fn registers_each_tool_behind_its_own_gate() {
        let reader = Arc::new(BentoReader::new("http://x"));
        let full = BentoConfig {
            enabled: true,
            protect_enabled: true,
            guard_binary: Some("/bin/true".into()),
            program_id: Some("Bento1111111111111111111111111111111111111".into()),
            ..Default::default()
        };
        let full_tools = bento_tools(reader.clone(), &full);
        let names: Vec<_> = full_tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&SCREEN_TOOL));
        assert!(names.contains(&REPUTATION_TOOL));

        // No guard binary means no screen tool even with protect_enabled.
        let no_guard = BentoConfig {
            guard_binary: None,
            ..full
        };
        let no_guard_tools = bento_tools(reader, &no_guard);
        let names: Vec<_> = no_guard_tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&SCREEN_TOOL));
        assert!(names.contains(&REPUTATION_TOOL));
    }

    #[tokio::test]
    async fn screen_returns_labeled_verdict() {
        let tool = screen_tool(StubGuard(verdict(Verdict::Escalated, 84)));
        let res = tool
            .call(json!({ "intent": "send 5 SOL to a fresh wallet" }))
            .await
            .unwrap();
        let b = body(&res);
        assert_eq!(b["recommendation"], "ESCALATED");
        assert_eq!(b["gate"], "escalate");
        assert_eq!(b["strike"], true);
        assert_eq!(b["trust"], TRUST_LABEL_SCREEN);
    }

    #[tokio::test]
    async fn screen_fails_closed_without_leaking_internals() {
        let tool = screen_tool(FailingGuard);
        let res = tool.call(json!({ "intent": "do a thing" })).await.unwrap();
        assert!(
            !res.is_error,
            "fail-closed is a structured block, not a tool error"
        );
        let b = body(&res);
        assert_eq!(b["recommendation"], "BLOCKED");
        assert_eq!(b["failClosed"], true);
        // The internal error string is not surfaced to the model.
        assert!(!b["reasoning"].as_str().unwrap().contains("down"));
    }

    #[tokio::test]
    async fn screen_rejects_blank_and_oversized_intent() {
        let tool = screen_tool(StubGuard(verdict(Verdict::Allow, 1)));
        assert!(matches!(
            tool.call(json!({ "intent": "   " })).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
        let huge = "x".repeat(MAX_INTENT_LEN + 1);
        assert!(matches!(
            tool.call(json!({ "intent": huge })).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn screen_rejects_bad_agent_address() {
        let tool = screen_tool(StubGuard(verdict(Verdict::Allow, 1)));
        let err = tool
            .call(json!({ "intent": "x", "agentAddress": "../../x" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn reputation_reports_pending_until_layout() {
        let reader = Arc::new(BentoReader::new("http://127.0.0.1:1"));
        let tool = ReputationTool {
            reader,
            program_id: "Bento1111111111111111111111111111111111111".into(),
        };
        let res = tool
            .call(json!({ "pubkey": "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg" }))
            .await
            .unwrap();
        assert_eq!(body(&res)["status"], "pending");
    }

    #[tokio::test]
    async fn reputation_requires_pubkey() {
        let reader = Arc::new(BentoReader::new("http://x"));
        let tool = ReputationTool {
            reader,
            program_id: "P".into(),
        };
        let err = tool.call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
