//! An OpenAI-compatible front door for the compute network. A client
//! built for OpenAI's `POST /v1/chat/completions` — the OpenAI SDKs,
//! LangChain, a bare `curl` — points its base URL here, and every chat
//! completion is bought on the compute network, paid, and returned with
//! the operator's signed, locally re-verified work receipt. No client
//! code changes; the buyer's spend stays under the same per-call and
//! session caps every other buyer surface enforces.
//!
//! The response is a standard `chat.completion` object. It carries one
//! extra `covenant` field with the verified receipt — a plain OpenAI
//! client ignores it, a Covenant-aware one checks the job on-chain.
//!
//! This module holds the router and handlers so they can be tested
//! in-process against a real coordinator; the `covenant-compute-openai`
//! binary is a thin `main` that reads the environment and serves it.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use covenant_compute_protocol::{
    canonical_model, parse_assistant_output, parse_embedding_output_expecting, parse_speech_output,
    parse_transcription_output, speech_input, transcription_input, CapacityView, ChatMessage,
    ChatRole, JobKind, JobMeter, ResponseFormat, SpeechInput, TokenLogprob, ToolCall, ToolChoice,
    ToolDefinition, TranscriptionInput, TranscriptionResult, TranscriptionSegment,
    MAX_AUDIO_B64_BYTES, MAX_SPEECH_SPEED, MAX_SPEECH_TEXT_CHARS, MIN_SPEECH_SPEED,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::{
    capacity, cheapest_matching_ask, dispatch_and_verify, stream_and_verify, submit_streaming,
    BuyerConfig, BuyerError, DispatchOutcome, InferArgs, JobRequest, SpendCaps, SpendReservation,
};

/// Everything a served request needs, shared behind an `Arc` across
/// concurrent handlers. `caps` holds the running session spend, so it is
/// interior-mutable and shared, never cloned per request.
pub struct OpenAiState {
    pub http: reqwest::Client,
    pub buyer: BuyerConfig,
    pub identity: LocalIdentity,
    pub caps: Arc<SpendCaps>,
    pub default_deadline_ms: u64,
    /// When set, every `/v1/*` route requires
    /// `Authorization: Bearer <api_key>`. `None` leaves the endpoint
    /// open, for a loopback-only deployment.
    pub api_key: Option<String>,
}

/// The largest multipart body the audio endpoints accept, above
/// axum's 2 MiB default so an ordinary voice clip is not rejected before
/// the handler can weigh it. It leaves headroom over the protocol's
/// base64 audio cap ([`MAX_AUDIO_B64_BYTES`]) for the other form fields
/// and the base64 expansion; a clip past the protocol cap is refused with
/// a clear message rather than a bare 413.
const MAX_AUDIO_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// The OpenAI-compatible router: `/v1/responses`, `/v1/chat/completions`,
/// the legacy `/v1/completions`, `/v1/embeddings`,
/// `/v1/audio/transcriptions`, `/v1/audio/translations`, `/v1/models`, and
/// an unauthenticated `/health`.
pub fn openai_router(state: Arc<OpenAiState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/models/:model", get(retrieve_model))
        .route("/v1/responses", post(crate::responses::create_response))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route(
            "/v1/audio/transcriptions",
            post(audio_transcriptions).layer(DefaultBodyLimit::max(MAX_AUDIO_UPLOAD_BYTES)),
        )
        .route(
            "/v1/audio/translations",
            post(audio_translations).layer(DefaultBodyLimit::max(MAX_AUDIO_UPLOAD_BYTES)),
        )
        .route("/v1/audio/speech", post(audio_speech))
        .with_state(state)
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    messages: Vec<OpenAiChatMessage>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    max_tokens: Option<u32>,
    /// OpenAI's newer name for `max_tokens`; either is accepted.
    #[serde(default)]
    max_completion_tokens: Option<u32>,
    #[serde(default)]
    seed: Option<i64>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    stop: Option<StopField>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
    #[serde(default)]
    n: Option<u32>,
    /// Whether to return per-token log probabilities.
    #[serde(default)]
    logprobs: bool,
    /// How many most-likely alternatives to report per token (`0..=20`).
    /// Requires `logprobs: true`; OpenAI's own coupling.
    #[serde(default)]
    top_logprobs: Option<u32>,
    /// The functions the model may call. OpenAI's `tools` shape is the
    /// protocol's [`ToolDefinition`] verbatim, so it deserializes straight
    /// through.
    #[serde(default)]
    tools: Vec<ToolDefinition>,
    #[serde(default)]
    tool_choice: Option<ToolChoice>,
    /// The deprecated function-calling API (superseded by `tools` /
    /// `tool_choice` in 2023). Its shape differs, so it is not carried; a
    /// client still on it is modeled to earn a refusal rather than a plain
    /// completion that silently ignored the function contract they paid to
    /// force.
    #[serde(default)]
    functions: Option<Value>,
    #[serde(default)]
    function_call: Option<Value>,
    /// Structured-output constraint. OpenAI's `text` / `json_object` /
    /// `json_schema` shape, narrowed to the protocol's [`ResponseFormat`].
    #[serde(default)]
    response_format: Option<OpenAiResponseFormat>,
    /// Per-token logit biases. This chat-backed network forwards no
    /// bias map to the backend, so a non-empty one is modeled to earn a
    /// refusal rather than a silently unbiased completion the buyer pays
    /// for; an empty map is the no-op OpenAI treats it as and passes.
    #[serde(default)]
    logit_bias: Option<Map<String, Value>>,
    /// Whether the model may emit several tool calls in one turn. The
    /// OpenAI default (`true`) constrains nothing and passes; `false` is
    /// a one-call-per-turn guarantee this network can't impose on the
    /// backend, so it earns a refusal when tools are in play rather than
    /// a silently parallel reply.
    #[serde(default)]
    parallel_tool_calls: Option<bool>,
    /// How hard a reasoning model should think before answering. The
    /// network's wire carries no reasoning-effort control and its serving
    /// path returns a final answer only, so any value is modeled to earn
    /// a refusal rather than a completion at the backend's default the
    /// buyer pays for as if the effort had been honored — the same stance
    /// the Responses door takes on `reasoning` and the Anthropic door on
    /// `thinking`.
    #[serde(default)]
    reasoning_effort: Option<String>,
    /// The output modalities the client wants back. This chat path serves
    /// text; audio output is a separate speech endpoint it never routes to, so
    /// a modality other than `text` earns a refusal rather than a text-only
    /// reply the buyer pays for after asking for audio. `["text"]` (or an
    /// omitted field) is the default and passes.
    #[serde(default)]
    modalities: Option<Vec<String>>,
    /// Built-in web-search grounding for search-enabled models. This network
    /// serves local models with no web access, so a request to ground the
    /// answer in a live search is modeled to earn a refusal rather than an
    /// answer drawn from the model's own weights and billed as if it had
    /// searched.
    #[serde(default)]
    web_search_options: Option<Value>,
    /// How verbose a GPT-5-class model's answer should be (`low`/`medium`/
    /// `high`). It shapes the output length, and this network's ordinary
    /// models expose no such control, so any value earns a refusal rather
    /// than a default-length completion the buyer pays for as if the
    /// verbosity had been honored — the same stance taken on `reasoning_effort`.
    #[serde(default)]
    verbosity: Option<String>,
}

/// One message as an OpenAI client sends it, before it is narrowed to the
/// protocol's canonical [`ChatMessage`]. `role` is a free string (the
/// protocol only models three; `developer` folds to system, the rest earn
/// a clear refusal) and `content` may be a plain string or the array of
/// typed parts the current SDKs emit — text parts join into the prompt,
/// image parts become the message's inline images.
#[derive(Debug, Clone, Deserialize)]
struct OpenAiChatMessage {
    role: String,
    #[serde(default)]
    content: OpenAiContent,
    /// An assistant turn's tool calls, replayed from history. OpenAI's
    /// shape matches the protocol's [`ToolCall`], so it passes straight
    /// through to the model.
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
    /// Which call a `tool` message answers.
    #[serde(default)]
    tool_call_id: Option<String>,
}

/// OpenAI message content: a plain string, or an array of typed parts
/// (`{"type":"text","text":"…"}` and `{"type":"image_url",…}`). Absent
/// content reads as empty, matching how the SDKs treat a tool/assistant
/// message with none.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(untagged)]
enum OpenAiContent {
    #[default]
    None,
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Deserialize)]
struct ContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<ImageUrlPart>,
}

/// The `image_url` part of an OpenAI content array. Only an inline base64
/// data URI is served; OpenAI's optional `detail` hint is accepted and
/// ignored, since the served backends expose no such control.
#[derive(Debug, Clone, Deserialize)]
struct ImageUrlPart {
    url: String,
}

impl OpenAiContent {
    /// Flatten to the text and base64 images the network embeds in the
    /// signed envelope. Text parts concatenate in order; each `image_url`
    /// part contributes its inline base64. Any other part type is refused
    /// rather than silently dropped.
    fn into_content(self) -> Result<(String, Vec<String>), String> {
        match self {
            OpenAiContent::None => Ok((String::new(), Vec::new())),
            OpenAiContent::Text(s) => Ok((s, Vec::new())),
            OpenAiContent::Parts(parts) => {
                let mut text = String::new();
                let mut images = Vec::new();
                for part in parts {
                    match part.kind.as_str() {
                        "text" => text.push_str(part.text.as_deref().unwrap_or_default()),
                        "image_url" => {
                            let url = part
                                .image_url
                                .map(|i| i.url)
                                .ok_or("an image_url content part carries no url")?;
                            images.push(image_data_uri_bytes(&url)?);
                        }
                        other => {
                            return Err(format!(
                                "message content part of type '{other}' is not supported: \
                                 this endpoint serves text and image parts"
                            ))
                        }
                    }
                }
                Ok((text, images))
            }
        }
    }
}

/// Pull the raw base64 out of an inline `data:` image URI, the only image
/// form the front door serves (shared with the Responses endpoint). A remote
/// `http(s)` URL is refused: the network never reaches out to fetch a buyer's
/// image, it relays inline bytes only. A data URI that isn't base64, or whose
/// base64 doesn't decode, is rejected here rather than paid for and failed at
/// a backend.
pub(crate) fn image_data_uri_bytes(url: &str) -> Result<String, String> {
    let rest = url.strip_prefix("data:").ok_or_else(|| {
        if url.starts_with("http://") || url.starts_with("https://") {
            "a remote image url is not supported: inline the image as a base64 data uri".to_string()
        } else {
            "unsupported image url: inline the image as a base64 data uri".to_string()
        }
    })?;
    let encoded = rest
        .split_once(";base64,")
        .map(|(_, data)| data)
        .ok_or("an image data uri must be base64: data:<mime>;base64,<data>")?;
    let encoded: String = encoded.split_whitespace().collect();
    if encoded.is_empty() {
        return Err("an image data uri carries no image bytes".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .map_err(|_| "an image data uri is not valid base64".to_string())?;
    Ok(encoded)
}

impl OpenAiChatMessage {
    /// Narrow to the protocol message, mapping OpenAI's wider role set. The
    /// `developer` role is OpenAI's newer name for a system instruction, so
    /// it folds to system; `tool` carries a tool-call result and maps to
    /// the protocol's tool role; the legacy `function` role is refused (use
    /// `tool` and `tool_call_id`).
    fn into_protocol(self) -> Result<ChatMessage, String> {
        let role = match self.role.as_str() {
            "system" | "developer" => ChatRole::System,
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            "tool" => ChatRole::Tool,
            other => {
                return Err(format!(
                    "message role '{other}' is not supported: use system, user, assistant, or tool"
                ))
            }
        };
        let (content, images) = self.content.into_content()?;
        Ok(ChatMessage {
            role,
            content,
            images,
            tool_calls: self.tool_calls,
            tool_call_id: self.tool_call_id,
        })
    }
}

/// OpenAI's `stop` is either a single string or a list of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum StopField {
    One(String),
    Many(Vec<String>),
}

impl StopField {
    fn into_vec(self) -> Vec<String> {
        match self {
            StopField::One(s) => vec![s],
            StopField::Many(v) => v,
        }
    }
}

/// OpenAI's `response_format` object, before it is narrowed to the
/// protocol's [`ResponseFormat`]. `text` is the default and carries no
/// constraint, so it maps to `None`; `json_object` and `json_schema` carry
/// across. The protocol type stays canonical; this shape holds only the
/// OpenAI wire quirks (the `text` default, the `json_schema` nesting).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: JsonSchemaField },
}

#[derive(Debug, Clone, Deserialize)]
struct JsonSchemaField {
    #[serde(default)]
    name: String,
    #[serde(default)]
    schema: Option<serde_json::Value>,
    #[serde(default)]
    strict: Option<bool>,
}

impl OpenAiResponseFormat {
    /// Narrow to the protocol constraint. `text` becomes `None`; a
    /// `json_schema` with no `schema` is refused here (the protocol
    /// validates the rest — name, object-ness, size — when the block is
    /// packed). `Ok(None)` means the reply is left free-form.
    fn into_protocol(self) -> Result<Option<ResponseFormat>, String> {
        match self {
            OpenAiResponseFormat::Text => Ok(None),
            OpenAiResponseFormat::JsonObject => Ok(Some(ResponseFormat::JsonObject)),
            OpenAiResponseFormat::JsonSchema { json_schema } => {
                let schema = json_schema
                    .schema
                    .ok_or("response_format json_schema requires a schema")?;
                Ok(Some(ResponseFormat::JsonSchema {
                    name: json_schema.name,
                    schema,
                    strict: json_schema.strict,
                }))
            }
        }
    }
}

/// OpenAI's `stream_options`. `include_usage` asks for a final usage
/// frame after the content, which the SDKs surface as the stream's token
/// accounting (LangChain's `stream_usage=True` sets it).
#[derive(Debug, Clone, Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: bool,
}

/// The most texts one `/v1/embeddings` call may carry. The whole batch
/// rides a single inline job envelope, so the bound keeps one request
/// well under the network's frame limit; a larger corpus splits across
/// calls. OpenAI's own ceiling is higher (2048), but this endpoint
/// dispatches the batch as one paid job over inline content, not a
/// content-addressed upload.
const MAX_EMBEDDING_INPUTS: usize = 256;

#[derive(Debug, Clone, Deserialize)]
struct EmbeddingsRequest {
    #[serde(default)]
    model: String,
    input: EmbeddingInput,
    /// `float` (the default) returns each vector as a JSON array of
    /// numbers; `base64` returns it as base64-packed little-endian
    /// float32 — the wire form the OpenAI SDKs request by default and
    /// decode transparently.
    #[serde(default)]
    encoding_format: Option<String>,
    /// OpenAI's post-hoc dimensionality reshape. The served model fixes
    /// the vector width here, so a request that names one is refused
    /// rather than silently answered at a different width.
    #[serde(default)]
    dimensions: Option<u32>,
}

/// OpenAI's `input` is a single string, a list of strings, or a
/// pre-tokenized integer array. Only text is embeddable on this network;
/// the token-array forms are modeled so they earn a clear refusal rather
/// than an opaque parse error.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum EmbeddingInput {
    Single(String),
    Many(Vec<String>),
    Tokens(Vec<i64>),
    TokenBatches(Vec<Vec<i64>>),
}

impl EmbeddingInput {
    /// The texts to embed, or a client-error message. Token-array inputs
    /// and blank text are refused before anything is priced or signed.
    fn into_texts(self) -> Result<Vec<String>, String> {
        let texts = match self {
            EmbeddingInput::Single(s) => vec![s],
            EmbeddingInput::Many(v) => v,
            EmbeddingInput::Tokens(tokens) => {
                return Err(format!(
                    "input is a {}-integer token array; send text strings, \
                     not pre-tokenized ids",
                    tokens.len()
                ))
            }
            EmbeddingInput::TokenBatches(batches) => {
                return Err(format!(
                    "input is {} pre-tokenized token arrays; send text strings",
                    batches.len()
                ))
            }
        };
        if texts.is_empty() {
            return Err("input must not be empty".into());
        }
        if texts.len() > MAX_EMBEDDING_INPUTS {
            return Err(format!(
                "input carries {} texts, past the {MAX_EMBEDDING_INPUTS}-per-call limit; \
                 split the batch across calls",
                texts.len()
            ));
        }
        if texts.iter().any(|t| t.trim().is_empty()) {
            return Err("every input text must be non-empty".into());
        }
        Ok(texts)
    }
}

/// How each vector is encoded in the response.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EncodingFormat {
    Float,
    Base64,
}

impl EncodingFormat {
    fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("float") => Ok(EncodingFormat::Float),
            Some("base64") => Ok(EncodingFormat::Base64),
            Some(other) => Err(format!(
                "encoding_format {other:?} is not supported; use \"float\" or \"base64\""
            )),
        }
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Dispatch a paid job so its settlement survives a client disconnect.
/// The reservation is released on drop only while the job is still in this
/// task; once the envelope is submitted the coordinator holds escrow and an
/// operator can complete it whether or not the client waits, so letting the
/// request future's cancellation drop the reservation would leave real
/// spend uncounted against the session cap. Spawning detaches the
/// submit-verify-settle sequence from that cancellation. The streaming path
/// spawns for the same reason.
pub(crate) async fn dispatch_settling(
    state: Arc<OpenAiState>,
    request: JobRequest,
    reservation: SpendReservation,
) -> Result<DispatchOutcome, Response> {
    let joined = tokio::spawn(async move {
        let result = dispatch_and_verify(&state.http, &state.buyer, &state.identity, request).await;
        match &result {
            Ok(_) => reservation.settle(),
            // The buy never settled — release the hold so its price returns
            // to the session's headroom.
            Err(_) => drop(reservation),
        }
        result
    })
    .await;
    match joined {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(e)) => Err(map_dispatch_error(e)),
        Err(_) => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "the dispatch task did not complete",
        )),
    }
}

/// The largest `n` a single request may fan out to. Each completion is an
/// independent paid job on the network, so this bounds the spend and the
/// operator load one request can trigger; OpenAI's own ceiling is far
/// higher, but a paid network keeps the fan-out modest.
const MAX_COMPLETIONS: u32 = 8;

/// Reads the OpenAI `n` (how many completions to return). `None` and `1`
/// take the ordinary single-job path; `2..=MAX_COMPLETIONS` fans out to
/// that many independent paid jobs; `0` or anything larger is refused
/// before any spend. Streaming is single-completion only — a live feed
/// interleaving several completions is refused, not silently collapsed to
/// one.
fn completions_count(n: Option<u32>, stream: bool) -> Result<u32, String> {
    let n = n.unwrap_or(1);
    if n == 0 {
        return Err("n must be at least 1".into());
    }
    if n > MAX_COMPLETIONS {
        return Err(format!("n must be between 1 and {MAX_COMPLETIONS}"));
    }
    if n > 1 && stream {
        return Err(
            "streaming supports only n=1; omit stream, or set n=1, to request several completions"
                .into(),
        );
    }
    Ok(n)
}

/// Shapes a spend-cap refusal into the OpenAI error a client expects: an
/// over-ceiling offer is a bad request, a full session cap is a quota
/// limit.
pub(crate) fn reservation_refusal(msg: String) -> Response {
    let (status, kind) = if msg.contains("per-call ceiling") {
        (StatusCode::BAD_REQUEST, "invalid_request_error")
    } else {
        (StatusCode::TOO_MANY_REQUESTS, "insufficient_quota")
    };
    api_error(status, kind, msg)
}

/// Fan a request out to `n` independent paid jobs and gather the
/// completions that settled.
///
/// Money: every completion is priced and reserved separately, so the whole
/// `n × price` is held against the session cap before any job is placed. If
/// the cap can't hold all `n`, none is dispatched and nothing is charged —
/// a partial batch is never submitted on a full budget. Once submitted,
/// each job settles or releases its own reservation on its own outcome (the
/// single-job contract, run `n` times), so the buyer is charged for exactly
/// the completions that come back. A job that faults has already released
/// its reservation; if every job faults, the first fault is surfaced.
async fn dispatch_fanned(
    state: &Arc<OpenAiState>,
    request: JobRequest,
    price: u64,
    n: u32,
) -> Result<Vec<DispatchOutcome>, Response> {
    let mut reservations = Vec::with_capacity(n as usize);
    for _ in 0..n {
        match state.caps.try_reserve(price) {
            Ok(reservation) => reservations.push(reservation),
            // The cap can't hold the full fan-out. Returning here drops the
            // reservations already taken, releasing each; nothing was
            // dispatched, so nothing is charged.
            Err(msg) => return Err(reservation_refusal(msg)),
        }
    }

    let dispatches = reservations
        .into_iter()
        .map(|reservation| dispatch_settling(Arc::clone(state), request.clone(), reservation));
    let results = futures::future::join_all(dispatches).await;

    let mut outcomes = Vec::with_capacity(results.len());
    let mut first_error = None;
    for result in results {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(response) => {
                if first_error.is_none() {
                    first_error = Some(response);
                }
            }
        }
    }
    if outcomes.is_empty() {
        return Err(first_error.expect("a fully-failed fan-out carries at least one error"));
    }
    Ok(outcomes)
}

async fn chat_completions(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("could not parse the request body: {e}"),
            )
        }
    };

    let n = match completions_count(req.n, req.stream) {
        Ok(n) => n,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    if req.logit_bias.as_ref().is_some_and(|m| !m.is_empty()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "logit_bias is not supported",
        );
    }
    if req.functions.is_some() || req.function_call.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the functions/function_call API is deprecated and not supported; use tools and \
             tool_choice",
        );
    }
    if req.parallel_tool_calls == Some(false) && !req.tools.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "parallel_tool_calls=false is not supported: this network can't guarantee \
             one tool call per turn; omit it to take the model's default",
        );
    }
    if req.reasoning_effort.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "reasoning_effort is not supported: this network serves ordinary models with \
             no reasoning-effort control; omit reasoning_effort",
        );
    }
    if req.verbosity.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "verbosity is not supported: this network serves ordinary models with no \
             output-verbosity control; omit verbosity",
        );
    }
    if req
        .modalities
        .as_ref()
        .is_some_and(|m| m.iter().any(|kind| kind != "text"))
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "a modality other than text is not supported: this chat path returns text only; \
             request audio through the speech endpoint",
        );
    }
    if req.web_search_options.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "web_search_options is not supported: this network's models have no web access \
             and can't ground an answer in a live search; omit web_search_options",
        );
    }
    if req.model.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required",
        );
    }
    if req.messages.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "messages must not be empty",
        );
    }
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let stream = req.stream;
    let model = req.model.clone();

    // Narrow each OpenAI message (wider roles, string-or-array content) to
    // the protocol's canonical form before anything is priced or signed.
    let messages = match req
        .messages
        .into_iter()
        .map(OpenAiChatMessage::into_protocol)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(messages) => messages,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };

    // An empty `tools` list is the same as offering none (OpenAI treats it
    // so). The `tool_choice` still rides along: `InferArgs::input` refuses
    // a forcing one (`required`/named) that has no tool to bind, rather
    // than silently billing a plain completion; `auto`/`none` drop quietly.
    let (tools, tool_choice) = if req.tools.is_empty() {
        (None, req.tool_choice)
    } else {
        (Some(req.tools), req.tool_choice)
    };

    let response_format = match req.response_format {
        None => None,
        Some(rf) => match rf.into_protocol() {
            Ok(rf) => rf,
            Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
        },
    };

    // OpenAI couples the two knobs: `top_logprobs` is only meaningful with
    // `logprobs: true`. Fold them to the protocol's single knob — the
    // number of alternatives to report, its presence meaning "on".
    let logprobs = if req.logprobs {
        Some(req.top_logprobs.unwrap_or(0))
    } else if req.top_logprobs.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "top_logprobs requires logprobs to be true",
        );
    } else {
        None
    };

    // Pack the input through the same shared arguments every buyer
    // surface uses, so a chat job means the same thing here as on the CLI
    // and the MCP server. `max_tokens` and its newer alias fold to one.
    let args = InferArgs {
        prompt: None,
        messages: Some(messages),
        images: None,
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: None,
        deadline_ms: None,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens.or(req.max_completion_tokens),
        seed: req.seed,
        presence_penalty: req.presence_penalty,
        frequency_penalty: req.frequency_penalty,
        logprobs,
        stop: req.stop.map(StopField::into_vec),
        response_format,
        tools,
        tool_choice,
        idempotency_key: None,
        dry_run: false,
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };

    // Refuse a model the network is not serving before pricing or
    // dispatching it — the SDK-correct 404, not a doomed submit that
    // would come back as a bad gateway naming an internal job.
    if let Err(resp) = ensure_model_servable(&state, &model, JobKind::InferenceCall).await {
        return resp;
    }

    // OpenAI requests carry no price. Offer the cheapest matching ask,
    // held under the per-call ceiling — the same default-price behaviour
    // the MCP server takes when a caller names no price.
    let cap = state.caps.max_price_micro_usdc();
    let price = cheapest_matching_ask(
        &state.http,
        &state.buyer,
        JobKind::InferenceCall,
        Some(&model),
        None,
        None,
        min_reputation_bps,
    )
    .await
    .ok()
    .flatten()
    .map(|floor| floor.min(cap))
    .unwrap_or(cap);

    let request = JobRequest {
        kind: JobKind::InferenceCall,
        input,
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms: state.default_deadline_ms,
    };

    // Reserve before dispatching: this handler runs concurrently, so the
    // session-cap check and the hold have to be one atomic step or two
    // requests could both pass the check and overshoot the cap.
    if stream {
        // Streaming is single-completion only (`completions_count` refuses
        // `n > 1` with `stream`), so one reservation covers it.
        let reservation = match state.caps.try_reserve(price) {
            Ok(reservation) => reservation,
            Err(msg) => return reservation_refusal(msg),
        };
        let include_usage = req
            .stream_options
            .as_ref()
            .map(|o| o.include_usage)
            .unwrap_or(false);
        return stream_chat(state, model, request, reservation, include_usage).await;
    }

    if n == 1 {
        let reservation = match state.caps.try_reserve(price) {
            Ok(reservation) => reservation,
            Err(msg) => return reservation_refusal(msg),
        };
        return match dispatch_settling(state, request, reservation).await {
            Ok(outcome) => Json(chat_completion_response(&model, &outcome)).into_response(),
            Err(response) => response,
        };
    }

    // n > 1: fan out to n independent paid jobs, each with its own
    // reservation, and return the completions that settled.
    match dispatch_fanned(&state, request, price, n).await {
        Ok(outcomes) => Json(chat_completion_response_multi(&model, &outcomes)).into_response(),
        Err(response) => response,
    }
}

/// The streaming variant of a chat completion: server-sent
/// `chat.completion.chunk` frames as the operator relays tokens, a final
/// frame with `finish_reason`, then `[DONE]`. The signed receipt is still
/// verified before the reservation settles — the live feed is a preview,
/// the receipt over the final output is the artifact, exactly as the
/// non-streaming path and the MCP streaming pair treat it.
///
/// The job is submitted here so a pre-flight refusal (an underfunded
/// buyer, no capable operator) is a real HTTP error, not a 200 event
/// stream that dies on its first frame. Only once the coordinator has
/// accepted the envelope does the drain move to a background task and the
/// response become an SSE body.
async fn stream_chat(
    state: Arc<OpenAiState>,
    model: String,
    request: JobRequest,
    reservation: SpendReservation,
    include_usage: bool,
) -> Response {
    let job_id = Uuid::new_v4();
    let id = format!("chatcmpl-{}", job_id.simple());
    let created = epoch_secs();
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    // Signals the submit outcome so the handler can shape a real HTTP
    // response — a pre-flight refusal as an error status, an accepted job
    // as a 200 SSE body. The submit runs inside the spawned task rather
    // than being awaited in this request frame: awaited here, a client
    // disconnect during submit would cancel the handler and drop the
    // reservation while the coordinator already holds escrow, leaving real
    // spend uncounted against the session cap. Spawning first gives the
    // streaming path the same cancellation-safety dispatch_settling gives
    // the non-streaming one.
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), BuyerError>>();

    // The exact text the live feed delivered. The feed is a best-effort
    // preview; the signed receipt is the paid artifact. A node that can't
    // stream (or a relay that lost the tail) delivers less than the
    // verified output — or, for a misbehaving node, something divergent —
    // yet the receipt still bills the buyer, so the feed is reconciled
    // against the verified output below rather than passed off as complete.
    let shown = Arc::new(Mutex::new(String::new()));
    let shown_feed = Arc::clone(&shown);
    tokio::spawn(async move {
        let envelope =
            match submit_streaming(&state.http, &state.buyer, &state.identity, job_id, request)
                .await
            {
                Ok(envelope) => {
                    let _ = ready_tx.send(Ok(()));
                    envelope
                }
                Err(e) => {
                    // The buy never placed — release the hold; the handler
                    // turns the reason into the HTTP error.
                    drop(reservation);
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
        // OpenAI's first frame announces the assistant role with no content.
        let _ = tx.send(chunk_event(
            &id,
            created,
            &model,
            json!({ "role": "assistant" }),
            None,
        ));

        let chunk_tx = tx.clone();
        let chunk_id = id.clone();
        let chunk_model = model.clone();
        let drained = stream_and_verify(
            &state.http,
            &state.buyer,
            &state.identity,
            envelope,
            move |delta| {
                if let Ok(mut feed) = shown_feed.lock() {
                    feed.push_str(delta);
                }
                let _ = chunk_tx.send(chunk_event(
                    &chunk_id,
                    created,
                    &chunk_model,
                    json!({ "content": delta }),
                    None,
                ));
            },
        )
        .await;
        match drained {
            Ok(streamed) => {
                reservation.settle();
                // Complete the feed from the verified output so the client
                // always ends holding what it paid for. When the feed is a
                // prefix (the common lost-tail or can't-stream case) only the
                // missing remainder is sent; when it diverged, the verified
                // output is sent in full to supersede the preview.
                let reply = parse_assistant_output(&streamed.outcome.output);
                let shown = shown.lock().map(|feed| feed.clone()).unwrap_or_default();
                let missing = match reply.text.strip_prefix(shown.as_str()) {
                    Some(tail) => tail,
                    None => reply.text.as_str(),
                };
                if !missing.is_empty() {
                    let _ = tx.send(chunk_event(
                        &id,
                        created,
                        &model,
                        json!({ "content": missing }),
                        None,
                    ));
                }
                // Logprobs never ride the live feed either (a logprobs job
                // runs the backend non-streaming), so they are emitted whole
                // from the verified output in one frame carrying OpenAI's
                // `choices[].logprobs` object.
                if let Some(tokens) = reply.logprobs.as_deref() {
                    let frame = json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "logprobs": openai_logprobs(tokens),
                            "finish_reason": Value::Null,
                        }],
                    });
                    let _ = tx.send(Event::default().data(frame.to_string()));
                }
                // Tool calls never ride the live feed (a tools job runs the
                // backend non-streaming), so they are emitted whole from the
                // verified output, one delta indexed the way OpenAI streams
                // them.
                if !reply.tool_calls.is_empty() {
                    let calls: Vec<Value> = reply
                        .tool_calls
                        .iter()
                        .enumerate()
                        .map(|(index, call)| {
                            json!({
                                "index": index,
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.function.name,
                                    "arguments": call.function.arguments,
                                },
                            })
                        })
                        .collect();
                    let _ = tx.send(chunk_event(
                        &id,
                        created,
                        &model,
                        json!({ "tool_calls": calls }),
                        None,
                    ));
                }
                let finish = openai_finish_reason(&streamed.outcome.receipt.receipt.meter);
                let _ = tx.send(chat_finish_event(
                    &id,
                    created,
                    &model,
                    finish,
                    &streamed.outcome,
                ));
                // A client that asked for usage gets the real token counts
                // in a final choices-empty frame, the way OpenAI closes a
                // stream_options.include_usage stream.
                if include_usage {
                    let meter = &streamed.outcome.receipt.receipt.meter;
                    let _ = tx.send(usage_event(
                        &id,
                        created,
                        &model,
                        "chat.completion.chunk",
                        meter,
                    ));
                }
                let _ = tx.send(Event::default().data("[DONE]"));
            }
            Err(e) => {
                // The buy never settled — release the hold. The stream is
                // already a 200, so the failure rides back as a terminal
                // error frame the client reads, the shape OpenAI's own API
                // uses mid-stream.
                drop(reservation);
                let body = json!({
                    "error": { "message": e.to_string(), "type": "server_error", "param": null, "code": null }
                });
                let _ = tx.send(Event::default().data(body.to_string()));
            }
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {
            let stream = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv()
                    .await
                    .map(|event| (Ok::<Event, Infallible>(event), rx))
            });
            Sse::new(stream).into_response()
        }
        Ok(Err(e)) => map_dispatch_error(e),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "the streaming dispatch task did not start",
        ),
    }
}

/// One `chat.completion.chunk` SSE frame.
fn chunk_event(
    id: &str,
    created: u64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> Event {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    Event::default().data(chunk.to_string())
}

/// The closing `chat.completion.chunk`: the finish reason and the
/// verified receipt, so a streamed chat carries the same proof a
/// non-streamed one does. A plain OpenAI client reads the finish reason
/// and ignores the `covenant` field, exactly as on the non-streaming body.
fn chat_finish_event(
    id: &str,
    created: u64,
    model: &str,
    finish_reason: &str,
    outcome: &DispatchOutcome,
) -> Event {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason }],
        "covenant": covenant_receipt(outcome),
    });
    Event::default().data(chunk.to_string())
}

/// The final frame of a stream a client asked usage for: no choices, the
/// metered token counts from the verified receipt. `object` is the
/// stream's chunk type (`chat.completion.chunk` for chat,
/// `text_completion` for the legacy completions stream).
fn usage_event(id: &str, created: u64, model: &str, object: &str, meter: &JobMeter) -> Event {
    let prompt = meter.tokens_in.unwrap_or(0);
    let completion = meter.tokens_out.unwrap_or(0);
    let chunk = json!({
        "id": id,
        "object": object,
        "created": created,
        "model": model,
        "choices": [],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt.saturating_add(completion),
        },
    });
    Event::default().data(chunk.to_string())
}

/// The legacy `POST /v1/completions` request. It shares every sampling
/// knob with chat, but takes a raw `prompt` instead of `messages` and has
/// no tools or `response_format`. The knobs this chat-backed network can't
/// honor (`logprobs`, `logit_bias`, `echo`, `suffix`, multi-candidate
/// sampling) are modeled so they earn a clear refusal rather than silent
/// acceptance.
#[derive(Debug, Clone, Deserialize)]
struct CompletionRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt: Option<CompletionPrompt>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    seed: Option<i64>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    stop: Option<StopField>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    logprobs: Option<u32>,
    #[serde(default)]
    echo: bool,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default)]
    best_of: Option<u32>,
    #[serde(default)]
    logit_bias: Option<Map<String, Value>>,
}

/// OpenAI's legacy `prompt`: one string, an array of strings, or
/// pre-tokenized integer arrays. This endpoint completes a single text
/// prompt per call — an array of one string unwraps to it; a
/// multi-prompt batch and the token-array forms earn a clear refusal
/// rather than a silent partial answer, the same discipline the chat
/// endpoint takes toward `n > 1`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum CompletionPrompt {
    One(String),
    Many(Vec<String>),
    Tokens(Vec<i64>),
    TokenBatches(Vec<Vec<i64>>),
}

impl CompletionPrompt {
    /// The single prompt text to complete, or a client-error message.
    fn into_text(self) -> Result<String, String> {
        let text = match self {
            CompletionPrompt::One(s) => s,
            CompletionPrompt::Many(mut v) => match v.len() {
                0 => return Err("prompt must not be empty".into()),
                1 => v.pop().expect("length checked"),
                n => {
                    return Err(format!(
                        "prompt carries {n} strings; this endpoint completes one prompt per call — \
                         send one prompt string, or split the batch across calls"
                    ))
                }
            },
            CompletionPrompt::Tokens(tokens) => {
                return Err(format!(
                    "prompt is a {}-integer token array; send a text string, \
                     not pre-tokenized ids",
                    tokens.len()
                ))
            }
            CompletionPrompt::TokenBatches(batches) => {
                return Err(format!(
                    "prompt is {} pre-tokenized token arrays; send one text string",
                    batches.len()
                ))
            }
        };
        if text.trim().is_empty() {
            return Err("prompt must not be empty".into());
        }
        Ok(text)
    }
}

impl CompletionRequest {
    /// The first field set that this chat-backed executor can't honor, if
    /// any. Refused before pricing, so a client never pays for a feature
    /// that was silently dropped — the same fail-closed posture the chat
    /// handler takes toward `n`.
    fn unsupported(&self) -> Option<&'static str> {
        if self.logprobs.is_some() {
            Some("logprobs is not supported")
        } else if self.echo {
            Some("echo is not supported")
        } else if self.suffix.is_some() {
            Some("suffix (fill-in-the-middle) is not supported")
        } else if self.best_of.unwrap_or(1) > 1 {
            Some("best_of > 1 is not supported")
        } else if self.logit_bias.as_ref().is_some_and(|m| !m.is_empty()) {
            Some("logit_bias is not supported")
        } else {
            None
        }
    }
}

/// `POST /v1/completions` — OpenAI's legacy text-completion endpoint. A
/// raw prompt is bought as one inference job on the network, paid under
/// the same caps every buyer surface enforces, and returned as a standard
/// `text_completion` object carrying the operator's signed, locally
/// re-verified receipt under `covenant`. The prompt travels the same
/// paid pipeline as a chat turn; the network serves it as a single-message
/// completion.
async fn completions(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let req: CompletionRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("could not parse the request body: {e}"),
            )
        }
    };

    let n = match completions_count(req.n, req.stream) {
        Ok(n) => n,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    if let Some(reason) = req.unsupported() {
        return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", reason);
    }
    if req.model.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required",
        );
    }
    let prompt = match req.prompt {
        Some(prompt) => match prompt.into_text() {
            Ok(text) => text,
            Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
        },
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "prompt is required",
            )
        }
    };
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let stream = req.stream;
    let model = req.model.clone();

    // Pack the raw prompt through the same shared arguments every buyer
    // surface uses, so a completion means the same thing here as a
    // `--prompt` on the CLI.
    let args = InferArgs {
        prompt: Some(prompt),
        messages: None,
        images: None,
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: None,
        deadline_ms: None,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        seed: req.seed,
        presence_penalty: req.presence_penalty,
        frequency_penalty: req.frequency_penalty,
        // Legacy completions logprobs use a different wire shape and are
        // refused up front (see the completion request's own guard).
        logprobs: None,
        stop: req.stop.map(StopField::into_vec),
        response_format: None,
        tools: None,
        tool_choice: None,
        idempotency_key: None,
        dry_run: false,
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };

    // Refuse a model the network is not serving before pricing or
    // dispatching it — the SDK-correct 404, not a doomed submit that
    // would come back as a bad gateway naming an internal job.
    if let Err(resp) = ensure_model_servable(&state, &model, JobKind::InferenceCall).await {
        return resp;
    }

    let cap = state.caps.max_price_micro_usdc();
    let price = cheapest_matching_ask(
        &state.http,
        &state.buyer,
        JobKind::InferenceCall,
        Some(&model),
        None,
        None,
        min_reputation_bps,
    )
    .await
    .ok()
    .flatten()
    .map(|floor| floor.min(cap))
    .unwrap_or(cap);

    let request = JobRequest {
        kind: JobKind::InferenceCall,
        input,
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms: state.default_deadline_ms,
    };

    if stream {
        // Streaming is single-completion only (`completions_count` refuses
        // `n > 1` with `stream`), so one reservation covers it.
        let reservation = match state.caps.try_reserve(price) {
            Ok(reservation) => reservation,
            Err(msg) => return reservation_refusal(msg),
        };
        let include_usage = req
            .stream_options
            .as_ref()
            .map(|o| o.include_usage)
            .unwrap_or(false);
        return stream_completion(state, model, request, reservation, include_usage).await;
    }

    if n == 1 {
        let reservation = match state.caps.try_reserve(price) {
            Ok(reservation) => reservation,
            Err(msg) => return reservation_refusal(msg),
        };
        return match dispatch_settling(state, request, reservation).await {
            Ok(outcome) => Json(completion_response(&model, &outcome)).into_response(),
            Err(response) => response,
        };
    }

    // n > 1: fan out to n independent paid jobs, each with its own
    // reservation, and return the completions that settled.
    match dispatch_fanned(&state, request, price, n).await {
        Ok(outcomes) => Json(completion_response_multi(&model, &outcomes)).into_response(),
        Err(response) => response,
    }
}

/// The streaming variant of a legacy completion: server-sent
/// `text_completion` frames as the operator relays tokens, a final frame
/// with `finish_reason`, then `[DONE]`. As with the chat stream, the live
/// feed is a preview and the signed receipt over the final output is the
/// artifact — a node that can't stream, or a feed that lost its tail, is
/// reconciled against the verified output before the reservation settles.
async fn stream_completion(
    state: Arc<OpenAiState>,
    model: String,
    request: JobRequest,
    reservation: SpendReservation,
    include_usage: bool,
) -> Response {
    let job_id = Uuid::new_v4();
    let id = format!("cmpl-{}", job_id.simple());
    let created = epoch_secs();
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    // Submit runs inside the spawned task, not awaited in this request
    // frame, so a client disconnect during submit cannot drop the
    // reservation while the coordinator already holds escrow — the same
    // cancellation-safety the non-streaming dispatch_settling gives, which
    // stream_chat documents in full.
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), BuyerError>>();

    let shown = Arc::new(Mutex::new(String::new()));
    let shown_feed = Arc::clone(&shown);
    tokio::spawn(async move {
        let envelope =
            match submit_streaming(&state.http, &state.buyer, &state.identity, job_id, request)
                .await
            {
                Ok(envelope) => {
                    let _ = ready_tx.send(Ok(()));
                    envelope
                }
                Err(e) => {
                    drop(reservation);
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
        let chunk_tx = tx.clone();
        let chunk_id = id.clone();
        let chunk_model = model.clone();
        let drained = stream_and_verify(
            &state.http,
            &state.buyer,
            &state.identity,
            envelope,
            move |delta| {
                if let Ok(mut feed) = shown_feed.lock() {
                    feed.push_str(delta);
                }
                let _ = chunk_tx.send(completion_chunk_event(
                    &chunk_id,
                    created,
                    &chunk_model,
                    delta,
                    None,
                ));
            },
        )
        .await;
        match drained {
            Ok(streamed) => {
                reservation.settle();
                // Complete the feed from the verified output so the client
                // always ends holding what it paid for: a prefix gets only
                // its missing tail, a diverged feed the full output.
                let reply = parse_assistant_output(&streamed.outcome.output);
                let shown = shown.lock().map(|feed| feed.clone()).unwrap_or_default();
                let missing = match reply.text.strip_prefix(shown.as_str()) {
                    Some(tail) => tail,
                    None => reply.text.as_str(),
                };
                if !missing.is_empty() {
                    let _ = tx.send(completion_chunk_event(&id, created, &model, missing, None));
                }
                let finish = openai_finish_reason(&streamed.outcome.receipt.receipt.meter);
                let _ = tx.send(completion_finish_event(
                    &id,
                    created,
                    &model,
                    finish,
                    &streamed.outcome,
                ));
                if include_usage {
                    let meter = &streamed.outcome.receipt.receipt.meter;
                    let _ = tx.send(usage_event(&id, created, &model, "text_completion", meter));
                }
                let _ = tx.send(Event::default().data("[DONE]"));
            }
            Err(e) => {
                drop(reservation);
                let body = json!({
                    "error": { "message": e.to_string(), "type": "server_error", "param": null, "code": null }
                });
                let _ = tx.send(Event::default().data(body.to_string()));
            }
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {
            let stream = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv()
                    .await
                    .map(|event| (Ok::<Event, Infallible>(event), rx))
            });
            Sse::new(stream).into_response()
        }
        Ok(Err(e)) => map_dispatch_error(e),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "the streaming dispatch task did not start",
        ),
    }
}

/// One `text_completion` SSE frame: the delta in `text`, `logprobs`
/// always null (this network does not surface them), and a terminal
/// `finish_reason` on the closing frame.
fn completion_chunk_event(
    id: &str,
    created: u64,
    model: &str,
    text: &str,
    finish_reason: Option<&str>,
) -> Event {
    let chunk = json!({
        "id": id,
        "object": "text_completion",
        "created": created,
        "model": model,
        "choices": [{
            "text": text,
            "index": 0,
            "logprobs": null,
            "finish_reason": finish_reason,
        }],
    });
    Event::default().data(chunk.to_string())
}

/// The closing `text_completion` frame: the finish reason and the
/// verified receipt, so a streamed completion carries the same proof a
/// non-streamed one does. A plain client reads the finish reason and
/// ignores the `covenant` field.
fn completion_finish_event(
    id: &str,
    created: u64,
    model: &str,
    finish_reason: &str,
    outcome: &DispatchOutcome,
) -> Event {
    let chunk = json!({
        "id": id,
        "object": "text_completion",
        "created": created,
        "model": model,
        "choices": [{ "text": "", "index": 0, "logprobs": null, "finish_reason": finish_reason }],
        "covenant": covenant_receipt(outcome),
    });
    Event::default().data(chunk.to_string())
}

/// One text-completion choice at `index`, plus its metered token counts.
/// Shared by the single- and multi-completion builders so a choice means
/// exactly the same thing whichever path shaped it.
fn completion_choice(index: usize, outcome: &DispatchOutcome) -> (Value, u64, u64) {
    let receipt = &outcome.receipt.receipt;
    let reply = parse_assistant_output(&outcome.output);
    let choice = json!({
        "text": reply.text,
        "index": index,
        "logprobs": null,
        "finish_reason": openai_finish_reason(&receipt.meter),
    });
    (
        choice,
        receipt.meter.tokens_in.unwrap_or(0),
        receipt.meter.tokens_out.unwrap_or(0),
    )
}

/// The signed, re-verified receipt shaped into a standard OpenAI
/// `text_completion`, plus the `covenant` proof field a plain client
/// ignores and a Covenant-aware one verifies on-chain.
fn completion_response(model: &str, outcome: &DispatchOutcome) -> Value {
    let (choice, prompt_tokens, completion_tokens) = completion_choice(0, outcome);
    json!({
        "id": format!("cmpl-{}", outcome.receipt.receipt.job_id.simple()),
        "object": "text_completion",
        "created": epoch_secs(),
        "model": model,
        "choices": [choice],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        },
        "covenant": covenant_receipt(outcome),
    })
}

/// The `n > 1` counterpart: one `text_completion` carrying every completion
/// that settled as its own indexed choice, `usage` summed across them, and a
/// `covenant.receipts` array with one verified receipt per choice — each a
/// distinct paid job on the network. The batch takes the first job's id.
fn completion_response_multi(model: &str, outcomes: &[DispatchOutcome]) -> Value {
    let mut choices = Vec::with_capacity(outcomes.len());
    let mut receipts = Vec::with_capacity(outcomes.len());
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    for (index, outcome) in outcomes.iter().enumerate() {
        let (choice, prompt, completion) = completion_choice(index, outcome);
        choices.push(choice);
        receipts.push(covenant_receipt(outcome));
        prompt_tokens = prompt_tokens.saturating_add(prompt);
        completion_tokens = completion_tokens.saturating_add(completion);
    }
    json!({
        "id": format!("cmpl-{}", outcomes[0].receipt.receipt.job_id.simple()),
        "object": "text_completion",
        "created": epoch_secs(),
        "model": model,
        "choices": choices,
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        },
        "covenant": { "receipts": receipts },
    })
}

/// `POST /v1/embeddings`. The batch of input texts is bought as one
/// embedding job on the network, paid under the same caps every buyer
/// surface enforces, and returned as a standard OpenAI embeddings list —
/// each vector in `float` or `base64` form — with the operator's signed,
/// locally re-verified receipt attached under `covenant`.
async fn embeddings(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let req: EmbeddingsRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("could not parse the request body: {e}"),
            )
        }
    };

    if req.model.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required",
        );
    }
    if req.dimensions.is_some() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "dimensions is not supported: the embedding model fixes the vector width; omit it",
        );
    }
    let encoding = match EncodingFormat::parse(req.encoding_format.as_deref()) {
        Ok(fmt) => fmt,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let texts = match req.input.into_texts() {
        Ok(texts) => texts,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let model = req.model.clone();
    let input: Vec<Content> = texts.into_iter().map(Content::text).collect();
    // The operator must return exactly one vector per input text; hold onto
    // the count so the response can reject a mismatched result rather than
    // pair vectors to inputs by position.
    let input_count = input.len();

    // Refuse a model the network is not serving before pricing or
    // dispatching it — the SDK-correct 404, not a doomed submit that
    // would come back as a bad gateway naming an internal job.
    if let Err(resp) = ensure_model_servable(&state, &model, JobKind::Embedding).await {
        return resp;
    }

    // OpenAI requests carry no price. Offer the cheapest matching ask
    // under the per-call ceiling, exactly as the chat handler does.
    let cap = state.caps.max_price_micro_usdc();
    let price = cheapest_matching_ask(
        &state.http,
        &state.buyer,
        JobKind::Embedding,
        Some(&model),
        None,
        None,
        min_reputation_bps,
    )
    .await
    .ok()
    .flatten()
    .map(|floor| floor.min(cap))
    .unwrap_or(cap);

    let reservation = match state.caps.try_reserve(price) {
        Ok(reservation) => reservation,
        Err(msg) => {
            let (status, kind) = if msg.contains("per-call ceiling") {
                (StatusCode::BAD_REQUEST, "invalid_request_error")
            } else {
                (StatusCode::TOO_MANY_REQUESTS, "insufficient_quota")
            };
            return api_error(status, kind, msg);
        }
    };

    let request = JobRequest {
        kind: JobKind::Embedding,
        input,
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms: state.default_deadline_ms,
    };

    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => match embeddings_response(&model, &outcome, encoding, input_count) {
            Ok(body) => Json(body).into_response(),
            // The job settled, but the operator's output was not
            // embedding-shaped — an upstream fault, surfaced as a bad
            // gateway the way a failed re-verification is.
            Err(msg) => api_error(StatusCode::BAD_GATEWAY, "server_error", msg),
        },
        Err(response) => response,
    }
}

/// The verified embedding job shaped into a standard OpenAI embeddings
/// list, plus the `covenant` receipt field a plain client ignores.
fn embeddings_response(
    model: &str,
    outcome: &DispatchOutcome,
    encoding: EncodingFormat,
    input_count: usize,
) -> Result<Value, String> {
    let result = parse_embedding_output_expecting(&outcome.output, input_count)
        .map_err(|e| format!("the operator returned no usable embedding: {e}"))?;
    let data: Vec<Value> = result
        .embeddings
        .iter()
        .enumerate()
        .map(|(index, vector)| {
            let embedding = match encoding {
                EncodingFormat::Float => json!(vector),
                EncodingFormat::Base64 => json!(base64_f32(vector)),
            };
            json!({ "object": "embedding", "index": index, "embedding": embedding })
        })
        .collect();
    let prompt_tokens = outcome.receipt.receipt.meter.tokens_in.unwrap_or(0);
    Ok(json!({
        "object": "list",
        "data": data,
        "model": model,
        // Embeddings consume input tokens only; there is no completion.
        "usage": {
            "prompt_tokens": prompt_tokens,
            "total_tokens": prompt_tokens,
        },
        "covenant": covenant_receipt(outcome),
    }))
}

/// Packs a float32 vector into base64 little-endian bytes — the form the
/// OpenAI SDKs request by default and decode with
/// `np.frombuffer(..., dtype="float32")`.
fn base64_f32(vector: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// How a transcription is returned, from the request's `response_format`.
/// The two text-shaped formats (`json`, `text`) carry the bare transcript;
/// the timestamp-bearing ones (`verbose_json`, `srt`, `vtt`) ask the
/// backend for per-segment timings and render them for captioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptFormat {
    Json,
    Text,
    VerboseJson,
    Srt,
    Vtt,
}

impl TranscriptFormat {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "" | "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            "verbose_json" => Ok(Self::VerboseJson),
            "srt" => Ok(Self::Srt),
            "vtt" => Ok(Self::Vtt),
            other => Err(format!(
                "response_format {other:?} is not supported; use 'json', 'text', \
                 'verbose_json', 'srt', or 'vtt'"
            )),
        }
    }

    /// The timestamp-bearing formats need the backend to return per-segment
    /// timings; the plain-text ones do not, so a plain request never asks
    /// for the extra work.
    fn needs_segments(self) -> bool {
        matches!(self, Self::VerboseJson | Self::Srt | Self::Vtt)
    }
}

/// `POST /v1/audio/transcriptions`: OpenAI's speech-to-text endpoint. A
/// multipart upload of an audio `file` (with an optional `model`,
/// `language`, and `response_format`) buys one transcription job on the
/// network, paid and re-verified like every other buy. `response_format`
/// defaults to `json` — `{"text": …}` plus the extra `covenant` receipt a
/// plain client ignores — and `text` returns the bare transcript.
async fn audio_transcriptions(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    audio_job(state, headers, multipart, false).await
}

/// `POST /v1/audio/translations`: OpenAI's speech-to-English endpoint.
/// The same upload as `/v1/audio/transcriptions`, but the transcript
/// comes back in English whatever the source language — so it takes no
/// `language` field, and every operator serving a speech model can do it
/// (the whisper backend's built-in translate mode). Same paid,
/// re-verified job and same `{"text": …}` / `text` response shape.
async fn audio_translations(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    audio_job(state, headers, multipart, true).await
}

/// The refusal for a `timestamp_granularities` value this network can't
/// honor, or `None` for one it can. The transcript carries segment-level
/// timings only, so `segment` (and a blank) pass; `word` is a distinct
/// output shape the segment-only path can't produce, refused rather than
/// answered with segment timings the buyer did not ask for.
fn timestamp_granularity_refusal(value: &str) -> Option<&'static str> {
    let want = value.trim();
    (!want.is_empty() && !want.eq_ignore_ascii_case("segment")).then_some(
        "timestamp_granularities other than 'segment' is not supported: this network returns \
         segment-level timings only; omit timestamp_granularities or set it to segment",
    )
}

/// The shared body of both audio endpoints: read the multipart upload,
/// price and reserve, dispatch one [`JobKind::Transcription`] job, and
/// shape the verified transcript for the client. `translate` is the only
/// difference — set, the job asks the operator for an English rendering
/// (`/v1/audio/translations`); clear, a transcription in the source
/// language (`/v1/audio/transcriptions`).
async fn audio_job(
    state: Arc<OpenAiState>,
    headers: HeaderMap,
    mut multipart: Multipart,
    translate: bool,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };

    let mut audio: Option<Vec<u8>> = None;
    let mut format_hint: Option<String> = None;
    let mut model = String::new();
    let mut language: Option<String> = None;
    let mut response_format = TranscriptFormat::Json;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    format!("could not read the multipart upload: {e}"),
                )
            }
        };
        let name = field.name().map(str::to_string);
        let file_name = field.file_name().map(str::to_string);
        match name.as_deref() {
            Some("file") => {
                format_hint = file_name
                    .as_deref()
                    .and_then(|n| std::path::Path::new(n).extension())
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase());
                match field.bytes().await {
                    Ok(bytes) => audio = Some(bytes.to_vec()),
                    Err(e) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            format!("could not read the audio file: {e}"),
                        )
                    }
                }
            }
            Some("model") => {
                if let Ok(text) = field.text().await {
                    if !text.trim().is_empty() {
                        model = text.trim().to_string();
                    }
                }
            }
            Some("language") => {
                if let Ok(text) = field.text().await {
                    let text = text.trim();
                    // OpenAI auto-detects when language is omitted; treat a
                    // blank or explicit "auto" the same way.
                    if !text.is_empty() && !text.eq_ignore_ascii_case("auto") {
                        language = Some(text.to_string());
                    }
                }
            }
            Some("response_format") => {
                if let Ok(text) = field.text().await {
                    match TranscriptFormat::parse(&text) {
                        Ok(fmt) => response_format = fmt,
                        Err(msg) => {
                            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg)
                        }
                    }
                }
            }
            Some("prompt") => {
                if let Ok(text) = field.text().await {
                    if !text.trim().is_empty() {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            "prompt is not supported: this network transcribes the audio as \
                             spoken and can't bias the transcript toward a prompt's wording; \
                             omit prompt",
                        );
                    }
                }
            }
            // Word-level timings shape the output the buyer pays for, and this
            // network returns segment-level ones only, so a request for any
            // other granularity is refused rather than answered with segment
            // timings it did not ask for. A `segment` value is what the
            // verbose transcript already carries and passes.
            Some("timestamp_granularities[]" | "timestamp_granularities") => {
                if let Ok(text) = field.text().await {
                    if let Some(msg) = timestamp_granularity_refusal(&text) {
                        return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg);
                    }
                }
            }
            // `temperature` and anything else OpenAI accepts are read and
            // ignored: they are soft decoding knobs the backend runs at its
            // own setting, not buyer content the transcript must reflect. A
            // `prompt`, which would steer the wording, is refused above
            // rather than dropped and billed as if it had been applied.
            _ => {}
        }
    }

    let Some(audio) = audio.filter(|a| !a.is_empty()) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "a non-empty 'file' part with the audio to transcribe is required",
        );
    };
    let audio_base64 = base64::engine::general_purpose::STANDARD.encode(&audio);
    if audio_base64.len() > MAX_AUDIO_B64_BYTES {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "the audio is too large ({} base64 bytes, over the {MAX_AUDIO_B64_BYTES} limit); \
                 send a shorter clip",
                audio_base64.len()
            ),
        );
    }
    if model.is_empty() {
        model = "whisper-1".to_string();
    }

    if let Err(resp) = ensure_model_servable(&state, &model, JobKind::Transcription).await {
        return resp;
    }

    let cap = state.caps.max_price_micro_usdc();
    let price = cheapest_matching_ask(
        &state.http,
        &state.buyer,
        JobKind::Transcription,
        Some(&model),
        None,
        None,
        min_reputation_bps,
    )
    .await
    .ok()
    .flatten()
    .map(|floor| floor.min(cap))
    .unwrap_or(cap);

    let reservation = match state.caps.try_reserve(price) {
        Ok(reservation) => reservation,
        Err(msg) => {
            let (status, kind) = if msg.contains("per-call ceiling") {
                (StatusCode::BAD_REQUEST, "invalid_request_error")
            } else {
                (StatusCode::TOO_MANY_REQUESTS, "insufficient_quota")
            };
            return api_error(status, kind, msg);
        }
    };

    let request = JobRequest {
        kind: JobKind::Transcription,
        input: transcription_input(TranscriptionInput {
            audio_base64,
            format: format_hint,
            // Translation always renders English, so the source language
            // is the model's to detect — /v1/audio/translations carries
            // none, matching OpenAI, even if a client sent one.
            language: if translate { None } else { language },
            translate,
            timestamps: response_format.needs_segments(),
        }),
        model: Some(model.clone()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms: state.default_deadline_ms,
    };

    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => match transcription_response(&outcome, response_format, translate) {
            Ok(response) => response,
            // The job settled, but the operator's output was not
            // transcription-shaped — an upstream fault, a bad gateway the
            // way a failed re-verification is.
            Err(msg) => api_error(StatusCode::BAD_GATEWAY, "server_error", msg),
        },
        Err(response) => response,
    }
}

/// The verified transcription shaped for the client: bare text for
/// `response_format=text`, the OpenAI `{"text": …}` object with the extra
/// `covenant` receipt for `json`, the segmented `verbose_json` object, or
/// the `srt`/`vtt` subtitle text. A timestamped format the operator
/// answered without segment timings is a bad gateway, the way an
/// un-transcription-shaped output is: the client asked for timings the
/// matched operator did not provide.
fn transcription_response(
    outcome: &DispatchOutcome,
    format: TranscriptFormat,
    translate: bool,
) -> Result<Response, String> {
    let result = parse_transcription_output(&outcome.output)
        .map_err(|e| format!("the operator returned no usable transcript: {e}"))?;
    let plain_text = || {
        (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            result.transcript.clone(),
        )
            .into_response()
    };
    match format {
        TranscriptFormat::Text => Ok(plain_text()),
        TranscriptFormat::Json => Ok(Json(json!({
            "text": result.transcript,
            "covenant": covenant_receipt(outcome),
        }))
        .into_response()),
        TranscriptFormat::VerboseJson => {
            let segments = require_segments(&result)?;
            Ok(Json(verbose_json_body(&result, segments, translate, outcome)).into_response())
        }
        TranscriptFormat::Srt => {
            let segments = require_segments(&result)?;
            Ok((
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                srt_body(segments),
            )
                .into_response())
        }
        TranscriptFormat::Vtt => {
            let segments = require_segments(&result)?;
            Ok((
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                vtt_body(segments),
            )
                .into_response())
        }
    }
}

/// The segment timings a timestamped response needs, or a clear error when
/// the operator returned none.
fn require_segments(result: &TranscriptionResult) -> Result<&[TranscriptionSegment], String> {
    match result.segments.as_deref() {
        Some(segments) if !segments.is_empty() => Ok(segments),
        _ => Err(
            "the operator returned no segment timings for a timestamped \
                  response_format"
                .to_string(),
        ),
    }
}

/// OpenAI's `verbose_json`: the transcript plus per-segment timings. The
/// network carries integer milliseconds; OpenAI's shape is float seconds,
/// so the conversion happens here, at the edge, off the settlement path.
/// Only the fields the network can stand behind are filled — id, start,
/// end, text — not a backend's internal decoder telemetry.
fn verbose_json_body(
    result: &TranscriptionResult,
    segments: &[TranscriptionSegment],
    translate: bool,
    outcome: &DispatchOutcome,
) -> Value {
    let duration = segments.iter().map(|s| s.end_ms).max().unwrap_or(0) as f64 / 1000.0;
    let mut body = json!({
        "task": if translate { "translate" } else { "transcribe" },
        "duration": duration,
        "text": result.transcript,
        "segments": verbose_segments(segments),
        "covenant": covenant_receipt(outcome),
    });
    if let Some(language) = &result.language {
        body["language"] = json!(language);
    }
    body
}

/// The `verbose_json` segment array: each timing in OpenAI's float
/// seconds, indexed from zero, carrying only the fields the network can
/// stand behind (id, start, end, text).
fn verbose_segments(segments: &[TranscriptionSegment]) -> Vec<Value> {
    segments
        .iter()
        .enumerate()
        .map(|(id, s)| {
            json!({
                "id": id,
                "start": s.start_ms as f64 / 1000.0,
                "end": s.end_ms as f64 / 1000.0,
                "text": s.text,
            })
        })
        .collect()
}

/// The transcript as SubRip (`.srt`): one numbered cue per segment, with
/// `HH:MM:SS,mmm` timings.
fn srt_body(segments: &[TranscriptionSegment]) -> String {
    let mut out = String::new();
    for (i, s) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            timestamp(s.start_ms, ','),
            timestamp(s.end_ms, ','),
            s.text,
        ));
    }
    out
}

/// The transcript as WebVTT (`.vtt`): the `WEBVTT` header, then one cue per
/// segment with `HH:MM:SS.mmm` timings.
fn vtt_body(segments: &[TranscriptionSegment]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for s in segments {
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            timestamp(s.start_ms, '.'),
            timestamp(s.end_ms, '.'),
            s.text,
        ));
    }
    out
}

/// `HH:MM:SS<sep>mmm` — a subtitle cue timestamp. SubRip separates the
/// milliseconds with a comma, WebVTT with a dot.
fn timestamp(ms: u64, sep: char) -> String {
    let (h, m, s, milli) = (
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000,
    );
    format!("{h:02}:{m:02}:{s:02}{sep}{milli:03}")
}

/// OpenAI's own stock voice names. The network's operators synthesize with
/// their backend's voices (`say`'s system voices, a Linux engine's), not
/// these, so a request naming a stock OpenAI voice speaks in the operator's
/// default rather than failing — a plain OpenAI client works unchanged, and
/// a backend-aware client can still name a real voice.
const OPENAI_BUILTIN_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "fable", "onyx", "nova", "sage", "shimmer", "verse",
];

#[derive(Debug, Clone, Deserialize)]
struct SpeechRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    input: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    response_format: Option<String>,
    #[serde(default)]
    speed: Option<f32>,
    /// Free-text voice direction (`gpt-4o-mini-tts`'s tone, emotion, and
    /// delivery control). This network's synthesizers voice text with a
    /// named voice at a set speed and take no free-text steering, so a
    /// request that carries `instructions` earns a refusal rather than a
    /// clip that ignored them and was billed as if it had followed them.
    #[serde(default)]
    instructions: Option<String>,
    /// How to deliver the audio: `audio` (the default) returns the finished
    /// clip as one body, `sse` streams it as Server-Sent Events. This endpoint
    /// returns the whole clip, so an `sse` request is refused rather than
    /// answered with raw bytes a client parsing an event stream can't read.
    #[serde(default)]
    stream_format: Option<String>,
}

/// Resolves the request's `voice` to what the network sends an operator: a
/// stock OpenAI voice — or none — becomes the node's default; any other
/// name passes through for a backend that knows it.
fn resolve_speech_voice(voice: Option<String>) -> Option<String> {
    let voice = voice
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    if OPENAI_BUILTIN_VOICES
        .iter()
        .any(|v| v.eq_ignore_ascii_case(&voice))
    {
        None
    } else {
        Some(voice)
    }
}

/// The container to synthesize, normalized from OpenAI's `response_format`.
/// The network produces WAV (the default and the form every client decodes)
/// and AIFF; a request for a container no operator can produce — OpenAI's
/// mp3/opus/aac/flac/pcm — is refused up front with a clear message, not
/// dispatched as a job doomed to fault, and never answered with WAV bytes
/// under an mp3 label.
fn resolve_speech_format(requested: Option<&str>) -> Result<&'static str, String> {
    match requested.map(|f| f.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("wav") | Some("wave") => Ok("wav"),
        Some("aiff") | Some("aif") => Ok("aiff"),
        Some(other) => Err(format!(
            "response_format {other:?} is not supported; this network produces 'wav' or 'aiff'"
        )),
    }
}

/// The refusal for a `stream_format` this endpoint can't deliver, or `None`
/// for one it can. The clip is returned as one audio body, so `audio` (and a
/// blank) pass; `sse` asks for an event stream this path never produces and is
/// refused rather than answered with raw bytes a client reading events can't
/// parse.
fn speech_stream_format_refusal(value: &str) -> Option<&'static str> {
    let want = value.trim();
    (!want.is_empty() && !want.eq_ignore_ascii_case("audio")).then_some(
        "stream_format other than 'audio' is not supported: this endpoint returns the finished \
         clip as one audio body, not an event stream; omit stream_format or set it to audio",
    )
}

/// The audio media type for a produced container, so the client decodes the
/// body without sniffing.
fn speech_content_type(format: &str) -> &'static str {
    match format {
        "aiff" => "audio/aiff",
        _ => "audio/wav",
    }
}

/// `POST /v1/audio/speech`: OpenAI's text-to-speech endpoint. A JSON body
/// with the `input` text (and an optional `voice`, `response_format`, and
/// `speed`) buys one [`JobKind::SpeechSynthesis`] job on the network, paid
/// and re-verified like every other buy. The response body is the raw audio
/// the operator synthesized — exactly OpenAI's shape — with the verified
/// `covenant` receipt carried in an `x-covenant-receipt` header a plain
/// client ignores.
async fn audio_speech(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    let req: SpeechRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("could not parse the request body: {e}"),
            )
        }
    };

    let text = req.input.trim();
    if text.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the 'input' text to speak is required",
        );
    }
    if text.chars().count() > MAX_SPEECH_TEXT_CHARS {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "the input is {} characters, over the {MAX_SPEECH_TEXT_CHARS} limit; send shorter text",
                text.chars().count()
            ),
        );
    }
    if let Some(speed) = req.speed {
        if !(MIN_SPEECH_SPEED..=MAX_SPEECH_SPEED).contains(&speed) {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("speed {speed} is outside {MIN_SPEECH_SPEED}..={MAX_SPEECH_SPEED}"),
            );
        }
    }
    let format = match resolve_speech_format(req.response_format.as_deref()) {
        Ok(format) => format,
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg),
    };
    if req
        .instructions
        .as_ref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "instructions is not supported: this network's synthesizers voice text with a \
             named voice and speed and take no free-text voice direction; omit instructions",
        );
    }
    if let Some(msg) = req
        .stream_format
        .as_deref()
        .and_then(speech_stream_format_refusal)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg);
    }

    let model = if req.model.trim().is_empty() {
        "say-1".to_string()
    } else {
        req.model.trim().to_string()
    };
    if let Err(resp) = ensure_model_servable(&state, &model, JobKind::SpeechSynthesis).await {
        return resp;
    }

    let cap = state.caps.max_price_micro_usdc();
    let price = cheapest_matching_ask(
        &state.http,
        &state.buyer,
        JobKind::SpeechSynthesis,
        Some(&model),
        None,
        None,
        min_reputation_bps,
    )
    .await
    .ok()
    .flatten()
    .map(|floor| floor.min(cap))
    .unwrap_or(cap);

    let reservation = match state.caps.try_reserve(price) {
        Ok(reservation) => reservation,
        Err(msg) => return reservation_refusal(msg),
    };

    let request = JobRequest {
        kind: JobKind::SpeechSynthesis,
        input: speech_input(SpeechInput {
            text: text.to_string(),
            voice: resolve_speech_voice(req.voice),
            format: Some(format.to_string()),
            speed: req.speed,
        }),
        model: Some(model),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps,
        price_micro_usdc: price,
        deadline_ms: state.default_deadline_ms,
    };

    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => match speech_response(&outcome) {
            Ok(response) => response,
            Err(msg) => api_error(StatusCode::BAD_GATEWAY, "server_error", msg),
        },
        Err(response) => response,
    }
}

/// The verified synthesis shaped for the client: the raw audio bytes with
/// the container's media type, plus the `covenant` receipt in a header. An
/// operator whose output was not speech-shaped, or whose audio will not
/// base64-decode, is a bad gateway the way a failed re-verification is.
fn speech_response(outcome: &DispatchOutcome) -> Result<Response, String> {
    let result = parse_speech_output(&outcome.output)
        .map_err(|e| format!("the operator returned no usable audio: {e}"))?;
    let audio = base64::engine::general_purpose::STANDARD
        .decode(result.audio_base64.as_bytes())
        .map_err(|e| format!("the operator's audio is not valid base64: {e}"))?;
    if audio.is_empty() {
        return Err("the operator returned an empty clip".into());
    }
    let mut response = audio.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(speech_content_type(&result.format)),
    );
    if let Ok(value) = HeaderValue::from_str(&covenant_receipt(outcome).to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-covenant-receipt"), value);
    }
    Ok(response)
}

/// The chat, embedding, transcription, and speech model ids the network is
/// serving right now, sorted and de-duplicated — one entry per named model
/// behind any endpoint a client can call with a `model`, so a client that
/// buys speech at `/v1/audio/speech` can also discover the voice model here.
/// Generic `any`-serving nodes are excluded — they advertise no concrete id
/// a client could name as `model`.
async fn served_models(state: &OpenAiState) -> Result<Vec<String>, Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    Ok(model_ids_in(&view))
}

/// The concrete model ids in `view` a client can name as `model`, across
/// every named-model endpoint — chat, embeddings, transcription, and speech
/// — sorted and de-duplicated. Kinds without a client-named model are left
/// out: batch `run` and lease sessions name no model, and wildcard (`any`)
/// nodes advertise no concrete id to list.
fn model_ids_in(view: &CapacityView) -> Vec<String> {
    let mut ids: Vec<String> = view
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                JobKind::InferenceCall
                    | JobKind::Embedding
                    | JobKind::Transcription
                    | JobKind::SpeechSynthesis
            ) && e.model != "any"
        })
        .map(|e| e.model.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// One OpenAI `model` object.
fn model_object(id: &str) -> Value {
    json!({ "id": id, "object": "model", "created": 0, "owned_by": "covenant-compute" })
}

async fn list_models(State(state): State<Arc<OpenAiState>>, headers: HeaderMap) -> Response {
    // Both dialects answer `/v1/models` on this one shared address; an
    // Anthropic client gets the Anthropic list shape, not the OpenAI one.
    if crate::anthropic::wants_anthropic_dialect(&headers) {
        return crate::anthropic::list_models(&state, &headers).await;
    }
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let ids = match served_models(&state).await {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let data: Vec<Value> = ids.iter().map(|id| model_object(id)).collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

/// The clean 404 an OpenAI SDK expects for a model no operator is serving
/// — the shape both `models.retrieve` and the completion endpoints return,
/// in place of a bare framework 404 or a bad gateway carrying the
/// coordinator's internal job id.
fn model_not_found_message(model: &str) -> String {
    format!("The model '{model}' does not exist or is not being served right now")
}

fn model_not_found(model: &str) -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "invalid_request_error",
        model_not_found_message(model),
    )
}

/// Confirms some operator can serve `model` for `kind` before a paid
/// dispatch: a concrete node advertises it, or a wildcard (`any`) node
/// takes any model. A model the network is not serving is refused up front
/// with the SDK-correct 404 from [`model_not_found`], sparing the caller a
/// doomed submit that would otherwise surface as a bad gateway naming an
/// internal job. Price, GPU and reputation are not judged here: a served
/// model no operator will take at the offered terms is a different
/// condition than a model that does not exist, and only the latter is a
/// 404.
pub(crate) async fn ensure_model_servable(
    state: &OpenAiState,
    model: &str,
    kind: JobKind,
) -> Result<(), Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    if model_is_served(&view, model, kind) {
        Ok(())
    } else {
        Err(model_not_found(model))
    }
}

/// Whether `view` shows an operator that could serve `model` for `kind`:
/// a concrete advertisement of it, or a wildcard (`any`) node that takes
/// whatever a job names. The match mirrors the coordinator's matcher
/// exactly — kind-exact, and model compared on its canonical form so a
/// `:latest` request still resolves — and ignores price, GPU and
/// reputation, which gate the match downstream, not the model's existence.
pub(crate) fn model_is_served(view: &CapacityView, model: &str, kind: JobKind) -> bool {
    let wanted = canonical_model(model);
    view.entries
        .iter()
        .any(|e| e.kind == kind && (e.model == "any" || canonical_model(&e.model) == wanted))
}

/// `GET /v1/models/{model}` — the retrieve half of the models API. Returns
/// the model object when the network is serving it, else a 404 in OpenAI's
/// error shape (what the SDKs expect from `models.retrieve`), rather than a
/// bare framework 404.
async fn retrieve_model(
    State(state): State<Arc<OpenAiState>>,
    axum::extract::Path(model): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    // Shared path, per-request shape: an Anthropic client gets the Anthropic
    // model object (or its `not_found_error`), not the OpenAI one.
    if crate::anthropic::wants_anthropic_dialect(&headers) {
        return crate::anthropic::retrieve_model(&state, &model, &headers).await;
    }
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let ids = match served_models(&state).await {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    if ids.iter().any(|id| id == &model) {
        Json(model_object(&model)).into_response()
    } else {
        model_not_found(&model)
    }
}

/// The OpenAI `finish_reason` for a completed job: the backend's own
/// reason when it reported one (`"length"` for a `max_tokens` cut-off),
/// else `"stop"` — the default a plain client expects.
pub(crate) fn openai_finish_reason(meter: &JobMeter) -> &'static str {
    meter.finish_reason.map(|r| r.as_openai()).unwrap_or("stop")
}

/// The attested token log probabilities shaped into OpenAI's
/// `choices[].logprobs` object. `bytes` renders as `null` when the
/// backend omitted it and `top_logprobs` is always an array (empty when
/// none were asked for) — the shape the OpenAI SDKs decode into a
/// non-optional list.
fn openai_logprobs(tokens: &[TokenLogprob]) -> Value {
    let content: Vec<Value> = tokens
        .iter()
        .map(|t| {
            json!({
                "token": t.token,
                "logprob": t.logprob,
                "bytes": t.bytes,
                "top_logprobs": t.top_logprobs.iter().map(|a| json!({
                    "token": a.token,
                    "logprob": a.logprob,
                    "bytes": a.bytes,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "content": content })
}

/// The `covenant` extension every response carries: the operator's
/// signed, locally re-verified receipt (job id, operator, result hash),
/// what the job charged, and the payout that honored it. A plain OpenAI
/// client ignores it; a Covenant-aware one holds the job to its on-chain
/// money trail.
///
/// `price_micro_usdc` is the escrowed envelope price the coordinator
/// releases — what the buyer actually pays — not the receipt's own signed
/// figure, which `verify_receipt` accepts at or below the offer. Reporting
/// the receipt price would let a low-signing operator make a Covenant-aware
/// client under-report its real cost, and it would contradict the on-chain
/// `payout` sitting beside it, which carries the real figure.
pub(crate) fn covenant_receipt(outcome: &DispatchOutcome) -> Value {
    let receipt = &outcome.receipt.receipt;
    json!({
        "job_id": receipt.job_id,
        "operator_pubkey_b58": receipt.operator.pubkey_base58(),
        "price_micro_usdc": outcome.envelope.payload.price_micro_usdc,
        "result_hash_hex": receipt.result_hash_hex,
        "wall_ms": receipt.meter.wall_ms,
        "receipt_verified": true,
        "payout": outcome.payout,
    })
}

/// One assistant choice at `index`, plus its metered prompt/completion
/// token counts. Shared by the single- and multi-completion builders so a
/// choice means exactly the same thing whichever path shaped it.
fn chat_choice(index: usize, outcome: &DispatchOutcome) -> (Value, u64, u64) {
    let receipt = &outcome.receipt.receipt;
    let reply = parse_assistant_output(&outcome.output);
    // OpenAI shape: a tool-call turn carries `tool_calls` and a `content`
    // that is null when there is no prose; an ordinary turn is just
    // `content`.
    let message = if reply.tool_calls.is_empty() {
        json!({ "role": "assistant", "content": reply.text })
    } else {
        json!({
            "role": "assistant",
            "content": if reply.text.is_empty() { Value::Null } else { Value::String(reply.text) },
            "tool_calls": reply.tool_calls,
        })
    };
    let choice = json!({
        "index": index,
        "message": message,
        "logprobs": reply.logprobs.as_deref().map(openai_logprobs),
        "finish_reason": openai_finish_reason(&receipt.meter),
    });
    (
        choice,
        receipt.meter.tokens_in.unwrap_or(0),
        receipt.meter.tokens_out.unwrap_or(0),
    )
}

/// The signed, re-verified receipt shaped into a standard OpenAI
/// `chat.completion`, plus a `covenant` field carrying the proof an
/// OpenAI client ignores and a Covenant-aware one verifies on-chain.
fn chat_completion_response(model: &str, outcome: &DispatchOutcome) -> Value {
    let (choice, prompt_tokens, completion_tokens) = chat_choice(0, outcome);
    json!({
        "id": format!("chatcmpl-{}", outcome.receipt.receipt.job_id.simple()),
        "object": "chat.completion",
        "created": epoch_secs(),
        "model": model,
        "choices": [choice],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        },
        "covenant": covenant_receipt(outcome),
    })
}

/// The `n > 1` counterpart: one `chat.completion` carrying every completion
/// that settled as its own indexed choice, `usage` summed across them, and
/// a `covenant.receipts` array with one verified receipt per choice — each
/// a distinct paid job on the network. The batch takes the first job's id.
fn chat_completion_response_multi(model: &str, outcomes: &[DispatchOutcome]) -> Value {
    let mut choices = Vec::with_capacity(outcomes.len());
    let mut receipts = Vec::with_capacity(outcomes.len());
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    for (index, outcome) in outcomes.iter().enumerate() {
        let (choice, prompt, completion) = chat_choice(index, outcome);
        choices.push(choice);
        receipts.push(covenant_receipt(outcome));
        prompt_tokens = prompt_tokens.saturating_add(prompt);
        completion_tokens = completion_tokens.saturating_add(completion);
    }
    json!({
        "id": format!("chatcmpl-{}", outcomes[0].receipt.receipt.job_id.simple()),
        "object": "chat.completion",
        "created": epoch_secs(),
        "model": model,
        "choices": choices,
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        },
        "covenant": { "receipts": receipts },
    })
}

/// Maps a dispatch failure onto an OpenAI-shaped error with a fitting
/// status: a 402 for an underfunded buyer, a 5xx for a coordinator or
/// operator that could not deliver.
pub(crate) fn map_dispatch_error(e: BuyerError) -> Response {
    let (status, kind) = match &e {
        _ if e.is_underfunded() => (StatusCode::PAYMENT_REQUIRED, "insufficient_quota"),
        BuyerError::ReceiptTimeout(_) => (StatusCode::GATEWAY_TIMEOUT, "server_error"),
        BuyerError::Rpc(_) | BuyerError::Protocol(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "server_error")
        }
        // The coordinator was reached but refused or could not serve, the
        // operator failed, or a commitment did not re-verify: upstream
        // faults, surfaced as a bad gateway.
        BuyerError::SubmitRefused { .. }
        | BuyerError::NotServed { .. }
        | BuyerError::Coordinator(_)
        | BuyerError::Unreachable { .. }
        | BuyerError::Verification(_) => (StatusCode::BAD_GATEWAY, "server_error"),
    };
    api_error(status, kind, e.to_string())
}

/// `None` when the request may proceed; `Some(response)` is the
/// ready-to-return 401 when a configured key is missing or wrong.
pub(crate) fn authorize(state: &OpenAiState, headers: &HeaderMap) -> Option<Response> {
    let expected = state.api_key.as_deref()?;
    let presented = bearer_token(headers).unwrap_or_default();
    if presented.as_bytes().ct_eq(expected.as_bytes()).into() {
        None
    } else {
        Some(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            "missing or invalid Authorization: Bearer <api key>",
        ))
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// The optional `x-covenant-min-reputation-bps` request header — a
/// Covenant extension that confines the completion to operators rated at
/// least this many basis points (`8000` = 80%). A plain OpenAI client
/// omits it and places no floor; a bad value is a client error, refused
/// before any operator is picked.
pub(crate) fn reputation_floor(headers: &HeaderMap) -> Result<Option<u32>, String> {
    const HEADER: &str = "x-covenant-min-reputation-bps";
    let Some(value) = headers.get(HEADER) else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| format!("{HEADER} must be a whole number of basis points"))?
        .trim();
    let bps: u32 = text
        .parse()
        .map_err(|_| format!("{HEADER} must be a whole number of basis points, got {text:?}"))?;
    match bps {
        0 => Err(format!(
            "{HEADER} is 0 — omit the header for no floor, or set a real basis-point floor \
             (8000 = 80%)"
        )),
        b if b > 10_000 => Err(format!("{HEADER} {b} is above the 10000 maximum (100%)")),
        b => Ok(Some(b)),
    }
}

pub(crate) fn api_error(
    status: StatusCode,
    kind: &'static str,
    message: impl Into<String>,
) -> Response {
    let body = json!({
        "error": { "message": message.into(), "type": kind, "param": null, "code": null }
    });
    (status, Json(body)).into_response()
}

pub(crate) fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(api_key: Option<&str>) -> OpenAiState {
        OpenAiState {
            http: reqwest::Client::new(),
            buyer: BuyerConfig {
                coordinator_url: "http://127.0.0.1:1".into(),
                poll_interval: std::time::Duration::from_millis(10),
                referral_code: None,
                rpc_url: None,
            },
            identity: LocalIdentity::generate("openai@test"),
            caps: Arc::new(SpendCaps::new(1_000_000, None)),
            default_deadline_ms: 1_000,
            api_key: api_key.map(String::from),
        }
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    fn with_reputation(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-covenant-min-reputation-bps", value.parse().unwrap());
        headers
    }

    #[test]
    fn transcript_formats_parse_and_only_timestamped_ones_need_segments() {
        assert_eq!(TranscriptFormat::parse("").unwrap(), TranscriptFormat::Json);
        assert_eq!(
            TranscriptFormat::parse("json").unwrap(),
            TranscriptFormat::Json
        );
        assert_eq!(
            TranscriptFormat::parse(" text ").unwrap(),
            TranscriptFormat::Text
        );
        assert_eq!(
            TranscriptFormat::parse("verbose_json").unwrap(),
            TranscriptFormat::VerboseJson
        );
        assert_eq!(
            TranscriptFormat::parse("srt").unwrap(),
            TranscriptFormat::Srt
        );
        assert_eq!(
            TranscriptFormat::parse("vtt").unwrap(),
            TranscriptFormat::Vtt
        );
        let err = TranscriptFormat::parse("yaml").unwrap_err();
        assert!(err.contains("verbose_json"), "{err}");

        for plain in [TranscriptFormat::Json, TranscriptFormat::Text] {
            assert!(!plain.needs_segments());
        }
        for timed in [
            TranscriptFormat::VerboseJson,
            TranscriptFormat::Srt,
            TranscriptFormat::Vtt,
        ] {
            assert!(timed.needs_segments());
        }
    }

    #[test]
    fn cue_timestamps_render_in_srt_and_vtt_spelling() {
        // 1h 2m 3.456s — SubRip separates milliseconds with a comma, WebVTT
        // with a dot.
        let ms = 3_723_456;
        assert_eq!(timestamp(ms, ','), "01:02:03,456");
        assert_eq!(timestamp(ms, '.'), "01:02:03.456");
        assert_eq!(timestamp(0, '.'), "00:00:00.000");
    }

    fn sample_segments() -> Vec<TranscriptionSegment> {
        vec![
            TranscriptionSegment {
                start_ms: 0,
                end_ms: 1_060,
                text: "Covenant".into(),
            },
            TranscriptionSegment {
                start_ms: 1_060,
                end_ms: 3_200,
                text: "Compute".into(),
            },
        ]
    }

    #[test]
    fn srt_body_numbers_each_cue() {
        let srt = srt_body(&sample_segments());
        assert_eq!(
            srt,
            "1\n00:00:00,000 --> 00:00:01,060\nCovenant\n\n\
             2\n00:00:01,060 --> 00:00:03,200\nCompute\n\n"
        );
    }

    #[test]
    fn vtt_body_leads_with_the_webvtt_header() {
        let vtt = vtt_body(&sample_segments());
        assert_eq!(
            vtt,
            "WEBVTT\n\n\
             00:00:00.000 --> 00:00:01.060\nCovenant\n\n\
             00:00:01.060 --> 00:00:03.200\nCompute\n\n"
        );
    }

    #[test]
    fn verbose_segments_are_zero_indexed_float_seconds() {
        let rendered = verbose_segments(&sample_segments());
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0]["id"], 0);
        assert_eq!(rendered[0]["start"], 0.0);
        assert_eq!(rendered[0]["end"], 1.06);
        assert_eq!(rendered[0]["text"], "Covenant");
        assert_eq!(rendered[1]["id"], 1);
        assert_eq!(rendered[1]["start"], 1.06);
    }

    #[test]
    fn a_timestamped_format_without_segments_is_refused() {
        let mut result = TranscriptionResult {
            model: "whisper-1".into(),
            transcript: "hi".into(),
            language: None,
            segments: None,
        };
        assert!(
            require_segments(&result).is_err(),
            "absent segments refused"
        );
        result.segments = Some(vec![]);
        assert!(require_segments(&result).is_err(), "empty segments refused");
        result.segments = Some(sample_segments());
        assert!(require_segments(&result).is_ok());
    }

    #[test]
    fn a_word_timestamp_granularity_is_refused_and_segment_passes() {
        // The buyer paid for word-level timings; this network produces only
        // segment-level, so it refuses rather than returning a different shape.
        assert!(timestamp_granularity_refusal("word")
            .unwrap()
            .contains("timestamp_granularities"));
        assert!(timestamp_granularity_refusal("WORD").is_some());
        // What the verbose transcript already carries, and the no-ops, pass.
        assert_eq!(timestamp_granularity_refusal("segment"), None);
        assert_eq!(timestamp_granularity_refusal("  "), None);
        assert_eq!(timestamp_granularity_refusal(""), None);
    }

    #[test]
    fn an_sse_speech_stream_format_is_refused_and_audio_passes() {
        // A client asking for an event stream this endpoint never produces is
        // refused, not handed raw audio bytes it would try to read as events.
        assert!(speech_stream_format_refusal("sse")
            .unwrap()
            .contains("stream_format"));
        assert!(speech_stream_format_refusal("SSE").is_some());
        // The one-body default, and the no-ops, pass.
        assert_eq!(speech_stream_format_refusal("audio"), None);
        assert_eq!(speech_stream_format_refusal("  "), None);
        assert_eq!(speech_stream_format_refusal(""), None);
    }

    #[tokio::test]
    async fn a_streaming_submit_holds_the_reservation_when_the_client_disconnects() {
        // A control coordinator that accepts the submit POST — the point at
        // which escrow is held on the real path — signals its arrival, then
        // never answers. This is the window a client disconnect must not
        // release the reservation in: the buy is live on the coordinator
        // whether or not the client is still reading the stream.
        let arrived = Arc::new(tokio::sync::Notify::new());
        let router = Router::new().route(
            "/federation/jobs",
            post({
                let arrived = Arc::clone(&arrived);
                move || {
                    let arrived = Arc::clone(&arrived);
                    async move {
                        arrived.notify_one();
                        std::future::pending::<()>().await;
                        Json(json!({}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        // A session cap with room for one 5_000 buy and no second one.
        let caps = Arc::new(SpendCaps::new(1_000_000, Some(10_000)));
        let reservation = caps.try_reserve(5_000).unwrap();
        let state = Arc::new(OpenAiState {
            http: reqwest::Client::new(),
            buyer: BuyerConfig {
                coordinator_url: format!("http://{addr}"),
                poll_interval: std::time::Duration::from_millis(10),
                referral_code: None,
                rpc_url: None,
            },
            identity: LocalIdentity::generate("openai@test"),
            caps: Arc::clone(&caps),
            default_deadline_ms: 1_000,
            api_key: None,
        });
        let request = JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("hi")],
            model: Some("m".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 5_000,
            deadline_ms: 1_000,
        };

        // Run the streaming handler as its own task so it can be cancelled
        // the way axum cancels a handler whose client vanished.
        let handle = tokio::spawn(stream_chat(state, "m".into(), request, reservation, false));
        // The submit reached the coordinator: escrow would now be held.
        arrived.notified().await;
        // The client disconnects mid-submit — the request handler is cancelled.
        handle.abort();
        // Let the cancellation (and, on the buggy path, the reservation's
        // release) run before the assertion.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // The reservation must still stand: the submit-owning background
        // task, not the cancelled handler frame, holds it, so the 5_000 is
        // still in flight against the session cap. Were it released — the
        // pre-fix behaviour — a further 5_001 would fit under the 10_000
        // cap; held, it must not.
        assert!(
            caps.session_refusal(5_001, 0).is_some(),
            "a disconnect during submit released the reservation, under-counting real spend"
        );
    }

    #[test]
    fn the_reputation_header_parses_and_rejects_bogus_values() {
        // Absent: no floor, the plain-OpenAI-client default.
        assert_eq!(reputation_floor(&HeaderMap::new()).unwrap(), None);
        // A real floor rides through.
        assert_eq!(
            reputation_floor(&with_reputation("8000")).unwrap(),
            Some(8_000)
        );
        // Zero, over-max and non-numeric are client errors.
        assert!(reputation_floor(&with_reputation("0")).is_err());
        assert!(reputation_floor(&with_reputation("10001")).is_err());
        assert!(reputation_floor(&with_reputation("lots")).is_err());
    }

    #[test]
    fn covenant_receipt_reports_the_charged_envelope_price_not_a_lower_receipt() {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_compute_protocol::{
            CapabilityRequirement, JobEnvelopePayload, SignedJobEnvelope, SignedWorkReceipt,
            WorkReceiptPayload,
        };

        let buyer = LocalIdentity::generate("buyer@test");
        let operator = LocalIdentity::generate("operator@test");
        let job_id = Uuid::new_v4();
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer.agent_id(),
            kind: JobKind::InferenceCall,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::InferenceCall,
                max_duration_secs: 30,
                min_reputation_bps: None,
            },
            input: vec![Content::text("q")],
            price_micro_usdc: 10_000,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "receipt-price"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
        // The operator signs a receipt UNDER the offer — verify_receipt
        // permits at-or-below, but the coordinator still releases the
        // escrowed 10_000, so 10_000 is the buyer's real charge.
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".repeat(32),
                result_hash_hex: "11".repeat(32),
                meter: JobMeter {
                    wall_ms: 5,
                    tokens_in: None,
                    tokens_out: None,
                    gpu_seconds: None,
                    finish_reason: None,
                },
                price_micro_usdc: 1,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 2,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();
        let outcome = crate::DispatchOutcome {
            envelope,
            receipt,
            output: vec![Content::text("answer")],
            payout: None,
        };
        assert_eq!(
            covenant_receipt(&outcome)["price_micro_usdc"],
            10_000,
            "the covenant extension reports the charged envelope price, not the receipt's claim"
        );
    }

    #[test]
    fn stop_field_accepts_a_string_or_a_list() {
        assert_eq!(
            StopField::One("END".into()).into_vec(),
            vec!["END".to_string()]
        );
        assert_eq!(
            StopField::Many(vec!["a".into(), "b".into()]).into_vec(),
            vec!["a".to_string(), "b".to_string()]
        );
        let parsed: ChatCompletionRequest =
            serde_json::from_value(json!({ "model": "m", "messages": [], "stop": "STOP" }))
                .unwrap();
        assert_eq!(parsed.stop.unwrap().into_vec(), vec!["STOP".to_string()]);
    }

    #[test]
    fn response_format_narrows_openai_shapes_to_the_protocol() {
        let parse = |v| serde_json::from_value::<OpenAiResponseFormat>(v).unwrap();
        // The explicit text default carries no constraint.
        assert_eq!(
            parse(json!({ "type": "text" })).into_protocol().unwrap(),
            None
        );
        // JSON-object mode.
        assert_eq!(
            parse(json!({ "type": "json_object" }))
                .into_protocol()
                .unwrap(),
            Some(ResponseFormat::JsonObject)
        );
        // A json_schema in OpenAI's nested shape unwraps to the flat
        // protocol type, keeping name, schema and strict.
        assert_eq!(
            parse(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "weather",
                    "schema": { "type": "object" },
                    "strict": true
                }
            }))
            .into_protocol()
            .unwrap(),
            Some(ResponseFormat::JsonSchema {
                name: "weather".into(),
                schema: json!({ "type": "object" }),
                strict: Some(true),
            })
        );
        // A json_schema missing its schema is a clear client error.
        let err = parse(json!({ "type": "json_schema", "json_schema": { "name": "x" } }))
            .into_protocol()
            .expect_err("schema is required");
        assert!(err.contains("requires a schema"), "got: {err}");
    }

    #[test]
    fn a_request_without_response_format_leaves_it_unset() {
        let parsed: ChatCompletionRequest =
            serde_json::from_value(json!({ "model": "m", "messages": [] })).unwrap();
        assert!(parsed.response_format.is_none());
    }

    #[test]
    fn a_message_accepts_string_or_array_content_and_maps_roles() {
        let parse = |v| serde_json::from_value::<OpenAiChatMessage>(v).unwrap();
        // Plain string content, the classic shape.
        assert_eq!(
            parse(json!({ "role": "user", "content": "hi" }))
                .into_protocol()
                .unwrap(),
            ChatMessage::user("hi")
        );
        // Array-of-parts content, what the current SDKs emit by default:
        // the text parts concatenate in order.
        assert_eq!(
            parse(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "one " },
                    { "type": "text", "text": "two" },
                ],
            }))
            .into_protocol()
            .unwrap(),
            ChatMessage::user("one two")
        );
        // The developer role folds to a system instruction.
        assert_eq!(
            parse(json!({ "role": "developer", "content": "be terse" }))
                .into_protocol()
                .unwrap(),
            ChatMessage::system("be terse")
        );
        // A tool-result role maps to the protocol's tool role, carrying the
        // id of the call it answers.
        assert_eq!(
            parse(json!({ "role": "tool", "tool_call_id": "call_0", "content": "42" }))
                .into_protocol()
                .unwrap(),
            ChatMessage::tool("call_0", "42")
        );
        // The legacy function role is still refused (use tool + tool_call_id).
        let fn_err = parse(json!({ "role": "function", "content": "42" }))
            .into_protocol()
            .expect_err("function role unsupported");
        assert!(fn_err.contains("role 'function'"), "got: {fn_err}");
        // An inline base64 image rides through as the message's images,
        // alongside the prompt text.
        let vision = parse(json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "what is this?" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,aGVsbG8=" } },
            ],
        }))
        .into_protocol()
        .expect("inline image accepted");
        assert_eq!(
            vision,
            ChatMessage::user_with_images("what is this?", vec!["aGVsbG8=".into()])
        );
        // A remote image url is refused: the network relays inline bytes,
        // it never fetches a buyer's url.
        let remote_err = parse(json!({
            "role": "user",
            "content": [{ "type": "image_url", "image_url": { "url": "https://example.com/x.png" } }],
        }))
        .into_protocol()
        .expect_err("remote image unsupported");
        assert!(remote_err.contains("remote image url"), "got: {remote_err}");
        // Base64 that doesn't decode is rejected before it can be paid for.
        let bad_b64 = parse(json!({
            "role": "user",
            "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,!!!!" } }],
        }))
        .into_protocol()
        .expect_err("bad base64");
        assert!(bad_b64.contains("valid base64"), "got: {bad_b64}");
    }

    #[test]
    fn a_request_maps_penalties_and_ignores_truly_unknown_fields() {
        // presence_penalty / frequency_penalty are modelled; `user` and
        // the rest a plain client sends must still be understood.
        let parsed: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "qwen2.5:0.5b",
            "messages": [{ "role": "user", "content": "hi" }],
            "presence_penalty": 0.2,
            "frequency_penalty": -0.4,
            "user": "someone",
            "response_format": { "type": "text" },
        }))
        .expect("unknown fields are ignored");
        assert_eq!(parsed.model, "qwen2.5:0.5b");
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.presence_penalty, Some(0.2));
        assert_eq!(parsed.frequency_penalty, Some(-0.4));
    }

    #[test]
    fn completion_prompt_maps_a_string_and_single_array_and_refuses_the_rest() {
        // The classic single-string prompt.
        assert_eq!(
            CompletionPrompt::One("finish this".into())
                .into_text()
                .unwrap(),
            "finish this"
        );
        // An array of exactly one string unwraps to it.
        assert_eq!(
            CompletionPrompt::Many(vec!["only one".into()])
                .into_text()
                .unwrap(),
            "only one"
        );
        // A multi-prompt batch is refused — one prompt per call.
        let batch = CompletionPrompt::Many(vec!["a".into(), "b".into()])
            .into_text()
            .expect_err("multi-prompt is refused");
        assert!(batch.contains("one prompt per call"), "got: {batch}");
        // Pre-tokenized inputs are refused, not silently completed.
        assert!(CompletionPrompt::Tokens(vec![1, 2]).into_text().is_err());
        assert!(CompletionPrompt::TokenBatches(vec![vec![1, 2]])
            .into_text()
            .is_err());
        // Empty and blank prompts are client errors.
        assert!(CompletionPrompt::Many(vec![]).into_text().is_err());
        assert!(CompletionPrompt::One("   ".into()).into_text().is_err());
    }

    #[test]
    fn a_completion_request_refuses_unsupported_fields() {
        let base = json!({ "model": "m", "prompt": "hi" });
        let parse = |v| serde_json::from_value::<CompletionRequest>(v).unwrap();
        // A plain request names nothing unsupported.
        assert_eq!(parse(base.clone()).unsupported(), None);
        // The knobs a chat-backed executor can't honor each earn a refusal.
        let with = |k: &str, val: Value| {
            let mut body = base.as_object().unwrap().clone();
            body.insert(k.into(), val);
            parse(Value::Object(body)).unsupported()
        };
        assert!(with("logprobs", json!(5)).unwrap().contains("logprobs"));
        assert!(with("echo", json!(true)).unwrap().contains("echo"));
        assert!(with("suffix", json!(")")).unwrap().contains("suffix"));
        assert!(with("best_of", json!(2)).unwrap().contains("best_of"));
        assert!(with("logit_bias", json!({ "13": -5 }))
            .unwrap()
            .contains("logit_bias"));
        // best_of=1 is the default and fine; echo=false is fine; an empty
        // logit_bias is the no-op OpenAI treats it as, so it passes.
        assert_eq!(with("best_of", json!(1)), None);
        assert_eq!(with("echo", json!(false)), None);
        assert_eq!(with("logit_bias", json!({})), None);
    }

    #[test]
    fn a_chat_request_models_logit_bias_and_parallel_tool_calls() {
        // Both are declared, so a client sending them is captured (and
        // then refused by the handler) rather than silently dropped and
        // charged for a completion that ignored them.
        let parsed: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "logit_bias": { "13": -5 },
            "parallel_tool_calls": false,
            "reasoning_effort": "high",
            "verbosity": "low",
        }))
        .expect("the fields are modelled");
        assert!(parsed.logit_bias.is_some_and(|m| !m.is_empty()));
        assert_eq!(parsed.parallel_tool_calls, Some(false));
        assert_eq!(parsed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(parsed.verbosity.as_deref(), Some("low"));
    }

    #[test]
    fn a_chat_request_models_the_deprecated_function_calling_fields() {
        // Captured, not dropped: a client still on the legacy functions API is
        // refused by the handler rather than billed for a plain completion that
        // ignored the function it paid to force. The shape differs from `tools`,
        // so it is refused, not carried.
        let parsed: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "weather in Paris?" }],
            "functions": [{ "name": "get_weather", "parameters": { "type": "object" } }],
            "function_call": { "name": "get_weather" },
        }))
        .expect("the deprecated fields are modelled");
        assert!(parsed.functions.is_some());
        assert!(parsed.function_call.is_some());
    }

    #[test]
    fn a_completion_request_parses_the_shared_sampling_knobs() {
        let parsed: CompletionRequest = serde_json::from_value(json!({
            "model": "qwen2.5:0.5b",
            "prompt": "once upon a time",
            "max_tokens": 32,
            "temperature": 0.2,
            "presence_penalty": 0.1,
            "frequency_penalty": -0.2,
            "stop": ["\n\n"],
            "seed": 7,
            "user": "someone",
        }))
        .expect("unknown fields like `user` are ignored");
        assert_eq!(parsed.model, "qwen2.5:0.5b");
        assert_eq!(parsed.max_tokens, Some(32));
        assert_eq!(parsed.temperature, Some(0.2));
        assert_eq!(parsed.presence_penalty, Some(0.1));
        assert_eq!(parsed.seed, Some(7));
        assert_eq!(parsed.stop.unwrap().into_vec(), vec!["\n\n".to_string()]);
    }

    #[test]
    fn embedding_input_maps_text_and_refuses_tokens_and_blanks() {
        assert_eq!(
            EmbeddingInput::Single("hi".into()).into_texts().unwrap(),
            vec!["hi".to_string()]
        );
        assert_eq!(
            EmbeddingInput::Many(vec!["a".into(), "b".into()])
                .into_texts()
                .unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        // Pre-tokenized integer input is refused, not silently embedded.
        assert!(EmbeddingInput::Tokens(vec![1, 2]).into_texts().is_err());
        assert!(EmbeddingInput::TokenBatches(vec![vec![1, 2]])
            .into_texts()
            .is_err());
        // Empty and blank inputs are client errors.
        assert!(EmbeddingInput::Many(vec![]).into_texts().is_err());
        assert!(EmbeddingInput::Single("   ".into()).into_texts().is_err());
        assert!(EmbeddingInput::Many(vec!["ok".into(), " ".into()])
            .into_texts()
            .is_err());
    }

    #[test]
    fn embedding_input_bounds_the_batch() {
        let over = EmbeddingInput::Many(vec!["x".to_string(); MAX_EMBEDDING_INPUTS + 1]);
        let err = over.into_texts().expect_err("past the per-call limit");
        assert!(err.contains("per-call limit"), "got: {err}");
        let at = EmbeddingInput::Many(vec!["x".to_string(); MAX_EMBEDDING_INPUTS]);
        assert!(at.into_texts().is_ok(), "exactly the limit is allowed");
    }

    #[test]
    fn encoding_format_parses_default_and_rejects_unknown() {
        assert_eq!(EncodingFormat::parse(None).unwrap(), EncodingFormat::Float);
        assert_eq!(
            EncodingFormat::parse(Some("float")).unwrap(),
            EncodingFormat::Float
        );
        assert_eq!(
            EncodingFormat::parse(Some("base64")).unwrap(),
            EncodingFormat::Base64
        );
        assert!(EncodingFormat::parse(Some("hex")).is_err());
    }

    #[test]
    fn base64_f32_is_little_endian_and_round_trips() {
        let vector = [0.5_f32, -0.25, 0.125];
        let encoded = base64_f32(&vector);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(decoded, vector);
    }

    #[test]
    fn an_open_endpoint_needs_no_bearer() {
        assert!(authorize(&state(None), &HeaderMap::new()).is_none());
    }

    #[test]
    fn a_keyed_endpoint_refuses_a_missing_or_wrong_bearer() {
        let st = state(Some("sk-secret"));
        assert!(authorize(&st, &HeaderMap::new()).is_some());
        assert!(authorize(&st, &bearer("sk-wrong")).is_some());
        assert!(authorize(&st, &bearer("sk-secret")).is_none());
    }

    #[test]
    fn the_underfunded_error_maps_to_402_insufficient_quota() {
        let resp = map_dispatch_error(BuyerError::SubmitRefused {
            status: 402,
            body: "insufficient funds".into(),
        });
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn an_unreachable_coordinator_maps_to_502() {
        let resp = map_dispatch_error(BuyerError::Unreachable {
            doing: "submit the job",
            url: "http://down".into(),
            why: "refused the connection",
        });
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    fn capacity_of(rows: &[(JobKind, &str)]) -> CapacityView {
        use covenant_compute_protocol::{CapacityEntry, PriceUnit};
        CapacityView {
            registered_operators: rows.len(),
            matchable_operators: rows.len(),
            liveness_window_ms: 60_000,
            min_score_bps: 0,
            min_bond_micro_usdc: 0,
            entries: rows
                .iter()
                .map(|(kind, model)| CapacityEntry {
                    kind: *kind,
                    model: (*model).into(),
                    operators: 1,
                    min_ask_micro_usdc: 1_000,
                    min_ask_unit: PriceUnit::PerJob,
                    max_ask_micro_usdc: 1_000,
                    max_vram_gb: 16,
                    gpu_classes: vec!["cpu".into()],
                    tee_capable: false,
                })
                .collect(),
        }
    }

    #[test]
    fn model_servability_matches_the_matcher_on_concrete_kind_and_wildcard() {
        let view = capacity_of(&[
            (JobKind::InferenceCall, "qwen2.5:0.5b"),
            (JobKind::Embedding, "nomic-embed"),
        ]);
        // A concrete advertisement is served for its own kind only.
        assert!(model_is_served(
            &view,
            "qwen2.5:0.5b",
            JobKind::InferenceCall
        ));
        assert!(model_is_served(&view, "nomic-embed", JobKind::Embedding));
        // A chat model is not thereby an embedding model, or vice versa.
        assert!(!model_is_served(&view, "qwen2.5:0.5b", JobKind::Embedding));
        assert!(!model_is_served(
            &view,
            "nomic-embed",
            JobKind::InferenceCall
        ));
        // A model no row advertises is not served.
        assert!(!model_is_served(
            &view,
            "gpt-4-turbo",
            JobKind::InferenceCall
        ));

        // A wildcard node serves any model a job names, for its kind.
        let wildcard = capacity_of(&[(JobKind::InferenceCall, "any")]);
        assert!(model_is_served(
            &wildcard,
            "gpt-4-turbo",
            JobKind::InferenceCall
        ));
        assert!(!model_is_served(
            &wildcard,
            "gpt-4-turbo",
            JobKind::Embedding
        ));

        // A `:latest` request resolves to the canonical advertisement, the
        // same normalization the matcher applies, so the pre-check never
        // refuses a model the coordinator would serve.
        let canonical = capacity_of(&[(JobKind::InferenceCall, "llama3")]);
        assert!(model_is_served(
            &canonical,
            "llama3:latest",
            JobKind::InferenceCall
        ));
    }

    #[test]
    fn served_models_lists_every_named_model_kind_including_speech() {
        let view = capacity_of(&[
            (JobKind::InferenceCall, "qwen2.5:0.5b"),
            (JobKind::Embedding, "nomic-embed"),
            (JobKind::Transcription, "whisper-base.en"),
            (JobKind::SpeechSynthesis, "say-1"),
            // A wildcard node names no id to list.
            (JobKind::InferenceCall, "any"),
            // A batch node names no client-facing model.
            (JobKind::BatchJob, "any"),
        ]);
        // Speech is discoverable alongside chat, embeddings and transcription
        // — the gap that left a served /v1/audio/speech model invisible to
        // /v1/models. Sorted, de-duplicated, wildcards excluded.
        assert_eq!(
            model_ids_in(&view),
            vec![
                "nomic-embed".to_string(),
                "qwen2.5:0.5b".to_string(),
                "say-1".to_string(),
                "whisper-base.en".to_string(),
            ]
        );
    }

    #[test]
    fn an_unserved_model_is_a_clean_404_naming_no_internals() {
        assert_eq!(
            model_not_found("gpt-4-turbo").status(),
            StatusCode::NOT_FOUND
        );
        // The user-facing message names the model and nothing internal —
        // no job id, coordinator plumbing, or bad-gateway wording, unlike
        // the raw dispatch refusal it replaces.
        let message = model_not_found_message("gpt-4-turbo");
        assert!(message.contains("gpt-4-turbo"));
        assert!(!message.contains("job"));
        assert!(!message.contains("coordinator"));
        assert!(!message.contains("502"));
    }
}
