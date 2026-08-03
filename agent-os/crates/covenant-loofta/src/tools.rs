//! Loofta MCP tools.
//!
//! One tool, `loofta.payout_check`: given a resolved recipient pubkey and a USD
//! amount, read the recipient's Covenant standing through the configured oracle,
//! run the payout policy, and return the verdict. Read-only, no funds move; it
//! previews the gate decision a payout would get. Minting and anchoring the
//! signed receipt is the daemon's step, not the tool's.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::config::LooftaConfig;
use crate::policy::{PayoutPolicy, Verdict};
use crate::recipient::RecipientOracle;

pub const PROVIDER: &str = "loofta";
pub const PAYOUT_CHECK_TOOL: &str = "loofta.payout_check";

/// The label stamped on every result: this is a gate decision, not a claim
/// about the recipient beyond their Covenant standing.
pub const TRUST_LABEL: &str = "covenant payout gate (pre-send decision)";

/// Build the Loofta tool set. Empty when disabled.
pub fn loofta_tools(
    oracle: Arc<dyn RecipientOracle>,
    policy: Arc<PayoutPolicy>,
    cfg: &LooftaConfig,
) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(PayoutCheckTool { oracle, policy }) as Arc<dyn Tool>]
}

struct PayoutCheckTool {
    oracle: Arc<dyn RecipientOracle>,
    policy: Arc<PayoutPolicy>,
}

#[async_trait]
impl Tool for PayoutCheckTool {
    fn name(&self) -> &str {
        PAYOUT_CHECK_TOOL
    }

    fn description(&self) -> &str {
        "Preview the Covenant payout gate for a Loofta transfer: given the resolved recipient \
         pubkey and a USD amount, return allow / refuse / needs_approval with the reason and the \
         recipient's Covenant standing. Read-only, no funds move."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "recipient": {
                    "type": "string",
                    "description": "Resolved destination Solana pubkey (base58)."
                },
                "amountUsd": {
                    "type": "number",
                    "description": "Payout amount normalized to USD."
                }
            },
            "required": ["recipient", "amountUsd"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let recipient = arguments
            .get("recipient")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("missing string \"recipient\"".into()))?;
        if !is_base58_pubkey(recipient) {
            return Err(ToolError::InvalidArguments(
                "\"recipient\" must be a base58 Solana address".into(),
            ));
        }
        let amount_usd = arguments
            .get("amountUsd")
            .and_then(Value::as_f64)
            .ok_or_else(|| ToolError::InvalidArguments("missing number \"amountUsd\"".into()))?;
        if !amount_usd.is_finite() || amount_usd < 0.0 {
            return Err(ToolError::InvalidArguments(
                "\"amountUsd\" must be a non-negative number".into(),
            ));
        }

        let standing = match self.oracle.standing(recipient).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(recipient, error = %e, "recipient standing read failed");
                return Ok(ToolCallResult::error(e.to_string()));
            }
        };

        let (decision, reason) = match self.policy.authorize(&standing, amount_usd) {
            Verdict::Allow => ("allow", None),
            Verdict::Refuse(r) => ("refuse", Some(r)),
            Verdict::NeedsApproval(r) => ("needs_approval", Some(r)),
        };

        Ok(ToolCallResult::ok(vec![Content::json(json!({
            "provider": PROVIDER,
            "trust": TRUST_LABEL,
            "recipient": recipient,
            "amountUsd": amount_usd,
            "decision": decision,
            "reason": reason,
            "standing": standing,
        }))]))
    }
}

/// A Solana address is base58 (Bitcoin alphabet, no `0` `O` `I` `l`) and 32-44
/// chars once encoded. Validate at the tool boundary so an agent-supplied string
/// can't smuggle a separator or traversal into a downstream lookup.
fn is_base58_pubkey(s: &str) -> bool {
    (32..=44).contains(&s.len())
        && s.bytes().all(|b| {
            matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipient::{RecipientStanding, StaticOracle};

    const RCPT: &str = "So11111111111111111111111111111111111111112";

    fn tools_for(standing: RecipientStanding) -> Vec<Arc<dyn Tool>> {
        let cfg = LooftaConfig { enabled: true };
        let oracle = Arc::new(StaticOracle(standing)) as Arc<dyn RecipientOracle>;
        loofta_tools(oracle, Arc::new(PayoutPolicy::conservative()), &cfg)
    }

    fn decision(res: &ToolCallResult) -> String {
        match &res.content[0] {
            Content::Json { value } => value["decision"].as_str().unwrap().to_string(),
            other => panic!("expected json, got {other:?}"),
        }
    }

    #[test]
    fn disabled_config_registers_no_tools() {
        let oracle = Arc::new(StaticOracle(RecipientStanding::unknown(RCPT, "t")))
            as Arc<dyn RecipientOracle>;
        let tools = loofta_tools(
            oracle,
            Arc::new(PayoutPolicy::conservative()),
            &LooftaConfig::default(),
        );
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn allows_established_clean_recipient() {
        let standing = RecipientStanding {
            recipient: RCPT.into(),
            score: 80,
            established: true,
            flagged: false,
            source: "test".into(),
        };
        let tools = tools_for(standing);
        let res = tools[0]
            .call(json!({ "recipient": RCPT, "amountUsd": 120.0 }))
            .await
            .unwrap();
        assert!(!res.is_error);
        assert_eq!(decision(&res), "allow");
    }

    #[tokio::test]
    async fn refuses_flagged_recipient() {
        let mut standing = RecipientStanding::unknown(RCPT, "test");
        standing.flagged = true;
        let tools = tools_for(standing);
        let res = tools[0]
            .call(json!({ "recipient": RCPT, "amountUsd": 5.0 }))
            .await
            .unwrap();
        assert_eq!(decision(&res), "refuse");
    }

    #[tokio::test]
    async fn escalates_large_unknown_payout() {
        let tools = tools_for(RecipientStanding::unknown(RCPT, "test"));
        let res = tools[0]
            .call(json!({ "recipient": RCPT, "amountUsd": 400.0 }))
            .await
            .unwrap();
        assert_eq!(decision(&res), "needs_approval");
    }

    #[tokio::test]
    async fn rejects_non_base58_recipient() {
        let tools = tools_for(RecipientStanding::unknown(RCPT, "test"));
        for bad in ["../../admin", "x?foo=bar", "short"] {
            let err = tools[0]
                .call(json!({ "recipient": bad, "amountUsd": 1.0 }))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidArguments(_)),
                "expected reject for {bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_negative_amount() {
        let tools = tools_for(RecipientStanding::unknown(RCPT, "test"));
        let err = tools[0]
            .call(json!({ "recipient": RCPT, "amountUsd": -1.0 }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
