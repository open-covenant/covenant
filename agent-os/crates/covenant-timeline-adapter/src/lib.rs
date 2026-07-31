use covenant_audit::{hash_hex, AuditEvent, AuditIntegrityReport};
use covenant_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod release;

pub const TIMELINE_REVISION: &str = "88da86d3dce4be33320f93db0ba4f4fc7c0643cf";
pub const CONTRACT_SCHEMA: &str = "covenant.timeline.contract.v0alpha1";
pub const EVENT_SCHEMA: &str = "covenant.timeline.event.v0alpha1";
pub const RUN_SCHEMA: &str = "covenant.timeline.run.v0alpha1";

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("JSON canonicalization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0} must be a lowercase portable identifier")]
    Identifier(String),
    #[error("evidence must support at least one claim")]
    EmptyClaims,
    #[error("provenance envelope schema must be covenant.provenance.v1")]
    ProvenanceSchema,
    #[error("unsupported Timeline command schema or kind")]
    CommandKind,
    #[error("event schema must be covenant.timeline.event.v0alpha1")]
    EventSchema,
    #[error("event sequence {actual} does not match {expected}")]
    EventSequence { expected: u64, actual: u64 },
    #[error("Timeline command must forbid execution during replay")]
    ReplayPolicy,
    #[error("Timeline command payload does not match the capability template")]
    PayloadMismatch,
    #[error("Covenant capability response action does not match the request")]
    ResponseMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEvidence {
    pub id: String,
    pub kind: String,
    pub claims: Vec<String>,
    pub payload_digest: String,
    pub producer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineReceipt {
    pub id: String,
    pub command_id: String,
    pub status: ReceiptStatus,
    pub effect_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptStatus {
    Succeeded,
    Failed,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineEvent {
    pub schema: String,
    pub id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub body: TimelineEventBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum TimelineEventBody {
    #[serde(rename = "evidence.recorded")]
    EvidenceRecorded { evidence: TimelineEvidence },
    #[serde(rename = "checkpoint.evaluated")]
    CheckpointEvaluated {
        #[serde(rename = "checkpointId")]
        checkpoint_id: String,
        #[serde(rename = "evidenceRefs")]
        evidence_refs: Vec<String>,
        #[serde(rename = "policyRef")]
        policy_ref: String,
    },
    #[serde(rename = "receipt.recorded")]
    ReceiptRecorded { receipt: TimelineReceipt },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineCommand {
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub payload_ref: String,
    pub idempotency_key: String,
    pub replay_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityTemplate {
    pub payload_ref: String,
    pub action: String,
    pub scope: Option<Value>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRun {
    pub schema: String,
    pub run_id: String,
    pub contract: Value,
    pub events: Vec<TimelineEvent>,
}

impl TimelineRun {
    pub fn new(run_id: impl Into<String>, contract: Value) -> Result<Self, AdapterError> {
        let run_id = run_id.into();
        validate_identifier(&run_id)?;
        if contract.get("schema").and_then(Value::as_str) != Some(CONTRACT_SCHEMA) {
            return Err(AdapterError::CommandKind);
        }
        Ok(Self {
            schema: RUN_SCHEMA.into(),
            run_id,
            contract,
            events: Vec::new(),
        })
    }

    pub fn push(&mut self, event: TimelineEvent) -> Result<(), AdapterError> {
        if event.schema != EVENT_SCHEMA {
            return Err(AdapterError::EventSchema);
        }
        let expected = self.events.len() as u64;
        if event.sequence != expected {
            return Err(AdapterError::EventSequence {
                expected,
                actual: event.sequence,
            });
        }
        validate_identifier(&event.id)?;
        self.events.push(event);
        Ok(())
    }
}

pub fn evidence_event(
    sequence: u64,
    id: impl Into<String>,
    kind: impl Into<String>,
    producer: impl Into<String>,
    claims: Vec<String>,
    payload: &Value,
) -> Result<TimelineEvent, AdapterError> {
    let id = id.into();
    let kind = kind.into();
    let producer = producer.into();
    validate_identifier(&id)?;
    validate_identifier(&kind)?;
    validate_identifier(&producer)?;
    if claims.is_empty() {
        return Err(AdapterError::EmptyClaims);
    }
    for claim in &claims {
        validate_identifier(claim)?;
    }

    Ok(TimelineEvent {
        schema: EVENT_SCHEMA.into(),
        id: format!("event-{sequence}"),
        sequence,
        body: TimelineEventBody::EvidenceRecorded {
            evidence: TimelineEvidence {
                id,
                kind,
                claims,
                payload_digest: digest(payload)?,
                producer,
            },
        },
    })
}

pub fn provenance_evidence_event(
    sequence: u64,
    id: impl Into<String>,
    claims: Vec<String>,
    envelope: &Value,
) -> Result<TimelineEvent, AdapterError> {
    if envelope.get("schema").and_then(Value::as_str) != Some("covenant.provenance.v1") {
        return Err(AdapterError::ProvenanceSchema);
    }
    evidence_event(
        sequence,
        id,
        "covenant.provenance",
        "covenant.git",
        claims,
        envelope,
    )
}

pub fn audit_evidence_event(
    sequence: u64,
    claims: Vec<String>,
    event: &AuditEvent,
) -> Result<TimelineEvent, AdapterError> {
    evidence_event(
        sequence,
        format!("audit/{}", event.id),
        "covenant.audit-event",
        "covenant.audit",
        claims,
        &serde_json::to_value(event)?,
    )
}

pub fn audit_integrity_evidence_event(
    sequence: u64,
    id: impl Into<String>,
    claims: Vec<String>,
    report: &AuditIntegrityReport,
) -> Result<TimelineEvent, AdapterError> {
    evidence_event(
        sequence,
        id,
        "covenant.audit-integrity",
        "covenant.audit",
        claims,
        &serde_json::to_value(report)?,
    )
}

pub fn checkpoint_event(
    sequence: u64,
    checkpoint_id: impl Into<String>,
    evidence_refs: Vec<String>,
    policy_ref: impl Into<String>,
) -> Result<TimelineEvent, AdapterError> {
    let checkpoint_id = checkpoint_id.into();
    let policy_ref = policy_ref.into();
    validate_identifier(&checkpoint_id)?;
    validate_identifier(&policy_ref)?;
    for evidence_ref in &evidence_refs {
        validate_identifier(evidence_ref)?;
    }
    Ok(TimelineEvent {
        schema: EVENT_SCHEMA.into(),
        id: format!("event-{sequence}"),
        sequence,
        body: TimelineEventBody::CheckpointEvaluated {
            checkpoint_id,
            evidence_refs,
            policy_ref,
        },
    })
}

pub fn capability_request(
    command: &TimelineCommand,
    template: &CapabilityTemplate,
) -> Result<Request, AdapterError> {
    if command.schema != "covenant.timeline.command.v0alpha1"
        || command.kind != "covenant.capability.request"
    {
        return Err(AdapterError::CommandKind);
    }
    if command.replay_policy != "forbid" {
        return Err(AdapterError::ReplayPolicy);
    }
    if command.payload_ref != template.payload_ref {
        return Err(AdapterError::PayloadMismatch);
    }
    Ok(Request::GrantCapability {
        action: template.action.clone(),
        scope: template.scope.clone(),
        expires_at: template.expires_at,
    })
}

pub fn capability_receipt_event(
    sequence: u64,
    command: &TimelineCommand,
    template: &CapabilityTemplate,
    response: &Response,
) -> Result<TimelineEvent, AdapterError> {
    capability_request(command, template)?;
    let status = match response {
        Response::CapabilityGranted { action, .. } if action == &template.action => {
            ReceiptStatus::Succeeded
        }
        Response::CapabilityGranted { .. } => return Err(AdapterError::ResponseMismatch),
        Response::Error { .. } => ReceiptStatus::Failed,
        _ => ReceiptStatus::Indeterminate,
    };
    let payload = serde_json::to_value(response)?;
    Ok(TimelineEvent {
        schema: EVENT_SCHEMA.into(),
        id: format!("event-{sequence}"),
        sequence,
        body: TimelineEventBody::ReceiptRecorded {
            receipt: TimelineReceipt {
                id: format!("receipt/{}", command.id),
                command_id: command.id.clone(),
                status,
                effect_digest: digest(&payload)?,
            },
        },
    })
}

fn digest(value: &Value) -> Result<String, AdapterError> {
    let bytes = serde_jcs::to_vec(value)?;
    Ok(format!("sha256:{}", hash_hex(&bytes)))
}

fn validate_identifier(value: &str) -> Result<(), AdapterError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(AdapterError::Identifier(value.into()));
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(AdapterError::Identifier(value.into()));
    }
    if value.len() > 128
        || !chars.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | ':' | '/' | '-')
        })
    {
        return Err(AdapterError::Identifier(value.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::AuditKind;
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn command() -> TimelineCommand {
        TimelineCommand {
            schema: "covenant.timeline.command.v0alpha1".into(),
            id: "run-42:release-ready:7".into(),
            kind: "covenant.capability.request".into(),
            payload_ref: "release.publish".into(),
            idempotency_key: "run-42/release-ready/7".into(),
            replay_policy: "forbid".into(),
        }
    }

    fn template() -> CapabilityTemplate {
        CapabilityTemplate {
            payload_ref: "release.publish".into(),
            action: "release.publish".into(),
            scope: Some(serde_json::json!({"repository": "open-covenant/covenant"})),
            expires_at: None,
        }
    }

    #[test]
    fn maps_audit_rows_to_payload_free_evidence() {
        let audit = AuditEvent {
            id: Uuid::from_u128(1),
            timestamp_ms: 1_700_000_000_000,
            issuer: AgentId::new("operator@covenant", [7; 32]),
            kind: AuditKind::IntentDispatched {
                intent_id: Uuid::from_u128(2),
                intent_text: "resume timeline integration".into(),
                matched_agent: Some("research".into()),
                result_hash_hex: "a".repeat(64),
                status: "ok".into(),
            },
        };

        let event =
            audit_evidence_event(0, vec!["engineering.resume.recorded".into()], &audit).unwrap();
        let value = serde_json::to_value(event).unwrap();

        assert_eq!(value["type"], "evidence.recorded");
        assert_eq!(value["evidence"]["id"], format!("audit/{}", audit.id));
        assert!(value["evidence"].get("payload").is_none());
        assert!(value["evidence"]["payloadDigest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }

    #[test]
    fn maps_commands_to_explicit_capability_requests_without_executing() {
        let request = capability_request(&command(), &template()).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "kind": "grant_capability",
                "action": "release.publish",
                "scope": {"repository": "open-covenant/covenant"},
                "expires_at": null
            })
        );
    }

    #[test]
    fn maps_covenant_outcomes_to_timeline_receipts() {
        let response = Response::CapabilityGranted {
            signature_b58: "GrantSigRelease".into(),
            subject_display: "operator@covenant".into(),
            action: "release.publish".into(),
        };
        let event = capability_receipt_event(8, &command(), &template(), &response).unwrap();
        let value = serde_json::to_value(event).unwrap();

        assert_eq!(value["type"], "receipt.recorded");
        assert_eq!(value["receipt"]["status"], "succeeded");
        assert_eq!(value["receipt"]["commandId"], "run-42:release-ready:7");
    }

    #[test]
    fn rejects_replay_execution_and_mismatched_responses() {
        let mut replayable = command();
        replayable.replay_policy = "allow".into();
        assert!(matches!(
            capability_request(&replayable, &template()),
            Err(AdapterError::ReplayPolicy)
        ));

        let response = Response::CapabilityGranted {
            signature_b58: "GrantSigOther".into(),
            subject_display: "operator@covenant".into(),
            action: "memory.write".into(),
        };
        assert!(matches!(
            capability_receipt_event(8, &command(), &template(), &response),
            Err(AdapterError::ResponseMismatch)
        ));
    }

    #[test]
    fn rejects_out_of_order_export_events() {
        let contract = serde_json::json!({
            "schema": CONTRACT_SCHEMA,
            "id": "software.release.v0"
        });
        let mut run = TimelineRun::new("run-42", contract).unwrap();
        let event = evidence_event(
            1,
            "ci-42",
            "ci",
            "github-actions",
            vec!["ci.tests.pass".into()],
            &serde_json::json!({"status": "passed"}),
        )
        .unwrap();

        assert!(matches!(
            run.push(event),
            Err(AdapterError::EventSequence {
                expected: 0,
                actual: 1
            })
        ));
    }
}
