//! Worker-driven SAID instructions. The worker owns the SDK and the
//! funding key; this module owns the gating and input validation.

use serde::{Deserialize, Serialize};

use crate::client::SaidBridge;
use crate::{worker, BridgeError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureResult {
    pub signature: String,
    #[serde(default)]
    pub slot: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterAgentInput {
    pub metadata_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterAgentResult {
    pub agent_pda: String,
    pub owner: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateWorkInput {
    pub agent: String,
    pub task_hash_hex: String,
    pub passed: bool,
    pub evidence_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationResult {
    pub validation_pda: String,
    pub validator: String,
    pub signature: String,
}

fn validate_hex32(label: &str, hex: &str) -> Result<()> {
    if hex.len() != 64 {
        return Err(BridgeError::Invalid(format!(
            "{label} must be 64 hex chars, got {}",
            hex.len()
        )));
    }
    if !hex
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(BridgeError::Invalid(format!(
            "{label} must be lowercase ASCII hex"
        )));
    }
    Ok(())
}

impl SaidBridge {
    pub async fn register_on_chain(
        &self,
        input: &RegisterAgentInput,
    ) -> Result<RegisterAgentResult> {
        self.require_paid("register_agent", self.config().paid.register)?;
        if input.metadata_uri.trim().is_empty() {
            return Err(BridgeError::Invalid(
                "metadata_uri must not be empty".into(),
            ));
        }
        worker::invoke(self.config(), "register-agent", input).await
    }

    pub async fn get_verified(&self) -> Result<SignatureResult> {
        self.require_paid("get_verified", self.config().paid.verify)?;
        worker::invoke(self.config(), "get-verified", &serde_json::json!({})).await
    }

    pub async fn validate_work(&self, input: &ValidateWorkInput) -> Result<ValidationResult> {
        self.require_paid("validate_work", self.config().paid.validate_work)?;
        validate_hex32("task_hash_hex", &input.task_hash_hex)?;
        if input.evidence_uri.trim().is_empty() {
            return Err(BridgeError::Invalid(
                "evidence_uri must not be empty".into(),
            ));
        }
        worker::invoke(self.config(), "validate-work", input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cluster, Config};

    fn bridge_with(paid: crate::config::PaidGates) -> SaidBridge {
        let mut cfg = Config::disabled(Cluster::Devnet);
        cfg.enabled = true;
        cfg.paid = paid;
        SaidBridge::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn register_requires_paid_gate() {
        let b = bridge_with(crate::config::PaidGates::default());
        let err = b
            .register_on_chain(&RegisterAgentInput {
                metadata_uri: "https://example.test/agent.json".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            BridgeError::PaidGateClosed {
                instruction: "register_agent"
            }
        ));
    }

    #[tokio::test]
    async fn validate_work_rejects_bad_hex() {
        let paid = crate::config::PaidGates {
            validate_work: true,
            ..Default::default()
        };
        let b = bridge_with(paid);
        let err = b
            .validate_work(&ValidateWorkInput {
                agent: "pda".into(),
                task_hash_hex: "ZZ".repeat(32),
                passed: true,
                evidence_uri: "https://x".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::Invalid(m) if m.contains("lowercase ASCII hex")));
    }
}
