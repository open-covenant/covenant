use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SolanaEventRecord {
    pub chain: String,
    pub cluster: String,
    pub program_id: String,
    pub slot: u64,
    pub signature: String,
    pub event_name: String,
    pub payload: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEnvelope {
    pub cluster: String,
    pub program_id: String,
    pub slot: u64,
    pub signature: String,
    pub event_name: String,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerSnapshot {
    pub chain: String,
    pub cluster: String,
    pub latest_slot: u64,
    pub indexed_events: usize,
    /// `"fixture"` while seeded; `"live"` once the real subscriber lands.
    pub mode: String,
}

fn normalize_base58(value: &str, min_len: usize, max_len: usize, label: &str) -> Result<String> {
    let trimmed = value.trim();
    if !(min_len..=max_len).contains(&trimmed.len()) {
        return Err(anyhow!("{label} length out of range"));
    }
    if !trimmed.chars().all(
        |ch| matches!(ch, '1'..='9' | 'A'..='H' | 'J'..='N' | 'P'..='Z' | 'a'..='k' | 'm'..='z'),
    ) {
        return Err(anyhow!("{label} must be base58"));
    }
    Ok(trimmed.to_string())
}

fn normalize_solana_address(value: &str) -> Result<String> {
    normalize_base58(value, 32, 44, "Solana address")
}

fn normalize_solana_signature(value: &str) -> Result<String> {
    normalize_base58(value, 64, 88, "Solana signature")
}

pub fn normalize_event(input: LogEnvelope) -> Result<SolanaEventRecord> {
    Ok(SolanaEventRecord {
        chain: "solana".to_string(),
        cluster: input.cluster,
        program_id: normalize_solana_address(&input.program_id)?,
        slot: input.slot,
        signature: normalize_solana_signature(&input.signature)?,
        event_name: input.event_name,
        payload: input.payload,
    })
}

/// Hardcoded sample events surfaced while the service is in fixture mode.
/// These are NOT observed from Solana — they exist so downstream consumers
/// (web console, SDK demos, integration fixtures) can exercise the wire
/// shape before a real `programSubscribe` consumer is wired. See
/// README.md "Path to live indexing" for the migration plan.
pub fn seed_events(cluster: &str, program_id: &str) -> Vec<SolanaEventRecord> {
    let samples = vec![
        LogEnvelope {
            cluster: cluster.to_string(),
            program_id: program_id.to_string(),
            slot: 12_345_678,
            signature: "5uA7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y9u1111"
                .to_string(),
            event_name: "AgentRegistered".to_string(),
            payload: Map::from_iter([
                (
                    "agent_key".to_string(),
                    Value::String("agent-alpha".to_string()),
                ),
                (
                    "operator".to_string(),
                    Value::String("Alpha111111111111111111111111111111111111".to_string()),
                ),
            ]),
        },
        LogEnvelope {
            cluster: cluster.to_string(),
            program_id: program_id.to_string(),
            slot: 12_345_700,
            signature: "3mQ7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y111111"
                .to_string(),
            event_name: "ReceiptBatchAnchored".to_string(),
            payload: Map::from_iter([
                ("receipt_count".to_string(), Value::Number(2.into())),
                (
                    "amount_covnt".to_string(),
                    Value::String("125000000".to_string()),
                ),
            ]),
        },
    ];

    samples
        .into_iter()
        .map(normalize_event)
        .collect::<Result<Vec<_>>>()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{normalize_event, LogEnvelope};
    use serde_json::{Map, Value};

    #[test]
    fn normalizes_solana_event_fields() {
        let event = normalize_event(LogEnvelope {
            cluster: "devnet".to_string(),
            program_id: "CovntSettLement1111111111111111111111111111".to_string(),
            slot: 42,
            signature: "5uA7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y9u1111"
                .to_string(),
            event_name: "TaskReleased".to_string(),
            payload: Map::from_iter([(
                "amount_covnt".to_string(),
                Value::String("100".to_string()),
            )]),
        })
        .expect("event should normalize");

        assert_eq!(event.chain, "solana");
        assert_eq!(event.cluster, "devnet");
        assert_eq!(event.slot, 42);
        assert_eq!(event.payload["amount_covnt"], "100");
    }
}
