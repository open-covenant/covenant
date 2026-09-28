//! Benchmark-on-register (B5): before the node signs a
//! `CapabilityProfile` and registers it, every claim in that profile is
//! driven through the node's REAL executor — the same `JobExecutor`
//! that will serve buyers. Nothing coordinator-side verifies a profile
//! beyond its signature, so the node refuses to make claims it can't
//! demonstrate to itself: a model that doesn't load, a shell that
//! doesn't run, an executor that hangs, all stop registration instead
//! of surfacing later as a stranger's failed (and refunded) job.
//!
//! Probes do not fail fast: an operator who declared five models and
//! can serve three should see both failures in one boot, not one per
//! restart. The caller (the node binary) records each probe into the
//! node's hash-chained audit log and refuses to register if any failed.

use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_protocol::{
    parse_embedding_output, parse_speech_output, parse_transcription_output, CapabilityRequirement,
    JobEnvelopePayload, JobKind,
};
use covenant_mcp::Content;
use covenant_types::AgentId;
use uuid::Uuid;

use crate::executor::JobExecutor;

/// What one benchmark run probes. The shape is executor-specific and
/// built by the caller, which knows what backend it constructed:
/// deterministic backends (echo, subprocess) get a nonce round-trip —
/// a true known answer — while generative backends get a tiny
/// generation judged on mechanical criteria (Ok status, non-empty
/// output), because asserting exact tokens out of a model is a flaky
/// test of the wrong thing. An [`JobKind::Embedding`] spec is judged on
/// a real vector instead of text, since an embedding model produces no
/// completion.
pub struct BenchmarkSpec {
    /// The job input driven through the executor, verbatim.
    pub input: Vec<Content>,
    /// Substring some text block of the output must contain — the
    /// known answer. `None` means non-empty output is the bar.
    pub expect_contains: Option<String>,
    /// Probe each declared model separately (model-serving backends)
    /// instead of once with no model pinned.
    pub per_model: bool,
    pub kind: JobKind,
}

#[derive(Debug, Clone)]
pub struct ProbeStats {
    pub wall_ms: u64,
    pub tokens_out: Option<u64>,
}

/// One claim, proven or refused. `result` carries the failure reason
/// an operator acts on ("model X: executor error: ...").
#[derive(Debug, Clone)]
pub struct BenchmarkProbe {
    /// The declared model this probe pinned, `None` for the single
    /// no-model probe of a deterministic backend.
    pub model_id: Option<String>,
    pub result: Result<ProbeStats, String>,
}

impl BenchmarkProbe {
    pub fn passed(&self) -> bool {
        self.result.is_ok()
    }
}

fn probe_payload(
    operator: &AgentId,
    spec: &BenchmarkSpec,
    model_id: Option<&str>,
    deadline: Duration,
) -> JobEnvelopePayload {
    JobEnvelopePayload {
        job_id: Uuid::new_v4(),
        // Self-test: the operator is its own buyer. Nothing is priced,
        // escrowed, or receipted — this never leaves the process.
        buyer: operator.clone(),
        kind: spec.kind,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: model_id.map(str::to_string),
            kind: spec.kind,
            max_duration_secs: u32::try_from(deadline.as_secs()).unwrap_or(u32::MAX).max(1),
            min_reputation_bps: None,
        },
        input: spec.input.clone(),
        price_micro_usdc: 0,
        deadline_ms: deadline.as_millis() as u64,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "capability-benchmark"),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    }
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn judge(
    outcome: Result<crate::executor::ExecutionOutcome, crate::executor::ExecutorError>,
    spec: &BenchmarkSpec,
) -> Result<ProbeStats, String> {
    let outcome = outcome.map_err(|e| format!("executor error: {e}"))?;

    // An embedding claim proves itself with a real vector, not text. The
    // result is a `Content::Json` block, so the generative bar below —
    // which reads text blocks — would call every honest embedding output
    // empty. Parse it and require at least one non-empty vector.
    if spec.kind == JobKind::Embedding {
        let result = parse_embedding_output(&outcome.output)
            .map_err(|e| format!("executor returned no usable embedding: {e}"))?;
        if result.embeddings.iter().all(Vec::is_empty) {
            return Err("embedding result carries no vectors".into());
        }
        return Ok(ProbeStats {
            wall_ms: outcome.wall_ms,
            tokens_out: outcome.tokens_out,
        });
    }

    // A transcription claim proves itself with a real transcript, also a
    // `Content::Json` block. The known answer is matched case-insensitively:
    // a speech model capitalizes and punctuates however it hears, so pinning
    // exact casing would fail an honest transcript of the probe clip.
    if spec.kind == JobKind::Transcription {
        let result = parse_transcription_output(&outcome.output)
            .map_err(|e| format!("executor returned no usable transcript: {e}"))?;
        if result.transcript.trim().is_empty() {
            return Err("transcript is empty".into());
        }
        if let Some(expected) = &spec.expect_contains {
            if !result
                .transcript
                .to_ascii_lowercase()
                .contains(&expected.to_ascii_lowercase())
            {
                return Err(format!(
                    "transcript does not contain the known answer {expected:?} (got {:?})",
                    result.transcript.chars().take(80).collect::<String>()
                ));
            }
        }
        return Ok(ProbeStats {
            wall_ms: outcome.wall_ms,
            tokens_out: outcome.tokens_out,
        });
    }

    // A speech claim proves itself with real audio, a `Content::Json` block
    // carrying base64 bytes — no text to substring-match, so the bar is a
    // non-empty clip. A synthesizer that returns nothing (an unknown voice,
    // a backend that wrote no file) fails the claim here rather than at a
    // buyer's paid job.
    if spec.kind == JobKind::SpeechSynthesis {
        let result = parse_speech_output(&outcome.output)
            .map_err(|e| format!("executor returned no usable audio: {e}"))?;
        if result.audio_base64.trim().is_empty() {
            return Err("speech result carries no audio".into());
        }
        return Ok(ProbeStats {
            wall_ms: outcome.wall_ms,
            tokens_out: outcome.tokens_out,
        });
    }

    let text: String = outcome
        .output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            Content::Json { .. } => None,
        })
        .collect();
    if outcome.output.is_empty() || text.trim().is_empty() {
        return Err("executor returned empty output".into());
    }
    if let Some(expected) = &spec.expect_contains {
        if !text.contains(expected.as_str()) {
            return Err(format!(
                "output does not contain the known answer {expected:?} (got {:?})",
                text.chars().take(80).collect::<String>()
            ));
        }
    }
    Ok(ProbeStats {
        wall_ms: outcome.wall_ms,
        tokens_out: outcome.tokens_out,
    })
}

/// Drives every claim through `executor` and reports per-claim. With
/// `spec.per_model` set, one probe per entry of `models_served`;
/// otherwise a single unpinned probe. Never fails fast and never
/// panics — the caller decides what a failed claim means (the node
/// binary refuses to register).
pub async fn run_benchmark<X: JobExecutor>(
    executor: &X,
    operator: &AgentId,
    models_served: &[String],
    spec: &BenchmarkSpec,
    deadline: Duration,
) -> Vec<BenchmarkProbe> {
    let targets: Vec<Option<String>> = if spec.per_model {
        models_served.iter().cloned().map(Some).collect()
    } else {
        vec![None]
    };
    let mut probes = Vec::with_capacity(targets.len());
    for model_id in targets {
        let payload = probe_payload(operator, spec, model_id.as_deref(), deadline);
        let result = judge(executor.execute(&payload, deadline).await, spec);
        probes.push(BenchmarkProbe { model_id, result });
    }
    probes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{EchoExecutor, ExecutionOutcome, ExecutorError};
    use async_trait::async_trait;
    use covenant_compute_protocol::embedding_output;

    struct BrokenExecutor;

    #[async_trait]
    impl JobExecutor for BrokenExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            Err(ExecutorError::Failed("backend unreachable".into()))
        }
    }

    struct SilentExecutor;

    #[async_trait]
    impl JobExecutor for SilentExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            Ok(ExecutionOutcome {
                output: vec![Content::text("   ")],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    /// An embedding backend: hands back one vector per input text, the
    /// `Content::Json` shape a real `/api/embed` node produces.
    struct EmbeddingExecutor;

    #[async_trait]
    impl JobExecutor for EmbeddingExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let texts = covenant_compute_protocol::embedding_texts(&job.input)
                .map_err(|e| ExecutorError::Failed(e.to_string()))?;
            let vectors = vec![vec![0.1_f32, 0.2, 0.3]; texts.len()];
            Ok(ExecutionOutcome {
                output: vec![embedding_output("bench-embed", vectors)],
                wall_ms: 2,
                tokens_in: Some(texts.len() as u64),
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    fn operator() -> AgentId {
        AgentId::new("operator@benchmark", [7u8; 32])
    }

    #[tokio::test]
    async fn a_nonce_round_trip_through_a_real_executor_passes() {
        let spec = BenchmarkSpec {
            input: vec![Content::text("nonce-4471")],
            expect_contains: Some("nonce-4471".into()),
            per_model: false,
            kind: JobKind::BatchJob,
        };
        let probes = run_benchmark(
            &EchoExecutor,
            &operator(),
            &["any".into()],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(probes.len(), 1, "unpinned spec probes exactly once");
        assert!(probes[0].passed());
        assert!(probes[0].model_id.is_none());
    }

    #[tokio::test]
    async fn a_missing_known_answer_fails_the_claim() {
        let spec = BenchmarkSpec {
            input: vec![Content::text("something else entirely")],
            expect_contains: Some("nonce-4471".into()),
            per_model: false,
            kind: JobKind::BatchJob,
        };
        let probes = run_benchmark(
            &EchoExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        let reason = probes[0].result.as_ref().unwrap_err();
        assert!(reason.contains("known answer"), "got: {reason}");
    }

    #[tokio::test]
    async fn every_declared_model_is_probed_and_failures_do_not_stop_the_rest() {
        let spec = BenchmarkSpec {
            input: vec![Content::text("generate")],
            expect_contains: None,
            per_model: true,
            kind: JobKind::InferenceCall,
        };
        let models = vec!["model-a".to_string(), "model-b".to_string()];

        let ok = run_benchmark(
            &EchoExecutor,
            &operator(),
            &models,
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].model_id.as_deref(), Some("model-a"));
        assert_eq!(ok[1].model_id.as_deref(), Some("model-b"));
        assert!(ok.iter().all(BenchmarkProbe::passed));

        let broken = run_benchmark(
            &BrokenExecutor,
            &operator(),
            &models,
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(broken.len(), 2, "no fail-fast: both failures reported");
        assert!(broken.iter().all(|p| !p.passed()));
        assert!(broken[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("backend unreachable"));
    }

    #[tokio::test]
    async fn whitespace_only_output_is_an_empty_claim() {
        let spec = BenchmarkSpec {
            input: vec![Content::text("generate")],
            expect_contains: None,
            per_model: false,
            kind: JobKind::InferenceCall,
        };
        let probes = run_benchmark(
            &SilentExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert!(probes[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("empty output"));
    }

    #[tokio::test]
    async fn an_embedding_claim_is_proven_by_a_real_vector() {
        let spec = BenchmarkSpec {
            input: vec![Content::text("compute embedding benchmark")],
            expect_contains: None,
            per_model: true,
            kind: JobKind::Embedding,
        };
        let probes = run_benchmark(
            &EmbeddingExecutor,
            &operator(),
            &["nomic-embed-text".into()],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(probes.len(), 1, "one probe per declared model");
        assert!(
            probes[0].passed(),
            "a real vector proves the embedding claim: {:?}",
            probes[0].result
        );
        assert_eq!(probes[0].model_id.as_deref(), Some("nomic-embed-text"));
    }

    #[tokio::test]
    async fn an_embedding_probe_rejects_a_text_completion() {
        // A generative backend handed an embedding probe echoes text, not
        // a vector. The claim must fail loudly, not pass on the empty-text
        // path a JSON-blind judge would take.
        let spec = BenchmarkSpec {
            input: vec![Content::text("compute embedding benchmark")],
            expect_contains: None,
            per_model: false,
            kind: JobKind::Embedding,
        };
        let probes = run_benchmark(
            &EchoExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        let reason = probes[0].result.as_ref().unwrap_err();
        assert!(reason.contains("no usable embedding"), "got: {reason}");
    }

    #[tokio::test]
    async fn a_speech_claim_is_proven_by_real_audio() {
        struct SpeechExecutor;
        #[async_trait]
        impl JobExecutor for SpeechExecutor {
            async fn execute(
                &self,
                _job: &JobEnvelopePayload,
                _deadline: Duration,
            ) -> Result<ExecutionOutcome, ExecutorError> {
                Ok(ExecutionOutcome {
                    output: vec![covenant_compute_protocol::speech_output(
                        "bench-say",
                        "UklGRiQAAABXQVZF",
                        "wav",
                        Some(22_050),
                    )],
                    wall_ms: 3,
                    tokens_in: None,
                    tokens_out: None,
                    finish_reason: None,
                })
            }
        }
        let spec = BenchmarkSpec {
            input: covenant_compute_protocol::speech_input(
                covenant_compute_protocol::SpeechInput::new("covenant compute"),
            ),
            expect_contains: None,
            per_model: false,
            kind: JobKind::SpeechSynthesis,
        };
        let probes = run_benchmark(
            &SpeechExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        assert!(
            probes[0].passed(),
            "a real clip proves the speech claim: {:?}",
            probes[0].result
        );
    }

    #[tokio::test]
    async fn a_speech_probe_rejects_a_text_completion() {
        // A generative backend handed a speech probe echoes text, not
        // audio; the claim must fail loudly rather than pass on the
        // JSON-blind text path.
        let spec = BenchmarkSpec {
            input: covenant_compute_protocol::speech_input(
                covenant_compute_protocol::SpeechInput::new("covenant compute"),
            ),
            expect_contains: None,
            per_model: false,
            kind: JobKind::SpeechSynthesis,
        };
        let probes = run_benchmark(
            &EchoExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        let reason = probes[0].result.as_ref().unwrap_err();
        assert!(reason.contains("no usable audio"), "got: {reason}");
    }

    #[tokio::test]
    async fn an_embedding_probe_rejects_a_vectorless_result() {
        struct EmptyEmbeddingExecutor;
        #[async_trait]
        impl JobExecutor for EmptyEmbeddingExecutor {
            async fn execute(
                &self,
                _job: &JobEnvelopePayload,
                _deadline: Duration,
            ) -> Result<ExecutionOutcome, ExecutorError> {
                Ok(ExecutionOutcome {
                    output: vec![embedding_output("bench-embed", vec![])],
                    wall_ms: 1,
                    tokens_in: Some(1),
                    tokens_out: None,
                    finish_reason: None,
                })
            }
        }
        let spec = BenchmarkSpec {
            input: vec![Content::text("compute embedding benchmark")],
            expect_contains: None,
            per_model: false,
            kind: JobKind::Embedding,
        };
        let probes = run_benchmark(
            &EmptyEmbeddingExecutor,
            &operator(),
            &[],
            &spec,
            Duration::from_secs(5),
        )
        .await;
        let reason = probes[0].result.as_ref().unwrap_err();
        assert!(reason.contains("no vectors"), "got: {reason}");
    }
}
