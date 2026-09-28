//! The OpenAI-compatible endpoint end to end: a real `openai_router`
//! served on a loopback port, driven by a plain HTTP client the way an
//! OpenAI SDK would, against a real coordinator and a real serving node.
//! Proves a `POST /v1/chat/completions` buys a job on the network, pays
//! it, and returns a standard `chat.completion` carrying the operator's
//! verified receipt — plus the auth, model-listing, and refusal edges an
//! OpenAI client hits.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_buyer::{
    anthropic_router, claim_deposit, gemini_router, http_client, openai_router, BuyerConfig,
    OpenAiState, SpendCaps,
};
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, VerifiedDeposit,
};
use covenant_compute_node::{
    ChunkSink, Coordinator as _, ExecutionOutcome, ExecutorError, HttpCoordinatorClient,
    InMemoryEarningsLedger, JobExecutor, Node, NodeConfig, OllamaExecutor, DEFAULT_OLLAMA_URL,
};
use covenant_compute_protocol::{
    embedding_output, embedding_texts, parse_generation_params, parse_speech_input,
    parse_transcription_input, speech_output, transcription_output, CapabilityProfile,
    FinishReason, FundingSource, HardwareClass, JobEnvelopePayload, JobKind, PriceAsk, PriceUnit,
    RegisterRequest, ResponseFormat, TranscriptionSegment,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde_json::{json, Value};

const SERVED_MODEL: &str = "qwen2.5:0.5b";
const EMBED_MODEL: &str = "nomic-embed-text";

/// A fixed float32 vector every embedding text maps to. The values are
/// exactly representable, so the base64 round-trip the OpenAI SDKs run is
/// bit-for-bit checkable.
const EMBED_VECTOR: [f32; 3] = [0.5, -0.25, 0.125];

/// Answers a chat job with a fixed assistant reply (and token counts, so
/// the endpoint's usage is meaningful — the echo executor would hand back
/// the packed chat JSON, which carries no text), and an embedding job
/// with one [`EMBED_VECTOR`] per input text.
#[derive(Clone, Copy)]
struct ReplyExecutor;

#[async_trait]
impl JobExecutor for ReplyExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if job.kind == JobKind::Embedding {
            let texts =
                embedding_texts(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
            let vectors = vec![EMBED_VECTOR.to_vec(); texts.len()];
            return Ok(ExecutionOutcome {
                output: vec![embedding_output(EMBED_MODEL, vectors)],
                wall_ms: 1,
                tokens_in: Some(texts.len() as u64 * 4),
                tokens_out: None,
                finish_reason: None,
            });
        }
        Ok(ExecutionOutcome {
            output: vec![Content::text("the assistant reply")],
            wall_ms: 1,
            tokens_in: Some(7),
            tokens_out: Some(3),
            finish_reason: None,
        })
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let _ = sink.send("the assistant reply".to_string()).await;
        self.execute(job, deadline).await
    }
}

/// Records the input of the job it runs, so a test can prove what an
/// endpoint request actually signed into the envelope; answers like
/// [`ReplyExecutor`] otherwise.
#[derive(Clone)]
struct CapturingExecutor {
    seen: Arc<std::sync::Mutex<Option<Vec<Content>>>>,
}

#[async_trait]
impl JobExecutor for CapturingExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        *self.seen.lock().unwrap() = Some(job.input.clone());
        ReplyExecutor.execute(job, deadline).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        *self.seen.lock().unwrap() = Some(job.input.clone());
        ReplyExecutor.execute_streaming(job, deadline, sink).await
    }
}

/// A node that produces a verified result but cannot stream: it emits no
/// live chunks, only the final output. The buyer's streaming path must
/// still deliver that output to the client rather than a paid-for empty
/// completion.
#[derive(Clone, Copy)]
struct SilentStreamExecutor;

#[async_trait]
impl JobExecutor for SilentStreamExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        ReplyExecutor.execute(job, deadline).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        _sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.execute(job, deadline).await
    }
}

/// Returns one fewer embedding than it was asked for: a nonconforming
/// operator whose output still hashes and verifies, but whose vector count
/// disagrees with the input. The buyer must reject it rather than pair
/// vectors to inputs by position.
#[derive(Clone, Copy)]
struct ShortEmbedExecutor;

#[async_trait]
impl JobExecutor for ShortEmbedExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let texts =
            embedding_texts(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        let vectors = vec![EMBED_VECTOR.to_vec(); texts.len().saturating_sub(1)];
        Ok(ExecutionOutcome {
            output: vec![embedding_output(EMBED_MODEL, vectors)],
            wall_ms: 1,
            tokens_in: Some(texts.len() as u64 * 4),
            tokens_out: None,
            finish_reason: None,
        })
    }
}

/// Answers exactly like [`ReplyExecutor`] but reports the generation was
/// cut off by the token limit — what a backend signals (Ollama
/// `done_reason: "length"`, OpenAI `finish_reason: "length"`) when
/// `max_tokens` truncates the output.
#[derive(Clone, Copy)]
struct TruncatedExecutor;

#[async_trait]
impl JobExecutor for TruncatedExecutor {
    async fn execute(
        &self,
        _job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        Ok(ExecutionOutcome {
            output: vec![Content::text("the assistant reply")],
            wall_ms: 1,
            tokens_in: Some(7),
            tokens_out: Some(3),
            finish_reason: Some(FinishReason::Length),
        })
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let _ = sink.send("the assistant reply".to_string()).await;
        self.execute(job, deadline).await
    }
}

/// Streams only a prefix of its output, then completes with the full
/// result — a relay that lost the tail, or a node whose live feed ran
/// short of what it ultimately signed. The endpoint must complete the
/// client's feed from the verified output, not present the short prefix
/// as a finished answer.
#[derive(Clone, Copy)]
struct PartialStreamExecutor;

#[async_trait]
impl JobExecutor for PartialStreamExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        ReplyExecutor.execute(job, deadline).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        // The full output is "the assistant reply"; the live feed stops
        // after the prefix "the assistant".
        let _ = sink.send("the assistant".to_string()).await;
        self.execute(job, deadline).await
    }
}

/// Answers a chat job with a single tool call and no prose, the shape a
/// real backend returns when the model decides to call a tool. The calls
/// ride the attested output via `assistant_output`, so the endpoint reads
/// them straight off the verified receipt.
#[derive(Clone, Copy)]
struct ToolCallExecutor;

#[async_trait]
impl JobExecutor for ToolCallExecutor {
    async fn execute(
        &self,
        _job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        use covenant_compute_protocol::{assistant_output, FunctionCall, ToolCall, ToolCallKind};
        let call = ToolCall {
            id: "call_0".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Paris"}"#.into(),
            },
        };
        Ok(ExecutionOutcome {
            output: assistant_output(String::new(), vec![call]),
            wall_ms: 1,
            tokens_in: Some(9),
            tokens_out: Some(5),
            finish_reason: Some(FinishReason::ToolCalls),
        })
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        _sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        // A tools job never streams the backend, so the node relays no live
        // chunks; the endpoint emits the calls from the verified output.
        self.execute(job, deadline).await
    }
}

/// Answers a chat job with prose plus per-token log probabilities in the
/// attested output — the shape a backend returns when the buyer asks for
/// logprobs. The node runs such a job non-streaming, so the probabilities
/// ride the verified receipt.
#[derive(Clone, Copy)]
struct LogprobsExecutor;

#[async_trait]
impl JobExecutor for LogprobsExecutor {
    async fn execute(
        &self,
        _job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        use covenant_compute_protocol::{
            assistant_output, logprobs_block, TokenLogprob, TopLogprob,
        };
        let mut output = assistant_output("Hi".into(), Vec::new());
        output.push(logprobs_block(vec![TokenLogprob {
            token: "Hi".into(),
            logprob: -0.25,
            bytes: Some(vec![72, 105]),
            top_logprobs: vec![TopLogprob {
                token: "Hi".into(),
                logprob: -0.25,
                bytes: Some(vec![72, 105]),
            }],
        }]));
        Ok(ExecutionOutcome {
            output,
            wall_ms: 1,
            tokens_in: Some(4),
            tokens_out: Some(1),
            finish_reason: Some(FinishReason::Stop),
        })
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        _sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        // A logprobs job runs the backend non-streaming, so no live chunks;
        // the endpoint emits the probabilities from the verified output.
        self.execute(job, deadline).await
    }
}

/// Fails its first job and answers the rest like [`ReplyExecutor`]: one
/// operator flaking on a single job of a fan-out while the others complete.
/// The buyer must return only the completions that settled and charge for
/// exactly those, releasing the flaked job's hold.
#[derive(Clone)]
struct FlakyOnceExecutor {
    failed: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl JobExecutor for FlakyOnceExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(ExecutorError::Failed("flaking on the first job".into()));
        }
        ReplyExecutor.execute(job, deadline).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let _ = sink;
        self.execute(job, deadline).await
    }
}

/// A live market: a coordinator with a claimable deposit rail plus one
/// registered node serving `SERVED_MODEL` through [`ReplyExecutor`].
/// Answers a transcription job with a fixed transcript, after checking the
/// buyer's audio actually arrived — the endpoint's job is to carry the
/// upload to the operator and shape the operator's text back.
#[derive(Clone, Copy)]
struct TranscribeExecutor;

#[async_trait]
impl JobExecutor for TranscribeExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let request = parse_transcription_input(&job.input)
            .map_err(|e| ExecutorError::Failed(e.to_string()))?;
        assert!(
            !request.audio_base64.is_empty(),
            "the uploaded audio reached the operator"
        );
        assert!(
            !request.translate,
            "/v1/audio/transcriptions transcribes in the source language, never translates"
        );
        // Honor a timestamps ask, so the endpoint's verbose_json/srt/vtt
        // rendering is exercised through a real coordinator and node.
        let segments = request.timestamps.then(|| {
            vec![
                TranscriptionSegment {
                    start_ms: 0,
                    end_ms: 900,
                    text: "hello from".into(),
                },
                TranscriptionSegment {
                    start_ms: 900,
                    end_ms: 1_500,
                    text: "the network".into(),
                },
            ]
        });
        Ok(ExecutionOutcome {
            output: vec![transcription_output(
                "whisper-1",
                "hello from the network",
                Some("en".into()),
                segments,
            )],
            wall_ms: 3,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }
}

/// The translations counterpart: proves the endpoint asked the operator
/// for an English rendering, then returns one. A translation job that
/// arrived without the translate flag set is the endpoint dropping the
/// one thing that distinguishes the two audio routes.
#[derive(Clone, Copy)]
struct TranslateExecutor;

#[async_trait]
impl JobExecutor for TranslateExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let request = parse_transcription_input(&job.input)
            .map_err(|e| ExecutorError::Failed(e.to_string()))?;
        assert!(
            request.translate,
            "/v1/audio/translations must set the translate flag on the job"
        );
        Ok(ExecutionOutcome {
            output: vec![transcription_output(
                "whisper-1",
                "hello from the network in english",
                Some("en".into()),
                None,
            )],
            wall_ms: 3,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }
}

/// The fixed clip the speech executor hands back. Not a real WAV — the
/// endpoint's contract is to carry whatever bytes the operator synthesized
/// straight through, so the test asserts the body is exactly these bytes.
const SYNTH_AUDIO: &[u8] = b"RIFF\x00\x00\x00\x00WAVEfake-pcm-audio-bytes";

/// Answers a speech-synthesis job with [`SYNTH_AUDIO`], after checking the
/// buyer's text and options actually arrived — the endpoint's job is to
/// carry the request to the operator and shape the operator's audio back.
#[derive(Clone, Copy)]
struct SpeakExecutor;

#[async_trait]
impl JobExecutor for SpeakExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        use base64::Engine as _;
        let request =
            parse_speech_input(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        assert!(
            !request.text.trim().is_empty(),
            "the text to speak reached the operator"
        );
        // A stock OpenAI voice ("alloy") is resolved to the node default at
        // the front door, so the operator sees no voice to honor.
        assert!(
            request.voice.is_none(),
            "a stock OpenAI voice becomes the node default"
        );
        assert_eq!(
            request.format.as_deref(),
            Some("wav"),
            "the requested container reached the operator"
        );
        let audio_base64 = base64::engine::general_purpose::STANDARD.encode(SYNTH_AUDIO);
        Ok(ExecutionOutcome {
            output: vec![speech_output("say-1", audio_base64, "wav", Some(22_050))],
            wall_ms: 4,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }
}

/// A minimal hand-rolled `multipart/form-data` body: one `file` part with
/// the audio, then a text part per extra field. Avoids a multipart client
/// dependency in the test.
fn multipart_audio(boundary: &str, audio: &[u8], fields: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"clip.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(audio);
    body.extend_from_slice(b"\r\n");
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

struct Rig {
    url: String,
    rail: Arc<MockRail>,
    _coordinator_home: tempfile::TempDir,
}

impl Rig {
    async fn launch() -> Self {
        Self::launch_with_executor(ReplyExecutor).await
    }

    async fn launch_with_executor<X: JobExecutor + 'static>(executor: X) -> Self {
        Self::launch_with_executor_serving(executor, vec![SERVED_MODEL.into(), EMBED_MODEL.into()])
            .await
    }

    async fn launch_with_executor_serving<X: JobExecutor + 'static>(
        executor: X,
        models: Vec<String>,
    ) -> Self {
        let coordinator_home = tempfile::tempdir().unwrap();
        let rail = Arc::new(MockRail::new());
        let state = CoordinatorState::with_journal(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig {
                long_poll_timeout: Duration::from_secs(5),
                default_funding_source: FundingSource::Organic,
                ..CoordinatorConfig::default()
            },
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            &coordinator_home.path().join("journal.jsonl"),
            Some(rail.clone()),
        )
        .await
        .unwrap();
        let coordinator_pubkey = state.coordinator_pubkey_b58();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, compute_router(state)).await.unwrap();
        });

        let operator_identity = LocalIdentity::generate("operator@test");
        let profile = CapabilityProfile {
            operator: operator_identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: models,
            job_kinds: vec![
                JobKind::InferenceCall,
                JobKind::Embedding,
                JobKind::Transcription,
                JobKind::SpeechSynthesis,
            ],
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 10_000,
            },
            tee_capable: false,
        };
        let client = Arc::new(HttpCoordinatorClient::with_config(
            url.clone(),
            Duration::from_secs(5),
            2,
        ));
        client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap(),
            )
            .await
            .unwrap();
        let node = Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(executor),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey,
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(1),
                fee_bps: 0,
            },
        );
        tokio::spawn(async move {
            loop {
                match node.run_once().await {
                    Ok(Some(_)) => {}
                    _ => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        });

        Self {
            url,
            rail,
            _coordinator_home: coordinator_home,
        }
    }
}

fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

/// A funded, served endpoint listening on a loopback port, with the
/// bearer token it requires. Holds the coordinator rig alive.
struct Endpoint {
    base: String,
    api_key: String,
    _rig: Rig,
}

impl Endpoint {
    async fn launch() -> Self {
        Self::launch_with_session_cap(None).await
    }

    async fn launch_with_session_cap(session_cap: Option<u64>) -> Self {
        Self::from_rig(Rig::launch().await, session_cap).await
    }

    async fn launch_with_executor<X: JobExecutor + 'static>(executor: X) -> Self {
        Self::from_rig(Rig::launch_with_executor(executor).await, None).await
    }

    async fn launch_with_executor_serving<X: JobExecutor + 'static>(
        executor: X,
        models: Vec<String>,
    ) -> Self {
        Self::from_rig(
            Rig::launch_with_executor_serving(executor, models).await,
            None,
        )
        .await
    }

    async fn from_rig(rig: Rig, session_cap: Option<u64>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let identity =
            LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute")
                .unwrap();
        rig.rail.preload(VerifiedDeposit {
            deposit_id: "openai-deposit".into(),
            buyer_pubkey_b58: identity.agent_id().pubkey_base58(),
            amount_micro_usdc: 100_000,
        });
        let buyer = BuyerConfig {
            coordinator_url: rig.url.clone(),
            poll_interval: Duration::from_millis(100),
            referral_code: None,
            rpc_url: None,
        };
        claim_deposit(&http_client(), &buyer, &identity, "openai-deposit")
            .await
            .expect("the buyer funds itself before serving");

        let state = Arc::new(OpenAiState {
            http: http_client(),
            buyer,
            identity,
            caps: Arc::new(SpendCaps::new(100_000, session_cap)),
            default_deadline_ms: 5_000,
            api_key: Some("sk-covenant-test".into()),
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        // The deployed binary serves every dialect on one address; the
        // harness mirrors that so `/v1/messages` and `/v1beta/models/...` are
        // exercised against the same funded rig as `/v1/chat/completions`.
        let router = openai_router(Arc::clone(&state))
            .merge(anthropic_router(Arc::clone(&state)))
            .merge(gemini_router(state));
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        Self {
            base,
            api_key: "sk-covenant-test".into(),
            _rig: rig,
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    async fn chat(&self, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }

    async fn embed(&self, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!("{}/v1/embeddings", self.base))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }

    async fn complete(&self, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!("{}/v1/completions", self.base))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }

    /// Post to the Anthropic `/v1/messages` endpoint with the `x-api-key`
    /// header a real Anthropic client sends, proving that credential path.
    async fn messages(&self, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }

    async fn responses(&self, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!("{}/v1/responses", self.base))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }

    /// Post to the Gemini `generateContent` endpoint with the `x-goog-api-key`
    /// header a real Google GenAI client sends, proving that credential path.
    async fn generate_content(&self, model: &str, body: Value) -> reqwest::Response {
        Self::client()
            .post(format!(
                "{}/v1beta/models/{model}:generateContent",
                self.base
            ))
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .expect("the endpoint answers")
    }
}

#[tokio::test]
async fn a_chat_completion_buys_a_verified_job_and_returns_openai_shape() {
    let endpoint = Endpoint::launch().await;

    // Health needs no auth.
    let health = Endpoint::client()
        .get(format!("{}/health", endpoint.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.json::<Value>().await.unwrap()["status"], "ok");

    // The model directory lists the served model as an OpenAI model.
    let models: Value = Endpoint::client()
        .get(format!("{}/v1/models", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["object"], "list");
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == SERVED_MODEL),
        "the served model is listed: {models}"
    );

    // The real buy: a standard chat request, no Covenant-specific fields.
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hello" },
            ],
            "temperature": 0,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "chat.completion");
    assert!(
        body["id"].as_str().unwrap().starts_with("chatcmpl-"),
        "id: {}",
        body["id"]
    );
    assert_eq!(body["model"], SERVED_MODEL);
    assert_eq!(body["choices"][0]["index"], 0);
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "the assistant reply"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 7);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    assert_eq!(body["usage"]["total_tokens"], 10);

    // The Covenant extension carries the verified receipt an OpenAI
    // client ignores but a Covenant-aware one checks.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["covenant"]["price_micro_usdc"], 10_000);
    assert!(body["covenant"]["job_id"].is_string());
    assert!(body["covenant"]["operator_pubkey_b58"].is_string());
}

#[tokio::test]
async fn a_chat_completion_accepts_array_content_and_the_developer_role() {
    // The message shapes a current OpenAI SDK sends by default: content as
    // an array of typed parts, and a `developer` instruction in place of
    // `system`. Both must buy a job, not 400 on an opaque parse error.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [
                { "role": "developer", "content": [{ "type": "text", "text": "be terse" }] },
                { "role": "user", "content": [{ "type": "text", "text": "hello" }] },
            ],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "the assistant reply"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn a_response_format_reaches_the_signed_job() {
    // An OpenAI client asking for structured output must have that
    // constraint travel all the way into the signed, paid envelope — not be
    // silently dropped so the buyer pays for free-form text.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "list three colors" }],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "colors",
                    "schema": { "type": "object" },
                    "strict": true
                }
            },
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let params = parse_generation_params(&input)
        .expect("well-formed")
        .expect("a generation block rides the signed job");
    assert_eq!(
        params.response_format,
        Some(ResponseFormat::JsonSchema {
            name: "colors".into(),
            schema: json!({ "type": "object" }),
            strict: Some(true),
        })
    );
}

#[tokio::test]
async fn an_inline_image_reaches_the_signed_job() {
    use covenant_compute_protocol::parse_chat_input;
    // An OpenAI client attaching an image must have those bytes travel into
    // the signed, paid envelope, so the operator's vision model sees exactly
    // what the buyer paid to have looked at.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "what is in this image?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,aGVsbG8=" } },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let messages = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    assert_eq!(messages[0].content, "what is in this image?");
    assert_eq!(messages[0].images, vec!["aGVsbG8=".to_string()]);
}

#[tokio::test]
async fn a_remote_image_url_refuses_in_openai_shape() {
    // The network never fetches a buyer's url; an inline data URI is the
    // only image form, and a remote url earns a clean refusal.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image_url", "image_url": { "url": "https://example.com/cat.png" } },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("remote image url"),
        "got: {body}"
    );
}

#[tokio::test]
async fn an_audio_upload_buys_a_verified_transcription() {
    let endpoint =
        Endpoint::launch_with_executor_serving(TranscribeExecutor, vec!["whisper-1".into()]).await;
    let boundary = "computeAudioBoundary";

    // The default json format: `{"text": …}` plus the covenant receipt.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["text"], "hello from the network");
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert!(body["covenant"]["job_id"].is_string());

    // response_format=text returns the bare transcript, no JSON envelope.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1"), ("response_format", "text")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello from the network");
}

#[tokio::test]
async fn a_transcription_refuses_a_steering_prompt() {
    // OpenAI's transcription `prompt` biases the model toward a wording or
    // spelling; this network transcribes the audio as spoken and carries no
    // prompt, so a request that sets one is refused rather than transcribed
    // unsteered and billed as if the prompt had shaped the text. A served
    // whisper model is present, so the refusal is about the field, not a
    // missing operator.
    let endpoint =
        Endpoint::launch_with_executor_serving(TranscribeExecutor, vec!["whisper-1".into()]).await;
    let boundary = "computeAudioBoundary";
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[
                ("model", "whisper-1"),
                ("prompt", "The speaker discusses USDC and Solana."),
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("prompt"),
        "the refusal names the field: {body}"
    );
    // A blank prompt is not a steering request and still transcribes.
    let plain = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1"), ("prompt", "")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), 200, "a blank prompt is not a refusal");
}

#[tokio::test]
async fn a_timestamped_upload_returns_segments_and_subtitles() {
    let endpoint =
        Endpoint::launch_with_executor_serving(TranscribeExecutor, vec!["whisper-1".into()]).await;
    let boundary = "computeAudioBoundary";

    // verbose_json: the transcript plus OpenAI-shaped segments in float
    // seconds, carried through a real coordinator and node.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1"), ("response_format", "verbose_json")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["task"], "transcribe");
    assert_eq!(body["text"], "hello from the network");
    let segments = body["segments"].as_array().expect("a segments array");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["id"], 0);
    assert_eq!(segments[0]["start"], 0.0);
    assert_eq!(segments[0]["end"], 0.9);
    assert_eq!(segments[0]["text"], "hello from");
    assert_eq!(segments[1]["start"], 0.9);
    assert_eq!(body["covenant"]["receipt_verified"], true);

    // srt: numbered cues with comma-millisecond timings, a subtitle file the
    // client saves straight to disk.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1"), ("response_format", "srt")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let srt = resp.text().await.unwrap();
    assert!(
        srt.starts_with("1\n00:00:00,000 --> 00:00:00,900\nhello from\n\n"),
        "{srt}"
    );
    assert!(
        srt.contains("2\n00:00:00,900 --> 00:00:01,500\nthe network"),
        "{srt}"
    );
}

#[tokio::test]
async fn an_audio_upload_buys_a_verified_translation() {
    let endpoint =
        Endpoint::launch_with_executor_serving(TranslateExecutor, vec!["whisper-1".into()]).await;
    let boundary = "computeAudioBoundary";

    // /v1/audio/translations carries the same upload but asks for English:
    // the job reaches the operator with translate set (asserted operator
    // side), and the English transcript rides back with the covenant receipt.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/translations", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["text"], "hello from the network in english");
    assert_eq!(body["covenant"]["receipt_verified"], true);

    // response_format=text returns the bare English transcript.
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/translations", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1"), ("response_format", "text")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "hello from the network in english"
    );
}

#[tokio::test]
async fn a_transcription_for_an_unserved_model_refuses_before_dispatch() {
    // The default rig serves chat and embeddings, not a whisper model, so a
    // transcription upload naming one has no operator. The endpoint refuses
    // at the model check — the SDK-correct 404 — rather than dispatch a
    // doomed job.
    let endpoint = Endpoint::launch().await;
    let boundary = "computeAudioBoundary";
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/transcriptions", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_audio(
            boundary,
            b"fake-wav-bytes",
            &[("model", "whisper-1")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "{}", resp.text().await.unwrap());
}

#[tokio::test]
async fn speech_synthesis_returns_audio_and_a_receipt_header() {
    let endpoint =
        Endpoint::launch_with_executor_serving(SpeakExecutor, vec!["say-1".into()]).await;

    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/speech", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .json(&json!({
            "model": "say-1",
            "input": "covenant compute speaks",
            "voice": "alloy",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("audio/wav"),
        "the client is told the container it received"
    );
    // The verified receipt rides a header, so the body stays pure audio a
    // plain OpenAI client plays unmodified.
    let receipt: Value = serde_json::from_str(
        resp.headers()
            .get("x-covenant-receipt")
            .and_then(|v| v.to_str().ok())
            .expect("the receipt header is present"),
    )
    .expect("the receipt header is JSON");
    assert_eq!(receipt["receipt_verified"], true);
    assert!(receipt["job_id"].is_string());

    let body = resp.bytes().await.unwrap();
    assert_eq!(
        body.as_ref(),
        SYNTH_AUDIO,
        "the operator's exact audio bytes come back as the body"
    );
}

#[tokio::test]
async fn speech_synthesis_refuses_an_unsupported_container_before_dispatch() {
    // mp3 is OpenAI's default, but no operator on this network produces it;
    // the endpoint refuses with a clear 400 rather than dispatch a doomed
    // job or return WAV bytes mislabeled as mp3.
    let endpoint =
        Endpoint::launch_with_executor_serving(SpeakExecutor, vec!["say-1".into()]).await;
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/speech", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .json(&json!({
            "model": "say-1",
            "input": "hello",
            "response_format": "mp3",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("wav"),
        "the refusal names the supported container: {body}"
    );
}

#[tokio::test]
async fn speech_synthesis_refuses_free_text_voice_instructions() {
    // `gpt-4o-mini-tts`'s `instructions` steer a voice's tone and delivery;
    // this network's synthesizers take a named voice and a speed and nothing
    // more, so a request carrying instructions is refused rather than voiced
    // plainly and billed as if it had followed them. A served speech model is
    // present, so the refusal is about the field, not a missing operator.
    let endpoint =
        Endpoint::launch_with_executor_serving(SpeakExecutor, vec!["say-1".into()]).await;
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/speech", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .json(&json!({
            "model": "say-1",
            "input": "hello",
            "instructions": "Speak in a cheerful, upbeat tone.",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("instructions"),
        "the refusal names the field: {body}"
    );
    // A blank instructions string is not a steering request and passes.
    let plain = Endpoint::client()
        .post(format!("{}/v1/audio/speech", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .json(&json!({
            "model": "say-1",
            "input": "hello",
            "instructions": "",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), 200, "a blank instructions is not a refusal");
}

#[tokio::test]
async fn speech_synthesis_for_an_unserved_model_refuses_before_dispatch() {
    // The default rig serves chat and embeddings, not a speech model, so a
    // synthesis request naming one has no operator and is refused at the
    // model check rather than dispatched.
    let endpoint = Endpoint::launch().await;
    let resp = Endpoint::client()
        .post(format!("{}/v1/audio/speech", endpoint.base))
        .header("authorization", format!("Bearer {}", endpoint.api_key))
        .json(&json!({ "model": "say-1", "input": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "{}", resp.text().await.unwrap());
}

#[tokio::test]
async fn a_json_schema_without_a_schema_refuses_in_openai_shape() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
            "response_format": { "type": "json_schema", "json_schema": { "name": "x" } },
        }))
        .await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires a schema"),
        "got: {body}"
    );
}

#[tokio::test]
async fn a_streaming_chat_completion_relays_chunks_then_done() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "stream please" }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/event-stream"),
        "content-type is an event stream"
    );
    let body = resp.text().await.unwrap();

    // The OpenAI SSE shape: an assistant-role opener, the content delta,
    // a terminal finish_reason, then the [DONE] sentinel.
    assert!(
        body.contains("\"object\":\"chat.completion.chunk\""),
        "chunks are chat.completion.chunk: {body}"
    );
    assert!(
        body.contains("\"role\":\"assistant\""),
        "opens with the assistant role: {body}"
    );
    assert!(
        body.contains("\"content\":\"the assistant reply\""),
        "relays the generated content: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"stop\""),
        "ends with a stop reason: {body}"
    );
    assert!(
        body.contains("\"covenant\":") && body.contains("\"receipt_verified\":true"),
        "the verified receipt rides the closing frame, so a streamed job is \
         as verifiable as a non-streamed one: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_truncated_completion_reports_finish_reason_length() {
    let endpoint = Endpoint::launch_with_executor(TruncatedExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "write forever" }],
            "max_tokens": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["finish_reason"], "length",
        "a completion the backend truncated reports length, not stop: {body}"
    );
}

#[tokio::test]
async fn a_streaming_truncated_completion_reports_finish_reason_length() {
    let endpoint = Endpoint::launch_with_executor(TruncatedExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "write forever" }],
            "stream": true,
            "max_tokens": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"finish_reason\":\"length\""),
        "the terminal stream frame reports length: {body}"
    );
    assert!(
        !body.contains("\"finish_reason\":\"stop\""),
        "and never a stop for a truncated generation: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_chat_completion_with_logprobs_returns_them_in_openai_shape() {
    let endpoint = Endpoint::launch_with_executor(LogprobsExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "logprobs": true,
            "top_logprobs": 1,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let entry = &body["choices"][0]["logprobs"]["content"][0];
    assert_eq!(entry["token"], "Hi", "the attested token surfaces: {body}");
    assert_eq!(entry["logprob"], -0.25);
    assert_eq!(entry["bytes"], json!([72, 105]));
    assert_eq!(
        entry["top_logprobs"][0]["token"], "Hi",
        "the alternatives ride along: {body}"
    );
}

#[tokio::test]
async fn a_chat_completion_without_logprobs_reports_them_as_null() {
    // A plain request must carry the OpenAI-shaped `logprobs: null`, not
    // omit the key, so an SDK that reads it never trips.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["choices"][0]["logprobs"].is_null(),
        "no logprobs requested, so the field is null: {body}"
    );
}

#[tokio::test]
async fn top_logprobs_without_logprobs_true_is_refused() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "top_logprobs": 3,
        }))
        .await;
    assert_eq!(resp.status(), 400, "top_logprobs needs logprobs: true");
}

#[tokio::test]
async fn a_logit_bias_is_refused_rather_than_silently_dropped() {
    // The network forwards no bias to the backend, so honouring a bias
    // map would be a lie; refuse before pricing rather than charge for a
    // completion that ignored it. An empty map is the OpenAI no-op and
    // must still buy a job.
    let endpoint = Endpoint::launch().await;
    let biased = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "logit_bias": { "1734": -100 },
        }))
        .await;
    assert_eq!(biased.status(), 400);
    let body: Value = biased.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("logit_bias"),
        "names the field: {body}"
    );
    let empty = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "logit_bias": {},
        }))
        .await;
    assert_eq!(
        empty.status(),
        200,
        "an empty bias is a no-op, not a refusal"
    );
}

#[tokio::test]
async fn a_reasoning_effort_is_refused_rather_than_silently_dropped() {
    // The wire carries no reasoning-effort control, so honouring one would
    // be a lie; refuse before pricing rather than charge for a completion
    // that ignored it, the same way the Responses door refuses `reasoning`
    // and the Anthropic door refuses `thinking`. Omitting it buys a job.
    let endpoint = Endpoint::launch().await;
    let asked = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "reasoning_effort": "high",
        }))
        .await;
    assert_eq!(asked.status(), 400);
    let body: Value = asked.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reasoning_effort"),
        "names the field: {body}"
    );
    let omitted = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .await;
    assert_eq!(
        omitted.status(),
        200,
        "omitting reasoning_effort is not a refusal"
    );
}

#[tokio::test]
async fn a_non_text_modality_is_refused() {
    // Audio output is a separate speech endpoint the chat path never routes
    // to, so asking chat for audio is refused rather than billed as a
    // text-only reply. `["text"]` is the default and buys a job.
    let endpoint = Endpoint::launch().await;
    let audio = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "modalities": ["text", "audio"],
        }))
        .await;
    assert_eq!(audio.status(), 400);
    let body: Value = audio.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("modality"),
        "names the modality: {body}"
    );
    let text_only = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "modalities": ["text"],
        }))
        .await;
    assert_eq!(text_only.status(), 200, "text is the default modality");
}

#[tokio::test]
async fn a_web_search_request_is_refused() {
    // The network's models have no web access, so grounding an answer in a
    // live search is refused rather than answered from weights and billed as
    // if it had searched.
    let endpoint = Endpoint::launch().await;
    let grounded = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "today's headlines?" }],
            "web_search_options": { "search_context_size": "medium" },
        }))
        .await;
    assert_eq!(grounded.status(), 400);
    let body: Value = grounded.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("web_search_options"),
        "names the field: {body}"
    );
}

#[tokio::test]
async fn parallel_tool_calls_false_is_refused_only_when_tools_are_in_play() {
    // `false` is a one-call-per-turn guarantee the backend can't be held
    // to, so with tools present it is refused. Without tools it is
    // meaningless — nothing to parallelise — and passes, the same way
    // OpenAI ignores it there.
    let endpoint = Endpoint::launch().await;
    let with_tools = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "weather?" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": { "type": "object", "properties": {} }
                }
            }],
            "parallel_tool_calls": false,
        }))
        .await;
    assert_eq!(with_tools.status(), 400);
    let body: Value = with_tools.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("parallel_tool_calls"),
        "names the field: {body}"
    );
    let no_tools = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "parallel_tool_calls": false,
        }))
        .await;
    assert_eq!(
        no_tools.status(),
        200,
        "with no tools the flag is a no-op, not a refusal"
    );
}

#[tokio::test]
async fn a_streaming_chat_with_logprobs_emits_them_from_the_receipt() {
    let endpoint = Endpoint::launch_with_executor(LogprobsExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": true,
            "logprobs": true,
            "top_logprobs": 1,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"logprobs\":{\"content\":"),
        "the stream carries a logprobs frame from the verified receipt: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_streaming_chat_against_a_node_that_cannot_stream_still_returns_the_paid_output() {
    // The operator completes the job (a verified receipt over the real
    // output) but relays no live chunks. The client asked for a stream and
    // is charged, so the endpoint must backfill the verified output rather
    // than close a well-formed but empty completion.
    let endpoint = Endpoint::launch_with_executor(SilentStreamExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "stream please" }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"content\":\"the assistant reply\""),
        "an empty live feed is backfilled from the verified output: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"stop\""),
        "still closes with a stop reason: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "still terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_streaming_feed_that_lost_its_tail_is_completed_from_the_verified_output() {
    // The live feed delivers only "the assistant" but the signed receipt
    // is over the full "the assistant reply". The client is charged the
    // receipt price, so the endpoint must complete the feed with the
    // missing remainder rather than present the short prefix as done.
    let endpoint = Endpoint::launch_with_executor(PartialStreamExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "stream please" }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"content\":\"the assistant\""),
        "relays the live prefix it did receive: {body}"
    );
    assert!(
        body.contains("\"content\":\" reply\""),
        "completes the lost tail from the verified output: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_streaming_chat_with_include_usage_ends_with_a_usage_frame() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "count please" }],
            "stream": true,
            "stream_options": { "include_usage": true },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // The metered token counts arrive in a final choices-empty frame, the
    // way OpenAI closes an include_usage stream.
    assert!(body.contains("\"usage\""), "carries a usage frame: {body}");
    assert!(
        body.contains("\"prompt_tokens\":7") && body.contains("\"completion_tokens\":3"),
        "the usage frame carries the receipt's token counts: {body}"
    );
    let usage_at = body.find("\"usage\"").unwrap();
    let done_at = body.find("data: [DONE]").unwrap();
    assert!(usage_at < done_at, "usage precedes [DONE]: {body}");
}

#[tokio::test]
async fn a_streaming_chat_without_include_usage_sends_no_usage_frame() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "no usage please" }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("\"usage\""),
        "no usage frame by default: {body}"
    );
    assert!(body.contains("data: [DONE]"), "still terminates: {body}");
}

#[tokio::test]
async fn a_text_completion_buys_a_verified_job_and_returns_openai_shape() {
    // A legacy `/v1/completions` request — a raw prompt, no messages —
    // buys a job on the network and comes back in OpenAI's text_completion
    // shape, carrying the same verified receipt the chat route attaches.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "once upon a time",
            "max_tokens": 16,
            "temperature": 0,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "text_completion");
    assert!(
        body["id"].as_str().unwrap().starts_with("cmpl-"),
        "id: {}",
        body["id"]
    );
    assert_eq!(body["model"], SERVED_MODEL);
    assert_eq!(body["choices"][0]["index"], 0);
    assert_eq!(body["choices"][0]["text"], "the assistant reply");
    assert_eq!(body["choices"][0]["logprobs"], Value::Null);
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 7);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    assert_eq!(body["usage"]["total_tokens"], 10);

    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["covenant"]["price_micro_usdc"], 10_000);
    assert!(body["covenant"]["job_id"].is_string());
    assert!(body["covenant"]["operator_pubkey_b58"].is_string());
}

#[tokio::test]
async fn a_streaming_text_completion_relays_text_then_done() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "stream please",
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/event-stream"),
        "content-type is an event stream"
    );
    let body = resp.text().await.unwrap();

    // Legacy completions stream `text_completion` frames carrying `text`
    // deltas (no chat role opener), a terminal finish_reason, then [DONE].
    assert!(
        body.contains("\"object\":\"text_completion\""),
        "chunks are text_completion: {body}"
    );
    assert!(
        !body.contains("\"role\":\"assistant\""),
        "the legacy stream sends no chat role opener: {body}"
    );
    assert!(
        body.contains("\"text\":\"the assistant reply\""),
        "relays the generated text: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"stop\""),
        "ends with a stop reason: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn a_streaming_text_completion_with_include_usage_ends_with_a_usage_frame() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "count please",
            "stream": true,
            "stream_options": { "include_usage": true },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("\"usage\""), "carries a usage frame: {body}");
    assert!(
        body.contains("\"prompt_tokens\":7") && body.contains("\"completion_tokens\":3"),
        "the usage frame carries the receipt's token counts: {body}"
    );
    let usage_at = body.find("\"usage\"").unwrap();
    let done_at = body.find("data: [DONE]").unwrap();
    assert!(usage_at < done_at, "usage precedes [DONE]: {body}");
}

#[tokio::test]
async fn a_truncated_text_completion_reports_finish_reason_length() {
    let endpoint = Endpoint::launch_with_executor(TruncatedExecutor).await;
    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "write forever",
            "max_tokens": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["finish_reason"], "length",
        "a completion the backend truncated reports length, not stop: {body}"
    );
}

#[tokio::test]
async fn a_completion_refuses_a_batch_prompt_and_unsupported_fields() {
    let endpoint = Endpoint::launch().await;

    // A multi-prompt batch: one prompt per call.
    let batch = endpoint
        .complete(json!({ "model": SERVED_MODEL, "prompt": ["a", "b"] }))
        .await;
    assert_eq!(batch.status(), 400);
    let body: Value = batch.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("one prompt per call"),
        "got: {body}"
    );

    // logprobs the network does not surface: a clear refusal, not a
    // silently free-form answer the client paid for.
    let logprobs = endpoint
        .complete(json!({ "model": SERVED_MODEL, "prompt": "hi", "logprobs": 5 }))
        .await;
    assert_eq!(logprobs.status(), 400);
    assert!(logprobs.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("logprobs"));

    // A missing prompt is a client error, not an opaque parse failure.
    let missing = endpoint.complete(json!({ "model": SERVED_MODEL })).await;
    assert_eq!(missing.status(), 400);
    assert!(missing.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("prompt is required"));
}

#[tokio::test]
async fn models_retrieve_returns_a_served_model_or_a_shaped_404() {
    let endpoint = Endpoint::launch().await;

    // A model the network serves comes back as an OpenAI model object.
    let served: Value = Endpoint::client()
        .get(format!("{}/v1/models/{}", endpoint.base, SERVED_MODEL))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["object"], "model");
    assert_eq!(served["id"], SERVED_MODEL);

    // An unserved model is a shaped 404, not a bare framework one.
    let missing = Endpoint::client()
        .get(format!("{}/v1/models/does-not-exist", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist"),
        "got: {body}"
    );

    // Retrieve is gated by the same bearer key as the rest of /v1/*.
    let unauth = Endpoint::client()
        .get(format!("{}/v1/models/{}", endpoint.base, SERVED_MODEL))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
}

#[tokio::test]
async fn models_list_negotiates_the_anthropic_shape_for_an_anthropic_client() {
    let endpoint = Endpoint::launch().await;

    // An Anthropic client (identified by its `x-api-key` credential) reading
    // `client.models.list()` gets Anthropic's paginated list shape, not the
    // OpenAI one, from the same shared `/v1/models` path.
    let anthropic: Value = Endpoint::client()
        .get(format!("{}/v1/models", endpoint.base))
        .header("x-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(anthropic["has_more"], false);
    assert!(anthropic["first_id"].is_string(), "{anthropic}");
    assert!(anthropic["last_id"].is_string(), "{anthropic}");
    let data = anthropic["data"].as_array().expect("data array");
    assert!(!data.is_empty(), "{anthropic}");
    for m in data {
        assert_eq!(m["type"], "model", "{m}");
        assert!(m["id"].is_string(), "{m}");
        assert_eq!(m["display_name"], m["id"], "{m}");
        assert_eq!(m["created_at"], "1970-01-01T00:00:00Z", "{m}");
    }
    assert!(
        data.iter().any(|m| m["id"] == SERVED_MODEL),
        "the served chat model is listed: {anthropic}"
    );

    // The exact same path, an OpenAI client (bearer, no Anthropic header):
    // the OpenAI list shape, proving the negotiation is per request.
    let openai: Value = Endpoint::client()
        .get(format!("{}/v1/models", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(openai["object"], "list");
    assert!(openai.get("has_more").is_none(), "{openai}");
}

#[tokio::test]
async fn models_retrieve_negotiates_the_anthropic_object_or_a_shaped_404() {
    let endpoint = Endpoint::launch().await;

    // A served chat model comes back as an Anthropic model object.
    let served: Value = Endpoint::client()
        .get(format!("{}/v1/models/{}", endpoint.base, SERVED_MODEL))
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["type"], "model");
    assert_eq!(served["id"], SERVED_MODEL);
    assert_eq!(served["display_name"], SERVED_MODEL);
    assert_eq!(served["created_at"], "1970-01-01T00:00:00Z");

    // An unserved model is Anthropic's error envelope, not the OpenAI one.
    let missing = Endpoint::client()
        .get(format!("{}/v1/models/does-not-exist", endpoint.base))
        .header("x-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not being served"),
        "got: {body}"
    );

    // A wrong key on the Anthropic path is Anthropic's 401 envelope.
    let unauth = Endpoint::client()
        .get(format!("{}/v1/models/{}", endpoint.base, SERVED_MODEL))
        .header("x-api-key", "sk-wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
    let body: Value = unauth.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
}

/// A 96x96 solid-red PNG, base64-encoded — the image the live vision test
/// sends through the whole stack. Inline so the test needs no file on disk.
const RED_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAGAAAABgCAIAAABt+uBvAAAApElEQVR4nO3QMQ0AMAzAsIEof2QDMwZ7m8NSAEQ+d0afzvpBPECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQoM0eq0WSHWx5IugAAAAASUVORK5CYII=";

#[tokio::test]
#[ignore = "requires a local Ollama serving the vision model moondream"]
async fn live_vision_against_a_real_backend() {
    // The whole vision chain against a real model: an OpenAI chat request
    // carrying an inline image → coordinator → node → real Ollama vision
    // model → a verified, paid receipt in OpenAI's shape. The one hop the
    // mocked e2e (a capturing executor) can't cover.
    const VISION_MODEL: &str = "moondream";
    let endpoint = Endpoint::launch_with_executor_serving(
        OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some(VISION_MODEL.into())),
        vec![VISION_MODEL.into()],
    )
    .await;
    let resp = endpoint
        .chat(json!({
            "model": VISION_MODEL,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Describe this image, naming its dominant color." },
                    {
                        "type": "image_url",
                        "image_url": { "url": format!("data:image/png;base64,{RED_PNG_B64}") },
                    },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let text = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(text.contains("red"), "the model should see red: {body}");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_text_completion_against_a_real_backend() {
    // The whole legacy-completions chain against a real model: a raw
    // `/v1/completions` prompt → coordinator → node → real Ollama → a
    // verified receipt → OpenAI's text_completion shape. Proves the
    // raw-prompt front door serves a real backend, the one hop the mocked
    // e2e can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "Write one short sentence about the sea.",
            "max_tokens": 64,
            "temperature": 0,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "text_completion");
    let text = body["choices"][0]["text"].as_str().unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "a real backend returns non-empty completion text: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_chat_logprobs_against_a_real_backend() {
    // The whole chat chain with logprobs against a real model: a
    // `/v1/chat/completions` request with `logprobs: true` → coordinator →
    // node → real Ollama → a verified receipt → OpenAI's
    // `choices[].logprobs` shape. Proves the probabilities a client reads
    // are the ones the operator signed for, the hop a mocked executor
    // can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "Say hello in one short word." }],
            "max_tokens": 16,
            "temperature": 0,
            "logprobs": true,
            "top_logprobs": 2,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let content = &body["choices"][0]["logprobs"]["content"];
    assert!(
        content.as_array().is_some_and(|c| !c.is_empty()),
        "a real backend returns per-token logprobs: {body}"
    );
    let first = &content[0];
    assert!(
        first["token"].as_str().is_some_and(|t| !t.is_empty()),
        "each entry names its token: {body}"
    );
    assert!(
        first["logprob"].is_number(),
        "and carries a logprob: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn the_endpoint_enforces_the_bearer_key() {
    let endpoint = Endpoint::launch().await;
    let valid = json!({ "model": SERVED_MODEL, "messages": [{ "role": "user", "content": "hi" }] });

    // No Authorization header at all.
    let missing = Endpoint::client()
        .post(format!("{}/v1/chat/completions", endpoint.base))
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 401);
    let err: Value = missing.json().await.unwrap();
    assert_eq!(err["error"]["type"], "invalid_request_error");

    // A wrong key.
    let wrong = Endpoint::client()
        .post(format!("{}/v1/chat/completions", endpoint.base))
        .bearer_auth("sk-not-it")
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn a_malformed_or_unsupported_request_refuses_in_openai_shape() {
    let endpoint = Endpoint::launch().await;

    // Unparseable body.
    let bad = Endpoint::client()
        .post(format!("{}/v1/chat/completions", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert_eq!(
        bad.json::<Value>().await.unwrap()["error"]["type"],
        "invalid_request_error"
    );

    // No messages.
    let empty = endpoint
        .chat(json!({ "model": SERVED_MODEL, "messages": [] }))
        .await;
    assert_eq!(empty.status(), 400);

    // n fans out to that many paid jobs, but only up to a modest ceiling —
    // a request above it is refused before any spend.
    let too_many = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
            "n": 9,
        }))
        .await;
    assert_eq!(too_many.status(), 400);
    assert!(
        too_many.json::<Value>().await.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("between 1 and 8"),
        "the refusal names the ceiling"
    );

    // n=0 is not a request for zero completions, it is a malformed one.
    let none = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
            "n": 0,
        }))
        .await;
    assert_eq!(none.status(), 400);

    // Streaming several completions at once is refused, not silently
    // collapsed to one — a client asking for both gets a clear error.
    let streamed_many = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
            "n": 2,
            "stream": true,
        }))
        .await;
    assert_eq!(streamed_many.status(), 400);
    assert!(
        streamed_many.json::<Value>().await.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only n=1"),
        "the refusal explains streaming is single-completion"
    );
}

#[tokio::test]
async fn a_model_no_operator_serves_is_refused_as_a_clean_404() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": "no-operator-serves-this",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await;
    // A model the network is not serving is refused up front — the shape an
    // SDK expects from an unknown model, not a doomed submit that comes back
    // as a bad gateway naming an internal job.
    assert_eq!(resp.status(), 404);
    let body = resp.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("no-operator-serves-this"),
        "names the model the caller asked for: {message}"
    );
    assert!(
        !message.contains("job") && !message.contains("coordinator"),
        "no internal job id or coordinator plumbing leaks: {message}"
    );

    // The refusal never reached the coordinator, so the endpoint keeps
    // serving: a following served buy still succeeds.
    let served = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await;
    assert_eq!(served.status(), 200);
}

#[tokio::test]
async fn an_embeddings_request_buys_a_verified_job_and_returns_openai_shape() {
    let endpoint = Endpoint::launch().await;

    // The model directory lists the embedding model too, so a client
    // discovers it the OpenAI way.
    let models: Value = Endpoint::client()
        .get(format!("{}/v1/models", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == EMBED_MODEL),
        "the embedding model is listed: {models}"
    );

    // A batch of two texts, the default float encoding.
    let resp = endpoint
        .embed(json!({
            "model": EMBED_MODEL,
            "input": ["the first text", "the second text"],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "list");
    assert_eq!(body["model"], EMBED_MODEL);
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "one vector per input text");
    for (index, entry) in data.iter().enumerate() {
        assert_eq!(entry["object"], "embedding");
        assert_eq!(entry["index"], index);
        let vector = entry["embedding"].as_array().unwrap();
        assert_eq!(vector.len(), EMBED_VECTOR.len());
        assert_eq!(vector[0].as_f64().unwrap(), 0.5);
    }
    // Embeddings meter input tokens only.
    assert_eq!(body["usage"]["prompt_tokens"], 8);
    assert_eq!(body["usage"]["total_tokens"], 8);

    // The verified receipt rides along under the Covenant extension.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert!(body["covenant"]["job_id"].is_string());

    // A single string is a batch of one.
    let single = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": "just one" }))
        .await;
    assert_eq!(single.status(), 200);
    let single: Value = single.json().await.unwrap();
    assert_eq!(single["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn embeddings_base64_encoding_packs_little_endian_floats() {
    use base64::Engine;

    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .embed(json!({
            "model": EMBED_MODEL,
            "input": "one text",
            "encoding_format": "base64",
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    // base64 is a JSON string, not an array — the wire form the OpenAI
    // SDKs request by default and decode transparently.
    let encoded = body["data"][0]["embedding"].as_str().unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid base64");
    let decoded: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(decoded, EMBED_VECTOR.to_vec());
}

#[tokio::test]
async fn an_embedding_result_short_a_vector_is_rejected_not_misindexed() {
    // The operator returns one vector for a two-text batch. Its receipt
    // still verifies (the hash binds what it returned), but pairing the one
    // vector positionally would silently mislabel it, so the endpoint
    // refuses the batch as an upstream fault.
    let endpoint = Endpoint::launch_with_executor(ShortEmbedExecutor).await;
    let resp = endpoint
        .embed(json!({
            "model": EMBED_MODEL,
            "input": ["first text", "second text"],
        }))
        .await;
    assert_eq!(resp.status(), 502);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("1 vectors for 2 input texts"),
        "names the arity mismatch: {body}"
    );
}

#[tokio::test]
async fn embeddings_refuse_unsupported_shapes_in_openai_form() {
    let endpoint = Endpoint::launch().await;

    // Pre-tokenized integer input is not embeddable here.
    let tokens = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": [1, 2, 3] }))
        .await;
    assert_eq!(tokens.status(), 400);
    assert_eq!(
        tokens.json::<Value>().await.unwrap()["error"]["type"],
        "invalid_request_error"
    );

    // Empty input.
    let empty = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": [] }))
        .await;
    assert_eq!(empty.status(), 400);

    // A blank text.
    let blank = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": ["  "] }))
        .await;
    assert_eq!(blank.status(), 400);

    // The served model fixes the width, so a dimensions reshape is refused
    // before anything is bought.
    let dims = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": "hi", "dimensions": 128 }))
        .await;
    assert_eq!(dims.status(), 400);

    // An unknown encoding format.
    let enc = endpoint
        .embed(json!({ "model": EMBED_MODEL, "input": "hi", "encoding_format": "hex" }))
        .await;
    assert_eq!(enc.status(), 400);
}

#[tokio::test]
async fn the_session_spend_cap_refuses_a_buy_that_would_cross_it() {
    // The one served operator asks 10_000; the session cap admits exactly
    // one such buy.
    let endpoint = Endpoint::launch_with_session_cap(Some(10_000)).await;
    let body = json!({
        "model": SERVED_MODEL,
        "messages": [{ "role": "user", "content": "hi" }],
    });

    let first = endpoint.chat(body.clone()).await;
    assert_eq!(first.status(), 200, "the first buy fits the cap");

    // The first buy settled at 10_000; a second would cross the cap and is
    // refused up front, before any dispatch, as an OpenAI quota error.
    let second = endpoint.chat(body).await;
    assert_eq!(second.status(), 429);
    let err: Value = second.json().await.unwrap();
    assert_eq!(err["error"]["type"], "insufficient_quota");
}

#[tokio::test]
async fn a_chat_completion_returns_tool_calls_when_the_model_calls_a_tool() {
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;

    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "weather in Paris?" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "look up the weather in a city",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }
                }
            }],
            "tool_choice": "auto",
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    let message = &body["choices"][0]["message"];
    assert_eq!(message["role"], "assistant");
    // A pure tool-call turn carries a null content, per OpenAI's shape.
    assert!(message["content"].is_null(), "content is null: {message}");
    let calls = message["tool_calls"]
        .as_array()
        .expect("tool_calls present");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_0");
    assert_eq!(calls[0]["type"], "function");
    assert_eq!(calls[0]["function"]["name"], "get_weather");
    assert_eq!(calls[0]["function"]["arguments"], r#"{"city":"Paris"}"#);
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");

    // The result is still a paid, verified job.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["usage"]["completion_tokens"], 5);
}

#[tokio::test]
async fn a_tool_result_conversation_is_accepted_and_priced() {
    // A follow-up turn that replays the assistant's tool call and feeds a
    // `tool` result back must be accepted (the endpoint no longer refuses
    // the tool role) and answered as an ordinary completion.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_0",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_0", "content": "18C and clear" }
            ]
        }))
        .await;
    assert_eq!(
        resp.status(),
        200,
        "a tool-result turn is served, not refused"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn a_streaming_chat_emits_tool_calls_from_the_verified_output() {
    // A tools job runs the backend non-streaming, so no tool call ever
    // rides the live feed. The streaming endpoint must still deliver the
    // calls — emitted whole from the verified receipt — and a "tool_calls"
    // finish reason, not a silent stop with an empty answer.
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "weather in Paris?" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
                }
            }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"tool_calls\""),
        "the stream carries the tool calls: {body}"
    );
    assert!(
        body.contains("\"name\":\"get_weather\""),
        "the tool name rides a delta: {body}"
    );
    assert!(
        body.contains("\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\""),
        "the arguments ride a delta as a JSON string: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"tool_calls\""),
        "ends with a tool_calls reason: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "terminates with [DONE]: {body}"
    );
}

#[tokio::test]
async fn n_fans_out_to_several_verified_paid_completions() {
    let endpoint = Endpoint::launch().await;

    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "chat.completion");
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 3, "n=3 returns three choices: {body}");
    for (index, choice) in choices.iter().enumerate() {
        assert_eq!(
            choice["index"], index as u64,
            "choices are indexed in order"
        );
        assert_eq!(choice["message"]["role"], "assistant");
        assert_eq!(choice["message"]["content"], "the assistant reply");
        assert_eq!(choice["finish_reason"], "stop");
    }

    // Usage is summed across the three completions the network actually ran
    // (7 prompt + 3 completion tokens each).
    assert_eq!(body["usage"]["prompt_tokens"], 21);
    assert_eq!(body["usage"]["completion_tokens"], 9);
    assert_eq!(body["usage"]["total_tokens"], 30);

    // Each completion is a distinct paid job, and each carries its own
    // locally re-verified receipt.
    let receipts = body["covenant"]["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 3, "one receipt per choice: {body}");
    let mut job_ids = std::collections::HashSet::new();
    for receipt in receipts {
        assert_eq!(receipt["receipt_verified"], true);
        assert_eq!(receipt["price_micro_usdc"], 10_000);
        assert!(job_ids.insert(receipt["job_id"].as_str().unwrap().to_string()));
    }
    assert_eq!(
        job_ids.len(),
        3,
        "three separate jobs, not one echoed thrice"
    );
}

#[tokio::test]
async fn a_fan_out_the_session_cap_cannot_hold_charges_nothing() {
    // The session cap holds two of the node's 10_000 asks but not three.
    let endpoint = Endpoint::launch_with_session_cap(Some(25_000)).await;

    // n=3 would need 30_000. The whole batch is refused before any job is
    // placed — a partial fan-out is never dispatched on a budget that can't
    // cover it.
    let refused = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 3,
        }))
        .await;
    assert_eq!(refused.status(), 429);
    assert_eq!(
        refused.json::<Value>().await.unwrap()["error"]["type"],
        "insufficient_quota"
    );

    // The refusal released every reservation it briefly held: a following
    // n=2 (20_000) still fits under the untouched cap. Had the failed batch
    // leaked even one hold, this would trip the cap too.
    let served = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 2,
        }))
        .await;
    assert_eq!(
        served.status(),
        200,
        "the cap was never consumed by the refused batch"
    );
    assert_eq!(
        served.json::<Value>().await.unwrap()["choices"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn a_legacy_completion_fans_n_out_to_several_paid_choices() {
    let endpoint = Endpoint::launch().await;

    let resp = endpoint
        .complete(json!({
            "model": SERVED_MODEL,
            "prompt": "once upon a time",
            "n": 2,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "text_completion");
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2, "n=2 returns two choices: {body}");
    for (index, choice) in choices.iter().enumerate() {
        assert_eq!(choice["index"], index as u64);
        assert_eq!(choice["text"], "the assistant reply");
        assert_eq!(choice["finish_reason"], "stop");
    }

    // Usage sums the two completions the network ran.
    assert_eq!(body["usage"]["prompt_tokens"], 14);
    assert_eq!(body["usage"]["completion_tokens"], 6);

    let receipts = body["covenant"]["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2, "one verified receipt per choice: {body}");
    let mut job_ids = std::collections::HashSet::new();
    for receipt in receipts {
        assert_eq!(receipt["receipt_verified"], true);
        assert!(job_ids.insert(receipt["job_id"].as_str().unwrap().to_string()));
    }
    assert_eq!(job_ids.len(), 2, "two separate paid jobs");
}

#[tokio::test]
async fn a_fan_out_charges_only_for_the_completions_that_come_back() {
    // A node that flakes on one job of the batch and completes the rest,
    // behind a session cap of exactly three of its 10_000 asks.
    let executor = FlakyOnceExecutor {
        failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let endpoint =
        Endpoint::from_rig(Rig::launch_with_executor(executor).await, Some(30_000)).await;

    // n=3: one job faults, two settle. The buyer returns only the two that
    // came back, each with its own verified receipt.
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"].as_array().unwrap().len(),
        2,
        "one job flaked, so two completions come back: {body}"
    );
    assert_eq!(body["covenant"]["receipts"].as_array().unwrap().len(), 2);

    // The faulted job's hold was released, not charged: of the 30_000 cap,
    // exactly the two settled completions (20_000) were spent. A further
    // 20_000 fan-out no longer fits, but a single 10_000 buy still does.
    let over = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
            "n": 2,
        }))
        .await;
    assert_eq!(
        over.status(),
        429,
        "only 10_000 of the cap is left, not 20_000"
    );

    let fits = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .await;
    assert_eq!(
        fits.status(),
        200,
        "the released hold left room for exactly one more completion"
    );
}

#[tokio::test]
async fn concurrent_fan_outs_never_overshoot_the_session_cap() {
    // Five completions' worth of headroom; several fan-outs race for it.
    const SLOTS: u64 = 5;
    const PRICE: u64 = 10_000;
    let endpoint = Endpoint::launch_with_session_cap(Some(SLOTS * PRICE)).await;
    let base = endpoint.base.clone();
    let key = endpoint.api_key.clone();

    // Six concurrent requests, each asking for two completions — twelve
    // demanded against five available, so the cap is genuinely contended.
    let mut handles = Vec::new();
    for _ in 0..6 {
        let base = base.clone();
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(&key)
                .json(&json!({
                    "model": SERVED_MODEL,
                    "messages": [{ "role": "user", "content": "hello" }],
                    "n": 2,
                }))
                .send()
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let choices = if status == 200 {
                resp.json::<Value>().await.unwrap()["choices"]
                    .as_array()
                    .unwrap()
                    .len() as u64
            } else {
                0
            };
            (status, choices)
        }));
    }

    let mut settled = 0u64;
    for handle in handles {
        let (status, choices) = handle.await.unwrap();
        assert!(
            status == 200 || status == 429,
            "a contended fan-out either serves or is refused, never errors: {status}"
        );
        settled += choices;
    }

    // The invariant that matters: however the six requests interleaved, the
    // completions actually served and paid never exceed the cap.
    assert!(
        settled <= SLOTS,
        "settled {settled} completions against a {SLOTS}-slot cap"
    );
    assert!(settled > 0, "at least one fan-out cleared the cap");
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_n_greater_than_one_against_a_real_backend() {
    // A real `n=2` chat request against a live model: two independent jobs
    // run through the coordinator and a real Ollama, each returning a
    // verified receipt, assembled into one `chat.completion` with two
    // choices. Proves the fan-out drives a real backend N times, the hop a
    // mocked executor can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .chat(json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "Say hello in one short word." }],
            "max_tokens": 16,
            "n": 2,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2, "n=2 returns two completions: {body}");
    for choice in choices {
        assert!(
            !choice["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "each completion carries real text: {body}"
        );
    }
    let receipts = body["covenant"]["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    let mut job_ids = std::collections::HashSet::new();
    for receipt in receipts {
        assert_eq!(receipt["receipt_verified"], true);
        assert!(job_ids.insert(receipt["job_id"].as_str().unwrap().to_string()));
    }
    assert_eq!(job_ids.len(), 2, "two distinct jobs ran on the backend");
    assert!(
        body["usage"]["completion_tokens"].as_u64().unwrap_or(0) > 0,
        "real tokens were metered: {body}"
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving nomic-embed-text"]
async fn live_embeddings_against_a_real_backend() {
    // The whole embeddings front door against a real model: a
    // `/v1/embeddings` request → coordinator → node → real Ollama
    // (`/api/embed`) → a verified, settled receipt → OpenAI's list shape.
    // The mocked embedding tests return exactly-representable floats that
    // round-trip through JSON unchanged; a real model returns messy ones, so
    // this is the path that actually exercises settlement's float round-trip
    // — the difference between an embedding job paying out and retrying to
    // its deadline — from the demand front door, the hop a mock can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .embed(json!({
            "model": EMBED_MODEL,
            "input": ["the first text", "a second, longer piece of text"],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "list");
    assert_eq!(body["model"], EMBED_MODEL);
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "one vector per input text: {body}");
    let mut width = None;
    for (index, entry) in data.iter().enumerate() {
        assert_eq!(entry["object"], "embedding");
        assert_eq!(entry["index"], index);
        let vector = entry["embedding"].as_array().unwrap();
        assert!(
            vector.len() > 64,
            "a real embedding model returns a wide vector, got {}: {body}",
            vector.len()
        );
        assert!(
            vector
                .iter()
                .all(|v| v.as_f64().is_some_and(f64::is_finite)),
            "every component is a finite float: {body}"
        );
        assert!(
            vector.iter().any(|v| v.as_f64() != Some(0.0)),
            "the vector is not all zeros: {body}"
        );
        assert_eq!(
            *width.get_or_insert(vector.len()),
            vector.len(),
            "the served model fixes one vector width: {body}"
        );
    }

    // The receipt verified only because the coordinator released escrow,
    // and release requires the operator's output hash to survive the JSON
    // wire round-trip — the real-float settlement path a clean mock vector
    // never puts under load.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert!(
        body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0,
        "real input tokens were metered: {body}"
    );
}

#[tokio::test]
async fn a_message_buys_a_verified_job_and_returns_anthropic_shape() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 32,
            "system": "You are terse.",
            // An explicitly disabled thinking block is the client opting out;
            // it rides through to a normal paid completion, not a refusal.
            "thinking": { "type": "disabled" },
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], SERVED_MODEL);
    assert!(
        body["id"].as_str().is_some_and(|id| id.starts_with("msg_")),
        "an Anthropic message id: {body}"
    );
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "the assistant reply");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["stop_sequence"], Value::Null);
    assert_eq!(body["usage"]["input_tokens"], 7);
    assert_eq!(body["usage"]["output_tokens"], 3);
    // The Covenant proof rides the same field as the OpenAI door.
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn the_messages_endpoint_accepts_x_api_key_and_bearer_and_refuses_others() {
    let endpoint = Endpoint::launch().await;
    let valid = json!({
        "model": SERVED_MODEL,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "hi" }],
    });

    // The `x-api-key` header an Anthropic client sends is accepted.
    assert_eq!(endpoint.messages(valid.clone()).await.status(), 200);

    // A bearer token is accepted too, so the two front doors share one key.
    let bearer = Endpoint::client()
        .post(format!("{}/v1/messages", endpoint.base))
        .bearer_auth(&endpoint.api_key)
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(bearer.status(), 200);

    // No credential at all is a 401 in Anthropic's error shape.
    let missing = Endpoint::client()
        .post(format!("{}/v1/messages", endpoint.base))
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 401);
    let err: Value = missing.json().await.unwrap();
    assert_eq!(err["type"], "error");
    assert_eq!(err["error"]["type"], "authentication_error");

    // A wrong key.
    let wrong = Endpoint::client()
        .post(format!("{}/v1/messages", endpoint.base))
        .header("x-api-key", "sk-not-it")
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn an_unsupported_message_request_refuses_in_anthropic_shape() {
    let endpoint = Endpoint::launch().await;
    let base = json!({
        "model": SERVED_MODEL,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "hi" }],
    });

    // Each of these is a distinct refusal the buyer must see before paying.
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({ "model": SERVED_MODEL, "messages": [{ "role": "user", "content": "hi" }] }),
            "max_tokens",
        ),
        (with(&base, "top_k", json!(40)), "top_k"),
        (
            with(
                &base,
                "thinking",
                json!({ "type": "enabled", "budget_tokens": 512 }),
            ),
            "thinking",
        ),
        (
            with(
                &base,
                "tool_choice",
                json!({ "type": "auto", "disable_parallel_tool_use": true }),
            ),
            "disable_parallel_tool_use",
        ),
        (
            with(
                &base,
                "tools",
                json!([{ "type": "web_search_20250305", "name": "web_search" }]),
            ),
            "web_search_20250305",
        ),
        (
            json!({ "model": SERVED_MODEL, "max_tokens": 16,
                    "messages": [{ "role": "system", "content": "be terse" }] }),
            "system field",
        ),
        (
            json!({ "model": SERVED_MODEL, "max_tokens": 16, "messages": [] }),
            "messages must not be empty",
        ),
        (
            json!({ "model": "", "max_tokens": 16,
                    "messages": [{ "role": "user", "content": "hi" }] }),
            "model is required",
        ),
    ];

    for (body, needle) in cases {
        let resp = endpoint.messages(body).await;
        assert_eq!(resp.status(), 400, "expected a 400 for {needle}");
        let err: Value = resp.json().await.unwrap();
        assert_eq!(err["type"], "error");
        assert_eq!(err["error"]["type"], "invalid_request_error");
        let message = err["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(needle),
            "refusal for {needle:?} should say so: {err}"
        );
    }

    // An unparseable body is a 400 in the same shape.
    let bad = Endpoint::client()
        .post(format!("{}/v1/messages", endpoint.base))
        .header("x-api-key", &endpoint.api_key)
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
}

/// Overlay one key onto a base request object, for the refusal table above.
fn with(base: &Value, key: &str, value: Value) -> Value {
    let mut obj = base.as_object().unwrap().clone();
    obj.insert(key.into(), value);
    Value::Object(obj)
}

#[tokio::test]
async fn a_gemini_request_buys_a_verified_job_and_returns_gemini_shape() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "systemInstruction": { "parts": [{ "text": "You are terse." }] },
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
                "generationConfig": { "temperature": 0.2, "maxOutputTokens": 32 },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    let candidate = &body["candidates"][0];
    assert_eq!(candidate["content"]["role"], "model");
    assert_eq!(
        candidate["content"]["parts"][0]["text"],
        "the assistant reply"
    );
    assert_eq!(candidate["finishReason"], "STOP");
    assert_eq!(candidate["index"], 0);
    assert_eq!(body["usageMetadata"]["promptTokenCount"], 7);
    assert_eq!(body["usageMetadata"]["candidatesTokenCount"], 3);
    assert_eq!(body["usageMetadata"]["totalTokenCount"], 10);
    assert_eq!(body["modelVersion"], SERVED_MODEL);
    // The Covenant proof rides the same field as the other doors.
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn the_gemini_endpoint_accepts_the_goog_header_and_query_key_and_refuses_others() {
    let endpoint = Endpoint::launch().await;
    let body = json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] });
    let path = format!(
        "{}/v1beta/models/{SERVED_MODEL}:generateContent",
        endpoint.base
    );

    // The x-goog-api-key header a Google GenAI client sends is accepted.
    assert_eq!(
        endpoint
            .generate_content(SERVED_MODEL, body.clone())
            .await
            .status(),
        200
    );

    // The ?key= query parameter is accepted too, the REST alternative.
    let via_query = Endpoint::client()
        .post(format!("{path}?key={}", endpoint.api_key))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(via_query.status(), 200);

    // No credential at all is a 401 in Gemini's error shape.
    let missing = Endpoint::client()
        .post(&path)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 401);
    let err: Value = missing.json().await.unwrap();
    assert_eq!(err["error"]["status"], "UNAUTHENTICATED");
    assert_eq!(err["error"]["code"], 401);

    // A wrong key.
    let wrong = Endpoint::client()
        .post(&path)
        .header("x-goog-api-key", "sk-not-it")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn an_unsupported_gemini_request_refuses_in_gemini_shape() {
    let endpoint = Endpoint::launch().await;
    let base = json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] });

    // Each of these is a distinct refusal the buyer must see before paying.
    let cases: Vec<(Value, &str)> = vec![
        (
            with(&base, "generationConfig", json!({ "topK": 40 })),
            "topK",
        ),
        (
            with(&base, "generationConfig", json!({ "candidateCount": 2 })),
            "candidateCount",
        ),
        (
            with(
                &base,
                "generationConfig",
                json!({ "responseMimeType": "text/x.enum" }),
            ),
            "responseMimeType",
        ),
        (
            with(&base, "generationConfig", json!({ "presencePenalty": 5.0 })),
            "presence_penalty",
        ),
        (json!({ "contents": [] }), "contents must not be empty"),
        (
            with(
                &base,
                "contents",
                json!([{ "role": "user", "parts": [
                    { "inlineData": { "mimeType": "image/png", "data": "not base64!!" } }
                ] }]),
            ),
            "not valid base64",
        ),
        (
            with(
                &base,
                "contents",
                json!([{ "role": "boss", "parts": [{ "text": "hi" }] }]),
            ),
            "unknown content role",
        ),
        (
            with(&base, "tools", json!([{ "googleSearch": {} }])),
            "hosted tool 'googleSearch'",
        ),
    ];

    for (body, needle) in cases {
        let resp = endpoint.generate_content(SERVED_MODEL, body).await;
        assert_eq!(resp.status(), 400, "expected a 400 for {needle}");
        let err: Value = resp.json().await.unwrap();
        assert_eq!(err["error"]["status"], "INVALID_ARGUMENT");
        let message = err["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(needle),
            "refusal for {needle:?} should say so: {err}"
        );
    }

    // An unknown method on the path is a 404 in the same shape, not the bare
    // framework 404 an unrouted path would give.
    let unknown = Endpoint::client()
        .post(format!(
            "{}/v1beta/models/{SERVED_MODEL}:countTokens",
            endpoint.base
        ))
        .header("x-goog-api-key", &endpoint.api_key)
        .json(&base)
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    let err: Value = unknown.json().await.unwrap();
    assert_eq!(err["error"]["status"], "NOT_FOUND");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("countTokens"));
}

#[tokio::test]
async fn a_gemini_request_for_an_unserved_model_is_a_404() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .generate_content(
            "no-such-model",
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let err: Value = resp.json().await.unwrap();
    assert_eq!(err["error"]["status"], "NOT_FOUND");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not being served"));
}

#[tokio::test]
async fn the_gemini_model_directory_lists_and_fetches_served_models() {
    let endpoint = Endpoint::launch().await;
    let want = format!("models/{SERVED_MODEL}");

    // The directory lists the served model with the methods this door serves.
    let list: Value = Endpoint::client()
        .get(format!("{}/v1beta/models", endpoint.base))
        .header("x-goog-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let served = list["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["name"].as_str() == Some(want.as_str()))
        .unwrap_or_else(|| panic!("the served model is listed: {list}"));
    let methods = served["supportedGenerationMethods"].as_array().unwrap();
    assert!(
        methods.iter().any(|m| m == "generateContent"),
        "the model supports generateContent: {served}"
    );

    // Fetching one model returns its directory entry.
    let one: Value = Endpoint::client()
        .get(format!("{}/v1beta/models/{SERVED_MODEL}", endpoint.base))
        .header("x-goog-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["name"], want);
    assert_eq!(one["displayName"], SERVED_MODEL);

    // Fetching an unserved model is the SDK-correct 404.
    let missing = Endpoint::client()
        .get(format!("{}/v1beta/models/no-such-model", endpoint.base))
        .header("x-goog-api-key", &endpoint.api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(
        missing.json::<Value>().await.unwrap()["error"]["status"],
        "NOT_FOUND"
    );
}

#[tokio::test]
async fn a_gemini_system_instruction_and_multi_turn_reach_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, ChatRole};
    // The system instruction and prior turns must travel into the signed, paid
    // envelope — Gemini names the assistant role `model`, and it has to narrow
    // to an assistant turn so the operator sees the conversation the buyer paid
    // to have answered.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "systemInstruction": { "parts": [{ "text": "You are terse." }] },
                "contents": [
                    { "role": "user", "parts": [{ "text": "hello" }] },
                    { "role": "model", "parts": [{ "text": "hi" }] },
                    { "role": "user", "parts": [{ "text": "again" }] },
                ],
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    let turns: Vec<(ChatRole, &str)> = chat.iter().map(|m| (m.role, m.content.as_str())).collect();
    assert_eq!(
        turns,
        vec![
            (ChatRole::System, "You are terse."),
            (ChatRole::User, "hello"),
            (ChatRole::Assistant, "hi"),
            (ChatRole::User, "again"),
        ]
    );
}

#[tokio::test]
async fn a_gemini_inline_image_reaches_the_signed_job() {
    use covenant_compute_protocol::parse_chat_input;
    // A user turn's `inlineData` base64 must travel into the signed, paid
    // envelope as vision input, so the operator's model sees the image the
    // buyer paid to have described.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "contents": [{
                    "role": "user",
                    "parts": [
                        { "text": "describe this" },
                        { "inlineData": { "mimeType": "image/png", "data": "AQID" } },
                    ],
                }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0].content, "describe this");
    assert_eq!(
        chat[0].images,
        vec!["AQID".to_string()],
        "the inline image rides the signed job as base64"
    );
}

#[tokio::test]
async fn a_gemini_response_schema_reaches_the_signed_job() {
    use covenant_compute_protocol::parse_generation_params;
    // A Gemini client asking for JSON output constrained to a schema must have
    // that constraint travel into the signed, paid envelope — not be silently
    // dropped so the buyer pays for free-form text.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "contents": [{ "role": "user", "parts": [{ "text": "list three colors" }] }],
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseSchema": { "type": "object" },
                },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let params = parse_generation_params(&input)
        .expect("well-formed")
        .expect("a generation block rides the signed job");
    assert_eq!(
        params.response_format,
        Some(ResponseFormat::JsonSchema {
            name: "response".into(),
            schema: json!({ "type": "object" }),
            strict: None,
        })
    );
}

#[tokio::test]
async fn a_gemini_sampling_knobs_reach_the_signed_job() {
    use covenant_compute_protocol::parse_generation_params;
    // The anti-repetition penalties, the reproducibility seed, and the
    // logprobs request are knobs the network honors, so a Gemini client that
    // sets them must have them ride the signed, paid envelope rather than pay
    // for a completion sampled without the controls it asked for.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "contents": [{ "role": "user", "parts": [{ "text": "write a limerick" }] }],
                "generationConfig": {
                    "presencePenalty": 1.5,
                    "frequencyPenalty": -0.5,
                    "seed": 42,
                    "responseLogprobs": true,
                    "logprobs": 3,
                },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let params = parse_generation_params(&input)
        .expect("well-formed")
        .expect("a generation block rides the signed job");
    assert_eq!(params.presence_penalty, Some(1.5));
    assert_eq!(params.frequency_penalty, Some(-0.5));
    assert_eq!(params.seed, Some(42));
    assert_eq!(params.logprobs, Some(3));
}

#[tokio::test]
async fn a_gemini_request_returns_a_function_call_when_the_model_calls_a_tool() {
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "contents": [{ "role": "user", "parts": [{ "text": "weather in Paris?" }] }],
                "tools": [{ "functionDeclarations": [{
                    "name": "get_weather",
                    "description": "look up the weather in a city",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"],
                    },
                }] }],
                "toolConfig": { "functionCallingConfig": { "mode": "ANY" } },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    // A tool turn comes back as a `functionCall` part carrying the object args,
    // the shape a Gemini agent runs and feeds back.
    let call = &body["candidates"][0]["content"]["parts"][0]["functionCall"];
    assert_eq!(call["name"], "get_weather");
    assert_eq!(call["args"]["city"], "Paris");
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn a_gemini_tool_loop_reaches_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, ChatRole};
    // The whole replayed loop — the model's `functionCall` and the agent's
    // `functionResponse` — must travel into the signed, paid envelope with a
    // matching call id, so the operator's backend pairs the result to its call.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "contents": [
                    { "role": "user", "parts": [{ "text": "weather in Paris?" }] },
                    { "role": "model", "parts": [
                        { "functionCall": { "name": "get_weather", "args": { "city": "Paris" } } }
                    ] },
                    { "role": "user", "parts": [
                        { "functionResponse": { "name": "get_weather", "response": { "tempC": 14 } } }
                    ] },
                ],
                "tools": [{ "functionDeclarations": [
                    { "name": "get_weather", "parameters": { "type": "object" } }
                ] }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    assert_eq!(chat.len(), 3);
    assert_eq!(chat[1].role, ChatRole::Assistant);
    assert_eq!(chat[1].tool_calls[0].function.name, "get_weather");
    assert_eq!(
        chat[1].tool_calls[0].function.arguments,
        r#"{"city":"Paris"}"#
    );
    assert_eq!(chat[2].role, ChatRole::Tool);
    assert_eq!(
        chat[2].tool_call_id.as_deref(),
        Some(chat[1].tool_calls[0].id.as_str()),
        "the tool result carries the call's id so the backend pairs them"
    );
    assert_eq!(chat[2].content, r#"{"tempC":14}"#);
}

#[tokio::test]
async fn a_streaming_gemini_request_emits_a_function_call_in_its_terminal_chunk() {
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = Endpoint::client()
        .post(format!(
            "{}/v1beta/models/{SERVED_MODEL}:streamGenerateContent?alt=sse",
            endpoint.base
        ))
        .header("x-goog-api-key", &endpoint.api_key)
        .json(&json!({
            "contents": [{ "role": "user", "parts": [{ "text": "weather in Paris?" }] }],
            "tools": [{ "functionDeclarations": [
                { "name": "get_weather", "parameters": { "type": "object" } }
            ] }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Tool calls never ride the live feed; the whole call lands in the terminal
    // chunk alongside the finish reason and the verified receipt.
    assert!(
        body.contains("\"functionCall\""),
        "the terminal chunk carries the tool call: {body}"
    );
    assert!(body.contains("\"get_weather\""), "names the tool: {body}");
    assert!(body.contains("\"finishReason\":\"STOP\""), "{body}");
    assert!(
        body.contains("\"covenant\":") && body.contains("\"receipt_verified\":true"),
        "the verified receipt rides the terminal chunk: {body}"
    );
}

#[tokio::test]
async fn a_streaming_gemini_request_relays_sse_chunks_then_a_terminal_chunk() {
    let endpoint = Endpoint::launch().await;
    let resp = Endpoint::client()
        .post(format!(
            "{}/v1beta/models/{SERVED_MODEL}:streamGenerateContent?alt=sse",
            endpoint.base
        ))
        .header("x-goog-api-key", &endpoint.api_key)
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "stream please" }] }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/event-stream"),
        "alt=sse selects an event stream"
    );
    let body = resp.text().await.unwrap();

    // The Gemini SSE shape: a data frame carrying the generated text, then a
    // terminal frame with the finish reason, the metered usage, and the
    // verified receipt.
    assert!(
        body.contains("data: {"),
        "chunks are SSE data frames: {body}"
    );
    assert!(
        body.contains("\"text\":\"the assistant reply\""),
        "relays the generated text: {body}"
    );
    assert!(
        body.contains("\"finishReason\":\"STOP\""),
        "ends with a stop reason: {body}"
    );
    assert!(
        body.contains("\"usageMetadata\""),
        "the terminal chunk carries usage: {body}"
    );
    assert!(
        body.contains("\"covenant\":") && body.contains("\"receipt_verified\":true"),
        "the verified receipt rides the terminal chunk, so a streamed job is \
         as verifiable as a non-streamed one: {body}"
    );
}

#[tokio::test]
async fn a_streaming_gemini_request_defaults_to_a_json_array() {
    let endpoint = Endpoint::launch().await;
    let resp = Endpoint::client()
        .post(format!(
            "{}/v1beta/models/{SERVED_MODEL}:streamGenerateContent",
            endpoint.base
        ))
        .header("x-goog-api-key", &endpoint.api_key)
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "stream please" }] }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("application/json"),
        "the default streaming form is a json array"
    );
    let body = resp.text().await.unwrap();

    // A well-formed JSON array of chunks whose parts concatenate to the reply.
    let chunks: Vec<Value> = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("a streamed json array: {e}: {body}"));
    assert!(!chunks.is_empty(), "at least one chunk: {body}");
    let mut text = String::new();
    for chunk in &chunks {
        if let Some(parts) = chunk["candidates"][0]["content"]["parts"].as_array() {
            for part in parts {
                if let Some(fragment) = part["text"].as_str() {
                    text.push_str(fragment);
                }
            }
        }
    }
    assert_eq!(
        text, "the assistant reply",
        "the concatenated parts are the full reply: {body}"
    );
    let last = chunks.last().unwrap();
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["covenant"]["receipt_verified"], true);
    assert!(
        last["usageMetadata"]["totalTokenCount"].as_u64().is_some(),
        "the terminal chunk meters usage: {body}"
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_generate_content_against_a_real_backend() {
    // The whole Gemini chain against a real model: a `generateContent` request
    // → coordinator → node → real Ollama → a verified receipt → Gemini's
    // response shape. The one hop the mocked e2e can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .generate_content(
            SERVED_MODEL,
            json!({
                "systemInstruction": { "parts": [{ "text": "Answer in one short sentence." }] },
                "contents": [{ "role": "user", "parts": [{ "text": "Say something about the sea." }] }],
                "generationConfig": { "maxOutputTokens": 64 },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "a real backend returns non-empty candidate text: {body}"
    );
    assert!(
        ["STOP", "MAX_TOKENS"].contains(
            &body["candidates"][0]["finishReason"]
                .as_str()
                .unwrap_or_default()
        ),
        "a real backend reports a natural stop or a token cut-off: {body}"
    );
    assert!(
        body["usageMetadata"]["totalTokenCount"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "a real backend meters tokens: {body}"
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving the vision model moondream"]
async fn live_gemini_vision_against_a_real_backend() {
    // A Gemini inline image through the whole stack against a real vision
    // model: base64 image → coordinator → node → real Ollama → a verified,
    // paid receipt in Gemini's shape.
    const VISION_MODEL: &str = "moondream";
    let endpoint = Endpoint::launch_with_executor_serving(
        OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some(VISION_MODEL.into())),
        vec![VISION_MODEL.into()],
    )
    .await;
    let resp = endpoint
        .generate_content(
            VISION_MODEL,
            json!({
                "contents": [{
                    "role": "user",
                    "parts": [
                        { "text": "Describe this image, naming its dominant color." },
                        { "inlineData": { "mimeType": "image/png", "data": RED_PNG_B64 } },
                    ],
                }],
                "generationConfig": { "maxOutputTokens": 64 },
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(text.contains("red"), "the model should see red: {body}");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn a_system_prompt_and_multi_turn_reach_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, ChatRole};
    // A system instruction and prior turns must travel into the signed, paid
    // envelope, so the operator's model sees exactly the conversation the
    // buyer paid to have answered.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 16,
            "system": [{ "type": "text", "text": "You are terse." }],
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "hi" },
                { "role": "user", "content": [{ "type": "text", "text": "again" }] },
            ],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    let turns: Vec<(ChatRole, &str)> = chat.iter().map(|m| (m.role, m.content.as_str())).collect();
    assert_eq!(
        turns,
        vec![
            (ChatRole::System, "You are terse."),
            (ChatRole::User, "hello"),
            (ChatRole::Assistant, "hi"),
            (ChatRole::User, "again"),
        ]
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_message_against_a_real_backend() {
    // The whole Anthropic-message chain against a real model: a
    // `/v1/messages` request → coordinator → node → real Ollama → a verified
    // receipt → Anthropic's message shape. The one hop the mocked e2e can't
    // cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 64,
            "system": "Answer in one short sentence.",
            "messages": [{ "role": "user", "content": "Say something about the sea." }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    let text = body["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "a real backend returns non-empty message text: {body}"
    );
    assert!(
        ["end_turn", "max_tokens"].contains(&body["stop_reason"].as_str().unwrap_or_default()),
        "a real backend reports a natural stop or a token cut-off: {body}"
    );
    assert!(
        body["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0,
        "real input tokens were metered: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
#[ignore = "requires a local Ollama serving the vision model moondream"]
async fn live_vision_message_against_a_real_backend() {
    // An Anthropic image block through the whole stack against a real vision
    // model: base64 image → coordinator → node → real Ollama → a verified,
    // paid receipt in Anthropic's shape.
    const VISION_MODEL: &str = "moondream";
    let endpoint = Endpoint::launch_with_executor_serving(
        OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some(VISION_MODEL.into())),
        vec![VISION_MODEL.into()],
    )
    .await;
    let resp = endpoint
        .messages(json!({
            "model": VISION_MODEL,
            "max_tokens": 64,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Describe this image, naming its dominant color." },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": RED_PNG_B64,
                        },
                    },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let text = body["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(text.contains("red"), "the model should see red: {body}");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn a_message_returns_tool_use_when_the_model_calls_a_tool() {
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "weather in Paris?" }],
            "tools": [{
                "name": "get_weather",
                "description": "look up the weather in a city",
                "input_schema": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            }],
            "tool_choice": { "type": "auto" },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    // The reply is a tool_use block carrying the parsed input object, and the
    // turn stops for tool_use so the client runs it and continues.
    let block = &body["content"][0];
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["id"], "call_0");
    assert_eq!(block["name"], "get_weather");
    assert_eq!(block["input"], json!({ "city": "Paris" }));
    assert_eq!(body["stop_reason"], "tool_use");
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["usage"]["output_tokens"], 5);
}

#[tokio::test]
async fn tools_and_a_tool_result_history_reach_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, parse_tools_input, ChatRole};
    // A tools list, a replayed assistant tool call, and a fed-back tool
    // result must all travel into the signed, paid envelope, so the operator
    // runs the agent's real tool loop.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 64,
            "tools": [{
                "name": "get_weather",
                "description": "look up the weather",
                "input_schema": { "type": "object" }
            }],
            "messages": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "checking" },
                        { "type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": { "city": "Paris" } }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "toolu_1", "content": "18C and clear" }
                    ]
                }
            ],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");

    // The tools list rode the signed job.
    let tools = parse_tools_input(&input)
        .expect("well-formed")
        .expect("a tools block rides the signed job");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].function.name, "get_weather");

    // The conversation carries the replayed call and the tool result.
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    let assistant = chat
        .iter()
        .find(|m| m.role == ChatRole::Assistant)
        .expect("the replayed assistant turn");
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].id, "toolu_1");
    assert_eq!(assistant.tool_calls[0].function.name, "get_weather");
    let tool = chat
        .iter()
        .find(|m| m.role == ChatRole::Tool)
        .expect("the fed-back tool result");
    assert_eq!(tool.tool_call_id.as_deref(), Some("toolu_1"));
    assert_eq!(tool.content, "18C and clear");
}

/// Positions of two markers in an SSE body, for asserting event order.
fn order(body: &str, first: &str, second: &str) -> bool {
    match (body.find(first), body.find(second)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

#[tokio::test]
async fn a_streaming_message_emits_the_anthropic_event_sequence() {
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 32,
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // The named events arrive in Anthropic's order.
    assert!(body.contains("event: message_start"), "{body}");
    assert!(
        order(&body, "message_start", "content_block_start"),
        "start opens the block: {body}"
    );
    assert!(
        body.contains("text_delta") && body.contains("the assistant reply"),
        "the reply rides a text_delta: {body}"
    );
    assert!(
        order(&body, "content_block_delta", "content_block_stop"),
        "deltas precede the block stop: {body}"
    );
    // The closing delta carries the stop reason, the metered tokens, and the
    // verified receipt.
    assert!(
        body.contains(r#""stop_reason":"end_turn""#),
        "a natural stop: {body}"
    );
    assert!(
        body.contains(r#""output_tokens":3"#),
        "the metered output tokens: {body}"
    );
    assert!(
        body.contains(r#""receipt_verified":true"#),
        "the covenant proof rides message_delta: {body}"
    );
    assert!(
        order(&body, "message_delta", "message_stop"),
        "delta precedes stop: {body}"
    );
    assert!(body.contains("event: message_stop"), "{body}");
}

#[tokio::test]
async fn a_streaming_message_emits_tool_use_from_the_verified_output() {
    // A tools job runs the backend non-streaming, so no tool call rides the
    // live feed. The stream must still deliver the call, emitted whole from
    // the verified receipt as a tool_use block, and stop for tool_use.
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 32,
            "messages": [{ "role": "user", "content": "weather in Paris?" }],
            "tools": [{
                "name": "get_weather",
                "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } }
            }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(
        body.contains(r#""type":"tool_use""#) && body.contains(r#""name":"get_weather""#),
        "a tool_use block opens: {body}"
    );
    assert!(
        body.contains(r#""type":"input_json_delta""#) && body.contains(r#"{\"city\":\"Paris\"}"#),
        "the arguments ride an input_json_delta: {body}"
    );
    assert!(
        body.contains(r#""stop_reason":"tool_use""#),
        "stops for tool_use: {body}"
    );
    assert!(body.contains("event: message_stop"), "{body}");
    assert!(body.contains(r#""receipt_verified":true"#), "{body}");

    // A pure tool-call turn carries no text block: none is opened, and the
    // tool_use block takes index 0, matching Anthropic's own stream rather
    // than leaving an empty leading text block at index 0 and the call at 1.
    assert!(
        !body.contains(r#""type":"text""#),
        "no empty text block is opened: {body}"
    );
    assert!(
        body.contains(r#""index":0"#) && !body.contains(r#""index":1"#),
        "the tool_use block opens at index 0: {body}"
    );
}

#[tokio::test]
async fn count_tokens_is_refused_with_a_clear_message() {
    let endpoint = Endpoint::launch().await;

    // The network cannot count tokens before a job runs (no local tokenizer),
    // so count_tokens is a clear, named refusal rather than a guessed number
    // or a bare framework 404 the SDK would surface as an opaque error.
    let resp = Endpoint::client()
        .post(format!("{}/v1/messages/count_tokens", endpoint.base))
        .header("x-api-key", &endpoint.api_key)
        .json(&json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "how many tokens is this?" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("token count"),
        "names the reason: {body}"
    );

    // A wrong key is rejected before the refusal, in Anthropic's 401 shape.
    let unauth = Endpoint::client()
        .post(format!("{}/v1/messages/count_tokens", endpoint.base))
        .header("x-api-key", "sk-wrong")
        .json(&json!({ "model": SERVED_MODEL, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
    assert_eq!(
        unauth.json::<Value>().await.unwrap()["error"]["type"],
        "authentication_error"
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_streaming_message_against_a_real_backend() {
    // A real streamed message: `/v1/messages` with stream → coordinator →
    // node → real Ollama live feed → a verified receipt on the closing
    // frame. The hop a mocked executor's canned feed can't cover.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .messages(json!({
            "model": SERVED_MODEL,
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "Say hello in one short sentence." }],
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("event: message_start"), "{body}");
    assert!(
        body.contains(r#""type":"text_delta""#),
        "a real backend streams text deltas: {body}"
    );
    assert!(
        order(&body, "message_delta", "message_stop"),
        "closes in order: {body}"
    );
    assert!(
        body.contains(r#""receipt_verified":true"#),
        "the streamed job settled on a verified receipt: {body}"
    );
}

#[tokio::test]
async fn a_response_buys_a_verified_job_and_returns_responses_shape() {
    // The modern `client.responses.create(...)` surface: a bare string
    // input, no Covenant-specific fields, buys a job on the network and comes
    // back as a standard Response object carrying the operator's receipt.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hello",
            "temperature": 0,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "response");
    assert!(
        body["id"].as_str().unwrap().starts_with("resp_"),
        "id: {}",
        body["id"]
    );
    assert_eq!(body["status"], "completed");
    assert_eq!(body["model"], SERVED_MODEL);
    assert_eq!(body["error"], Value::Null);
    assert_eq!(body["incomplete_details"], Value::Null);

    let item = &body["output"][0];
    assert_eq!(item["type"], "message");
    assert_eq!(item["role"], "assistant");
    assert!(
        item["id"].as_str().unwrap().starts_with("msg_"),
        "message id: {}",
        item["id"]
    );
    let part = &item["content"][0];
    assert_eq!(part["type"], "output_text");
    assert_eq!(part["text"], "the assistant reply");

    assert_eq!(body["usage"]["input_tokens"], 7);
    assert_eq!(body["usage"]["output_tokens"], 3);
    assert_eq!(body["usage"]["total_tokens"], 10);

    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert!(body["covenant"]["job_id"].is_string());
    assert!(body["covenant"]["operator_pubkey_b58"].is_string());
}

#[tokio::test]
async fn responses_instructions_and_array_input_reach_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, ChatRole};
    // `instructions` leads as a system turn and a typed input array becomes
    // the conversation, all inside the signed, paid envelope the node runs —
    // not reshaped or dropped at the door.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "instructions": "be terse",
            "input": [
                { "role": "user", "content": "hi" },
                { "type": "message", "role": "assistant", "content": "hello" },
                { "role": "user", "content": [{ "type": "input_text", "text": "again" }] },
            ],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let messages = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    let shape: Vec<(ChatRole, &str)> = messages
        .iter()
        .map(|m| (m.role, m.content.as_str()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (ChatRole::System, "be terse"),
            (ChatRole::User, "hi"),
            (ChatRole::Assistant, "hello"),
            (ChatRole::User, "again"),
        ]
    );
}

#[tokio::test]
async fn a_truncated_response_reports_incomplete_with_max_output_tokens() {
    // A generation the backend cut at the token cap is `incomplete`, and the
    // Responses API reports why so a client can tell a short answer from a
    // truncated one.
    let endpoint = Endpoint::launch_with_executor(TruncatedExecutor).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "write forever",
            "max_output_tokens": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "incomplete", "{body}");
    assert_eq!(body["incomplete_details"]["reason"], "max_output_tokens");
    assert_eq!(body["output"][0]["status"], "incomplete");
    assert_eq!(body["covenant"]["receipt_verified"], true);
}

#[tokio::test]
async fn responses_refuses_unserved_features_in_openai_shape() {
    // The slice serves plain text, streaming or not. A knob it can't honor is
    // a clear 400 in OpenAI error shape before any spend, never a silent drop
    // that bills the buyer for something other than what they asked.
    let endpoint = Endpoint::launch().await;

    // A hosted tool runs inside OpenAI's own service, so the network can't
    // execute it: refused, not forwarded as a name-only no-op.
    let hosted = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "tools": [{ "type": "web_search" }],
        }))
        .await;
    assert_eq!(hosted.status(), 400);
    let hosted_body = hosted.json::<Value>().await.unwrap();
    assert_eq!(hosted_body["error"]["type"], "invalid_request_error");
    assert!(
        hosted_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hosted tool"),
        "names the hosted tool: {hosted_body}"
    );

    // The network can't promise one tool call per turn, so disabling parallel
    // calls is refused rather than accepted and broken.
    let serial = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "tools": [{ "type": "function", "name": "f", "parameters": {} }],
            "parallel_tool_calls": false,
        }))
        .await;
    assert_eq!(serial.status(), 400);
    assert!(serial.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("parallel_tool_calls=false is not supported"));

    let chained = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "previous_response_id": "resp_123",
        }))
        .await;
    assert_eq!(chained.status(), 400);
    assert!(chained.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("previous_response_id is not supported"));

    // `truncation: "auto"` asks the network to drop input items to fit an
    // overflowing context, which the wire can't carry — so it is refused, not
    // dropped and then falsely echoed back as "disabled".
    let truncated = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "truncation": "auto",
        }))
        .await;
    assert_eq!(truncated.status(), 400);
    assert!(truncated.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("truncation"));
}

#[tokio::test]
async fn a_streaming_response_relays_deltas_then_completes() {
    // `client.responses.create(stream=True)`: the typed Responses event
    // sequence a modern SDK or the Agents SDK reads, ending on
    // response.completed carrying the verified receipt.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hello",
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"],
        "text/event-stream",
        "a streamed response is server-sent events"
    );
    let body = resp.text().await.unwrap();

    for event in [
        "event: response.created",
        "event: response.output_item.added",
        "event: response.content_part.added",
        "event: response.output_text.delta",
        "event: response.output_text.done",
        "event: response.output_item.done",
        "event: response.completed",
    ] {
        assert!(body.contains(event), "missing {event}: {body}");
    }
    assert!(
        body.contains(r#""delta":"the assistant reply""#),
        "the live feed carries the token delta: {body}"
    );
    assert!(
        body.contains(r#""text":"the assistant reply""#),
        "output_text.done carries the full verified text: {body}"
    );
    assert!(
        body.contains(r#""receipt_verified":true"#),
        "the streamed job settled on a verified receipt: {body}"
    );
    assert!(
        !body.contains("event: response.incomplete"),
        "a full generation never reports incomplete: {body}"
    );
    assert!(
        order(&body, "response.created", "response.completed"),
        "opens before it closes: {body}"
    );
}

#[tokio::test]
async fn a_streaming_response_against_a_node_that_cannot_stream_still_completes() {
    // A node that returns a verified result but emits no live tokens must
    // still deliver the paid output: the reconciliation sends the whole
    // verified text as the delta before it closes, never an empty stream.
    let endpoint = Endpoint::launch_with_executor(SilentStreamExecutor).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hello",
            "stream": true,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(r#""delta":"the assistant reply""#),
        "the verified output rides back as a delta: {body}"
    );
    assert!(
        body.contains(r#""text":"the assistant reply""#),
        "and the completed text: {body}"
    );
    assert!(body.contains("event: response.completed"), "{body}");
    assert!(body.contains(r#""receipt_verified":true"#), "{body}");
}

#[tokio::test]
async fn a_streaming_truncated_response_ends_incomplete() {
    // A generation the backend cut at the token cap closes on
    // response.incomplete, never response.completed, so a streaming client
    // learns the answer was truncated.
    let endpoint = Endpoint::launch_with_executor(TruncatedExecutor).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "write forever",
            "stream": true,
            "max_output_tokens": 3,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("event: response.incomplete"),
        "a truncated stream closes incomplete: {body}"
    );
    assert!(
        body.contains(r#""reason":"max_output_tokens""#),
        "and reports why: {body}"
    );
    assert!(
        !body.contains("event: response.completed"),
        "never completed for a truncated generation: {body}"
    );
    assert!(body.contains(r#""receipt_verified":true"#), "{body}");
}

#[tokio::test]
async fn a_responses_json_schema_reaches_the_signed_job_and_echoes_back() {
    // The Agents SDK's structured `output_type` sends text.format json_schema.
    // The constraint must ride the signed, paid envelope, not be dropped so
    // the buyer pays for free-form text, and the response echoes the format a
    // strict SDK reads back.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "list three colors",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "colors",
                    "schema": { "type": "object" },
                    "strict": true
                }
            },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["text"]["format"]["name"], "colors");

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let params = parse_generation_params(&input)
        .expect("well-formed")
        .expect("a generation block rides the signed job");
    assert_eq!(
        params.response_format,
        Some(ResponseFormat::JsonSchema {
            name: "colors".into(),
            schema: json!({ "type": "object" }),
            strict: Some(true),
        })
    );
}

#[tokio::test]
async fn a_response_returns_function_calls_when_the_model_calls_a_tool() {
    // The Agents SDK's tool loop: a request offering a function comes back with
    // a function_call output item the SDK runs, then feeds the result back. The
    // call is emitted whole from the verified receipt, not streamed.
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "weather in Paris?",
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "look up the weather in a city",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "completed", "{body}");

    // A pure tool-call turn returns no message item, just the function_call.
    let output = body["output"].as_array().expect("output array");
    assert_eq!(output.len(), 1, "one function_call item: {body}");
    let call = &output[0];
    assert_eq!(call["type"], "function_call");
    assert_eq!(call["call_id"], "call_0");
    assert_eq!(call["name"], "get_weather");
    assert_eq!(call["arguments"], r#"{"city":"Paris"}"#);
    assert_eq!(call["status"], "completed");
    assert!(
        call["id"].as_str().unwrap().starts_with("fc_"),
        "the item carries its own id: {call}"
    );

    // The result is still a paid, verified job.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["usage"]["output_tokens"], 5);
}

#[tokio::test]
async fn responses_tools_and_a_tool_result_history_reach_the_signed_job() {
    use covenant_compute_protocol::{parse_chat_input, parse_tools_input, ChatRole};
    // A function tool, a replayed function_call, and a fed-back
    // function_call_output all travel into the signed, paid envelope, so the
    // operator runs the agent's real tool loop rather than a reshaped copy.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "look up the weather",
                "parameters": { "type": "object" }
            }],
            "input": [
                { "role": "user", "content": "weather in Paris?" },
                {
                    "type": "function_call",
                    "call_id": "call_0",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_0",
                    "output": "18C and clear"
                }
            ],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");

    // The tools list rode the signed job.
    let tools = parse_tools_input(&input)
        .expect("well-formed")
        .expect("a tools block rides the signed job");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].function.name, "get_weather");

    // The conversation carries the replayed call and the tool result.
    let chat = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    let assistant = chat
        .iter()
        .find(|m| m.role == ChatRole::Assistant)
        .expect("the replayed function_call turn");
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].id, "call_0");
    assert_eq!(assistant.tool_calls[0].function.name, "get_weather");
    let tool = chat
        .iter()
        .find(|m| m.role == ChatRole::Tool)
        .expect("the fed-back tool result");
    assert_eq!(tool.tool_call_id.as_deref(), Some("call_0"));
    assert_eq!(tool.content, "18C and clear");
}

#[tokio::test]
async fn a_response_echoes_the_offered_tools_and_forced_choice() {
    // A strict SDK reads response.tools and response.tool_choice back, so the
    // echo must reflect what the job actually offered, not a hardcoded default.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "look up the weather",
                "parameters": { "type": "object" }
            }],
            "tool_choice": { "type": "function", "name": "get_weather" },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "get_weather");
    assert_eq!(body["tools"][0]["description"], "look up the weather");
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "get_weather");
}

#[tokio::test]
async fn a_streaming_response_emits_function_calls_from_the_verified_output() {
    // A tools job runs the backend non-streaming, so no call rides the live
    // feed. The stream must still deliver each call, emitted whole from the
    // verified receipt as its own function_call item, and close on
    // response.completed carrying the same items and the receipt.
    let endpoint = Endpoint::launch_with_executor(ToolCallExecutor).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "weather in Paris?",
            "stream": true,
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "parameters": { "type": "object" }
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let body = resp.text().await.unwrap();

    for event in [
        "event: response.output_item.added",
        "event: response.function_call_arguments.delta",
        "event: response.function_call_arguments.done",
        "event: response.output_item.done",
        "event: response.completed",
    ] {
        assert!(body.contains(event), "missing {event}: {body}");
    }
    assert!(
        body.contains(r#""name":"get_weather""#),
        "the streamed call names the function: {body}"
    );
    assert!(
        body.contains(r#""call_id":"call_0""#),
        "the streamed call carries its correlation id: {body}"
    );
    assert!(
        body.contains(r#"\"city\":\"Paris\""#),
        "the arguments ride the stream: {body}"
    );
    assert!(
        order(
            &body,
            "function_call_arguments.delta",
            "function_call_arguments.done"
        ),
        "the argument delta precedes its done frame: {body}"
    );
    assert!(
        order(&body, "function_call_arguments.done", "response.completed"),
        "the call closes before the response does: {body}"
    );
    assert!(
        body.contains(r#""receipt_verified":true"#),
        "the streamed tool job settled on a verified receipt: {body}"
    );
}

#[tokio::test]
async fn a_response_echoes_request_metadata() {
    // A client attaches metadata to correlate a reply to its request. The
    // network stores nothing, so it rides back unchanged on the response.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "metadata": { "trace_id": "abc123", "tenant": "acme" },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["metadata"]["trace_id"], "abc123");
    assert_eq!(body["metadata"]["tenant"], "acme");

    // Non-object metadata is refused before any spend.
    let bad = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "metadata": "nope",
        }))
        .await;
    assert_eq!(bad.status(), 400);
    assert!(bad.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("metadata must be an object"));
}

#[tokio::test]
async fn a_streaming_response_echoes_request_metadata() {
    // The streamed terminal frame carries the same metadata a non-streamed
    // response does, so a client correlating on it works either way.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": "hi",
            "stream": true,
            "metadata": { "trace_id": "xyz789" },
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(r#""trace_id":"xyz789""#),
        "the streamed response carries the metadata: {body}"
    );
}

#[tokio::test]
async fn an_input_image_reaches_the_signed_job_on_responses() {
    use covenant_compute_protocol::parse_chat_input;
    // A Responses client attaching an image must have those bytes travel into
    // the signed, paid envelope, so the operator's vision model sees exactly
    // what the buyer paid to have looked at.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let endpoint = Endpoint::launch_with_executor(CapturingExecutor { seen: seen.clone() }).await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "what is in this image?" },
                    { "type": "input_image", "image_url": "data:image/png;base64,aGVsbG8=" },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 200);

    let input = seen.lock().unwrap().clone().expect("the node ran the job");
    let messages = parse_chat_input(&input)
        .expect("well-formed")
        .expect("a chat conversation rides the signed job");
    assert_eq!(messages[0].content, "what is in this image?");
    assert_eq!(messages[0].images, vec!["aGVsbG8=".to_string()]);
}

#[tokio::test]
async fn a_responses_remote_image_url_is_refused() {
    // The network never fetches a buyer's url; a remote input_image earns a
    // clean refusal before any spend, the same as the chat door.
    let endpoint = Endpoint::launch().await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_image", "image_url": "https://example.com/cat.png" },
                ],
            }],
        }))
        .await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("remote image url"),
        "got: {body}"
    );
}

#[tokio::test]
#[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
async fn live_response_against_a_real_backend() {
    // The whole Responses chain against a real model: a `/v1/responses`
    // request -> coordinator -> node -> real Ollama -> a verified receipt ->
    // a standard response object. Proves the Agents SDK's front door serves a
    // real backend, the one hop the mocked e2e can't reach.
    let endpoint = Endpoint::launch_with_executor(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let resp = endpoint
        .responses(json!({
            "model": SERVED_MODEL,
            "instructions": "Answer in one short sentence.",
            "input": "Name a primary color.",
            "max_output_tokens": 64,
            "temperature": 0,
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed", "{body}");
    let text = body["output"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "a real backend returns non-empty output text: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);
}
