//! The buyer's signed job envelope — the dispatch request sent to the
//! coordinator and relayed to the assigned operator (design-02 §1.2).
//!
//! Signature binds every payload field, domain-separated under
//! `covenant.compute.job.v1`. Wrap-don't-embed: the wire form carries
//! the exact JSON the signature covers, so a verifier checks the
//! embedded bytes rather than re-serializing `payload` and risking
//! canonicalization drift (see `sign.rs`).

use covenant_a2a::A2AIdempotency;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capability::{CapabilityRequirement, JobKind};
use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, JOB_ENVELOPE_DOMAIN,
};

/// Unsigned job payload. `input` is small inline content. A payload
/// near or above the shared IPC/HTTP frame cap (`MAX_FRAME`, 8 MiB —
/// `covenant-ipc/src/lib.rs:69`) belongs in blob storage referenced by
/// hash instead of inlined here; that content-addressed fetch path is
/// an open seam (build-notes-phase1-foundation.md), not built in this
/// slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEnvelopePayload {
    pub job_id: Uuid,
    pub buyer: AgentId,
    pub kind: JobKind,
    pub capability_requirement: CapabilityRequirement,
    pub input: Vec<Content>,
    pub price_micro_usdc: u64,
    pub deadline_ms: u64,
    /// REUSED VERBATIM from `covenant-a2a` (`covenant-a2a/src/lib.rs:56-68`).
    pub idempotency: A2AIdempotency,
    pub issued_at_ms: u64,
    /// Demand-side partner attribution (C8): the referral code of the
    /// partner who brought this buyer, inside the signed payload so a
    /// relay can't re-attribute the job. Skipped when `None` so a
    /// referral-free payload keeps the pre-referral wire bytes — old
    /// signatures keep verifying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral_code: Option<String>,
    /// The buyer asks for incremental output ([`crate::StreamPush`])
    /// while the job runs. Advisory: a node whose executor cannot
    /// stream still serves the job one-shot, and the signed receipt
    /// over the final output settles it either way. Skipped when
    /// `false` so a non-streaming payload keeps the pre-stream wire
    /// bytes — old signatures keep verifying.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl JobEnvelopePayload {
    /// Structural validation of the paid input, for kinds whose
    /// executors read it as more than opaque text. A malformed chat or
    /// generation block is the buyer's error: the coordinator refuses
    /// it at submission (before any escrow hold) and the operator at
    /// admission (before acceptance), so it never reaches an executor
    /// that would fail the job — and take the reputation fault — on
    /// input only the buyer controls. Batch input stays advisory: a
    /// batch executor reads its command from the first text block and
    /// ignores the rest, malformed or not.
    pub fn validate_input(&self) -> Result<(), ProtocolError> {
        // A job's kind and the kind it requires of an operator travel as
        // two signed fields but must agree — the buyer builds both from
        // one `kind`. A divergence only serves to smuggle input past the
        // parse below: declaring `BatchJob` here skips it while the
        // requirement says `InferenceCall`, so the coordinator still
        // routes to an inference operator whose executor then fails on
        // the malformed input and eats the fault. Refuse the mismatch.
        if self.kind != self.capability_requirement.kind {
            return Err(ProtocolError::Invalid(format!(
                "job kind {:?} disagrees with its capability requirement {:?}",
                self.kind, self.capability_requirement.kind
            )));
        }
        match self.kind {
            JobKind::InferenceCall => {
                crate::chat::parse_chat_input(&self.input)?;
                crate::generation::parse_generation_params(&self.input)?;
            }
            // A transcription's audio is the buyer's, so its structural
            // faults (no clip, an oversized one) are caught here rather
            // than at the operator's executor.
            JobKind::Transcription => {
                crate::transcription::parse_transcription_input(&self.input)?;
            }
            // A synthesis carries the buyer's text, so its faults (nothing
            // to speak, text past the cap, an out-of-range speed) are
            // caught here too, before any operator is matched.
            JobKind::SpeechSynthesis => {
                crate::speech::parse_speech_input(&self.input)?;
            }
            // A lease without terms has no window to bound and no price
            // to check, and its ceiling is what the buyer signs — both
            // are validated here, before any operator is matched or a
            // hold is taken.
            JobKind::LeaseSession => {
                let terms = crate::lease::parse_lease_terms(&self.input)?.ok_or_else(|| {
                    ProtocolError::Invalid(
                        "a lease_session envelope must carry a lease terms block".into(),
                    )
                })?;
                let ceiling = terms.max_price_micro_usdc()?;
                if self.price_micro_usdc != ceiling {
                    return Err(ProtocolError::Invalid(format!(
                        "lease price {} micro-USDC does not equal the terms' ceiling {} \
                         (rate {} × {}s)",
                        self.price_micro_usdc,
                        ceiling,
                        terms.rate_micro_usdc_per_sec,
                        terms.max_duration_secs
                    )));
                }
                // The lease window travels as two signed fields the buyer
                // builds from one number: the terms' `max_duration_secs`,
                // which prices, escrows and meters the session, and the
                // capability requirement's, which the operator's admission
                // and the coordinator's matcher scale a per-hour ask's price
                // floor by. A divergence only serves to floor the operator on
                // a shorter window than the one it commits and bills: a
                // requirement window below the terms window clears the ask at
                // a fraction of the advertised per-second rate, then rents the
                // fuller window the terms grant. Refuse it, as the kind
                // mismatch above is refused.
                if u64::from(self.capability_requirement.max_duration_secs)
                    != terms.max_duration_secs
                {
                    return Err(ProtocolError::Invalid(format!(
                        "lease requirement window {}s does not equal the terms' {}s window; the \
                         per-hour price floor scales by the requirement window while the escrow \
                         and meter bill the terms window, so the two must match",
                        self.capability_requirement.max_duration_secs, terms.max_duration_secs
                    )));
                }
                // The deadline clock starts at issue and the refund
                // sweep is merciless past it — a deadline the window
                // itself consumes guarantees a full-window session
                // settles as an expired refund. Demand room for the
                // window plus a floor for acceptance and settlement.
                let window_ms = terms.max_duration_secs.saturating_mul(1_000);
                let needed_ms = window_ms.saturating_add(crate::lease::LEASE_DEADLINE_SLACK_MS);
                if self.deadline_ms < needed_ms {
                    return Err(ProtocolError::Invalid(format!(
                        "lease deadline_ms {} cannot cover the {}s window plus the {}ms \
                         acceptance/settlement floor (need at least {needed_ms})",
                        self.deadline_ms,
                        terms.max_duration_secs,
                        crate::lease::LEASE_DEADLINE_SLACK_MS
                    )));
                }
            }
            // An embedding's texts are the buyer's, and both embedding
            // backends fail an empty request outright (`embedding_texts`),
            // so a malformed one would fault the matched operator for
            // input only the buyer controls. Screen it here, like the
            // kinds above, before any hold or match.
            JobKind::Embedding => {
                crate::embedding::embedding_texts(&self.input)?;
            }
            // Batch input stays advisory: its executor reads a command
            // from the first text block and ignores the rest.
            JobKind::BatchJob => {}
            // A task routes by its harness, so the required model must name
            // it: an unpinned task would match any agent node, including
            // one running a harness the buyer never asked for. Its deadline
            // must hold the build window, the checks and the slack between
            // them, or a builder that used its window is refunded unpaid.
            JobKind::AgentTask => {
                let spec = crate::agent::parse_agent_task(&self.input)?;
                let label = spec.runtime.label();
                if self.capability_requirement.model_id.as_deref() != Some(label) {
                    return Err(ProtocolError::Invalid(format!(
                        "an agent task for {label} must require model {label:?}, got {:?}",
                        self.capability_requirement.model_id
                    )));
                }
                let build_ms = u64::from(self.capability_requirement.max_duration_secs) * 1_000;
                let check_ms = u64::from(spec.acceptance.timeout_secs) * 1_000;
                let needed_ms = build_ms + check_ms + crate::agent::AGENT_CHECK_SLACK_MS;
                if self.deadline_ms < needed_ms {
                    return Err(ProtocolError::Invalid(format!(
                        "agent task deadline_ms {} cannot cover the {build_ms}ms build window, \
                         the {check_ms}ms checks and the {}ms slack (need at least {needed_ms})",
                        self.deadline_ms,
                        crate::agent::AGENT_CHECK_SLACK_MS
                    )));
                }
            }
            // A check routes by the image it runs in, for the same reason.
            JobKind::AgentCheck => {
                let spec = crate::agent::parse_agent_check(&self.input)?;
                if self.capability_requirement.model_id.as_deref()
                    != Some(spec.acceptance.image.as_str())
                {
                    return Err(ProtocolError::Invalid(format!(
                        "an agent check in {} must require that image as its model, got {:?}",
                        spec.acceptance.image, self.capability_requirement.model_id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A [`JobEnvelopePayload`] signed by the buyer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedJobEnvelope {
    pub payload: JobEnvelopePayload,
    /// The exact bytes (before domain-prefixing) the signature covers.
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl SignedJobEnvelope {
    pub fn sign(
        payload: JobEnvelopePayload,
        buyer_identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        let payload_json = to_canonical_json(&payload)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(buyer_identity, JOB_ENVELOPE_DOMAIN, &payload_json);
        Ok(Self {
            payload,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    /// Verifies the signature against the embedded `payload_json`, that
    /// the signer is the claimed buyer (pinned at signing time so a
    /// relaying coordinator cannot substitute a different signer post
    /// hoc), and that `payload_json` actually decodes back to `payload`
    /// — closing the gap where a caller could hand a validly-signed
    /// blob whose bytes disagree with the struct the rest of the code
    /// reads.
    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.payload.buyer.pubkey_base58() {
            return Err(ProtocolError::Invalid(format!(
                "signer_pubkey_b58 {} does not match buyer.pubkey {}",
                self.signer_pubkey_b58,
                self.payload.buyer.pubkey_base58()
            )));
        }
        verify_domain(
            JOB_ENVELOPE_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: JobEnvelopePayload = serde_json::from_str(&self.payload_json)?;
        if decoded != self.payload {
            return Err(ProtocolError::Invalid(
                "payload_json does not decode to the embedded payload".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::JobKind;
    use covenant_a2a::A2ADuplicateSafety;
    use covenant_mcp::Content;

    fn payload(buyer: &LocalIdentity) -> JobEnvelopePayload {
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: buyer.agent_id(),
            kind: JobKind::InferenceCall,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: Some(8),
                model_id: Some("llama-3-8b".into()),
                kind: JobKind::InferenceCall,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: vec![Content::text("summarize this")],
            price_micro_usdc: 1_000,
            deadline_ms: 60_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "job-key-1"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        }
    }

    #[test]
    fn sign_and_verify_round_trips() {
        let buyer = LocalIdentity::generate("buyer@local");
        let signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        signed.verify().expect("verify");
    }

    #[test]
    fn verify_rejects_tampered_payload_json() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        signed.payload_json = signed.payload_json.replace("1000", "9999999");
        assert!(signed.verify().is_err());
    }

    #[test]
    fn verify_rejects_payload_that_disagrees_with_payload_json() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        // payload_json still verifies against the signature, but the
        // struct field no longer matches it — must be rejected even
        // though the raw signature check alone would pass.
        signed.payload.price_micro_usdc = 999_999;
        assert!(signed.verify().is_err());
    }

    #[test]
    fn a_referral_free_payload_keeps_the_pre_referral_wire_bytes() {
        let buyer = LocalIdentity::generate("buyer@local");
        let signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        assert!(
            !signed.payload_json.contains("referral_code"),
            "None must serialize to the old wire bytes so old signatures keep verifying"
        );
        signed.verify().expect("verify");
    }

    #[test]
    fn a_referred_job_signs_the_attribution_and_rejects_reassignment() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.referral_code = Some("partner-b".into());
        let signed = SignedJobEnvelope::sign(p, &buyer).expect("sign");
        signed.verify().expect("verify");

        // A relay rewriting the attribution must fail verification —
        // the decoded payload_json no longer matches the struct.
        let mut stolen = signed.clone();
        stolen.payload.referral_code = Some("partner-c".into());
        assert!(stolen.verify().is_err());
        let mut stripped = signed;
        stripped.payload.referral_code = None;
        assert!(stripped.verify().is_err());
    }

    #[test]
    fn a_non_streaming_payload_keeps_the_pre_stream_wire_bytes() {
        let buyer = LocalIdentity::generate("buyer@local");
        let signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        assert!(
            !signed.payload_json.contains("stream"),
            "false must serialize to the old wire bytes so old signatures keep verifying"
        );
        signed.verify().expect("verify");
    }

    #[test]
    fn a_streaming_job_signs_the_flag_and_rejects_stripping() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.stream = true;
        let signed = SignedJobEnvelope::sign(p, &buyer).expect("sign");
        assert!(signed.payload_json.contains("\"stream\":true"));
        signed.verify().expect("verify");

        // A relay downgrading the ask must fail verification — the
        // decoded payload_json no longer matches the struct.
        let mut stripped = signed;
        stripped.payload.stream = false;
        assert!(stripped.verify().is_err());
    }

    #[test]
    fn validate_input_refuses_malformed_inference_blocks() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.input = vec![
            Content::text("hi"),
            Content::json(serde_json::json!({ "generation": { "temperature": 99 } })),
        ];
        let err = p.validate_input().expect_err("out-of-range knob");
        assert!(err.to_string().contains("outside 0..=2"), "got: {err}");

        p.input = vec![Content::json(
            serde_json::json!({ "messages": [{ "role": "overlord", "content": "hi" }] }),
        )];
        assert!(p.validate_input().is_err(), "malformed chat must refuse");
    }

    #[test]
    fn validate_input_accepts_well_formed_inference_input() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.input = crate::chat::chat_input(vec![crate::chat::ChatMessage::user("hi")]);
        p.input.push(
            crate::generation_input(crate::GenerationParams {
                seed: Some(7),
                ..Default::default()
            })
            .expect("valid"),
        );
        p.validate_input().expect("well-formed");
    }

    #[test]
    fn validate_input_leaves_batch_input_advisory() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.kind = JobKind::BatchJob;
        p.capability_requirement.kind = JobKind::BatchJob;
        // The same junk an inference job refuses rides along unread on
        // a batch job — its executor never parses these blocks.
        p.input = vec![
            Content::text("echo ok"),
            Content::json(serde_json::json!({ "generation": { "temprature": 9 } })),
        ];
        p.validate_input().expect("batch input is advisory");
    }

    #[test]
    fn validate_input_refuses_an_embedding_with_nothing_to_embed() {
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.kind = JobKind::Embedding;
        p.capability_requirement.kind = JobKind::Embedding;
        // No text to embed: both embedding backends fail this outright, so
        // it must be refused before an operator is matched and faulted for
        // the buyer's malformed request — unlike batch, whose executor
        // reads its input as advisory.
        p.input = vec![Content::json(serde_json::json!({ "note": "no text here" }))];
        let err = p.validate_input().expect_err("nothing to embed");
        assert!(err.to_string().contains("no text to embed"), "got: {err}");

        p.input = vec![Content::text("embed me")];
        p.validate_input()
            .expect("a real text to embed is well-formed");
    }

    #[test]
    fn validate_input_refuses_a_kind_that_disagrees_with_its_requirement() {
        // The smuggling vector: declare `BatchJob` (which skips the
        // inference-input parse) while the requirement asks for
        // `InferenceCall` (which routes the job to an inference
        // operator), carrying malformed chat input. Left unchecked the
        // honest operator's executor would fail the job and eat the
        // fault; the mismatch must be refused before any hold or match.
        let buyer = LocalIdentity::generate("buyer@local");
        let mut p = payload(&buyer);
        p.kind = JobKind::BatchJob;
        p.capability_requirement.kind = JobKind::InferenceCall;
        p.input = vec![Content::json(
            serde_json::json!({ "messages": [{ "role": "overlord", "content": "hi" }] }),
        )];
        let err = p.validate_input().expect_err("kind mismatch");
        assert!(err.to_string().contains("disagrees"), "got: {err}");
    }

    #[test]
    fn validate_input_refuses_a_lease_window_that_disagrees_with_its_requirement() {
        // The underpayment vector: admission and the matcher scale a
        // per-hour lease's price floor by the capability requirement's
        // window, while the escrow and meter bill the terms' window. A
        // crafted envelope whose requirement window is shorter than its
        // terms window clears the operator's per-hour ask at a fraction of
        // the advertised rate, then rents the fuller window the terms grant.
        // The mismatch must be refused before any hold or match, exactly as
        // the kind mismatch is.
        use crate::lease::{lease_input, LeaseTerms, LEASE_DEADLINE_SLACK_MS};
        let buyer = LocalIdentity::generate("buyer@local");
        let terms = LeaseTerms {
            max_duration_secs: 3_600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        };
        let mut p = payload(&buyer);
        p.kind = JobKind::LeaseSession;
        p.capability_requirement.kind = JobKind::LeaseSession;
        p.capability_requirement.max_duration_secs = 60;
        p.price_micro_usdc = terms.max_price_micro_usdc().unwrap();
        p.deadline_ms = terms.max_duration_secs * 1_000 + LEASE_DEADLINE_SLACK_MS + 1_000;
        p.input = vec![lease_input(terms).unwrap()];
        let err = p.validate_input().expect_err("window mismatch");
        assert!(
            err.to_string().contains("does not equal the terms"),
            "got: {err}"
        );

        // The honest envelope sets the requirement window to the terms
        // window, and validates.
        p.capability_requirement.max_duration_secs = 3_600;
        p.input = vec![lease_input(LeaseTerms {
            max_duration_secs: 3_600,
            rate_micro_usdc_per_sec: 100,
            client_public_key: None,
        })
        .unwrap()];
        p.validate_input().expect("matched windows validate");
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let buyer = LocalIdentity::generate("buyer@local");
        let impostor = LocalIdentity::generate("impostor@local");
        let mut signed = SignedJobEnvelope::sign(payload(&buyer), &buyer).expect("sign");
        // The impostor genuinely signs these exact bytes — the raw
        // ed25519 check alone would pass — but the pubkey no longer
        // matches payload.buyer, so the buyer-pin check must still
        // reject it (design-02 3.2: prevents a relaying coordinator
        // from substituting a different signer post hoc).
        let (sig, pk) = crate::sign::sign_domain(
            &impostor,
            crate::sign::JOB_ENVELOPE_DOMAIN,
            &signed.payload_json,
        );
        signed.signature_b58 = sig;
        signed.signer_pubkey_b58 = pk;
        assert!(signed.verify().is_err());
    }
}
