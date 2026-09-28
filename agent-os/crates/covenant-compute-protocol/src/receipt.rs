//! The operator's signed work receipt (design-01 §3) — produced after
//! executing an admitted job, submitted to the coordinator as evidence
//! for payout. Same wrap-don't-embed, domain-separated signing shape as
//! [`crate::envelope::SignedJobEnvelope`]; see `sign.rs` for the convention,
//! which mirrors `covenantd/src/escrow.rs`'s `CompletionProof`.

use covenant_a2a::A2ATaskStatus;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, WORK_RECEIPT_DOMAIN,
};

/// Canonical hash of a job's output: sha256 over the `serde_json`
/// serialization of the content blocks. The node computes
/// `result_hash_hex` with this, the coordinator refuses a
/// `JobResultMessage` whose accompanying output doesn't hash to the
/// signed receipt's `result_hash_hex`, and the buyer re-checks the same
/// equation before trusting what the coordinator hands back — three
/// parties, one definition, so it lives next to the receipt it anchors.
pub fn output_hash_hex(output: &[Content]) -> String {
    let json = serde_json::to_vec(output).unwrap_or_default();
    let digest = Sha256::digest(&json);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Why a generation stopped, as reported by the executor's backend.
/// `Length` means the output was cut off by the token limit
/// (`max_tokens` / Ollama's `num_predict`), so the answer is incomplete;
/// `ContentFilter` means the backend's content policy cut the output short,
/// so what came back is a partial answer stopped for safety, not a natural
/// end; `ToolCalls` means the model stopped to call one or more tools rather
/// than answer in prose; `Stop` is a natural end. Carried in
/// [`JobMeter::finish_reason`] as an `Option`: `None` means the backend
/// didn't report one (a generic command / echo / subprocess job, or a
/// backend that omits it), read downstream as a normal stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
}

impl FinishReason {
    /// Maps a backend's stop-reason string (Ollama `done_reason`, an
    /// OpenAI-compatible `finish_reason`) to the protocol reason. An
    /// unknown or absent value is `None` — never a guess — so a truncated
    /// answer is only ever labelled `Length` when the backend said so.
    pub fn from_backend(reason: &str) -> Option<Self> {
        match reason {
            "length" => Some(Self::Length),
            "stop" => Some(Self::Stop),
            "content_filter" => Some(Self::ContentFilter),
            "tool_calls" => Some(Self::ToolCalls),
            _ => None,
        }
    }

    /// The reason to report for a turn that produced `tool_calls`, given the
    /// backend's own [`from_backend`](Self::from_backend) reason. A tool-call
    /// turn labels itself [`ToolCalls`](Self::ToolCalls) whatever the backend
    /// called it — unless the backend cut it off ([`Length`](Self::Length))
    /// or filtered it ([`ContentFilter`](Self::ContentFilter)): the tool
    /// arguments are then partial, so the caller must see the truncation, the
    /// way a truncated prose answer keeps its `length` rather than reading as
    /// a clean stop. A turn with no tool calls keeps the backend's reason
    /// unchanged. Both inference executors reconcile through this one place so
    /// they cannot label a cut-off tool call differently.
    pub fn for_tool_turn(backend: Option<Self>, has_tool_calls: bool) -> Option<Self> {
        if !has_tool_calls {
            return backend;
        }
        match backend {
            Some(reason @ (Self::Length | Self::ContentFilter)) => Some(reason),
            _ => Some(Self::ToolCalls),
        }
    }

    /// The OpenAI `finish_reason` string for this reason.
    pub fn as_openai(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ContentFilter => "content_filter",
            Self::ToolCalls => "tool_calls",
        }
    }

    /// The Anthropic `stop_reason` string for this reason: a natural end
    /// is `end_turn`, a token cut-off is `max_tokens`, a turn that asked to
    /// call tools is `tool_use`, and output the model's safety policy
    /// declined to continue is `refusal` — Anthropic's own term for a
    /// content-policy stop, so a filtered turn reads as one rather than a
    /// clean `end_turn`.
    pub fn as_anthropic(self) -> &'static str {
        match self {
            Self::Stop => "end_turn",
            Self::Length => "max_tokens",
            Self::ContentFilter => "refusal",
            Self::ToolCalls => "tool_use",
        }
    }

    /// The Gemini `finishReason` string for this reason. Gemini has no
    /// distinct tool-call reason — a turn that asked to call tools still
    /// ends with `STOP` and carries the request in a `functionCall` part —
    /// so both a natural end and a tool turn read as `STOP`; a token
    /// cut-off is `MAX_TOKENS`, and output stopped by the content policy is
    /// `SAFETY`, Gemini's own reason for a filtered turn.
    pub fn as_gemini(self) -> &'static str {
        match self {
            Self::Stop | Self::ToolCalls => "STOP",
            Self::Length => "MAX_TOKENS",
            Self::ContentFilter => "SAFETY",
        }
    }
}

/// Measured billable for one job. Populated by the node's own execution
/// wrapper (wall clock, token counts from the executor's own report),
/// never from the buyer's request — same posture `covenantd/src/escrow.rs`
/// already takes toward completion claims (design-01 §3). The metering
/// unit split (tokens vs. seconds vs. a unified compute-unit) is flagged
/// as an open decision in build-notes-phase1-foundation.md; this struct
/// carries all three so the choice isn't forced here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobMeter {
    pub wall_ms: u64,
    #[serde(default)]
    pub tokens_in: Option<u64>,
    #[serde(default)]
    pub tokens_out: Option<u64>,
    #[serde(default)]
    pub gpu_seconds: Option<f64>,
    /// Why generation stopped, when the backend reported it. `None` for
    /// non-generation jobs and backends that omit it. Signed into the
    /// receipt, so an operator commits to it exactly as it does the token
    /// counts.
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkReceiptPayload {
    pub job_id: Uuid,
    pub operator: AgentId,
    /// Hash of the [`crate::envelope::JobEnvelopePayload`] the node
    /// accepted — binds the receipt to one specific admitted job.
    pub job_hash_hex: String,
    /// Hash of the output, not the output itself.
    pub result_hash_hex: String,
    pub meter: JobMeter,
    pub price_micro_usdc: u64,
    /// REUSED VERBATIM from `covenant-a2a` (`covenant-a2a/src/lib.rs:42-46`).
    pub status: A2ATaskStatus,
    pub executed_at_ms: u64,
    /// The node's own local audit-chain root at receipt time (§4 of
    /// design-01 — a separate chain from the operator's personal
    /// `covenantd` installation).
    pub node_audit_root_hex: String,
}

/// Prefix of the on-chain payout memo. The full memo is
/// `compute-payout:v1:<job_id>:<receipt_signature_b58>` — see
/// [`SignedWorkReceipt::payout_memo`].
pub const PAYOUT_MEMO_PREFIX: &str = "compute-payout:v1:";

/// Splits a payout memo into `(job_id, receipt_signature_b58)`.
/// `None` for anything that isn't a well-formed compute payout memo —
/// verifiers scan every memo instruction of a transaction with this
/// and ignore the rest.
pub fn parse_payout_memo(memo: &str) -> Option<(Uuid, String)> {
    let rest = memo.strip_prefix(PAYOUT_MEMO_PREFIX)?;
    let (job_id, signature) = rest.split_once(':')?;
    if signature.is_empty() {
        return None;
    }
    Some((Uuid::parse_str(job_id).ok()?, signature.to_string()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedWorkReceipt {
    pub receipt: WorkReceiptPayload,
    pub receipt_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl SignedWorkReceipt {
    pub fn sign(
        receipt: WorkReceiptPayload,
        operator_identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let receipt_json = to_canonical_json(&receipt)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(operator_identity, WORK_RECEIPT_DOMAIN, &receipt_json);
        Ok(Self {
            receipt,
            receipt_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.receipt.operator.pubkey_base58() {
            return Err(ProtocolError::Invalid(format!(
                "signer_pubkey_b58 {} does not match operator.pubkey {}",
                self.signer_pubkey_b58,
                self.receipt.operator.pubkey_base58()
            )));
        }
        verify_domain(
            WORK_RECEIPT_DOMAIN,
            &self.receipt_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: WorkReceiptPayload = serde_json::from_str(&self.receipt_json)?;
        if decoded != self.receipt {
            return Err(ProtocolError::Invalid(
                "receipt_json does not decode to the embedded receipt".into(),
            ));
        }
        Ok(())
    }

    /// The memo the coordinator stamps onto this receipt's on-chain
    /// payout transfer. The operator's signature commits to the whole
    /// payload (and ed25519 signing is deterministic, so the same
    /// receipt always derives the same memo) — anyone holding the
    /// signed receipt can recompute this string, fetch the transaction
    /// the coordinator cited, and check that the chain paid for
    /// exactly this work. One definition for the writer (payout path)
    /// and every verifier, like [`output_hash_hex`].
    pub fn payout_memo(&self) -> String {
        crate::payout::payout_memo_for(self.receipt.job_id, &self.signature_b58)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(operator: &LocalIdentity) -> WorkReceiptPayload {
        WorkReceiptPayload {
            job_id: Uuid::new_v4(),
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: JobMeter {
                wall_ms: 1_200,
                tokens_in: Some(50),
                tokens_out: Some(200),
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 42,
            node_audit_root_hex: "cc".repeat(32),
        }
    }

    #[test]
    fn sign_and_verify_round_trips() {
        let operator = LocalIdentity::generate("operator@local");
        let signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");
        signed.verify().expect("verify");
    }

    #[test]
    fn verify_rejects_tampered_receipt_json() {
        let operator = LocalIdentity::generate("operator@local");
        let mut signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");
        signed.receipt_json = signed.receipt_json.replace("1000", "1");
        assert!(signed.verify().is_err());
    }

    #[test]
    fn verify_rejects_receipt_that_disagrees_with_receipt_json() {
        let operator = LocalIdentity::generate("operator@local");
        let mut signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");
        signed.receipt.price_micro_usdc = 999_999;
        assert!(signed.verify().is_err());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let operator = LocalIdentity::generate("operator@local");
        let impostor = LocalIdentity::generate("impostor@local");
        let mut signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");
        let (sig, pk) = crate::sign::sign_domain(
            &impostor,
            crate::sign::WORK_RECEIPT_DOMAIN,
            &signed.receipt_json,
        );
        signed.signature_b58 = sig;
        signed.signer_pubkey_b58 = pk;
        assert!(signed.verify().is_err());
    }

    #[test]
    fn verify_rejects_truncated_signature() {
        let operator = LocalIdentity::generate("operator@local");
        let mut signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");
        signed
            .signature_b58
            .truncate(signed.signature_b58.len() - 4);
        assert!(signed.verify().is_err());
    }

    #[test]
    fn finish_reason_maps_backend_strings_and_openai() {
        assert_eq!(
            FinishReason::from_backend("length"),
            Some(FinishReason::Length)
        );
        assert_eq!(FinishReason::from_backend("stop"), Some(FinishReason::Stop));
        assert_eq!(
            FinishReason::from_backend("content_filter"),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(FinishReason::from_backend("load"), None);
        assert_eq!(FinishReason::from_backend(""), None);
        assert_eq!(FinishReason::Length.as_openai(), "length");
        assert_eq!(FinishReason::Stop.as_openai(), "stop");
        assert_eq!(FinishReason::ContentFilter.as_openai(), "content_filter");
        assert_eq!(FinishReason::Stop.as_anthropic(), "end_turn");
        assert_eq!(FinishReason::Length.as_anthropic(), "max_tokens");
        assert_eq!(FinishReason::ToolCalls.as_anthropic(), "tool_use");
        // A content-filter stop is Anthropic's `refusal`, not a clean end.
        assert_eq!(FinishReason::ContentFilter.as_anthropic(), "refusal");
        assert_eq!(FinishReason::Stop.as_gemini(), "STOP");
        assert_eq!(FinishReason::Length.as_gemini(), "MAX_TOKENS");
        assert_eq!(FinishReason::ToolCalls.as_gemini(), "STOP");
        assert_eq!(FinishReason::ContentFilter.as_gemini(), "SAFETY");
    }

    #[test]
    fn a_tool_turn_keeps_a_truncation_reason_but_labels_a_clean_stop_tool_calls() {
        use FinishReason::*;
        // No tool calls: the backend's reason passes through untouched.
        assert_eq!(FinishReason::for_tool_turn(Some(Stop), false), Some(Stop));
        assert_eq!(
            FinishReason::for_tool_turn(Some(Length), false),
            Some(Length)
        );
        assert_eq!(FinishReason::for_tool_turn(None, false), None);
        // A tool call over a clean or unstated stop is the salient outcome.
        assert_eq!(
            FinishReason::for_tool_turn(Some(Stop), true),
            Some(ToolCalls)
        );
        assert_eq!(FinishReason::for_tool_turn(None, true), Some(ToolCalls));
        assert_eq!(
            FinishReason::for_tool_turn(Some(ToolCalls), true),
            Some(ToolCalls)
        );
        // But a cut-off or filtered tool call carries partial arguments, so
        // the truncation reason survives rather than reading as a clean call.
        assert_eq!(
            FinishReason::for_tool_turn(Some(Length), true),
            Some(Length)
        );
        assert_eq!(
            FinishReason::for_tool_turn(Some(ContentFilter), true),
            Some(ContentFilter)
        );
    }

    #[test]
    fn a_meter_from_before_the_finish_reason_field_decodes_as_none() {
        // A receipt an older node signed carries no `finish_reason` key;
        // it must still decode (as None) and verify, so the field is
        // strictly additive on the signed wire.
        let json = r#"{"wall_ms":1200,"tokens_in":50,"tokens_out":200,"gpu_seconds":null}"#;
        let meter: JobMeter = serde_json::from_str(json).expect("decode legacy meter");
        assert_eq!(meter.finish_reason, None);

        let operator = LocalIdentity::generate("operator@local");
        let mut payload = receipt(&operator);
        let signed = SignedWorkReceipt::sign(payload.clone(), &operator).expect("sign");
        let legacy_json = signed.receipt_json.replace(",\"finish_reason\":null", "");
        assert!(!legacy_json.contains("finish_reason"));
        let (signature_b58, signer_pubkey_b58) =
            crate::sign::sign_domain(&operator, WORK_RECEIPT_DOMAIN, &legacy_json);
        payload.meter.finish_reason = None;
        let legacy = SignedWorkReceipt {
            receipt: payload,
            receipt_json: legacy_json,
            signature_b58,
            signer_pubkey_b58,
        };
        legacy.verify().expect("a legacy receipt still verifies");
    }

    #[test]
    fn a_truncated_generation_round_trips_on_the_signed_receipt() {
        let operator = LocalIdentity::generate("operator@local");
        let mut payload = receipt(&operator);
        payload.meter.finish_reason = Some(FinishReason::Length);
        let signed = SignedWorkReceipt::sign(payload, &operator).expect("sign");
        assert!(signed.receipt_json.contains("\"finish_reason\":\"length\""));
        signed.verify().expect("verify");
        let decoded: WorkReceiptPayload =
            serde_json::from_str(&signed.receipt_json).expect("decode");
        assert_eq!(decoded.meter.finish_reason, Some(FinishReason::Length));
    }

    #[test]
    fn output_hash_is_deterministic_and_content_sensitive() {
        let a = vec![Content::text("hello")];
        let b = vec![Content::text("hello")];
        let c = vec![Content::text("hell0")];
        assert_eq!(output_hash_hex(&a), output_hash_hex(&b));
        assert_ne!(output_hash_hex(&a), output_hash_hex(&c));
        assert_eq!(output_hash_hex(&[]).len(), 64);
    }

    #[test]
    fn payout_memo_round_trips_and_stays_within_the_memo_program_cap() {
        let operator = LocalIdentity::generate("operator@local");
        let signed = SignedWorkReceipt::sign(receipt(&operator), &operator).expect("sign");

        let memo = signed.payout_memo();
        assert!(memo.starts_with(PAYOUT_MEMO_PREFIX));
        // The SPL memo program caps unsigned memos at 566 bytes; a
        // uuid + base58 ed25519 signature sits far under it.
        assert!(memo.len() < 200, "memo unexpectedly long: {}", memo.len());

        let (job_id, signature) = parse_payout_memo(&memo).expect("parse");
        assert_eq!(job_id, signed.receipt.job_id);
        assert_eq!(signature, signed.signature_b58);
    }

    #[test]
    fn payout_memo_is_deterministic_for_the_same_receipt() {
        let operator = LocalIdentity::generate("operator@local");
        let payload = receipt(&operator);
        let a = SignedWorkReceipt::sign(payload.clone(), &operator).expect("sign");
        let b = SignedWorkReceipt::sign(payload, &operator).expect("sign");
        assert_eq!(a.payout_memo(), b.payout_memo());
    }

    #[test]
    fn parse_payout_memo_ignores_foreign_and_malformed_memos() {
        assert!(parse_payout_memo("gm").is_none());
        assert!(parse_payout_memo("compute-payout:v1:").is_none());
        assert!(parse_payout_memo("compute-payout:v1:not-a-uuid:sig").is_none());
        assert!(parse_payout_memo(&format!("compute-payout:v1:{}:", Uuid::nil())).is_none());
        assert!(parse_payout_memo(&format!("compute-payout:v2:{}:sig", Uuid::nil())).is_none());
    }
}
