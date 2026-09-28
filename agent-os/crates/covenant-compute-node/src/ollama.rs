//! Real model-serving executor: inference over a local Ollama server
//! (design-01 §3's open "real model backend" seam, filled with the
//! operator-friendliest option — one install, consumer hardware, GPU
//! use handled by Ollama itself).
//!
//! Trust shape: the model server is the operator's own trusted-local
//! daemon; a job's prompt travels to it over loopback HTTP and only
//! generated text comes back. Unlike [`crate::executor::SubprocessJobExecutor`],
//! no buyer-supplied bytes are ever executed — for inference-shaped
//! work this is a strictly smaller untrusted surface, which is why the
//! sandbox question (design-01 §7) doesn't block this backend.
//!
//! Metering is real: Ollama reports `prompt_eval_count`/`eval_count`
//! per generation, which land as `tokens_in`/`tokens_out` in the
//! signed receipt's [`covenant_compute_protocol::JobMeter`].

use std::time::{Duration, Instant};

use async_trait::async_trait;
use covenant_compute_protocol::{
    assistant_output, embedding_output, embedding_texts, logprobs_block, parse_chat_input,
    parse_generation_params, parse_tools_input, ChatMessage, FinishReason, FunctionCall,
    GenerationParams, JobEnvelopePayload, JobKind, RequestTools, ResponseFormat, TokenLogprob,
    ToolCall, ToolCallKind,
};
use covenant_mcp::Content;
use serde::Deserialize;
use serde_json::Value;

use crate::executor::{ChunkSink, ExecutionOutcome, ExecutorError, GenerationResult, JobExecutor};

pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

pub struct OllamaExecutor {
    http: reqwest::Client,
    base_url: String,
    /// Served when a job's `capability_requirement.model_id` is `None`.
    /// With no default either, such a job fails before any HTTP.
    default_model: Option<String>,
    /// What [`JobExecutor::health`] re-verifies against the live
    /// `/api/tags` list — the models this node's registration
    /// advertised. Empty means reachability alone decides health.
    required_models: Vec<String>,
    /// Ollama's `keep_alive` on every request: how long the model stays
    /// resident after a job. `None` leaves Ollama's own default (unload
    /// after 5 min idle); an operator serving one model steadily sets
    /// this longer so a sparse-traffic reload can't blow a job deadline.
    keep_alive: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    done_reason: Option<String>,
    /// Present only when the job asked for logprobs; Ollama reports one
    /// entry per generated token, already in the protocol's shape.
    #[serde(default)]
    logprobs: Option<Vec<TokenLogprob>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    logprobs: Option<Vec<TokenLogprob>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
}

/// A tool call as Ollama reports it: no `id` and no `type` (both are
/// synthesized into the OpenAI shape at [`map_ollama_tool_calls`]), and
/// `arguments` as a JSON object rather than the string OpenAI uses.
#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// `/api/embed` always answers with a list of vectors (one per input,
/// in order), plus the prompt-token count that meters the job.
#[derive(Debug, Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    embeddings: Vec<Vec<f32>>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
}

/// One NDJSON line of a `"stream": true` response — the same shape for
/// `/api/generate` (delta in `response`) and `/api/chat` (delta in
/// `message.content`). The final line has `done: true` and carries the
/// token counts; an `error` line can appear anywhere.
#[derive(Debug, Deserialize)]
struct StreamLine {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    message: Option<ChatResponseMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<TaggedModel>,
}

#[derive(Debug, Deserialize)]
struct TaggedModel {
    name: String,
}

impl OllamaExecutor {
    pub fn new(base_url: impl Into<String>, default_model: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client builds with default TLS backend");
        Self {
            http,
            base_url: trim_base(base_url.into()),
            default_model,
            required_models: Vec::new(),
            keep_alive: None,
        }
    }

    /// Pins the models this node advertises, so `health` fails when one
    /// vanishes from the server (`ollama rm` mid-serving). The `"any"`
    /// wildcard means nothing to a model server and is dropped.
    pub fn require_models(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.required_models = models.into_iter().filter(|m| m != "any").collect();
        self
    }

    /// Sets Ollama's `keep_alive` for every request from an operator's
    /// raw value (`"5m"`, `"1h"`, `"-1"` to keep loaded, `"0"` to unload
    /// at once). An unset or blank value leaves Ollama's default.
    pub fn keep_alive(mut self, raw: Option<String>) -> Self {
        self.keep_alive = raw
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(keep_alive_value);
        self
    }
}

/// An operator's `keep_alive` value in the form Ollama's API accepts:
/// a bare integer is seconds (`-1` keeps the model loaded, `0` unloads
/// it immediately), anything else a duration string (`"5m"`, `"1h"`).
/// Maps Ollama's tool calls onto the protocol's OpenAI-shaped
/// [`ToolCall`]: synthesize the `id` Ollama omits (index-based, so the
/// attested output stays deterministic) and the fixed `function` kind,
/// and render the object-valued arguments as the JSON string the
/// protocol and OpenAI carry.
fn map_ollama_tool_calls(calls: Vec<OllamaToolCall>) -> Vec<ToolCall> {
    calls
        .into_iter()
        .enumerate()
        .map(|(index, call)| ToolCall {
            id: format!("call_{index}"),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: call.function.name,
                arguments: match call.function.arguments {
                    Value::Null => "{}".to_string(),
                    Value::String(s) => s,
                    other => other.to_string(),
                },
            },
        })
        .collect()
}

/// Ollama expects a tool call's `arguments` as a JSON object, while the
/// protocol (and OpenAI) carry it as a string. Convert on the way out so
/// a multi-turn conversation that replays prior tool calls reaches the
/// backend in the shape it wants; a call whose arguments are not valid
/// JSON is left as-is rather than dropped.
fn ollama_messages(messages: &[ChatMessage]) -> Value {
    let mut value = serde_json::to_value(messages).unwrap_or_else(|_| Value::Array(Vec::new()));
    if let Some(array) = value.as_array_mut() {
        for message in array.iter_mut() {
            let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
                continue;
            };
            for call in calls {
                let Some(function) = call.get_mut("function") else {
                    continue;
                };
                let Some(arguments) = function.get("arguments").and_then(Value::as_str) else {
                    continue;
                };
                if let Ok(parsed) = serde_json::from_str::<Value>(arguments) {
                    function["arguments"] = parsed;
                }
            }
        }
    }
    value
}

fn keep_alive_value(raw: &str) -> serde_json::Value {
    match raw.parse::<i64>() {
        Ok(secs) => secs.into(),
        Err(_) => raw.into(),
    }
}

fn trim_base(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

/// Buyer knobs → Ollama's `options` object. Only present fields land,
/// so anything the buyer left unset stays the model's own default.
/// `max_tokens` is Ollama's `num_predict`.
fn ollama_options(params: &GenerationParams) -> serde_json::Value {
    let mut options = serde_json::Map::new();
    if let Some(t) = params.temperature {
        options.insert("temperature".into(), t.into());
    }
    if let Some(p) = params.top_p {
        options.insert("top_p".into(), p.into());
    }
    if let Some(n) = params.max_tokens {
        options.insert("num_predict".into(), n.into());
    }
    if let Some(s) = params.seed {
        options.insert("seed".into(), s.into());
    }
    if let Some(pp) = params.presence_penalty {
        options.insert("presence_penalty".into(), pp.into());
    }
    if let Some(fp) = params.frequency_penalty {
        options.insert("frequency_penalty".into(), fp.into());
    }
    if let Some(stop) = &params.stop {
        options.insert("stop".into(), stop.clone().into());
    }
    options.into()
}

/// Ollama takes `logprobs`/`top_logprobs` at the top level of the
/// request, not inside `options`. Set them only when the buyer asked, so
/// an ordinary job's request stays byte-identical to before.
fn apply_logprobs(body: &mut Value, params: &GenerationParams) {
    if let Some(top) = params.logprobs {
        body["logprobs"] = Value::Bool(true);
        body["top_logprobs"] = Value::from(top);
    }
}

/// A buyer's `response_format` → Ollama's top-level `format`: `"json"`
/// for free-form JSON, the schema itself for structured output. Ollama
/// has no separate strict flag; handing it a schema always constrains
/// the decode.
fn ollama_format(params: &GenerationParams) -> Option<serde_json::Value> {
    match &params.response_format {
        Some(ResponseFormat::JsonObject) => Some(Value::String("json".into())),
        Some(ResponseFormat::JsonSchema { schema, .. }) => Some(schema.clone()),
        None => None,
    }
}

/// Model names the server actually has (`GET /api/tags`) — what an
/// honest node declares as `models_served` instead of a hand-typed
/// claim nothing checks.
pub async fn list_models(base_url: &str) -> Result<Vec<String>, ExecutorError> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| ExecutorError::Failed(format!("ollama unreachable at {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(ExecutorError::Failed(format!(
            "ollama {url} returned {}",
            resp.status()
        )));
    }
    let tags: TagsResponse = resp
        .json()
        .await
        .map_err(|e| ExecutorError::Failed(format!("decode /api/tags: {e}")))?;
    Ok(tags.models.into_iter().map(|m| m.name).collect())
}

#[async_trait]
impl JobExecutor for OllamaExecutor {
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
        let live = list_models(&self.base_url).await?;
        crate::executor::require_models_served(&self.required_models, &live)
    }
}

impl OllamaExecutor {
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
                     (it serves several — set COVENANT_COMPUTE_OLLAMA_DEFAULT_MODEL)"
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
        let tools =
            parse_tools_input(&job.input).map_err(|e| ExecutorError::Failed(e.to_string()))?;

        // Tool calls arrive whole in one response, not as a token stream,
        // so a job that offers tools runs the model non-streaming even
        // when the buyer asked to stream. The attested output still
        // carries every call, and the buyer's streaming front door emits
        // them from the verified receipt. Logprobs are the same: Ollama
        // reports them on the final response, so a logprobs job also runs
        // non-streaming and its probabilities ride the verified receipt.
        let logprobs_requested = generation.as_ref().and_then(|g| g.logprobs).is_some();
        let effective_sink = if tools.is_some() || logprobs_requested {
            None
        } else {
            sink
        };

        let started = Instant::now();
        let result = match chat {
            Some(messages) => {
                self.chat(
                    &model,
                    &messages,
                    generation.as_ref(),
                    tools.as_ref(),
                    effective_sink,
                    deadline,
                )
                .await?
            }
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
                match tools.as_ref() {
                    // A raw-prompt job that still offers tools needs the
                    // chat path — `/api/generate` has no tools parameter —
                    // so wrap the prompt as one user turn.
                    Some(_) => {
                        let messages = vec![ChatMessage::user(prompt)];
                        self.chat(
                            &model,
                            &messages,
                            generation.as_ref(),
                            tools.as_ref(),
                            None,
                            deadline,
                        )
                        .await?
                    }
                    None => {
                        self.generate(
                            &model,
                            &prompt,
                            generation.as_ref(),
                            effective_sink,
                            deadline,
                        )
                        .await?
                    }
                }
            }
        };

        crate::executor::ensure_answered(&result.text, &result.tool_calls)?;
        crate::executor::ensure_tool_choice_honored(
            tools.as_ref().and_then(|t| t.tool_choice.as_ref()),
            &result.tool_calls,
        )?;
        crate::executor::ensure_logprobs_delivered(logprobs_requested, result.logprobs.as_deref())?;
        let mut output = assistant_output(result.text, result.tool_calls);
        if let Some(logprobs) = result.logprobs {
            output.push(logprobs_block(logprobs));
        }
        Ok(ExecutionOutcome {
            output,
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in: result.tokens_in,
            tokens_out: result.tokens_out,
            finish_reason: result.finish_reason,
        })
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        generation: Option<&GenerationParams>,
        sink: Option<&ChunkSink>,
        deadline: Duration,
    ) -> Result<GenerationResult, ExecutorError> {
        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": sink.is_some(),
        });
        if let Some(params) = generation {
            body["options"] = ollama_options(params);
            if let Some(format) = ollama_format(params) {
                body["format"] = format;
            }
            apply_logprobs(&mut body, params);
        }
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = keep_alive.clone();
        }
        let resp = self.post("/api/generate", body, deadline).await?;
        let Some(sink) = sink else {
            let raw =
                crate::executor::read_body_capped(resp, MAX_STREAM_OUTPUT_BYTES, deadline).await?;
            let generated: GenerateResponse = serde_json::from_slice(&raw)
                .map_err(|e| ExecutorError::Failed(format!("decode /api/generate: {e}")))?;
            return Ok(GenerationResult {
                text: generated.response,
                tokens_in: generated.prompt_eval_count,
                tokens_out: generated.eval_count,
                finish_reason: generated
                    .done_reason
                    .as_deref()
                    .and_then(FinishReason::from_backend),
                tool_calls: Vec::new(),
                logprobs: generated.logprobs,
            });
        };
        let (text, tokens_in, tokens_out, finish_reason) =
            drain_stream(resp, sink, deadline).await?;
        Ok(GenerationResult {
            text,
            tokens_in,
            tokens_out,
            finish_reason,
            tool_calls: Vec::new(),
            logprobs: None,
        })
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        generation: Option<&GenerationParams>,
        tools: Option<&RequestTools>,
        sink: Option<&ChunkSink>,
        deadline: Duration,
    ) -> Result<GenerationResult, ExecutorError> {
        let mut body = serde_json::json!({
            "model": model,
            "messages": ollama_messages(messages),
            "stream": sink.is_some(),
        });
        if let Some(tools) = tools {
            body["tools"] = serde_json::to_value(&tools.tools)
                .map_err(|e| ExecutorError::Failed(format!("encode tools: {e}")))?;
            if let Some(choice) = &tools.tool_choice {
                body["tool_choice"] = serde_json::to_value(choice)
                    .map_err(|e| ExecutorError::Failed(format!("encode tool_choice: {e}")))?;
            }
        }
        if let Some(params) = generation {
            body["options"] = ollama_options(params);
            if let Some(format) = ollama_format(params) {
                body["format"] = format;
            }
            apply_logprobs(&mut body, params);
        }
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = keep_alive.clone();
        }
        let resp = self.post("/api/chat", body, deadline).await?;
        let Some(sink) = sink else {
            let raw =
                crate::executor::read_body_capped(resp, MAX_STREAM_OUTPUT_BYTES, deadline).await?;
            let chat: ChatResponse = serde_json::from_slice(&raw)
                .map_err(|e| ExecutorError::Failed(format!("decode /api/chat: {e}")))?;
            let tool_calls = map_ollama_tool_calls(chat.message.tool_calls);
            let backend_reason = chat
                .done_reason
                .as_deref()
                .and_then(FinishReason::from_backend);
            let finish_reason = FinishReason::for_tool_turn(backend_reason, !tool_calls.is_empty());
            return Ok(GenerationResult {
                text: chat.message.content,
                tokens_in: chat.prompt_eval_count,
                tokens_out: chat.eval_count,
                finish_reason,
                tool_calls,
                logprobs: chat.logprobs,
            });
        };
        let (text, tokens_in, tokens_out, finish_reason) =
            drain_stream(resp, sink, deadline).await?;
        Ok(GenerationResult {
            text,
            tokens_in,
            tokens_out,
            finish_reason,
            tool_calls: Vec::new(),
            logprobs: None,
        })
    }

    /// Embeds `input`'s text blocks over `/api/embed`: one request
    /// carrying every text, one vector back per text (in order), metered
    /// by the backend's prompt-token count. There is no streaming form —
    /// a vector is produced whole — so an embedding job runs one-shot
    /// regardless of the envelope's `stream` flag. A count mismatch fails
    /// the job rather than pairing vectors to the wrong inputs under an
    /// Ok receipt.
    async fn embed(
        &self,
        model: &str,
        input: &[Content],
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let texts = embedding_texts(input).map_err(|e| ExecutorError::Failed(e.to_string()))?;
        let started = Instant::now();
        let mut body = serde_json::json!({ "model": model, "input": texts });
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = keep_alive.clone();
        }
        let resp = self.post("/api/embed", body, deadline).await?;
        let raw =
            crate::executor::read_body_capped(resp, MAX_STREAM_OUTPUT_BYTES, deadline).await?;
        let parsed: EmbedResponse = serde_json::from_slice(&raw)
            .map_err(|e| ExecutorError::Failed(format!("decode /api/embed: {e}")))?;
        if parsed.embeddings.len() != texts.len() {
            return Err(ExecutorError::Failed(format!(
                "backend returned {} embeddings for {} inputs",
                parsed.embeddings.len(),
                texts.len()
            )));
        }
        crate::executor::ensure_finite_embeddings(&parsed.embeddings)?;
        crate::executor::ensure_uniform_embedding_width(&parsed.embeddings)?;
        Ok(ExecutionOutcome {
            output: vec![embedding_output(model, parsed.embeddings)],
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in: parsed.prompt_eval_count,
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
        let resp = self
            .http
            .post(format!("{}{path}", self.base_url))
            .timeout(deadline)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ExecutorError::Timeout(deadline)
                } else {
                    ExecutorError::Failed(format!("ollama request: {e}"))
                }
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = crate::executor::read_error_snippet(resp).await;
            // Ollama answers errors as {"error": "..."} — surface that
            // message so a "model not found" reads as one in the
            // operator's logs, not as the JSON envelope around it. A body
            // of any other shape falls back to the raw text.
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(body);
            return Err(ExecutorError::Failed(format!(
                "ollama returned {status}: {}",
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

/// Reads an NDJSON stream to its end: relays each text delta into
/// `sink` as it arrives, accumulates the complete text for the receipt
/// path, and takes the token counts off the final `done` line. A delta
/// nobody receives anymore is dropped, never an error — the sink is a
/// preview, and the accumulated text still settles the job.
async fn drain_stream(
    mut resp: reqwest::Response,
    sink: &ChunkSink,
    deadline: Duration,
) -> Result<(String, Option<u64>, Option<u64>, Option<FinishReason>), ExecutorError> {
    let mut full = String::new();
    let mut tokens = (None, None);
    let mut finish_reason = None;
    let mut buf: Vec<u8> = Vec::new();
    let mut saw_done = false;
    loop {
        let piece = match resp.chunk().await {
            Ok(Some(piece)) => piece,
            Ok(None) => break,
            Err(e) if e.is_timeout() => return Err(ExecutorError::Timeout(deadline)),
            Err(e) => return Err(ExecutorError::Failed(format!("ollama stream: {e}"))),
        };
        buf.extend_from_slice(&piece);
        // Bound the unparsed tail too: a backend that never sends a
        // newline would otherwise grow `buf` without limit before the
        // per-line cap in `apply_line` ever gets to look at it.
        if buf.len() > MAX_STREAM_OUTPUT_BYTES {
            return Err(ExecutorError::Failed(format!(
                "output exceeded the {MAX_STREAM_OUTPUT_BYTES}-byte ceiling"
            )));
        }
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            saw_done |= apply_line(&line, sink, &mut full, &mut tokens, &mut finish_reason).await?;
        }
    }
    // NDJSON ends every line with a newline, but a backend that omits
    // the last one must not cost the buyer its final delta.
    saw_done |= apply_line(&buf, sink, &mut full, &mut tokens, &mut finish_reason).await?;
    // Ollama always terminates a stream with a `done: true` line; a
    // clean EOF without one means the generation was cut short, so the
    // assembled text is not a complete result and must not settle under
    // an Ok receipt. (A hard drop mid-stream already surfaces as an
    // error above; this covers a graceful close that omits the marker.)
    if !saw_done {
        return Err(ExecutorError::Failed(
            "ollama stream ended without a final done line; the generation did not complete".into(),
        ));
    }
    Ok((full, tokens.0, tokens.1, finish_reason))
}

async fn apply_line(
    raw: &[u8],
    sink: &ChunkSink,
    full: &mut String,
    tokens: &mut (Option<u64>, Option<u64>),
    finish_reason: &mut Option<FinishReason>,
) -> Result<bool, ExecutorError> {
    let line = String::from_utf8_lossy(raw);
    let line = line.trim();
    if line.is_empty() {
        return Ok(false);
    }
    let parsed: StreamLine = serde_json::from_str(line)
        .map_err(|e| ExecutorError::Failed(format!("decode stream line: {e}")))?;
    if let Some(error) = parsed.error {
        return Err(ExecutorError::Failed(format!(
            "ollama stream error: {error}"
        )));
    }
    let delta = parsed
        .response
        .or_else(|| parsed.message.map(|m| m.content))
        .unwrap_or_default();
    if !delta.is_empty() {
        if full.len() + delta.len() > MAX_STREAM_OUTPUT_BYTES {
            return Err(ExecutorError::Failed(format!(
                "output exceeded the {MAX_STREAM_OUTPUT_BYTES}-byte ceiling"
            )));
        }
        full.push_str(&delta);
        let _ = sink.send(delta).await;
    }
    if parsed.done {
        *tokens = (parsed.prompt_eval_count, parsed.eval_count);
        *finish_reason = parsed
            .done_reason
            .as_deref()
            .and_then(FinishReason::from_backend);
    }
    Ok(parsed.done)
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
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "ollama-test"),
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

    #[tokio::test]
    async fn generates_and_meters_tokens_from_the_backend_report() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "response": "the answer",
                        "prompt_eval_count": 12,
                        "eval_count": 34,
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let outcome = executor
            .execute(
                &job("what is the answer?", Some("qwen-test")),
                Duration::from_secs(2),
            )
            .await
            .expect("generation succeeds");

        assert_eq!(outcome.output, vec![Content::text("the answer")]);
        assert_eq!(outcome.tokens_in, Some(12));
        assert_eq!(outcome.tokens_out, Some(34));

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["model"], "qwen-test");
        assert_eq!(body["prompt"], "what is the answer?");
        assert_eq!(body["stream"], false);
        assert!(
            body.get("options").is_none(),
            "a job without generation params must not constrain the backend"
        );
    }

    #[tokio::test]
    async fn buyer_generation_knobs_land_as_ollama_options() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": { "role": "assistant", "content": "4" },
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
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
            .expect("chat succeeds");

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["options"]["temperature"], 0.0);
        assert_eq!(body["options"]["num_predict"], 64);
        assert_eq!(body["options"]["seed"], 7);
        assert_eq!(body["options"]["presence_penalty"], 0.25);
        assert_eq!(body["options"]["stop"][0], "\n");
        assert!(
            body["options"].get("top_p").is_none()
                && body["options"].get("frequency_penalty").is_none(),
            "an unset knob must stay the backend's default"
        );
    }

    #[tokio::test]
    async fn a_response_format_lands_as_ollama_format() {
        for (rf, expected) in [
            (ResponseFormat::JsonObject, serde_json::json!("json")),
            (
                ResponseFormat::JsonSchema {
                    name: "weather".into(),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                    }),
                    strict: Some(true),
                },
                serde_json::json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                }),
            ),
        ] {
            let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
            let seen_handle = seen.clone();
            let router = Router::new().route(
                "/api/chat",
                post(move |Json(body): Json<serde_json::Value>| {
                    let seen = seen_handle.clone();
                    async move {
                        *seen.lock().await = Some(body);
                        Json(serde_json::json!({
                            "message": { "role": "assistant", "content": "{}" },
                        }))
                    }
                }),
            );
            let base = spawn(router).await;

            let executor = OllamaExecutor::new(base, None);
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
                .expect("chat succeeds");

            let body = seen.lock().await.clone().expect("backend was called");
            assert_eq!(body["format"], expected);
        }
    }

    #[tokio::test]
    async fn a_logprobs_request_fails_when_ollama_reports_none() {
        // A backend build that ignores the logprobs options and answers
        // without any must not settle as a paid completion missing them;
        // the job fails so the coordinator refunds.
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                Json(serde_json::json!({
                    "message": { "role": "assistant", "content": "hi" },
                }))
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("hi")]);
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
    async fn a_logprobs_request_fails_when_ollama_reports_an_empty_set() {
        // A backend can accept the request and answer with an empty
        // logprobs array — every token the buyer paid to score dropped. A
        // bare None check waves this through; an answered completion always
        // scored at least one token, so an empty set is the same dropped
        // feature and must fail so the coordinator refunds.
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                Json(serde_json::json!({
                    "message": { "role": "assistant", "content": "hi" },
                    "logprobs": [],
                }))
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("hi")]);
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
    async fn a_required_tool_choice_fails_when_the_backend_answers_with_prose() {
        // A buyer who set tool_choice=required paid to force a tool call. A
        // backend that ignores it and returns ordinary prose must not settle
        // as a paid completion; the job fails so the coordinator refunds.
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                Json(serde_json::json!({
                    "message": { "role": "assistant", "content": "It is sunny in Paris." },
                    "done": true,
                    "done_reason": "stop",
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let mut tool_job = job("unused", Some("qwen-test"));
        let mut input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("weather in Paris?")]);
        input.push(
            covenant_compute_protocol::tools_input(
                vec![weather_tool()],
                Some(covenant_compute_protocol::ToolChoice::Mode(
                    covenant_compute_protocol::ToolChoiceMode::Required,
                )),
            )
            .expect("valid tools"),
        );
        tool_job.input = input;

        let err = executor
            .execute(&tool_job, Duration::from_secs(2))
            .await
            .expect_err("a required tool call the backend skipped must fail");
        assert!(err.to_string().contains("required"), "got: {err}");
    }

    #[tokio::test]
    async fn a_job_without_a_response_format_leaves_ollama_unconstrained() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": { "role": "assistant", "content": "hi" },
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("hi")]);
        chat_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                seed: Some(1),
                ..Default::default()
            })
            .expect("valid params"),
        );
        executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("chat succeeds");

        let body = seen.lock().await.clone().expect("backend was called");
        assert!(
            body.get("format").is_none(),
            "a job with no response_format must not constrain the decode"
        );
    }

    #[tokio::test]
    async fn a_raw_prompt_job_carries_its_knobs_to_api_generate() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({ "response": "ok" }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut raw_job = job("count to three", Some("qwen-test"));
        raw_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                seed: Some(42),
                ..Default::default()
            })
            .expect("valid params"),
        );
        executor
            .execute(&raw_job, Duration::from_secs(2))
            .await
            .expect("generation succeeds");

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["prompt"], "count to three");
        assert_eq!(body["options"]["seed"], 42);
    }

    #[tokio::test]
    async fn keep_alive_rides_the_request_when_set_and_is_absent_otherwise() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({ "response": "ok" }))
                }
            }),
        );
        let base = spawn(router).await;

        // A duration string rides through verbatim.
        let executor = OllamaExecutor::new(base.clone(), None).keep_alive(Some("5m".into()));
        executor
            .execute(&job("hi", Some("qwen-test")), Duration::from_secs(2))
            .await
            .expect("generation succeeds");
        assert_eq!(
            seen.lock().await.clone().expect("backend called")["keep_alive"],
            "5m"
        );

        // A bare integer is seconds, so "-1" (keep loaded) must be a JSON
        // number, not the string "-1" Ollama would reject.
        let executor = OllamaExecutor::new(base.clone(), None).keep_alive(Some("-1".into()));
        executor
            .execute(&job("hi", Some("qwen-test")), Duration::from_secs(2))
            .await
            .expect("generation succeeds");
        assert_eq!(
            seen.lock().await.clone().expect("backend called")["keep_alive"],
            -1
        );

        // Blank leaves Ollama's own default — no field sent.
        let executor = OllamaExecutor::new(base, None).keep_alive(Some("  ".into()));
        executor
            .execute(&job("hi", Some("qwen-test")), Duration::from_secs(2))
            .await
            .expect("generation succeeds");
        assert!(
            seen.lock().await.clone().expect("backend called")["keep_alive"].is_null(),
            "a blank keep_alive must not be sent"
        );
    }

    #[test]
    fn keep_alive_value_is_seconds_for_an_integer_and_a_string_otherwise() {
        assert_eq!(keep_alive_value("300"), serde_json::json!(300));
        assert_eq!(keep_alive_value("-1"), serde_json::json!(-1));
        assert_eq!(keep_alive_value("0"), serde_json::json!(0));
        assert_eq!(keep_alive_value("5m"), serde_json::json!("5m"));
        assert_eq!(keep_alive_value("1h30m"), serde_json::json!("1h30m"));
    }

    #[tokio::test]
    async fn a_malformed_generation_block_fails_without_reaching_the_backend() {
        // Unroutable base: touching the network would surface as a
        // different error than the parse failure asserted here.
        let executor = OllamaExecutor::new("http://127.0.0.1:1", None);
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
    async fn falls_back_to_the_default_model_when_the_job_names_none() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({ "response": "ok" }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, Some("default-model".into()));
        executor
            .execute(&job("hi", None), Duration::from_secs(2))
            .await
            .expect("generation succeeds");
        let body = seen.lock().await.clone().unwrap();
        assert_eq!(body["model"], "default-model");
    }

    #[tokio::test]
    async fn serves_its_sole_model_when_the_job_names_none_and_no_default() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({ "response": "ok" }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None).require_models(["qwen2.5:0.5b".to_string()]);
        executor
            .execute(&job("hi", None), Duration::from_secs(2))
            .await
            .expect("a single-model node serves the job it advertised");
        let body = seen.lock().await.clone().unwrap();
        assert_eq!(body["model"], "qwen2.5:0.5b");
    }

    #[tokio::test]
    async fn refuses_a_modelless_job_when_several_are_served_and_no_default() {
        let executor = OllamaExecutor::new("http://127.0.0.1:1", None)
            .require_models(["a".to_string(), "b".to_string()]);
        let err = executor
            .execute(&job("hi", None), Duration::from_secs(1))
            .await
            .expect_err("ambiguous model choice");
        assert!(err.to_string().contains("no model_id"), "got: {err}");
    }

    #[tokio::test]
    async fn a_chat_job_routes_to_api_chat_with_the_full_conversation() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": { "role": "assistant", "content": "green" },
                        "prompt_eval_count": 21,
                        "eval_count": 2,
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
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

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["model"], "qwen-test");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "what color is grass?");
    }

    #[tokio::test]
    async fn a_vision_job_carries_its_images_to_api_chat() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": { "role": "assistant", "content": "a red square" },
                        "prompt_eval_count": 700,
                        "eval_count": 4,
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut chat_job = job("unused", Some("moondream"));
        chat_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user_with_images(
                "what is in this image?",
                vec!["aW1hZ2UtYnl0ZXM=".into()],
            )]);
        let outcome = executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("vision chat succeeds");

        assert_eq!(outcome.output, vec![Content::text("a red square")]);

        // The base64 image rides the message untouched: Ollama reads a
        // message-level `images` array, so no executor-side reshaping.
        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["messages"][0]["content"], "what is in this image?");
        assert_eq!(body["messages"][0]["images"][0], "aW1hZ2UtYnl0ZXM=");
    }

    #[tokio::test]
    async fn a_malformed_chat_job_fails_without_reaching_the_backend() {
        // Unroutable base: touching the network would surface as a
        // different error than the parse failure asserted here.
        let executor = OllamaExecutor::new("http://127.0.0.1:1", None);
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
        // Guaranteed-unroutable base: reaching the network would fail
        // differently than the expected pre-flight refusal.
        let executor = OllamaExecutor::new("http://127.0.0.1:1", None);
        let err = executor
            .execute(&job("hi", None), Duration::from_secs(1))
            .await
            .expect_err("no model resolvable");
        assert!(err.to_string().contains("no model_id"), "got: {err}");
    }

    #[tokio::test]
    async fn maps_a_backend_error_status_to_failed() {
        let router = Router::new().route(
            "/api/generate",
            post(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    r#"{"error":"model 'missing' not found"}"#,
                )
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let err = executor
            .execute(&job("hi", Some("missing")), Duration::from_secs(2))
            .await
            .expect_err("backend 404");
        assert!(matches!(err, ExecutorError::Failed(_)));
        let msg = err.to_string();
        assert!(msg.contains("404"), "keeps the status: {msg}");
        assert!(
            msg.contains("model 'missing' not found"),
            "surfaces ollama's message, not the envelope: {msg}"
        );
        assert!(
            !msg.contains(r#"{"error""#),
            "sheds the json envelope: {msg}"
        );
    }

    #[tokio::test]
    async fn a_generation_past_the_deadline_is_a_timeout() {
        let router = Router::new().route(
            "/api/generate",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Json(serde_json::json!({ "response": "too late" }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let err = executor
            .execute(&job("hi", Some("slow")), Duration::from_millis(100))
            .await
            .expect_err("deadline exceeded");
        assert!(matches!(err, ExecutorError::Timeout(_)), "got: {err}");
    }

    #[tokio::test]
    async fn a_streaming_generation_relays_deltas_and_meters_from_the_final_line() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/generate",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    [
                        serde_json::json!({"response": "the ", "done": false}).to_string(),
                        serde_json::json!({"response": "answer", "done": false}).to_string(),
                        serde_json::json!({
                            "response": "",
                            "done": true,
                            "prompt_eval_count": 12,
                            "eval_count": 34,
                        })
                        .to_string(),
                    ]
                    .join("\n")
                        + "\n"
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(
                &job("what is the answer?", Some("qwen-test")),
                Duration::from_secs(2),
                tx,
            )
            .await
            .expect("streaming generation succeeds");

        assert_eq!(outcome.output, vec![Content::text("the answer")]);
        assert_eq!(outcome.tokens_in, Some(12));
        assert_eq!(outcome.tokens_out, Some(34));

        let mut deltas = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }
        assert_eq!(deltas, vec!["the ", "answer"]);

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn a_streaming_chat_relays_message_deltas() {
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                [
                    serde_json::json!({"message": {"role": "assistant", "content": "gr"}, "done": false})
                        .to_string(),
                    serde_json::json!({"message": {"role": "assistant", "content": "een"}, "done": false})
                        .to_string(),
                    serde_json::json!({
                        "message": {"role": "assistant", "content": ""},
                        "done": true,
                        "prompt_eval_count": 21,
                        "eval_count": 2,
                    })
                    .to_string(),
                ]
                .join("\n")
                    + "\n"
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("what color is grass?")]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&chat_job, Duration::from_secs(2), tx)
            .await
            .expect("streaming chat succeeds");

        assert_eq!(outcome.output, vec![Content::text("green")]);
        assert_eq!(outcome.tokens_in, Some(21));
        assert_eq!(outcome.tokens_out, Some(2));

        let mut deltas = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }
        assert_eq!(deltas, vec!["gr", "een"]);
    }

    #[tokio::test]
    async fn a_stream_that_never_says_done_fails_rather_than_settling_partial() {
        // Ollama always ends a stream with `done: true`; a clean close
        // without it is a cut-short generation, which must not settle as
        // a complete Ok result the buyer pays for.
        let router = Router::new().route(
            "/api/generate",
            post(|| async {
                [
                    serde_json::json!({"response": "half ", "done": false}).to_string(),
                    serde_json::json!({"response": "an answer", "done": false}).to_string(),
                ]
                .join("\n")
                    + "\n"
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = executor
            .execute_streaming(&job("hi", Some("qwen-test")), Duration::from_secs(2), tx)
            .await
            .expect_err("a stream with no done line is an incomplete generation");
        assert!(err.to_string().contains("did not complete"), "got: {err}");
    }

    #[tokio::test]
    async fn a_mid_stream_error_line_fails_the_job() {
        let router = Router::new().route(
            "/api/generate",
            post(|| async {
                [
                    serde_json::json!({"response": "half", "done": false}).to_string(),
                    serde_json::json!({"error": "model crashed"}).to_string(),
                ]
                .join("\n")
                    + "\n"
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = executor
            .execute_streaming(&job("hi", Some("crashy")), Duration::from_secs(2), tx)
            .await
            .expect_err("mid-stream error");
        assert!(err.to_string().contains("model crashed"), "got: {err}");
    }

    #[tokio::test]
    async fn a_dropped_chunk_receiver_does_not_fail_the_stream() {
        let router = Router::new().route(
            "/api/generate",
            post(|| async {
                [
                    serde_json::json!({"response": "still ", "done": false}).to_string(),
                    serde_json::json!({"response": "served", "done": true}).to_string(),
                ]
                .join("\n")
                    + "\n"
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let outcome = executor
            .execute_streaming(&job("hi", Some("qwen-test")), Duration::from_secs(2), tx)
            .await
            .expect("nobody listening is not a job failure");
        assert_eq!(outcome.output, vec![Content::text("still served")]);
    }

    #[tokio::test]
    async fn streamed_output_past_the_ceiling_fails_the_job() {
        let big = "a".repeat(3 * 1024 * 1024);
        let big_line = serde_json::json!({ "response": big, "done": false }).to_string();
        let router = Router::new().route(
            "/api/generate",
            post(move || async move { [big_line.clone(), big_line].join("\n") + "\n" }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let err = executor
            .execute_streaming(&job("hi", Some("qwen-test")), Duration::from_secs(10), tx)
            .await
            .expect_err("oversized output must fail, not truncate under an Ok receipt");
        assert!(err.to_string().contains("ceiling"), "got: {err}");
    }

    #[tokio::test]
    async fn a_non_streamed_body_past_the_ceiling_fails_the_job() {
        // The default (non-streaming) path used to decode with an
        // unbounded `resp.json()`; an oversized single-object body must
        // fail, not ride into an Ok receipt.
        let big = "a".repeat(5 * 1024 * 1024);
        let body = serde_json::json!({ "response": big, "done": true }).to_string();
        let router =
            Router::new().route("/api/generate", post(move || async move { body.clone() }));
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let err = executor
            .execute(&job("hi", Some("qwen-test")), Duration::from_secs(10))
            .await
            .expect_err(
                "an oversized non-streamed body must fail, not be paid under an Ok receipt",
            );
        assert!(err.to_string().contains("ceiling"), "got: {err}");
    }

    #[tokio::test]
    async fn a_stream_line_that_never_ends_trips_the_ceiling() {
        // A backend that streams megabytes without a single newline must
        // trip the ceiling, not buffer the whole unparsed line first.
        let big = "a".repeat(5 * 1024 * 1024);
        let router = Router::new().route("/api/generate", post(move || async move { big.clone() }));
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        drop(rx);
        let err = executor
            .execute_streaming(&job("hi", Some("qwen-test")), Duration::from_secs(10), tx)
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
                b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: application/x-ndjson\r\n\r\n",
            )
            .await
            .unwrap();
            sock.write_all(
                format!(
                    "{}\n",
                    serde_json::json!({"response": "partial", "done": false})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            sock.flush().await.unwrap();
            // Never send the rest, never close: the reader must give up
            // on its own deadline, not ours.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let executor = OllamaExecutor::new(format!("http://{addr}"), None);
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
    async fn list_models_returns_the_backend_tags() {
        let router = Router::new().route(
            "/api/tags",
            get(|| async {
                Json(serde_json::json!({
                    "models": [
                        { "name": "qwen2.5:7b", "size": 1 },
                        { "name": "llama3:8b", "size": 2 },
                    ]
                }))
            }),
        );
        let base = spawn(router).await;
        let models = list_models(&base).await.expect("tags list");
        assert_eq!(models, vec!["qwen2.5:7b", "llama3:8b"]);
    }

    #[tokio::test]
    async fn health_passes_while_the_advertised_models_are_live() {
        let router = Router::new().route(
            "/api/tags",
            get(|| async {
                Json(serde_json::json!({
                    "models": [
                        { "name": "qwen2.5:7b" },
                        { "name": "llama3:latest" },
                    ]
                }))
            }),
        );
        let base = spawn(router).await;
        // Tag folding matches the matcher's routing rule, and the
        // `"any"` wildcard is dropped rather than demanded of a model
        // server.
        let executor = OllamaExecutor::new(base, None).require_models(vec![
            "qwen2.5:7b".into(),
            "llama3".into(),
            "any".into(),
        ]);
        executor.health().await.expect("all advertised models live");
    }

    #[tokio::test]
    async fn health_names_the_advertised_model_the_server_lost() {
        let router = Router::new().route(
            "/api/tags",
            get(|| async { Json(serde_json::json!({ "models": [{ "name": "llama3:8b" }] })) }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None).require_models(vec!["qwen2.5:7b".into()]);
        let err = executor.health().await.expect_err("model was removed");
        assert!(err.to_string().contains("qwen2.5:7b"), "got: {err}");
    }

    #[tokio::test]
    async fn health_fails_when_the_server_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let executor = OllamaExecutor::new(format!("http://{addr}"), None);
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
    async fn embeds_texts_and_meters_from_the_prompt_eval_count() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/embed",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "model": "nomic-embed-text",
                        "embeddings": [[0.1, 0.2, 0.3], [0.4, 0.5, 0.6]],
                        "prompt_eval_count": 8,
                    }))
                }
            }),
        );
        let base = spawn(router).await;

        let executor = OllamaExecutor::new(base, None);
        let outcome = executor
            .execute(
                &embed_job(&["hello", "world"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect("embedding succeeds");

        let result = covenant_compute_protocol::parse_embedding_output(&outcome.output)
            .expect("output is embedding-shaped");
        assert_eq!(result.model, "nomic-embed-text");
        assert_eq!(result.dimensions, 3);
        assert_eq!(
            result.embeddings,
            vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]]
        );
        // Metered on input tokens only; an embedding generates none.
        assert_eq!(outcome.tokens_in, Some(8));
        assert_eq!(outcome.tokens_out, None);

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["model"], "nomic-embed-text");
        assert_eq!(body["input"], serde_json::json!(["hello", "world"]));
    }

    #[tokio::test]
    async fn a_non_finite_embedding_fails_the_job_rather_than_paying_for_it() {
        // 1e40 is a finite JSON number that overflows f32; the node parses
        // embeddings at full precision, so it refuses the component at decode
        // rather than signing an Ok receipt over an embedding the buyer's
        // parser rejects — a paid-for failure either way.
        let router = Router::new().route(
            "/api/embed",
            post(|| async {
                Json(serde_json::json!({
                    "model": "nomic-embed-text",
                    "embeddings": [[1e40, 0.2, 0.3]],
                    "prompt_eval_count": 4,
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let err = executor
            .execute(
                &embed_job(&["hello"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("an out-of-range embedding must not settle Ok");
        assert!(err.to_string().contains("out of range"), "got: {err}");
    }

    #[tokio::test]
    async fn a_ragged_embedding_batch_fails_the_job_rather_than_paying_for_it() {
        // Two inputs, two vectors of differing widths: the count check passes
        // but the signed `dimensions` would be the first vector's width, so
        // the node must refuse rather than sign an Ok receipt over a batch
        // the buyer's own parser rejects.
        let router = Router::new().route(
            "/api/embed",
            post(|| async {
                Json(serde_json::json!({
                    "model": "nomic-embed-text",
                    "embeddings": [[0.1, 0.2, 0.3], [0.4, 0.5]],
                    "prompt_eval_count": 4,
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
        let err = executor
            .execute(
                &embed_job(&["hello", "world"], Some("nomic-embed-text")),
                Duration::from_secs(2),
            )
            .await
            .expect_err("a ragged embedding batch must not settle Ok");
        assert!(err.to_string().contains("width"), "got: {err}");
    }

    #[tokio::test]
    async fn the_boot_benchmark_proves_an_embedding_claim_through_the_real_executor() {
        // An embedding node's boot self-test end to end: run_benchmark
        // drives an embedding probe through the real OllamaExecutor against
        // a stub /api/embed, and the returned vector proves the claim.
        // This is the path that lets an embedding node register at all —
        // a chat probe would have failed the model here.
        let router = Router::new().route(
            "/api/embed",
            post(|| async {
                Json(serde_json::json!({
                    "model": "nomic-embed-text",
                    "embeddings": [[0.1, 0.2, 0.3]],
                    "prompt_eval_count": 4,
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let spec = crate::benchmark::BenchmarkSpec {
            input: vec![Content::text("compute embedding benchmark")],
            expect_contains: None,
            per_model: true,
            kind: JobKind::Embedding,
        };
        let operator = AgentId::new("operator@embed-bench", [3u8; 32]);
        let probes = crate::benchmark::run_benchmark(
            &executor,
            &operator,
            &["nomic-embed-text".to_string()],
            &spec,
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(probes.len(), 1, "one probe per declared model");
        assert!(
            probes[0].passed(),
            "the embedding vector proves the claim: {:?}",
            probes[0].result
        );
    }

    #[tokio::test]
    async fn a_count_mismatch_fails_the_job_rather_than_mispairing() {
        // Two inputs, one vector back: pairing them would bind a buyer's
        // text to the wrong vector under an Ok receipt.
        let router = Router::new().route(
            "/api/embed",
            post(|| async { Json(serde_json::json!({ "embeddings": [[0.1, 0.2]] })) }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);
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
    async fn an_embedding_job_with_no_text_fails_before_any_http() {
        // Unroutable base: an embedding job carrying nothing to embed
        // must fail on its own input, never on the network.
        let executor = OllamaExecutor::new("http://127.0.0.1:1", None);
        let mut j = embed_job(&["ignored"], Some("nomic-embed-text"));
        j.input = vec![Content::json(serde_json::json!({ "meta": 1 }))];
        let err = executor
            .execute(&j, Duration::from_secs(1))
            .await
            .expect_err("nothing to embed");
        assert!(err.to_string().contains("no text"), "got: {err}");
    }

    fn weather_tool() -> covenant_compute_protocol::ToolDefinition {
        covenant_compute_protocol::ToolDefinition {
            kind: covenant_compute_protocol::ToolKind::Function,
            function: covenant_compute_protocol::FunctionDefinition {
                name: "get_weather".into(),
                description: Some("look up the weather in a city".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                })),
            },
        }
    }

    fn tool_call_job() -> JobEnvelopePayload {
        let mut job = job("unused", Some("qwen-test"));
        let mut input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user("weather in Paris?")]);
        input.push(
            covenant_compute_protocol::tools_input(
                vec![weather_tool()],
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
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": {
                            "role": "assistant",
                            "content": "",
                            "tool_calls": [
                                {"function": {"name": "get_weather", "arguments": {"city": "Paris"}}}
                            ]
                        },
                        "done": true,
                        "done_reason": "stop",
                        "prompt_eval_count": 20,
                        "eval_count": 5,
                    }))
                }
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let outcome = executor
            .execute(&tool_call_job(), Duration::from_secs(2))
            .await
            .expect("tool-call generation succeeds");

        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(reply.text, "");
        assert_eq!(reply.tool_calls.len(), 1);
        let call = &reply.tool_calls[0];
        assert_eq!(call.id, "call_0");
        assert_eq!(call.function.name, "get_weather");
        assert_eq!(call.function.arguments, r#"{"city":"Paris"}"#);
        assert_eq!(outcome.finish_reason, Some(FinishReason::ToolCalls));

        let body = seen.lock().await.clone().expect("backend was called");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(
            body["stream"], false,
            "a tools job never streams the backend"
        );
    }

    #[tokio::test]
    async fn a_tools_job_runs_non_streaming_and_relays_no_deltas() {
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                Json(serde_json::json!({
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [
                            {"function": {"name": "get_weather", "arguments": {"city": "Paris"}}}
                        ]
                    },
                    "done": true,
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = executor
            .execute_streaming(&tool_call_job(), Duration::from_secs(2), tx)
            .await
            .expect("a streamed tools job still completes non-streaming");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert_eq!(
            reply.tool_calls.len(),
            1,
            "the call survives a streamed request"
        );
        assert!(rx.recv().await.is_none(), "nothing was relayed live");
    }

    #[tokio::test]
    async fn outgoing_tool_call_arguments_are_objects_for_ollama() {
        let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let seen_handle = seen.clone();
        let router = Router::new().route(
            "/api/chat",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_handle.clone();
                async move {
                    *seen.lock().await = Some(body);
                    Json(serde_json::json!({
                        "message": {"role": "assistant", "content": "18C and clear"},
                        "done": true,
                    }))
                }
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let history = vec![
            ChatMessage::user("weather in Paris?"),
            ChatMessage {
                role: covenant_compute_protocol::ChatRole::Assistant,
                content: String::new(),
                images: Vec::new(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    kind: ToolCallKind::Function,
                    function: FunctionCall {
                        name: "get_weather".into(),
                        arguments: r#"{"city":"Paris"}"#.into(),
                    },
                }],
                tool_call_id: None,
            },
            ChatMessage::tool("call_0", "18C and clear"),
        ];
        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(history);
        executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect("a multi-turn conversation runs");

        let body = seen.lock().await.clone().expect("backend was called");
        let args = &body["messages"][1]["tool_calls"][0]["function"]["arguments"];
        assert!(
            args.is_object(),
            "arguments reach Ollama as an object: {args}"
        );
        assert_eq!(args["city"], "Paris");
    }

    #[tokio::test]
    async fn an_empty_completion_is_refused_not_settled_as_paid_output() {
        // A chat turn with empty content and no tool calls is a
        // non-answer; the job must fail so it refunds, never settle an
        // `Ok` receipt over empty output the buyer paid for — the same
        // rule the OpenAI-compat backend enforces.
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                Json(serde_json::json!({
                    "message": { "role": "assistant", "content": "" },
                    "done": true,
                    "done_reason": "stop",
                }))
            }),
        );
        let base = spawn(router).await;
        let executor = OllamaExecutor::new(base, None);

        let mut chat_job = job("unused", Some("qwen-test"));
        chat_job.input = covenant_compute_protocol::chat_input(vec![ChatMessage::user("hi")]);
        let err = executor
            .execute(&chat_job, Duration::from_secs(2))
            .await
            .expect_err("an empty completion must not settle as paid output");
        assert!(matches!(err, ExecutorError::Failed(_)), "refused: {err:?}");
    }

    /// A 96x96 solid-red PNG, base64-encoded — the fixture the live vision
    /// test hands the model. Kept inline so the test needs no file on disk.
    const RED_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAGAAAABgCAIAAABt+uBvAAAApElEQVR4nO3QMQ0AMAzAsIEof2QDMwZ7m8NSAEQ+d0afzvpBPECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQIEDhAAECBAhQOECAAAECFA4QIECAAIUDBAgQoM0eq0WSHWx5IugAAAAASUVORK5CYII=";

    #[tokio::test]
    #[ignore = "requires a local Ollama serving the vision model moondream"]
    async fn live_vision_against_a_real_backend() {
        let executor = OllamaExecutor::new("http://127.0.0.1:11434", Some("moondream".into()));
        let mut vision_job = job("unused", Some("moondream"));
        vision_job.input =
            covenant_compute_protocol::chat_input(vec![ChatMessage::user_with_images(
                "Describe this image, naming its dominant color.",
                vec![RED_PNG_B64.into()],
            )]);

        let outcome = executor
            .execute(&vision_job, Duration::from_secs(120))
            .await
            .expect("the real backend serves a vision job");

        let text = match &outcome.output[0] {
            Content::Text { text } => text.to_lowercase(),
            other => panic!("expected text output, got {other:?}"),
        };
        assert!(text.contains("red"), "model should see red; said: {text:?}");
        // Real image tokens metered — a solid color still costs the vision
        // encoder hundreds of prompt tokens, which a text prompt never would.
        assert!(
            outcome.tokens_in.unwrap_or(0) > 100,
            "the image was encoded and metered; tokens_in was {:?}",
            outcome.tokens_in
        );
    }

    #[tokio::test]
    #[ignore = "requires a local Ollama serving the tool-capable qwen2.5:0.5b"]
    async fn live_tool_call_against_a_real_backend() {
        let executor = OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some("qwen2.5:0.5b".into()));
        let mut tool_job = job("unused", Some("qwen2.5:0.5b"));
        let mut input = covenant_compute_protocol::chat_input(vec![ChatMessage::user(
            "What is the weather in Paris? Call the get_weather tool to find out.",
        )]);
        input.push(
            covenant_compute_protocol::tools_input(
                vec![weather_tool()],
                Some(covenant_compute_protocol::ToolChoice::Mode(
                    covenant_compute_protocol::ToolChoiceMode::Required,
                )),
            )
            .expect("valid tools"),
        );
        tool_job.input = input;

        let outcome = executor
            .execute(&tool_job, Duration::from_secs(60))
            .await
            .expect("the real backend serves a tool call");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert!(
            !reply.tool_calls.is_empty(),
            "the model asked for a tool; output was {:?}",
            outcome.output
        );
        assert_eq!(reply.tool_calls[0].function.name, "get_weather");
        assert_eq!(outcome.finish_reason, Some(FinishReason::ToolCalls));
    }

    #[tokio::test]
    #[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
    async fn live_json_mode_against_a_real_backend() {
        let executor = OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some("qwen2.5:0.5b".into()));
        let mut json_job = job("unused", Some("qwen2.5:0.5b"));
        let mut input = covenant_compute_protocol::chat_input(vec![ChatMessage::user(
            "Give me the capital of France as a JSON object with a 'city' field.",
        )]);
        input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                response_format: Some(ResponseFormat::JsonObject),
                ..Default::default()
            })
            .expect("valid params"),
        );
        json_job.input = input;

        let outcome = executor
            .execute(&json_job, Duration::from_secs(60))
            .await
            .expect("the real backend serves a constrained completion");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        let text = reply.text.trim();
        let parsed: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("json mode must return valid JSON, got {text:?}: {e}"));
        assert!(parsed.is_object(), "expected a JSON object, got {parsed}");
    }

    #[tokio::test]
    #[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
    async fn live_sampling_penalties_against_a_real_backend() {
        // Proves the real backend accepts the penalty options rather than
        // rejecting the request or 400ing on an unknown key — the failure
        // mode a mocked test can't catch.
        let executor = OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some("qwen2.5:0.5b".into()));
        let mut penalized = job(
            "Write one short sentence about the sea.",
            Some("qwen2.5:0.5b"),
        );
        penalized.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                presence_penalty: Some(1.5),
                frequency_penalty: Some(1.5),
                max_tokens: Some(64),
                ..Default::default()
            })
            .expect("valid params"),
        );

        let outcome = executor
            .execute(&penalized, Duration::from_secs(60))
            .await
            .expect("the real backend honors the penalty options rather than rejecting them");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        assert!(
            !reply.text.trim().is_empty(),
            "a penalized completion still returns text, got {:?}",
            reply.text
        );
    }

    #[tokio::test]
    #[ignore = "requires a local Ollama serving qwen2.5:0.5b"]
    async fn live_logprobs_against_a_real_backend() {
        // Proves the real backend reports per-token log probabilities and
        // that they ride the attested output in the protocol's shape — the
        // point of the feature, unverifiable against a mock.
        let executor = OllamaExecutor::new(DEFAULT_OLLAMA_URL, Some("qwen2.5:0.5b".into()));
        let mut lp_job = job("Say hello in one short word.", Some("qwen2.5:0.5b"));
        lp_job.input.push(
            covenant_compute_protocol::generation_input(GenerationParams {
                logprobs: Some(2),
                max_tokens: Some(16),
                ..Default::default()
            })
            .expect("valid params"),
        );

        let outcome = executor
            .execute(&lp_job, Duration::from_secs(60))
            .await
            .expect("the real backend reports logprobs");
        let reply = covenant_compute_protocol::parse_assistant_output(&outcome.output);
        let logprobs = reply
            .logprobs
            .expect("logprobs ride the attested output when requested");
        assert!(!logprobs.is_empty(), "at least one token was scored");
        let first = &logprobs[0];
        assert!(!first.token.is_empty(), "each entry names its token");
        assert!(
            first.logprob.is_finite(),
            "a real log probability is finite, got {}",
            first.logprob
        );
        assert!(
            !first.top_logprobs.is_empty(),
            "the requested alternatives ride along"
        );
    }
}
