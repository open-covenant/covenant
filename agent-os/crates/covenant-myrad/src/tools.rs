//! The buyer's side, as an MCP tool.
//!
//! `myrad.verify_signal` takes the two things a buyer holds after a purchase,
//! the delivered artifact and its receipt, and answers the only question that
//! matters: is this the artifact this receipt was issued over, was the receipt
//! signed by the key it names, and what did the checks say. It reads nothing off
//! the network and needs no key, so an agent can run it on data it already has.
//!
//! Pinning the attestor key is the caller's job and cannot be delegated to this
//! tool: a receipt that carries its own key is self-consistent by construction.
//! Pass `expectedAttestorPubkeyB64` from a channel the buyer already trusts and
//! the tool enforces it. Omit it and the result says so rather than implying a
//! check that did not happen.
//!
//! The daemon does not register these tools yet; the connector is at proposal
//! stage and nothing depends on it.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::config::MyradConfig;
use crate::integrity::Status;
use crate::receipt::SignedReceipt;

pub const PROVIDER: &str = "myrad";
pub const VERIFY_SIGNAL_TOOL: &str = "myrad.verify_signal";

/// Stamped on every result, so a caller cannot read it as a claim about the
/// source account. That is what Myrad's Reclaim proofs speak to.
pub const TRUST_LABEL: &str =
    "covenant signal receipt (provenance of the aggregation step, not of the source account)";

pub fn myrad_tools(cfg: &MyradConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![Arc::new(VerifySignalTool) as Arc<dyn Tool>]
}

struct VerifySignalTool;

#[async_trait]
impl Tool for VerifySignalTool {
    fn name(&self) -> &str {
        VERIFY_SIGNAL_TOOL
    }

    fn description(&self) -> &str {
        "Verify a Covenant signal receipt against the Myrad artifact it was issued over: checks the \
         attestor signature, that the delivered bytes are the ones the receipt covers, and returns \
         the contributor count, evidence root, and every integrity finding. Offline and read-only."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "receipt": {
                    "type": "object",
                    "description": "The signed receipt (covenant.myrad.signal.v1) as delivered."
                },
                "delivered": {
                    "description": "The artifact the receipt should cover, exactly as received."
                },
                "expectedAttestorPubkeyB64": {
                    "type": "string",
                    "description": "Attestor key pinned through a trusted channel. When set, a receipt signed by any other key is rejected."
                }
            },
            "required": ["receipt", "delivered"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let receipt_value = arguments
            .get("receipt")
            .ok_or_else(|| ToolError::InvalidArguments("missing \"receipt\"".into()))?;
        let delivered = arguments
            .get("delivered")
            .ok_or_else(|| ToolError::InvalidArguments("missing \"delivered\"".into()))?;

        let signed: SignedReceipt = serde_json::from_value(receipt_value.clone()).map_err(|e| {
            ToolError::InvalidArguments(format!("receipt is not a {}: {e}", crate::receipt::SCHEMA))
        })?;

        let signature_ok = signed.verify();
        let covers = signed.receipt.covers(delivered);

        // A pin that isn't a string is a caller mistake, not an absent pin.
        // Reading it as absent would turn a typo into a silently unenforced
        // check, which is the whole defense.
        let pin = arguments.get("expectedAttestorPubkeyB64");
        let pin_supplied = !matches!(pin, None | Some(Value::Null));
        let attestor_ok = match pin {
            None | Some(Value::Null) => true,
            Some(Value::String(pinned)) => *pinned == signed.attestor_pubkey_b64,
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "expectedAttestorPubkeyB64 must be a string, got {other}"
                )))
            }
        };

        let integrity = &signed.receipt.integrity;
        let blocking: Vec<&crate::integrity::Finding> = integrity
            .findings
            .iter()
            .filter(|f| f.status == Status::Fail)
            .collect();
        let warnings: Vec<&crate::integrity::Finding> = integrity
            .findings
            .iter()
            .filter(|f| f.status == Status::Warn)
            .collect();

        let verified = signature_ok && covers && attestor_ok;
        let summary = if !attestor_ok {
            "receipt is signed by a key other than the one pinned"
        } else if !signature_ok {
            "receipt signature does not verify"
        } else if !covers {
            "receipt does not cover the delivered artifact"
        } else if !pin_supplied {
            "receipt is internally consistent; no attestor key was pinned, so the issuer is unchecked"
        } else if !blocking.is_empty() {
            "receipt verifies; the cohort failed one or more integrity checks"
        } else if !warnings.is_empty() {
            "receipt verifies with warnings"
        } else {
            "receipt verifies and every integrity check passed"
        };

        let body = json!({
            "trust_label": TRUST_LABEL,
            "verified": verified,
            "summary": summary,
            "signature_ok": signature_ok,
            "covers_delivered_artifact": covers,
            "attestor_pubkey_b64": signed.attestor_pubkey_b64,
            "attestor_pin_supplied": pin_supplied,
            "attestor_matches_pin": attestor_ok,
            // Outside the signed bytes by design, so it carries no
            // authentication of its own.
            "anchor_unauthenticated": signed.anchor,
            "signal": {
                "provider": signed.receipt.signal.provider,
                "cohort_id": signed.receipt.signal.cohort_id,
                "delivered_sha256": signed.receipt.signal.delivered_sha256,
            },
            "evidence": {
                "contributors": signed.receipt.evidence.contributors,
                "merkle_root": signed.receipt.evidence.merkle_root,
            },
            "freshness": signed.receipt.freshness,
            "integrity": {
                "status": integrity.status,
                "policy": integrity.policy,
                "failed": blocking,
                "warnings": warnings,
            },
        });

        Ok(ToolCallResult {
            content: vec![Content::text(
                serde_json::to_string_pretty(&body).unwrap_or_else(|_| summary.to_string()),
            )],
            is_error: !verified,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrity::IntegrityPolicy;
    use crate::receipt::SignalReceipt;
    use crate::signal::SignalRecord;
    use ed25519_dalek::SigningKey;

    fn cohort(n: usize) -> Vec<SignalRecord> {
        (0..n)
            .map(|i| {
                let value = json!({
                    "provider": "netflix",
                    "reclaim_proof_id": format!("{:024x}", i + 1),
                    "verification_status": "verified",
                    "signal": {
                        "dataset_id": "myrad_netflix_v1",
                        "generated_at": "2026-05-19T17:40:20.697Z",
                        "metadata": { "privacy_compliance": { "cohort_id": "netflix_drama", "pii_stripped": true } },
                        "user_profile": { "total_titles_watched": 40 },
                        "viewing_summary": { "data_window_days": 90, "total_titles_watched": 40 },
                        "viewing_behavior": { "monthly_pattern": { "2026-05": 3 } }
                    }
                });
                SignalRecord::from_sample(&value, Some(format!("subject-{i}")), false).unwrap()
            })
            .collect()
    }

    fn signed_pair(n: usize) -> (Value, Value) {
        let delivered = json!({ "cohort_id": "netflix_drama", "contributors": n });
        let signed = SignalReceipt::build(&delivered, &cohort(n), IntegrityPolicy::default())
            .unwrap()
            .attest(&SigningKey::from_bytes(&[5u8; 32]))
            .unwrap();
        (serde_json::to_value(signed).unwrap(), delivered)
    }

    fn body(result: &ToolCallResult) -> Value {
        let Content::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn disabled_registers_nothing() {
        assert!(myrad_tools(&MyradConfig::default()).is_empty());
        assert_eq!(myrad_tools(&MyradConfig { enabled: true }).len(), 1);
    }

    #[tokio::test]
    async fn verifies_a_good_receipt() {
        let (receipt, delivered) = signed_pair(5);
        let result = VerifySignalTool
            .call(json!({ "receipt": receipt, "delivered": delivered }))
            .await
            .unwrap();
        assert!(!result.is_error);
        let body = body(&result);
        assert_eq!(body["verified"], json!(true));
        assert_eq!(body["evidence"]["contributors"], json!(5));
    }

    #[tokio::test]
    async fn rejects_an_artifact_the_receipt_does_not_cover() {
        let (receipt, _) = signed_pair(5);
        let result = VerifySignalTool
            .call(json!({ "receipt": receipt, "delivered": { "cohort_id": "netflix_drama", "contributors": 500 } }))
            .await
            .unwrap();
        assert!(result.is_error);
        let body = body(&result);
        assert_eq!(body["covers_delivered_artifact"], json!(false));
        assert_eq!(body["signature_ok"], json!(true));
    }

    #[tokio::test]
    async fn rejects_a_receipt_signed_by_an_unpinned_key() {
        let (receipt, delivered) = signed_pair(5);
        let result = VerifySignalTool
            .call(json!({
                "receipt": receipt,
                "delivered": delivered,
                "expectedAttestorPubkeyB64": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            }))
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(body(&result)["attestor_matches_pin"], json!(false));
    }

    #[tokio::test]
    async fn surfaces_a_failing_cohort_without_hiding_the_signature() {
        let (receipt, delivered) = signed_pair(2);
        let result = VerifySignalTool
            .call(json!({ "receipt": receipt, "delivered": delivered }))
            .await
            .unwrap();
        let body = body(&result);
        assert_eq!(body["verified"], json!(true));
        assert_eq!(body["integrity"]["status"], json!("fail"));
        assert!(!body["integrity"]["failed"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pin_that_is_not_a_string_is_an_error_rather_than_no_pin() {
        let (receipt, delivered) = signed_pair(5);
        for bad in [json!(0), json!(["k"]), json!({ "k": 1 }), json!(true)] {
            let err = VerifySignalTool
                .call(json!({
                    "receipt": receipt,
                    "delivered": delivered,
                    "expectedAttestorPubkeyB64": bad
                }))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidArguments(_)),
                "{bad} was accepted"
            );
        }
    }

    #[tokio::test]
    async fn an_omitted_pin_is_reported_rather_than_implied() {
        let (receipt, delivered) = signed_pair(5);
        let result = VerifySignalTool
            .call(json!({ "receipt": receipt, "delivered": delivered }))
            .await
            .unwrap();
        let body = body(&result);
        assert_eq!(body["attestor_pin_supplied"], json!(false));
        assert!(
            body["summary"]
                .as_str()
                .unwrap()
                .contains("no attestor key was pinned"),
            "{}",
            body["summary"]
        );
    }

    #[tokio::test]
    async fn a_malformed_receipt_is_an_argument_error() {
        let err = VerifySignalTool
            .call(json!({ "receipt": { "nope": true }, "delivered": {} }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
