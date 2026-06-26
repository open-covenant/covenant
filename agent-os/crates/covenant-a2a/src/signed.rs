//! Authenticated cross-host A2A envelopes.
//!
//! The local IPC/HTTP gateway authenticates callers with a per-daemon bearer
//! token resolved against the *local* peer registry. A daemon on another host
//! holds no such token, so cross-host A2A authenticity is proven instead by an
//! ed25519 signature over the task by the sender's identity key — the same key
//! carried in [`AgentId::pubkey`](covenant_types::AgentId).
//!
//! [`SignedA2ATask`] is that wire envelope. It proves *authenticity* of the
//! claimed sender; *authorization* — whether that pubkey is an allowed
//! cross-host peer and whether the recipient admits it — is the receiving
//! daemon's job and stays out of this primitive.

use covenant_identity::{verify_with_pubkey_bytes, LocalIdentity};
use serde::{Deserialize, Serialize};

use crate::A2ATask;

/// Domain-separation tag mixed into every signed message so an ed25519
/// signature minted for another Covenant context (an attestation, a capability
/// token) can never be replayed as a cross-host A2A envelope, and vice versa.
const DOMAIN_TAG: &[u8] = b"covenant.a2a.cross-host.v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum A2AEnvelopeError {
    #[error("cross-host envelope signature does not verify against the claimed sender")]
    SignatureInvalid,
    #[error("cross-host envelope signature is not the 64-byte ed25519 form")]
    MalformedSignature,
    #[error("cross-host envelope payload is not a valid A2A task")]
    MalformedPayload,
}

/// An [`A2ATask`] sealed by its sender for delivery to a remote daemon.
///
/// The signature covers `DOMAIN_TAG || issued_at_ms.to_le_bytes() || task_json`
/// where `task_json` is the *exact* bytes transmitted — verification never
/// re-serializes the task, so the signature check is immune to serde
/// field-ordering or re-encoding drift: a faithful envelope verifies and a
/// forged one does not, independent of how the local [`A2ATask`] type happens
/// to re-encode. Payload *schema* compatibility is a separate concern —
/// recovering the task still deserializes `task_json` against the running
/// [`A2ATask`], so new fields must stay `#[serde(default)]` or old envelopes
/// will fail to [`open`] as [`A2AEnvelopeError::MalformedPayload`].
///
/// [`open`] is the only way to recover the task and yields it solely when the
/// signature verifies against the pubkey the payload itself names as sender.
///
/// [`open`]: SignedA2ATask::open
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedA2ATask {
    task_json: Vec<u8>,
    issued_at_ms: u64,
    signature: Vec<u8>,
}

/// The exact bytes the sender signs and the receiver verifies. A single helper
/// used by both [`SignedA2ATask::seal`] and [`SignedA2ATask::open`] so the two
/// can never drift apart.
///
/// No length prefixes are needed because `DOMAIN_TAG` and `issued_at_ms` are
/// fixed-length and `task_json` (the only variable-length field) is last, so
/// the concatenation parses to exactly one `(tag, timestamp, payload)` split —
/// attacker bytes cannot shift across a field boundary. A future change that
/// made the tag variable-length would reopen that ambiguity and MUST then add
/// an explicit length delimiter.
fn signed_message(issued_at_ms: u64, task_json: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN_TAG.len() + 8 + task_json.len());
    msg.extend_from_slice(DOMAIN_TAG);
    msg.extend_from_slice(&issued_at_ms.to_le_bytes());
    msg.extend_from_slice(task_json);
    msg
}

impl SignedA2ATask {
    /// Seal `task` under the daemon's identity for cross-host delivery.
    /// `issued_at_ms` is bound into the signature so a captured envelope's
    /// freshness cannot be rewritten to slip past the receiver's window.
    pub fn seal(task: &A2ATask, issued_at_ms: u64, identity: &LocalIdentity) -> Self {
        // A2ATask is plain serde data — no maps, no sets, no fallible custom
        // serializers — and travels this same path into the mailbox JSONL, so
        // serialization is infallible.
        let task_json = serde_json::to_vec(task)
            .expect("A2ATask serializes infallibly (plain data, mailbox JSONL path)");
        let signature = identity
            .sign(&signed_message(issued_at_ms, &task_json))
            .to_bytes()
            .to_vec();
        Self {
            task_json,
            issued_at_ms,
            signature,
        }
    }

    /// The signer's claimed send time, in epoch milliseconds. Exposed for the
    /// receiver's anti-replay freshness window; it is meaningful only after
    /// [`open`](Self::open) has proven the envelope authentic, since the
    /// signature is what binds this value.
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Verify the envelope and return the sealed task. The payload is parsed to
    /// learn the *claimed* sender, then the signature is checked against that
    /// sender's pubkey — so a returned task's `sender` is provably the signer.
    /// Authorizing that sender is the caller's responsibility.
    pub fn open(&self) -> Result<A2ATask, A2AEnvelopeError> {
        let task: A2ATask = serde_json::from_slice(&self.task_json)
            .map_err(|_| A2AEnvelopeError::MalformedPayload)?;
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| A2AEnvelopeError::MalformedSignature)?;
        verify_with_pubkey_bytes(
            task.sender.pubkey,
            &signed_message(self.issued_at_ms, &self.task_json),
            &signature,
        )
        .map_err(|_| A2AEnvelopeError::SignatureInvalid)?;
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_types::AgentId;
    use uuid::Uuid;

    fn task_from(sender: AgentId) -> A2ATask {
        A2ATask {
            id: Uuid::from_u128(0x0a2a_0b2b_0c2c_0d2d),
            sender,
            recipient: AgentId::new("bob@host2", [7u8; 32]),
            intent_text: "ship the receipt batch".into(),
            task_kind: None,
            parent: None,
            deadline_ms: None,
            idempotency: None,
        }
    }

    #[test]
    fn seal_then_open_roundtrips_and_binds_sender_to_signer() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let env = SignedA2ATask::seal(&task, 1000, &id);
        let opened = env.open().expect("a faithfully sealed envelope must open");
        assert_eq!(opened, task, "open must return the exact sealed task");
        assert_eq!(
            opened.sender.pubkey,
            id.pubkey_bytes(),
            "the returned sender must be the key that signed the envelope"
        );
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let mut env = SignedA2ATask::seal(&task, 1000, &id);
        // A still-parseable but different payload (same sender) keeps the parse
        // step happy so the failure is provably the signature check, not a
        // malformed-JSON shortcut.
        let mut tampered = task.clone();
        tampered.intent_text = "drain the treasury".into();
        env.task_json = serde_json::to_vec(&tampered).unwrap();
        assert_eq!(
            env.open(),
            Err(A2AEnvelopeError::SignatureInvalid),
            "a payload altered after signing must not verify"
        );
    }

    #[test]
    fn tampered_issued_at_ms_is_rejected() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let mut env = SignedA2ATask::seal(&task, 1000, &id);
        env.issued_at_ms = 999_999;
        assert_eq!(
            env.open(),
            Err(A2AEnvelopeError::SignatureInvalid),
            "issued_at_ms must be bound into the signed message"
        );
    }

    #[test]
    fn signature_by_a_key_other_than_the_claimed_sender_is_rejected() {
        let signer = LocalIdentity::generate("mallory@evil");
        let claimed = LocalIdentity::generate("alice@host1");
        assert_ne!(signer.pubkey_bytes(), claimed.pubkey_bytes());
        // The payload claims alice as sender, but mallory holds the pen.
        let task = task_from(claimed.agent_id());
        let env = SignedA2ATask::seal(&task, 1000, &signer);
        assert_eq!(
            env.open(),
            Err(A2AEnvelopeError::SignatureInvalid),
            "verification must key on the claimed sender, not the actual signer"
        );
    }

    #[test]
    fn signature_without_the_domain_tag_is_rejected() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let mut env = SignedA2ATask::seal(&task, 1000, &id);
        // Re-sign the same task+timestamp but omit DOMAIN_TAG. If the tag were
        // dropped from signed_message, this would verify and the test fails.
        let mut undomained = Vec::new();
        undomained.extend_from_slice(&1000u64.to_le_bytes());
        undomained.extend_from_slice(&env.task_json);
        env.signature = id.sign(&undomained).to_bytes().to_vec();
        assert_eq!(
            env.open(),
            Err(A2AEnvelopeError::SignatureInvalid),
            "a signature missing the domain tag must not be accepted"
        );
    }

    #[test]
    fn wrong_length_signature_is_rejected_without_panic() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let valid = SignedA2ATask::seal(&task, 1000, &id);

        let mut empty = valid.clone();
        empty.signature = Vec::new();
        assert_eq!(empty.open(), Err(A2AEnvelopeError::MalformedSignature));

        let mut short = valid.clone();
        short.signature.truncate(63);
        assert_eq!(short.open(), Err(A2AEnvelopeError::MalformedSignature));

        let mut long = valid;
        long.signature.push(0); // 65 bytes
        assert_eq!(long.open(), Err(A2AEnvelopeError::MalformedSignature));
    }

    #[test]
    fn correctly_sized_but_wrong_signature_is_invalid_not_malformed() {
        // Length and cryptographic validity are distinct failure modes: a
        // 64-byte signature passes the length guard and must then be rejected
        // by verification as SignatureInvalid, never mislabelled MalformedSignature.
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let mut env = SignedA2ATask::seal(&task, 1000, &id);
        env.signature = vec![0u8; 64];
        assert_eq!(env.open(), Err(A2AEnvelopeError::SignatureInvalid));
    }

    #[test]
    fn unparseable_payload_is_rejected_as_malformed() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let mut env = SignedA2ATask::seal(&task, 1000, &id);
        env.task_json = b"not a task at all".to_vec();
        assert_eq!(env.open(), Err(A2AEnvelopeError::MalformedPayload));
    }

    #[test]
    fn envelope_survives_a_json_wire_roundtrip() {
        let id = LocalIdentity::generate("alice@host1");
        let task = task_from(id.agent_id());
        let env = SignedA2ATask::seal(&task, 1000, &id);
        let wire = serde_json::to_string(&env).unwrap();
        let back: SignedA2ATask = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, env, "the envelope must round-trip through JSON intact");
        assert_eq!(
            back.open().expect("round-tripped envelope still opens"),
            task
        );
    }
}
