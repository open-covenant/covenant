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
    use super::{normalize_event, seed_events, LogEnvelope};
    use serde_json::Map;

    // '1' is a valid base58 char; lengths here match the 32..44 address and
    // 64..88 signature ranges so the surrounding tests vary exactly one axis.
    fn valid_addr() -> String {
        "1".repeat(44)
    }
    fn valid_sig() -> String {
        "1".repeat(64)
    }
    fn envelope(program_id: &str, signature: &str) -> LogEnvelope {
        LogEnvelope {
            cluster: "devnet".to_string(),
            program_id: program_id.to_string(),
            slot: 42,
            signature: signature.to_string(),
            event_name: "TaskReleased".to_string(),
            payload: Map::new(),
        }
    }

    #[test]
    fn normalizes_solana_event_fields() {
        let event =
            normalize_event(envelope(&valid_addr(), &valid_sig())).expect("event should normalize");

        assert_eq!(event.chain, "solana");
        assert_eq!(event.cluster, "devnet");
        assert_eq!(event.slot, 42);
        assert_eq!(event.program_id, valid_addr());
    }

    #[test]
    fn rejects_program_id_below_min_length() {
        let err = normalize_event(envelope(&"1".repeat(31), &valid_sig()))
            .expect_err("31-char program id must be rejected");
        assert!(
            err.to_string().contains("length out of range"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn rejects_program_id_above_max_length() {
        let err = normalize_event(envelope(&"1".repeat(45), &valid_sig()))
            .expect_err("45-char program id must be rejected");
        assert!(
            err.to_string().contains("length out of range"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn rejects_non_base58_program_id() {
        // '0' is outside the base58 alphabet (excludes 0/O/I/l).
        let err = normalize_event(envelope(&"0".repeat(44), &valid_sig()))
            .expect_err("program id with non-base58 char must be rejected");
        assert!(
            err.to_string().contains("must be base58"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn rejects_signature_below_min_length() {
        let err = normalize_event(envelope(&valid_addr(), &"1".repeat(63)))
            .expect_err("63-char signature must be rejected");
        assert!(
            err.to_string().contains("length out of range"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn rejects_non_base58_signature() {
        let err = normalize_event(envelope(&valid_addr(), &"0".repeat(64)))
            .expect_err("signature with non-base58 char must be rejected");
        assert!(
            err.to_string().contains("must be base58"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn trims_surrounding_whitespace_from_program_id() {
        // Spaces around a valid 44-char address make the raw value 46 chars;
        // trim() brings it back into range and the stored value is the trimmed form.
        let padded = format!(" {} ", valid_addr());
        let event = normalize_event(envelope(&padded, &valid_sig()))
            .expect("whitespace-padded valid program id should normalize after trim");
        assert_eq!(event.program_id, valid_addr());
    }

    #[test]
    fn seed_events_returns_two_normalized_fixture_records() {
        let addr = valid_addr();
        let events = seed_events("devnet", &addr);
        assert_eq!(
            events.len(),
            2,
            "fixture mode surfaces exactly two sample events"
        );
        assert_eq!(events[0].chain, "solana");
        assert_eq!(events[0].cluster, "devnet");
        let names: Vec<&str> = events.iter().map(|e| e.event_name.as_str()).collect();
        assert_eq!(names, vec!["AgentRegistered", "ReceiptBatchAnchored"]);
        assert!(events.iter().all(|e| e.program_id == addr));
        // Payloads pass through unchanged from the seeded envelopes.
        assert_eq!(events[0].payload["agent_key"], "agent-alpha");
        assert_eq!(events[1].payload["receipt_count"], 2);
    }
}
