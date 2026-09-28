//! Real model-serving executor over the de-facto standard OpenAI
//! chat-completions API — one backend for the whole class of servers
//! that speak it: vLLM, llama.cpp server, LM Studio, TGI, a hosted
//! endpoint, or Ollama's own `/v1` shim. Where [`crate::ollama`] is
//! the operator-friendliest single install, this is the pro seam:
//! whatever serving stack an operator already runs, if it answers
//! `POST /chat/completions` it can earn.
//!
//! Trust shape matches [`crate::ollama`]: the model server is the
//! operator's own trusted daemon; a job's prompt travels to it as HTTP
//! and only generated text comes back — no buyer-supplied bytes are
//! ever executed. A hosted endpoint additionally sees the prompt, which
//! is the operator's call to make; the optional bearer key is
//! operator-supplied config and is never logged.
//!
//! Metering is real where the backend reports it: `usage.prompt_tokens`
//! / `usage.completion_tokens` land as `tokens_in`/`tokens_out` in the
//! signed receipt's [`covenant_compute_protocol::JobMeter`]. Streaming
//! requests ask for the usage-bearing final chunk
//! (`stream_options.include_usage`), so a streamed job meters the same
//! as a one-shot one.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use covenant_compute_protocol::{
    assistant_output, embedding_output, embedding_texts, logprobs_block, parse_chat_input,
    parse_generation_params, parse_tools_input, ChatMessage, FinishReason, GenerationParams,
    JobEnvelopePayload, JobKind, ResponseFormat, TokenLogprob, ToolCall,
};
use covenant_mcp::Content;
use serde::Deserialize;

use crate::executor::{ChunkSink, ExecutionOutcome, ExecutorError, JobExecutor};

/// vLLM's default listen address; every other server in the class wants
/// an explicit `COVENANT_COMPUTE_OPENAI_URL` anyway (Ollama's shim is
/// `http://127.0.0.1:11434/v1`, LM Studio's `http://127.0.0.1:1234/v1`).
pub const DEFAULT_OPENAI_COMPAT_URL: &str = "http://127.0.0.1:8000/v1";

pub struct OpenAiCompatExecutor {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    /// Served when a job's `capability_requirement.model_id` is `None`.
    /// With no default either, such a job fails before any HTTP.
    default_model: Option<String>,
    /// What [`JobExecutor::health`] re-verifies against the live
    /// `/models` list — the models this node's registration advertised.
    /// Empty means reachability alone decides health.
    required_models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Completion {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
    /// Present only when the job asked for logprobs. OpenAI nests them
    /// under `logprobs.content`, one entry per generated token, already in
    /// the protocol's shape.
    #[serde(default)]
    logprobs: Option<ChoiceLogprobs>,
}

#[derive(Debug, Deserialize)]
struct ChoiceLogprobs {
    #[serde(default)]
    content: Vec<TokenLogprob>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    /// OpenAI's tool calls already carry an `id`, a `"function"` type and
    /// string-valued `arguments`, so they deserialize straight into the
    /// protocol shape — no per-backend mapping like Ollama's.
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// One `/embeddings` datum. `index` places the vector against its input;
/// the spec returns them in input order, but sorting by it removes any
/// dependence on that.
#[derive(Debug, Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

/// One `data:` payload of a `"stream": true` response. Deltas ride
/// `choices[0].delta.content`; the usage chunk (empty `choices`) comes
/// last before `[DONE]` on OpenAI/vLLM, while llama.cpp puts `usage` on
/// the final delta itself — both land here. An `error` object can
/// replace a chunk anywhere in the stream.
#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

impl OpenAiCompatExecutor {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client builds with default TLS backend");
        Self {
            http,
            base_url: trim_base(base_url.into()),
            api_key,
            default_model,
            required_models: Vec::new(),
        }
    }

    /// Pins the models this node advertises, so `health` fails when one
    /// vanishes from the backend (a vLLM restart with a different
    /// `--model`, an unloaded LM Studio model). The `"any"` wildcard
    /// means nothing to a model server and is dropped.
    pub fn require_models(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.required_models = models.into_iter().filter(|m| m != "any").collect();
        self
    }
}

fn trim_base(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

/// The OpenAI chat API carries images as `image_url` content parts, not
/// the message-level `images` array Ollama takes. Rewrite any message
/// that has images into that shape — its text becomes a `text` part and
/// each base64 image an `image_url` part, reconstructing the data URI the
/// wire form wants. A message with no images serializes unchanged, so
/// tool calls and plain turns pass through as before.
fn openai_messages(messages: &[ChatMessage]) -> serde_json::Value {
    let mut value =
        serde_json::to_value(messages).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    let Some(array) = value.as_array_mut() else {
        return value;
    };
    for message in array.iter_mut() {
        let images = match message.get("images").and_then(serde_json::Value::as_array) {
            Some(images) if !images.is_empty() => images.clone(),
            _ => continue,
        };
        let text = message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(serde_json::json!({"type": "text", "text": text}));
        }
        for image in images {
            let Some(b64) = image.as_str() else { continue };
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{b64}", image_mime_from_b64(b64)) },
            }));
        }
        if let Some(object) = message.as_object_mut() {
            object.insert("content".into(), serde_json::Value::Array(parts));
            object.remove("images");
        }
    }
    value
}

/// Best-effort image media type from a base64 payload's leading bytes,
/// read off the base64 prefix so nothing has to be decoded. Covers the
/// formats a vision model actually takes; an unrecognized prefix falls
/// back to PNG, which servers that sniff the real bytes accept anyway.
fn image_mime_from_b64(b64: &str) -> &'static str {
    if b64.starts_with("/9j/") {
        "image/jpeg"
    } else if b64.starts_with("R0lGOD") {
        "image/gif"
    } else if b64.starts_with("UklGR") {
        "image/webp"
    } else {
        "image/png"
    }
}

/// Buyer knobs → the chat-completions request's top-level fields. Only
/// present fields land, so anything the buyer left unset stays the
/// backend's default.
fn apply_generation(body: &mut serde_json::Value, params: &GenerationParams) {
    if let Some(t) = params.temperature {
        body["temperature"] = t.into();
    }
    if let Some(p) = params.top_p {
        body["top_p"] = p.into();
    }
    if let Some(n) = params.max_tokens {
        body["max_tokens"] = n.into();
    }
    if let Some(s) = params.seed {
        body["seed"] = s.into();
    }
    if let Some(pp) = params.presence_penalty {
        body["presence_penalty"] = pp.into();
    }
    if let Some(fp) = params.frequency_penalty {
        body["frequency_penalty"] = fp.into();
    }
    if let Some(stop) = &params.stop {
        body["stop"] = stop.clone().into();
    }
    if let Some(rf) = &params.response_format {
        body["response_format"] = openai_response_format(rf);
    }
    if let Some(top) = params.logprobs {
        body["logprobs"] = true.into();
        body["top_logprobs"] = top.into();
    }
}

/// A buyer's `response_format` → an OpenAI-compatible backend's own
/// `response_format` object: `{"type":"json_object"}` for free-form JSON,
/// the `{"type":"json_schema", "json_schema":{…}}` wrapper for structured
/// output. This is the shape the OpenAI front door parses on the way in,
/// serialized back for the backend on the way out.
fn openai_response_format(rf: &ResponseFormat) -> serde_json::Value {
    match rf {
        ResponseFormat::JsonObject => serde_json::json!({ "type": "json_object" }),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let mut json_schema = serde_json::json!({ "name": name, "schema": schema });
            if let Some(strict) = strict {
                json_schema["strict"] = (*strict).into();
            }
            serde_json::json!({ "type": "json_schema", "json_schema": json_schema })
        }
    }
}

/// Extracts the human-readable message from either error shape on the
/// wire — `{"error": {"message": "..."}}` or `{"error": "..."}`.
fn error_message(error: &serde_json::Value) -> String {
    error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

/// Model ids the server actually has (`GET /models`) — what an honest
/// node declares as `models_served` instead of a hand-typed claim
/// nothing checks.
pub async fn list_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>, ExecutorError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5));
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| ExecutorError::Failed(format!("backend unreachable at {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(ExecutorError::Failed(format!(
            "backend {url} returned {}",
            resp.status()
        )));
    }
    let models: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| ExecutorError::Failed(format!("decode /models: {e}")))?;
    Ok(models.data.into_iter().map(|m| m.id).collect())
}

/// Reachability that does not require a `/models` catalog: any HTTP
/// response — even a 404 — proves the server is up, and only a transport
/// failure is a dead backend. The health gate falls back to this for a
/// node whose models are pinned, so a backend that serves only
/// `/chat/completions` (some proxied or hosted openai-compat servers do)
/// still counts as healthy.
async fn reachable(base_url: &str, api_key: Option<&str>) -> Result<(), ExecutorError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5));
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    req.send()
        .await
        .map(|_| ())
        .map_err(|e| ExecutorError::Failed(format!("backend unreachable at {url}: {e}")))
}

#[async_trait]
impl JobExecutor for OpenAiCompatExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.run(job, deadline, None).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.run(job, deadline, Some(&sink)).await
    }

    async fn health(&self) -> Result<(), ExecutorError> {
        match list_models(&self.base_url, self.api_key.as_deref()).await {
            Ok(live) => crate::executor::require_models_served(&self.required_models, &live),
            // The catalog read failed. With models pinned, the operator
            // has declared what this node serves, so a backend that
            // answers but has no /models route is still healthy —
            // reachability is enough. Unpinned, the node has no other way
            // to know what it serves, so the failure stands.
            Err(_) if !self.required_models.is_empty() => {
                reachable(&self.base_url, self.api_key.as_deref()).await
            }
            Err(e) => Err(e),
        }
    }
}

impl OpenAiCompatExecutor {
    /// A job that names no model, on a node advertising exactly one,
    /// unambiguously wants that model — serve it rather than fault a job
    /// the node can plainly fulfil. With several advertised and no
    /// default the choice is genuine, so resolution fails instead.
    fn sole_served_model(&self) -> Option<String> {
        match self.required_models.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }

    async fn run(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: Option<&ChunkSink>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let model = job
            .capability_requirement
            .model_id
            .clone()
            .or_else(|| self.default_model.clone())
            .or_else(|| self.sole_served_model())
            .ok_or_else(|| {
                ExecutorError::Failed(
                    "job names no model_id and the node has no default model to fall back on \
                     (it serves several — set COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL)"
                        .into(),
                )
            })?;
        if job.kind == JobKind::Embedding {
            return self.embed(&model, &job.input, deadline).await;
        }
        // A chat-shaped job that fails to parse must fail here, not
        // fall through to the raw-prompt path with misread paid input —
        // and a malformed generation block must fail the same way, not
        // run the job at backend defaults.
        let chat =
            parse_chat_input(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        let generation = parse_generation_params(&job.input)
            .map_err(|e| ExecutorError::Failed(e.to_string()))?;
        let messages = match chat {
            Some(messages) => messages,
            None => {
                let prompt = job
                    .input
                    .iter()
                    .find_map(|c| match c {
                        Content::Text { text } => Some(text.clone()),
                        Content::Json { .. } => None,
                    })
                    .ok_or_else(|| {
                        ExecutorError::Failed("no Content::Text prompt in job.input".into())
                    })?;
                vec![ChatMessage::user(prompt)]
            }
        };

        let tools =
            parse_tools_input(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        // Tool calls arrive whole in one response, so a job that offers
        // tools runs the model non-streaming even when the buyer asked to
        // stream. The attested output still carries every call, and the
        // streaming front door emits them from the verified receipt.
        // Logprobs ride the same path: the backend reports them on the
        // final response, so a logprobs job runs non-streaming too.
        let logprobs_requested = generation.as_ref().and_then(|g| g.logprobs).is_some();
        let effective_sink = if tools.is_some() || logprobs_requested {
            None
        } else {
            sink
        };

        let started = Instant::now();
        let mut body = serde_json::json!({
            "model": model,
            "messages": openai_messages(&messages),
            "stream": effective_sink.is_some(),
        });
        if effective_sink.is_some() {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
        if let Some(tools) = &tools {
            body["tools"] = serde_json::to_value(&tools.tools)
                .map_err(|e| ExecutorError::Failed(format!("encode tools: {e}")))?;
            if let Some(choice) = &tools.tool_choice {
                body["tool_choice"] = serde_json::to_value(choice)
                    .map_err(|e| ExecutorError::Failed(format!("encode tool_choice: {e}")))?;
            }
        }
        if let Some(params) = generation {
            apply_generation(&mut body, &params);
        }
        let resp = self.post("/chat/completions", body, deadline).await?;

        let (text, tokens_in, tokens_out, finish_reason, tool_calls, logprobs) =
            match effective_sink {
                None => {
                    let raw =
                        crate::executor::read_body_capped(resp, MAX_STREAM_OUTPUT_BYTES, deadline)
                            .await?;
                    let completion: Completion = serde_json::from_slice(&raw).map_err(|e| {
                        ExecutorError::Failed(format!("decode /chat/completions: {e}"))
                    })?;
                    let choice = completion.choices.into_iter().next().ok_or_else(|| {
                        ExecutorError::Failed("backend returned no choices".into())
                    })?;
                    let backend_reason = choice
                        .finish_reason
                        .as_deref()
                        .and_then(FinishReason::from_backend);
                    let tool_calls = choice.message.tool_calls;
                    // A `null` content is a non-answer — a content-filter block
                    // or an empty completion — and still fails so it refunds,
                    // rather than settling an empty `Ok` the buyer paid for. A
                    // tool-only turn is the exception: it carries no prose but
                    // the tool calls are the paid result.
                    let content = match choice.message.content {
                        Some(content) => content,
                        None if !tool_calls.is_empty() => String::new(),
                        None => {
                            return Err(ExecutorError::Failed(
                                "backend returned a choice with null content and no tool calls"
                                    .into(),
                            ))
                        }
                    };
                    let (tokens_in, tokens_out) = completion
                        .usage
                        .map(|u| (u.prompt_tokens, u.completion_tokens))
                        .unwrap_or((None, None));
                    let finish_reason =
                        FinishReason::for_tool_turn(backend_reason, !tool_calls.is_empty());
                    let logprobs = choice.logprobs.map(|l| l.content);
                    (
                        content,
                        tokens_in,
                        tokens_out,
                        finish_reason,
                        tool_calls,
                        logprobs,
                    )
                }
                Some(sink) => {
                    let (text, tokens_in, tokens_out, finish_reason) =
                        drain_sse(resp, sink, deadline).await?;
                    (text, tokens_in, tokens_out, finish_reason, Vec::new(), None)
                }
            };

        crate::executor::ensure_answered(&text, &tool_calls)?;
        crate::executor::ensure_tool_choice_honored(
            tools.as_ref().and_then(|t| t.tool_choice.as_ref()),
            &tool_calls,
        )?;
        crate::executor::ensure_logprobs_delivered(logprobs_requested, logprobs.as_deref())?;
        let mut output = assistant_output(text, tool_calls);
        if let Some(logprobs) = logprobs {
            output.push(logprobs_block(logprobs));
        }
        Ok(ExecutionOutcome {
            output,
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in,
            tokens_out,
            finish_reason,
        })
    }

    /// Embeds `input`'s text blocks over `/embeddings`: one request
    /// carrying every text, one vector back per text, metered by
    /// `usage.prompt_tokens`. Vectors are re-ordered by their reported
    /// `index` before pairing, so a backend that answers out of order
    /// still binds each vector to the right input. There is no streaming
    /// form, so an embedding job runs one-shot whatever the envelope's
    /// `stream` flag says; a count mismatch fails the job rather than
    /// mispairing under an Ok receipt.
    async fn embed(
        &self,
        model: &str,
        input: &[Content],
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let texts = embedding_texts(input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        let started = Instant::now();
        let body = serde_json::json!({ "model": model, "input": texts });
        let resp = self.post("/embeddings", body, deadline).await?;
        let raw =
            crate::executor::read_body_capped(resp, MAX_STREAM_OUTPUT_BYTES, deadline).await?;
        let mut parsed: EmbeddingsResponse = serde_json::from_slice(&raw)
            .map_err(|e| ExecutorError::Failed(format!("decode /embeddings: {e}")))?;
        if parsed.data.len() != texts.len() {
            return Err(ExecutorError::Failed(format!(
                "backend returned {} embeddings for {} inputs",
                parsed.data.len(),
                texts.len()
            )));
        }
        parsed.data.sort_by_key(|d| d.index);
        // A passing count check plus a sort only pairs correctly when the
        // indices are exactly 0..n; a gap or a duplicate (a broken backend)
        // would silently bind a vector to the wrong input, so reject it.
        if parsed.data.iter().enumerate().any(|(i, d)| d.index != i) {
            return Err(ExecutorError::Failed(
                "backend returned embeddings with non-sequential indices".into(),
            ));
        }
        let tokens_in = parsed.usage.and_then(|u| u.prompt_tokens);
        let embeddings: Vec<Vec<f32>> = parsed.data.into_iter().map(|d| d.embedding).collect();
        crate::executor::ensure_finite_embeddings(&embeddings)?;
        crate::executor::ensure_uniform_embedding_width(&embeddings)?;
        Ok(ExecutionOutcome {
            output: vec![embedding_output(model, embeddings)],
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in,
            tokens_out: None,
            finish_reason: None,
        })
    }

    async fn post(
        &self,
        path: &str,
        body: serde_json::Value,
        deadline: Duration,
    ) -> Result<reqwest::Response, ExecutorError> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .timeout(deadline)
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                ExecutorError::Timeout(deadline)
            } else {
                ExecutorError::Failed(format!("backend request: {e}"))
            }
        })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = crate::executor::read_error_snippet(resp).await;
            // Model servers answer errors as {"error": {"message": ...}}
            // or {"error": "..."} — surface that message so a "model not
            // found" reads as one in the operator's logs, not as the JSON
            // envelope around it (the same shape `error_message` already
            // unwraps on the streaming path). A body of any other shape
            // falls back to the raw text.
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").map(error_message))
                .unwrap_or(body);
            return Err(ExecutorError::Failed(format!(
                "backend returned {status}: {}",
                detail.chars().take(300).collect::<String>()
            )));
        }
        Ok(resp)
    }
}

/// Ceiling on accumulated streamed output, mirroring the subprocess
/// executor's stdout cap and for the same reason: the full text rides
/// back through the coordinator's job record and journal. Past it the
/// job FAILS rather than truncating — a clipped result under an `Ok`
/// receipt would be paid-for data the buyer never got.
const MAX_STREAM_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Reads an SSE stream to `[DONE]` (or EOF): relays each content delta
/// into `sink` as it arrives, accumulates the complete text for the
/// receipt path, and takes token counts off whichever chunk carries
/// `usage`. Non-`data:` lines (`event:`, `id:`, keep-alive comments)
/// are protocol furniture, skipped. A delta nobody receives anymore is
/// dropped, never an error — the sink is a preview, and the accumulated
/// text still settles the job.
async fn drain_sse(
    mut resp: reqwest::Response,
    sink: &ChunkSink,
    deadline: Duration,
) -> Result<(String, Option<u64>, Option<u64>, Option<FinishReason>), ExecutorError> {
    let mut full = String::new();
    let mut tokens = (None, None);
    let mut finish_reason = None;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let piece = match resp.chunk().await {
            Ok(Some(piece)) => piece,
            Ok(None) => break,
            Err(e) if e.is_timeout() => return Err(ExecutorError::Timeout(deadline)),
            Err(e) => return Err(ExecutorError::Failed(format!("backend stream: {e}"))),
        };
        buf.extend_from_slice(&piece);
        // Bound the unparsed tail too: a backend that never sends a
        // newline would otherwise grow `buf` without limit before the
        // per-line cap in `apply_sse_line` ever gets to look at it.
        if buf.len() > MAX_STREAM_OUTPUT_BYTES {
            return Err(ExecutorError::Failed(format!(
                "output exceeded the {MAX_STREAM_OUTPUT_BYTES}-byte ceiling"
            )));
        }
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            if apply_sse_line(&line, sink, &mut full, &mut tokens, &mut finish_reason).await? {
                return Ok((full, tokens.0, tokens.1, finish_reason));
            }
        }
    }
    // A clean EOF can still deliver the terminator on an unterminated
    // final line, so apply that line before judging completeness.
    let saw_done = apply_sse_line(&buf, sink, &mut full, &mut tokens, &mut finish_reason).await?;
    // A stream ends with `data: [DONE]` or, on a backend that omits it, a
    // terminal `finish_reason` on the last chunk. A clean EOF carrying
    // neither means the generation was cut short — a proxy half-close, an
    // OOM-killed upstream — so the assembled text is not a complete result
    // and must not settle under an Ok receipt, the same guard the ollama
    // adapter applies to a missing `done` line. A hard mid-stream drop has
    // already surfaced as an error above.
    if !saw_done && finish_reason.is_none() {
        return Err(ExecutorError::Failed(
            "backend stream ended without [DONE] or a finish_reason; the generation did not \
             complete"
                .into(),
        ));
    }
    Ok((full, tokens.0, tokens.1, finish_reason))
}

/// Applies one SSE line; `Ok(true)` means the stream signalled
/// completion with `data: [DONE]`.
async fn apply_sse_line(
    raw: &[u8],
    sink: &ChunkSink,
    full: &mut String,
    tokens: &mut (Option<u64>, Option<u64>),
    finish_reason: &mut Option<FinishReason>,
) -> Result<bool, ExecutorError> {
    let line = String::from_utf8_lossy(raw);
    let line = line.trim();
    let Some(payload) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let payload = payload.trim();
    if payload == "[DONE]" {
        return Ok(true);
    }
    let event: StreamEvent = serde_json::from_str(payload)
        .map_err(|e| ExecutorError::Failed(format!("decode stream event: {e}")))?;
    if let Some(error) = event.error {
        return Err(ExecutorError::Failed(format!(
            "backend stream error: {}",
            error_message(&error)
        )));
    }
    if let Some(usage) = event.usage {
        *tokens = (usage.prompt_tokens, usage.completion_tokens);
    }
    if let Some(choice) = event.choices.into_iter().next() {
        // The final content chunk carries `finish_reason`; a later
        // usage-only chunk has no choices, so this keeps the last one seen.
        if let Some(reason) = choice
            .finish_reason
            .as_deref()
            .and_then(FinishReason::from_backend)
        {
            *finish_reason = Some(reason);
        }
        if let Some(delta) = choice.delta.content {
            if !delta.is_empty() {
                if full.len() + delta.len() > MAX_STREAM_OUTPUT_BYTES {
                    return Err(ExecutorError::Failed(format!(
                        "output exceeded the {MAX_STREAM_OUTPUT_BYTES}-byte ceiling"
                    )));
                }
                full.push_str(&delta);
                let _ = sink.send(delta).await;
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{CapabilityRequirement, JobKind};
    use covenant_types::AgentId;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    fn job(prompt: &str, model_id: Option<&str>) -> JobEnvelopePayload {
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@local", [1u8; 32]),
            kind: JobKind::InferenceCall,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: model_id.map(String::from),
                kind: JobKind::InferenceCall,
                max_duration_secs: 5,
                min_reputation_bps: None,
            },
            input: vec![Content::text(prompt)],
            price_micro_usdc: 10,
            deadline_ms: 5_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "openai-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    type Captured = Arc<Mutex<Option<(Option<String>, serde_json::Value)>>>;

    /// A completions stub that records the Authorization header and
    /// request body, answering with `response`.
    fn capturing_completions(seen: Captured, response: String) -> Router {
        Router::new().route(
            "/chat/completions",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| {
                    let seen = seen.clone();
                    let response = response.clone();
                    async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        *seen.lock().await = Some((auth, body));
                        response
                    }
                },
            ),
        )
    }

    fn completion_json(content: &str, tokens: Option<(u64, u64)>) -> String {
        let mut body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }],
        });
        if let Some((prompt, completion)) = tokens {
            body["usage"] = serde_json::json!({
                "prompt_tokens": prompt,
                "completion_tokens": completion,
            });
        }
        body.to_string()
    }

    #[tokio::test]
    async fn completes_and_meters_tokens_from_the_usage_report() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router =
            capturing_completions(seen.clone(), completion_json("the answer", Some((12, 34))));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let outcome = executor
            .execute(
                &job("what is the answer?", Some("qwen-test")),
                Duration::from_secs(2),
            )
            .await
            .expect("completion succeeds");

        assert_eq!(outcome.output, vec![Content::text("the answer")]);
        assert_eq!(outcome.tokens_in, Some(12));
        assert_eq!(outcome.tokens_out, Some(34));

        let (auth, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(auth, None, "no key configured, no Authorization header");
        assert_eq!(body["model"], "qwen-test");
        assert_eq!(body["stream"], false);
        assert!(
            body.get("stream_options").is_none(),
            "stream_options only rides streaming requests"
        );
        // A raw prompt becomes the single user message.
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "what is the answer?");
        assert!(
            body.get("temperature").is_none() && body.get("max_tokens").is_none(),
            "a job without generation params must not constrain the backend"
        );
    }

    #[tokio::test]
    async fn a_null_content_completion_is_refused_not_settled_as_empty() {
        // vLLM/OpenAI return `content: null` on a content-filter block. With
        // no tool calls to stand in for the answer, the executor must fail
        // the job (so it refunds), not sign an empty `Ok` the buyer pays
        // for. A tool-only turn (null content WITH tool calls) is the
        // separately-tested exception.
        let seen: Captured = Arc::new(Mutex::new(None));
        let response = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": null },
                "finish_reason": "content_filter"
            }]
        })
        .to_string();
        let router = capturing_completions(seen, response);
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(&job("blocked?", Some("qwen-test")), Duration::from_secs(2))
            .await
            .expect_err("a null-content completion must not settle as empty");
        assert!(
            matches!(err, ExecutorError::Failed(_)),
            "refused as a failed job: {err:?}"
        );
    }

    #[tokio::test]
    async fn an_empty_string_completion_is_refused_like_a_null_one() {
        // A backend that returns `content: ""` (not `null`) is a
        // non-answer just the same; it must refund, not settle an empty
        // paid `Ok`.
        let seen: Captured = Arc::new(Mutex::new(None));
        let response = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "" },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        let router = capturing_completions(seen, response);
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &job("say nothing", Some("qwen-test")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("an empty-string completion must not settle as paid output");
        assert!(matches!(err, ExecutorError::Failed(_)), "refused: {err:?}");
    }

    #[tokio::test]
    async fn a_whitespace_only_completion_is_refused_like_an_empty_one() {
        // A backend that answers with only whitespace ("\n\n") returned no
        // usable answer; it must refund, not settle a paid Ok over blank
        // output the buyer got nothing from.
        let seen: Captured = Arc::new(Mutex::new(None));
        let response = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "  \n\t " },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        let router = capturing_completions(seen, response);
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &job("say nothing", Some("qwen-test")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("a whitespace-only completion must not settle as paid output");
        assert!(matches!(err, ExecutorError::Failed(_)), "refused: {err:?}");
    }

    #[tokio::test]
    async fn a_content_filtered_stream_is_refused_not_settled_as_empty() {
        // The streaming twin of the null-content refusal: a stream that
        // carries a `finish_reason` but no content deltas (a content-filter
        // block, or a backend that closes having emitted nothing) must fail
        // the job, exactly as the non-streaming path does — the refusal
        // can't depend on the `stream` flag.
        let body = sse(
            &[
                serde_json::json!({
                    "choices": [{ "delta": { "role": "assistant" }, "finish_reason": "content_filter" }]
                }),
                serde_json::json!({
                    "choices": [],
                    "usage": { "prompt_tokens": 20, "completion_tokens": 0 },
                }),
            ],
            true,
        );
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = executor
            .execute_streaming(&job("blocked?", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect_err("an empty stream must not settle as paid output");
        assert!(matches!(err, ExecutorError::Failed(_)), "refused: {err:?}");
    }

    #[tokio::test]
    async fn a_content_filter_with_partial_content_settles_but_says_it_was_filtered() {
        // The backend cut the answer short for content policy but still
        // returned the part it generated. That is a real, paid partial
        // result — it settles — but the receipt must carry `ContentFilter`,
        // not a `None` the buyer would read as a clean stop over a full
        // answer.
        let response = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "here is the start of the ans" },
                "finish_reason": "content_filter"
            }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 6 }
        })
        .to_string();
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen, response);
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let outcome = executor
            .execute(
                &job("write an essay", Some("qwen-test")),
                Duration::from_secs(2),
            )
            .await
            .expect("partial content is a paid result, not a refusal");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(reply.text, "here is the start of the ans");
        assert_eq!(outcome.finish_reason, Some(FinishReason::ContentFilter));
    }

    #[tokio::test]
    async fn buyer_generation_knobs_land_as_completion_fields() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("4", None));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("2 + 2?")]);
        chat_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                temperature: Some(0.0),
                top_p: None,
                max_tokens: Some(64),
                seed: Some(7),
                presence_penalty: Some(0.25),
                frequency_penalty: None,
                stop: Some(vec!["\n".into()]),
                response_format: None,
                logprobs: None,
            })
            .expect("valid params"),
        );
        executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("completion succeeds");

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["seed"], 7);
        assert_eq!(body["presence_penalty"], 0.25);
        assert_eq!(body["stop"][0], "\n");
        assert!(
            body.get("top_p").is_none() && body.get("frequency_penalty").is_none(),
            "an unset knob must stay the backend's default"
        );
    }

    #[tokio::test]
    async fn a_logprobs_request_fails_when_the_backend_reports_none() {
        // A backend that ignores the logprobs request and answers without
        // any must not settle as a paid completion missing what the buyer
        // paid for; the job fails so the coordinator refunds.
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("4", Some((3, 1))));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("2 + 2?")]);
        chat_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                logprobs: Some(3),
                ..Default::default()
            })
            .expect("valid params"),
        );
        let err = executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect_err("a logprobs request the backend ignored must fail");
        assert!(err.to_string().contains("logprobs"), "got: {err}");
    }

    #[tokio::test]
    async fn a_logprobs_request_fails_when_the_backend_reports_an_empty_set() {
        // A backend can accept the request and return an empty
        // logprobs.content array — every token the buyer paid to score
        // dropped. A bare None check waves this through; an answered
        // completion always scored at least one token, so an empty set is
        // the same dropped feature and must fail so the coordinator refunds.
        let empty = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hi" },
                "logprobs": { "content": [] },
            }],
        })
        .to_string();
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), empty);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("2 + 2?")]);
        chat_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                logprobs: Some(3),
                ..Default::default()
            })
            .expect("valid params"),
        );
        let err = executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect_err("an empty logprobs set must fail like a missing one");
        assert!(err.to_string().contains("logprobs"), "got: {err}");
    }

    #[tokio::test]
    async fn a_named_tool_choice_fails_when_the_backend_calls_a_different_tool() {
        // A buyer who named get_weather paid to force that call. A backend
        // that ignores the choice and calls some other tool must not settle
        // as a paid completion; the job fails so the coordinator refunds.
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_backend",
                        "type": "function",
                        "function": { "name": "get_time", "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        let router = capturing_completions(Arc::new(Mutex::new(None)), response);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let mut tool_job = job("unused", Some("qwen-test"));
        tool_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("weather in Paris?")]);
        tool_job.input.push(
            covenant_compute_protocol::tools_input(
                vec![covenant_compute_protocol::ToolDefinition {
                    kind: covenant_compute_protocol::ToolKind::Function,
                    function: covenant_compute_protocol::FunctionDefinition {
                        name: "get_weather".into(),
                        description: None,
                        parameters: None,
                    },
                }],
                Some(covenant_compute_protocol::ToolChoice::Named(
                    covenant_compute_protocol::NamedToolChoice {
                        kind: covenant_compute_protocol::ToolKind::Function,
                        function: covenant_compute_protocol::NamedFunction {
                            name: "get_weather".into(),
                        },
                    },
                )),
            )
            .expect("valid tools"),
        );

        let err = executor
            .execute(&tool_job, Duration::from_secs(2))
            .await
            .expect_err("a forced tool the backend skipped must fail");
        assert!(err.to_string().contains("get_weather"), "got: {err}");
    }

    #[tokio::test]
    async fn a_truncated_tool_call_reports_length_not_a_clean_stop() {
        // A backend that hit its token limit mid-arguments returns
        // `finish_reason: "length"` beside a partial tool call. The turn must
        // report that truncation, not settle as a clean `tool_calls` stop
        // that hides the cut-off arguments from the buyer.
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_x",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Par" }
                    }]
                },
                "finish_reason": "length"
            }]
        })
        .to_string();
        let router = capturing_completions(Arc::new(Mutex::new(None)), response);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let mut tool_job = job("unused", Some("qwen-test"));
        tool_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("weather in Paris?")]);
        tool_job.input.push(
            covenant_compute_protocol::tools_input(
                vec![covenant_compute_protocol::ToolDefinition {
                    kind: covenant_compute_protocol::ToolKind::Function,
                    function: covenant_compute_protocol::FunctionDefinition {
                        name: "get_weather".into(),
                        description: None,
                        parameters: None,
                    },
                }],
                None,
            )
            .expect("valid tools"),
        );

        let outcome = executor
            .execute(&tool_job, Duration::from_secs(2))
            .await
            .expect("a truncated tool call still settles");
        assert_eq!(
            outcome.finish_reason,
            Some(covenant_compute_protocol::FinishReason::Length),
            "a cut-off tool call keeps its length reason, not tool_calls"
        );
    }

    #[tokio::test]
    async fn a_response_format_lands_on_the_completion_request() {
        for (rf, expected) in [
            (
                ResponseFormat::JsonObject,
                serde_json::json!({ "type": "json_object" }),
            ),
            (
                ResponseFormat::JsonSchema {
                    name: "weather".into(),
                    schema: serde_json::json!({ "type": "object" }),
                    strict: Some(true),
                },
                serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "weather",
                        "schema": { "type": "object" },
                        "strict": true,
                    }
                }),
            ),
        ] {
            let seen: Captured = Arc::new(Mutex::new(None));
            let router = capturing_completions(seen.clone(), completion_json("{}", None));
            let base = spawn(router).await;

            let executor = OpenAiCompatExecutor::new(base, None, None);
            let mut chat_job = job("unused", Some("qwen-test"));
            chat_job.input =
                covenant_compute_protocol::chat_input(vec![ChatMessage::user("reply in json")]);
            chat_job.input.push(
                covenant_compute_protocol::generation_input(GenerationParams {
                    response_format: Some(rf),
                    ..Default::default()
                })
                .expect("valid params"),
            );
            executor
                .execute(&chat_job, Duration::from_secs(2))
                .await
                .expect("completion succeeds");

            let (_, body) = seen.lock().await.clone().expect("backend was called");
            assert_eq!(body["response_format"], expected);
        }
    }

    #[tokio::test]
    async fn a_system_message_reaches_the_backend_with_its_role() {
        // The buyer's `--system` prompt packs as a system-role chat
        // message; the executor must relay it as one, ahead of the user
        // turn, not flatten the conversation into a single prompt.
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("ok", None));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![
            ChatMessage::system("be terse"),
            ChatMessage::user("what color is the sky?"),
        ]);
        executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("completion succeeds");

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "what color is the sky?");
    }

    #[tokio::test]
    async fn a_vision_message_becomes_openai_image_url_parts() {
        // OpenAI-shaped backends take images as `image_url` content parts,
        // not the message-level `images` array Ollama reads, so the
        // executor must reshape a vision turn and reconstruct each data URI.
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("a red square", None));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let mut chat_job = job("unused", Some("vision-model"));
        chat_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user_with_images(
                "what is in this image?",
                vec!["iVBORw0KGgoPNGDATA".into(), "/9j/JPEGDATA".into()],
            )]);
        executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("vision completion succeeds");

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        let message = &body["messages"][0];
        // The message-level images field is gone; content is the parts array.
        assert!(message.get("images").is_none());
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "what is in this image?");
        assert_eq!(message["content"][1]["type"], "image_url");
        assert_eq!(
            message["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgoPNGDATA"
        );
        // MIME is read off each image's own base64 prefix, per image.
        assert_eq!(
            message["content"][2]["image_url"]["url"],
            "data:image/jpeg;base64,/9j/JPEGDATA"
        );
    }

    #[tokio::test]
    async fn a_malformed_generation_block_fails_without_reaching_the_backend() {
        // Unroutable base: touching the network would surface as a
        // different error than the parse failure asserted here.
        let executor = OpenAiCompatExecutor::new("http://127.0.0.1:1", None, None);
        let mut bad_job = job("hi", Some("qwen-test"));
        bad_job.input.push(Content::json(serde_json::json!({
            "generation": { "temprature": 1.9 }
        })));
        let err = executor
            .execute(&bad_job, Duration::from_secs(1))
            .await
            .expect_err("typoed knob");
        assert!(err.to_string().contains("generation input"), "got: {err}");
    }

    #[tokio::test]
    async fn a_configured_api_key_rides_as_a_bearer() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("ok", None));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, Some("test-key-1".into()), None);
        let outcome = executor
            .execute(&job("hi", Some("m")), Duration::from_secs(2))
            .await
            .expect("completion succeeds");
        // No usage block: the job still completes, just unmetered.
        assert_eq!(outcome.tokens_in, None);
        assert_eq!(outcome.tokens_out, None);

        let (auth, _) = seen.lock().await.clone().unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer test-key-1"));
    }

    #[tokio::test]
    async fn falls_back_to_the_default_model_when_the_job_names_none() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("ok", None));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, Some("default-model".into()));
        executor
            .execute(&job("hi", None), Duration::from_secs(2))
            .await
            .expect("completion succeeds");
        let (_, body) = seen.lock().await.clone().unwrap();
        assert_eq!(body["model"], "default-model");
    }

    #[tokio::test]
    async fn serves_its_sole_model_when_the_job_names_none_and_no_default() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("ok", None));
        let base = spawn(router).await;

        let executor =
            OpenAiCompatExecutor::new(base, None, None).require_models(["only-model".to_string()]);
        executor
            .execute(&job("hi", None), Duration::from_secs(2))
            .await
            .expect("a single-model node serves the job it advertised");
        let (_, body) = seen.lock().await.clone().unwrap();
        assert_eq!(body["model"], "only-model");
    }

    #[tokio::test]
    async fn refuses_a_modelless_job_when_several_are_served_and_no_default() {
        let executor = OpenAiCompatExecutor::new("http://127.0.0.1:1", None, None)
            .require_models(["a".to_string(), "b".to_string()]);
        let err = executor
            .execute(&job("hi", None), Duration::from_secs(1))
            .await
            .expect_err("ambiguous model choice");
        assert!(err.to_string().contains("no model_id"), "got: {err}");
    }

    #[tokio::test]
    async fn a_chat_job_sends_the_full_conversation() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let router = capturing_completions(seen.clone(), completion_json("green", Some((21, 2))));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![
            ChatMessage::system("answer in one word"),
            ChatMessage::user("what color is grass?"),
        ]);
        let outcome = executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("chat succeeds");

        assert_eq!(outcome.output, vec![Content::text("green")]);
        assert_eq!(outcome.tokens_in, Some(21));
        assert_eq!(outcome.tokens_out, Some(2));

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "what color is grass?");
    }

    #[tokio::test]
    async fn a_malformed_chat_job_fails_without_reaching_the_backend() {
        // Unroutable base: touching the network would surface as a
        // different error than the parse failure asserted here.
        let executor = OpenAiCompatExecutor::new("http://127.0.0.1:1", None, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = vec![Content::json(serde_json::json!({
            "messages": [{ "role": "operator", "content": "hi" }]
        }))];
        let err = executor
            .execute(&chat_job, Duration::from_secs(1))
            .await
            .expect_err("malformed chat");
        assert!(err.to_string().contains("chat input"), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_before_any_http_when_no_model_is_resolvable() {
        let executor = OpenAiCompatExecutor::new("http://127.0.0.1:1", None, None);
        let err = executor
            .execute(&job("hi", None), Duration::from_secs(1))
            .await
            .expect_err("no model resolvable");
        assert!(err.to_string().contains("no model_id"), "got: {err}");
    }

    #[tokio::test]
    async fn maps_a_backend_error_status_to_failed() {
        let router = Router::new().route(
            "/chat/completions",
            post(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    r#"{"error":{"message":"model 'missing' not found"}}"#,
                )
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(&job("hi", Some("missing")), Duration::from_secs(2))
            .await
            .expect_err("backend 404");
        assert!(matches!(err, ExecutorError::Failed(_)));
        let msg = err.to_string();
        assert!(msg.contains("404"), "keeps the status: {msg}");
        assert!(
            msg.contains("model 'missing' not found"),
            "surfaces the backend's message, not the envelope: {msg}"
        );
        assert!(
            !msg.contains(r#"{"error""#),
            "sheds the json envelope: {msg}"
        );
    }

    #[tokio::test]
    async fn an_empty_choices_completion_is_a_failure_not_empty_paid_output() {
        let router =
            Router::new().route("/chat/completions", post(|| async { r#"{"choices":[]}"# }));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(&job("hi", Some("m")), Duration::from_secs(2))
            .await
            .expect_err("no choices");
        assert!(err.to_string().contains("no choices"), "got: {err}");
    }

    #[tokio::test]
    async fn a_completion_past_the_deadline_is_a_timeout() {
        let router = Router::new().route(
            "/chat/completions",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                completion_json("too late", None)
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(&job("hi", Some("slow")), Duration::from_millis(100))
            .await
            .expect_err("deadline exceeded");
        assert!(matches!(err, ExecutorError::Timeout(_)), "got: {err}");
    }

    fn sse(events: &[serde_json::Value], done: bool) -> String {
        let mut body = String::new();
        for event in events {
            body.push_str(&format!("data: {event}\n\n"));
        }
        if done {
            body.push_str("data: [DONE]\n\n");
        }
        body
    }

    fn delta_event(content: &str) -> serde_json::Value {
        serde_json::json!({ "choices": [{ "delta": { "content": content } }] })
    }

    #[tokio::test]
    async fn a_streaming_completion_relays_deltas_and_meters_from_the_usage_chunk() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let body = sse(
            &[
                // Role-announcing first delta with no content, the
                // OpenAI-style opener.
                serde_json::json!({ "choices": [{ "delta": { "role": "assistant" } }] }),
                delta_event("the "),
                delta_event("answer"),
                serde_json::json!({
                    "choices": [],
                    "usage": { "prompt_tokens": 12, "completion_tokens": 34 },
                }),
            ],
            true,
        );
        let router = capturing_completions(seen.clone(), body);
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(
                &job("what is the answer?", Some("qwen-test")),
                Duration::from_secs(2),
                tx,
            )
            .await
            .expect("streaming completion succeeds");

        assert_eq!(outcome.output, vec![Content::text("the answer")]);
        assert_eq!(outcome.tokens_in, Some(12));
        assert_eq!(outcome.tokens_out, Some(34));

        let mut deltas = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }
        assert_eq!(deltas, vec!["the ", "answer"]);

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[tokio::test]
    async fn usage_on_the_final_delta_chunk_still_meters() {
        // llama.cpp's shape: no separate usage chunk, counts ride the
        // last delta.
        let body = sse(
            &[
                delta_event("gr"),
                serde_json::json!({
                    "choices": [{ "delta": { "content": "een" } }],
                    "usage": { "prompt_tokens": 21, "completion_tokens": 2 },
                }),
            ],
            true,
        );
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect("streaming completion succeeds");

        assert_eq!(outcome.output, vec![Content::text("green")]);
        assert_eq!(outcome.tokens_in, Some(21));
        assert_eq!(outcome.tokens_out, Some(2));
        assert_eq!(rx.recv().await.as_deref(), Some("gr"));
        assert_eq!(rx.recv().await.as_deref(), Some("een"));
    }

    #[tokio::test]
    async fn sse_furniture_lines_are_skipped_not_fatal() {
        let body = format!(
            ": keep-alive\nevent: message\nid: 7\n{}",
            sse(&[delta_event("served")], true)
        );
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect("furniture is not data");
        assert_eq!(outcome.output, vec![Content::text("served")]);
        assert_eq!(rx.recv().await.as_deref(), Some("served"));
    }

    #[tokio::test]
    async fn a_stream_cut_short_of_its_terminator_fails_rather_than_bill_a_partial() {
        // A clean EOF after some content but with neither `[DONE]` nor a
        // terminal `finish_reason` is indistinguishable from a proxy
        // half-close or an OOM-killed upstream: the generation did not
        // complete, so it must fail and refund, never settle the partial
        // text under an Ok receipt. This is the guard the ollama adapter
        // already applies to a missing `done` line.
        let body = sse(&[delta_event("partial answer")], false);
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect_err("a truncated stream must not settle");
        assert!(err.to_string().contains("did not complete"), "got: {err}");
    }

    #[tokio::test]
    async fn a_stream_that_omits_done_but_sends_a_finish_reason_still_settles() {
        // Some OpenAI-compatible backends close on the final chunk's
        // `finish_reason` without a trailing `[DONE]`. That is a complete
        // generation, so it settles: the completeness guard accepts a
        // terminal reason as proof, not only `[DONE]`.
        let body = sse(
            &[
                delta_event("done "),
                serde_json::json!({
                    "choices": [{ "delta": { "content": "answer" }, "finish_reason": "stop" }]
                }),
            ],
            false,
        );
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect("a finish_reason completes the stream even without [DONE]");
        assert_eq!(outcome.output, vec![Content::text("done answer")]);
    }

    #[tokio::test]
    async fn a_mid_stream_error_event_fails_the_job() {
        let body = sse(
            &[
                delta_event("half"),
                serde_json::json!({ "error": { "message": "model crashed" } }),
            ],
            false,
        );
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = executor
            .execute_streaming(&job("hi", Some("crashy")), Duration::from_secs(2), tx)
            .await
            .expect_err("mid-stream error");
        assert!(err.to_string().contains("model crashed"), "got: {err}");
    }

    #[tokio::test]
    async fn a_dropped_chunk_receiver_does_not_fail_the_stream() {
        let body = sse(&[delta_event("still "), delta_event("served")], true);
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let outcome = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(2), tx)
            .await
            .expect("nobody listening is not a job failure");
        assert_eq!(outcome.output, vec![Content::text("still served")]);
    }

    #[tokio::test]
    async fn streamed_output_past_the_ceiling_fails_the_job() {
        let big = "a".repeat(3 * 1024 * 1024);
        let body = sse(&[delta_event(&big), delta_event(&big)], true);
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let err = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(10), tx)
            .await
            .expect_err("oversized output must fail, not truncate");
        assert!(err.to_string().contains("ceiling"), "got: {err}");
    }

    #[tokio::test]
    async fn a_non_streamed_body_past_the_ceiling_fails_the_job() {
        // The default (non-streaming) path used to decode the body with
        // an unbounded `resp.json()`: a remote backend returning a body
        // past the streaming path's own ceiling would be hashed into an
        // Ok receipt and paid, or OOM the node. It must fail the same
        // way the streamed path does.
        let big = "a".repeat(5 * 1024 * 1024);
        let body = completion_json(&big, None);
        let router = Router::new().route("/chat/completions", post(move || async move { body }));
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(&job("hi", Some("big")), Duration::from_secs(10))
            .await
            .expect_err(
                "an oversized non-streamed body must fail, not be paid under an Ok receipt",
            );
        assert!(err.to_string().contains("ceiling"), "got: {err}");
    }

    #[tokio::test]
    async fn a_stream_line_that_never_ends_trips_the_ceiling() {
        // A backend that streams megabytes on one SSE line with no
        // newline must trip the ceiling, not buffer the line first.
        let big = format!("data: {}", "a".repeat(5 * 1024 * 1024));
        let router = Router::new().route(
            "/chat/completions",
            post(move || async move { big.clone() }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let err = executor
            .execute_streaming(&job("hi", Some("m")), Duration::from_secs(10), tx)
            .await
            .expect_err("an unbounded line must trip the ceiling");
        assert!(err.to_string().contains("ceiling"), "got: {err}");
    }

    #[tokio::test]
    async fn a_stalled_stream_past_the_deadline_is_a_timeout() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut discard = [0u8; 4096];
            let _ = sock.read(&mut discard).await;
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: text/event-stream\r\n\r\n",
            )
            .await
            .unwrap();
            sock.write_all(format!("data: {}\n\n", delta_event("partial")).as_bytes())
                .await
                .unwrap();
            sock.flush().await.unwrap();
            // Never send the rest, never close: the reader must give up
            // on its own deadline, not ours.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let executor = OpenAiCompatExecutor::new(format!("http://{addr}"), None, None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let err = executor
            .execute_streaming(&job("hi", Some("slow")), Duration::from_millis(300), tx)
            .await
            .expect_err("stalled stream");
        assert!(matches!(err, ExecutorError::Timeout(_)), "got: {err}");
        // The delta that made it out before the stall was still relayed.
        assert_eq!(rx.recv().await.as_deref(), Some("partial"));
    }

    #[tokio::test]
    async fn list_models_returns_the_backend_ids_with_the_key_attached() {
        let seen: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/models",
            get(move |headers: axum::http::HeaderMap| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(
                        headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from),
                    );
                    Json(serde_json::json!({
                        "object": "list",
                        "data": [
                            { "id": "qwen2.5-coder:7b", "object": "model" },
                            { "id": "llama3:8b", "object": "model" },
                        ]
                    }))
                }
            }),
        );
        let base = spawn(router).await;
        let models = list_models(&base, Some("test-key-2"))
            .await
            .expect("models list");
        assert_eq!(models, vec!["qwen2.5-coder:7b", "llama3:8b"]);
        let auth = seen.lock().await.clone().unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer test-key-2"));
    }

    #[tokio::test]
    async fn health_carries_the_key_and_verdicts_by_the_live_models() {
        let seen: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/models",
            get(move |headers: axum::http::HeaderMap| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(
                        headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from),
                    );
                    Json(serde_json::json!({
                        "object": "list",
                        "data": [{ "id": "qwen2.5-coder:7b", "object": "model" }]
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let healthy = OpenAiCompatExecutor::new(&base, Some("health-key".into()), None)
            .require_models(vec!["qwen2.5-coder:7b".into(), "any".into()]);
        healthy.health().await.expect("advertised model is live");
        let auth = seen.lock().await.clone().unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer health-key"));

        let stale =
            OpenAiCompatExecutor::new(&base, None, None).require_models(vec!["mistral:7b".into()]);
        let err = stale.health().await.expect_err("model no longer loaded");
        assert!(err.to_string().contains("mistral:7b"), "got: {err}");
    }

    #[tokio::test]
    async fn health_fails_when_the_backend_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let executor = OpenAiCompatExecutor::new(format!("http://{addr}"), None, None);
        let err = executor.health().await.expect_err("nothing listens there");
        assert!(err.to_string().contains("unreachable"), "got: {err}");
    }

    fn embed_job(texts: &[&str], model_id: Option<&str>) -> JobEnvelopePayload {
        let mut j = job("unused", model_id);
        j.kind = JobKind::Embedding;
        j.capability_requirement.kind = JobKind::Embedding;
        j.input = texts.iter().map(|t| Content::text(*t)).collect();
        j
    }

    #[tokio::test]
    async fn embeds_texts_reorders_by_index_and_meters_from_usage() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        // Answered out of order: the executor must pair each vector to
        // its input by `index`, not by arrival order.
        let router = Router::new().route(
            "/embeddings",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some((None, body));
                    Json(serde_json::json!({
                        "data": [
                            { "embedding": [0.4, 0.5, 0.6], "index": 1 },
                            { "embedding": [0.1, 0.2, 0.3], "index": 0 },
                        ],
                        "model": "nomic-embed-text",
                        "usage": { "prompt_tokens": 8, "total_tokens": 8 },
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OpenAiCompatExecutor::new(base, None, None);
        let outcome = executor
            .execute(
                &embed_job(&["hello", "world"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect("embedding succeeds");

        let result = covenant_compute_protocol::parse_embedding_output(&outcome.output)
            .expect("output is embedding-shaped");
        assert_eq!(result.dimensions, 3);
        assert_eq!(
            result.embeddings,
            vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]],
            "vectors land in input order, not response order"
        );
        assert_eq!(outcome.tokens_in, Some(8));
        assert_eq!(outcome.tokens_out, None);

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["model"], "nomic-embed-text");
        assert_eq!(body["input"], serde_json::json!(["hello", "world"]));
    }

    #[tokio::test]
    async fn a_count_mismatch_fails_the_job_rather_than_mispairing() {
        let router = Router::new().route(
            "/embeddings",
            post(|| async {
                Json(serde_json::json!({ "data": [{ "embedding": [0.1], "index": 0 }] }))
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &embed_job(&["one", "two"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("count mismatch");
        assert!(err.to_string().contains("1 embeddings for 2"), "got: {err}");
    }

    #[tokio::test]
    async fn duplicate_or_gapped_indices_fail_the_job_rather_than_mispairing() {
        // Right count, wrong indices: two data entries both claim index 0,
        // so input 1 has no vector. The count check passes; the executor
        // must still refuse rather than pair a vector to the wrong input.
        let router = Router::new().route(
            "/embeddings",
            post(|| async {
                Json(serde_json::json!({
                    "data": [
                        { "embedding": [0.1], "index": 0 },
                        { "embedding": [0.2], "index": 0 },
                    ],
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &embed_job(&["one", "two"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("non-sequential indices");
        assert!(
            err.to_string().contains("non-sequential indices"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn a_non_finite_embedding_fails_the_job_rather_than_paying_for_it() {
        // 1e40 is a finite JSON number that overflows f32. The right count
        // and index pass their checks; the executor parses embeddings at full
        // precision, so it refuses the component at decode rather than signing
        // an Ok receipt over an embedding the buyer cannot use.
        let router = Router::new().route(
            "/embeddings",
            post(|| async {
                Json(serde_json::json!({
                    "data": [{ "embedding": [1e40, 0.2], "index": 0 }],
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &embed_job(&["one"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("an out-of-range embedding must not settle Ok");
        assert!(err.to_string().contains("out of range"), "got: {err}");
    }

    #[tokio::test]
    async fn a_ragged_embedding_batch_fails_the_job_rather_than_paying_for_it() {
        // Two inputs, sequential indices, but vectors of differing widths:
        // the count and index checks pass, yet the signed `dimensions` would
        // be the first vector's width, so the node must refuse rather than
        // sign an Ok receipt over a batch the buyer's parser rejects.
        let router = Router::new().route(
            "/embeddings",
            post(|| async {
                Json(serde_json::json!({
                    "data": [
                        { "embedding": [0.1, 0.2, 0.3], "index": 0 },
                        { "embedding": [0.4, 0.5], "index": 1 },
                    ],
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);
        let err = executor
            .execute(
                &embed_job(&["one", "two"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("a ragged embedding batch must not settle Ok");
        assert!(err.to_string().contains("width"), "got: {err}");
    }

    #[tokio::test]
    async fn a_pinned_node_stays_healthy_when_the_backend_has_no_models_route() {
        // Some hosted or proxied openai-compat servers serve only
        // /chat/completions, so /models 404s.
        let router = Router::new().route(
            "/models",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "no such route") }),
        );
        let base = spawn(router).await;

        // Pinned: the operator declared the models, so a reachable backend
        // is healthy even without a catalog route.
        let pinned =
            OpenAiCompatExecutor::new(&base, None, None).require_models(vec!["llama3:8b".into()]);
        pinned
            .health()
            .await
            .expect("a pinned node is healthy on a reachable backend with no /models route");

        // Unpinned: the node relies on /models to know what it serves, so
        // the 404 is a genuine failure.
        let unpinned = OpenAiCompatExecutor::new(&base, None, None);
        let err = unpinned
            .health()
            .await
            .expect_err("an unpinned node needs the catalog");
        assert!(err.to_string().contains("404"), "got: {err}");
    }

    fn tool_call_job() -> JobEnvelopePayload {
        let mut job = job("unused", Some("qwen-test"));
        let tool = covenant_compute_protocol::ToolDefinition {
            kind: covenant_compute_protocol::ToolKind::Function,
            function: covenant_compute_protocol::FunctionDefinition {
                name: "get_weather".into(),
                description: None,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}}
                })),
            },
        };
        let mut input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("weather in Paris?")]);
        input.push(
            covenant_compute_protocol::tools_input(
                vec![tool],
                Some(covenant_compute_protocol::ToolChoice::Mode(
                    covenant_compute_protocol::ToolChoiceMode::Auto,
                )),
            )
            .expect("valid tools"),
        );
        job.input = input;
        job
    }

    #[tokio::test]
    async fn a_tools_job_sends_the_tools_and_maps_the_backend_tool_calls() {
        let seen: Captured = Arc::new(Mutex::new(None));
        // OpenAI-shaped tool calls: id + "function" type + string arguments,
        // and a `null` content that would fail without the tool calls.
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 18, "completion_tokens": 7}
        })
        .to_string();
        let router = capturing_completions(seen.clone(), response);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let outcome = executor
            .execute(&tool_call_job(), Duration::from_secs(2))
            .await
            .expect("a tool-only turn is a real result, not a null-content refusal");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(reply.text, "");
        assert_eq!(reply.tool_calls.len(), 1);
        // The backend's random `call_abc` is renumbered to its position in
        // the attested output, so a differently-configured node hashes the
        // same call identically instead of on the backend's random handle.
        assert_eq!(reply.tool_calls[0].id, "call_0");
        assert_eq!(reply.tool_calls[0].function.name, "get_weather");
        assert_eq!(
            reply.tool_calls[0].function.arguments,
            r#"{"city":"Paris"}"#
        );
        assert_eq!(outcome.finish_reason, Some(FinishReason::ToolCalls));

        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(
            body["stream"], false,
            "a tools job never streams the backend"
        );
    }

    #[tokio::test]
    async fn a_backends_tool_call_arguments_are_canonicalized_in_the_attested_output() {
        // The receipt must commit to the logical call, not the backend's
        // key order or spacing, so the same call re-run on a differently
        // configured node hashes to the same bytes. This backend returns the
        // arguments spaced and key-reversed; the attested output is canonical.
        let seen: Captured = Arc::new(Mutex::new(None));
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_0",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"units\": \"C\", \"city\": \"Paris\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 18, "completion_tokens": 7}
        })
        .to_string();
        let router = capturing_completions(seen.clone(), response);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let outcome = executor
            .execute(&tool_call_job(), Duration::from_secs(2))
            .await
            .expect("a tool-only turn is a real result");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(
            reply.tool_calls[0].function.arguments, r#"{"city":"Paris","units":"C"}"#,
            "the backend's spacing and key order are normalized away"
        );
    }

    #[tokio::test]
    async fn a_tools_job_runs_non_streaming_and_relays_no_deltas() {
        let seen: Captured = Arc::new(Mutex::new(None));
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        let router = capturing_completions(seen.clone(), response);
        let base = spawn(router).await;
        let executor = OpenAiCompatExecutor::new(base, None, None);

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&tool_call_job(), Duration::from_secs(2), tx)
            .await
            .expect("a streamed tools job still completes non-streaming");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(reply.tool_calls.len(), 1);
        assert!(rx.recv().await.is_none(), "nothing was relayed live");
        let (_, body) = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["stream"], false);
    }
}
