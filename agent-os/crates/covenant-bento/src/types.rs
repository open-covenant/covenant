//! Wire types shared between the guard sidecar, the on-chain reader, and the
//! tools. The sidecar normalizes Bento's SDK output into [`AnalysisResult`],
//! so the field shape here is the contract we control on both ends.

use serde::{Deserialize, Serialize};

/// Bento's verdict for an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    Allow,
    Blocked,
    Escalated,
}

/// The result of `protect()`, as the guard sidecar emits it. The sidecar
/// normalizes Bento's SDK output to this exact shape, including clamping
/// `riskScore` to an integer in 0..=100.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResult {
    pub recommendation: Verdict,
    /// 0-100, where 100 is a critical threat. Bento records a strike above
    /// [`crate::STRIKE_RISK_THRESHOLD`].
    pub risk_score: u8,
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// What the caller should do with a verdict. A pure mapping of [`Verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    Proceed,
    Block,
    Escalate,
}

/// An agent's standing as decoded from Bento's on-chain accounts. The
/// reputation reader fills this once the account layout is known; the pure
/// [`crate::clean_signal`] derivation works against it today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BentoStanding {
    pub strikes: u8,
    pub locked: bool,
    #[serde(default)]
    pub actions_total: u32,
    #[serde(default)]
    pub blocked_total: u32,
    /// On-chain reputation, an EMA over verdict history, in basis points if
    /// decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reputation_ema_bps: Option<u32>,
}

/// The labeled soft signal the reputation reader produces from a standing.
/// Serialize-only: `trust` is a `'static` label, so this is never decoded.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReputationSignal {
    /// No strikes and not locked.
    pub clean: bool,
    pub strikes: u8,
    pub locked: bool,
    pub trust: &'static str,
    pub standing: BentoStanding,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verdict_serializes_uppercase() {
        assert_eq!(
            serde_json::to_value(Verdict::Blocked).unwrap(),
            json!("BLOCKED")
        );
        let v: Verdict = serde_json::from_value(json!("ESCALATED")).unwrap();
        assert_eq!(v, Verdict::Escalated);
    }

    #[test]
    fn analysis_result_reads_camel_case_and_omits_empties() {
        let raw = json!({
            "recommendation": "ALLOW",
            "riskScore": 12,
            "reasoning": "routine transfer",
            "actionId": "act_123"
        });
        let r: AnalysisResult = serde_json::from_value(raw).unwrap();
        assert_eq!(r.recommendation, Verdict::Allow);
        assert_eq!(r.risk_score, 12);
        assert_eq!(r.action_id.as_deref(), Some("act_123"));
        assert!(r.review_url.is_none());
        // Empty optionals are not serialized back out.
        let out = serde_json::to_value(&r).unwrap();
        assert!(out.get("reviewUrl").is_none());
        assert_eq!(out["riskScore"], 12);
    }
}
