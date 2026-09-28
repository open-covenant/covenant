//! A Google Gemini-compatible front door for the compute network. A client
//! built for Google's `generateContent` API — the Google GenAI SDKs
//! (`google-genai`, `@google/genai`) pointed at a custom base URL, an agent
//! framework speaking the Gemini dialect, a bare `curl` — reaches the same
//! network the OpenAI and Anthropic front doors serve, and every request is
//! bought on the network, paid, and returned with the operator's signed,
//! locally re-verified work receipt. No client code changes; the buyer's
//! spend stays under the same per-call and session caps every other buyer
//! surface enforces.
//!
//! Text, inline image parts, a system instruction, structured output, tool
//! use, and streaming all carry across: a turn's `inlineData` base64 rides the
//! signed job as vision input, a `responseMimeType`/`responseSchema` constrains
//! the reply to JSON, a `tools`/`toolConfig` request comes back with
//! `functionCall` parts the agent runs and feeds back as `functionResponse`
//! parts, and `streamGenerateContent` returns Google's chunk sequence, so a UI,
//! a multimodal prompt, and an agent's whole tool loop all run end to end.
//!
//! The response is a standard Gemini `GenerateContentResponse`: a `candidates`
//! array, a `usageMetadata` block, and a `modelVersion`. It carries one extra
//! `covenant` field with the verified receipt — a plain Gemini client ignores
//! it, a Covenant-aware one checks the job on-chain. This is the Gemini
//! dialect of the same front door; the shared dispatch, pricing, and receipt
//! machinery live in [`crate::openai`] and [`crate`], and only the request and
//! response wire shapes differ.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use covenant_compute_protocol::{
    parse_assistant_output, AssistantReply, ChatMessage, ChatRole, FunctionCall,
    FunctionDefinition, JobKind, NamedFunction, NamedToolChoice, ResponseFormat, ToolCall,
    ToolCallKind, ToolChoice, ToolChoiceMode, ToolDefinition, ToolKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::openai::{covenant_receipt, model_is_served, reputation_floor, OpenAiState};
use crate::{
    capacity, cheapest_matching_ask, dispatch_and_verify, stream_and_verify, submit_streaming,
    BuyerError, DispatchOutcome, InferArgs, JobRequest, SpendReservation,
};

/// The Gemini front-door router. Google addresses a model and a method in one
/// path segment (`/v1beta/models/<model>:generateContent`), so a single route
/// captures `<model>:<method>` and [`generate_content`] splits it. Kept
/// separate from [`crate::openai::openai_router`] so the binary can serve
/// every dialect on one address (`openai_router(state).merge(anthropic_router
/// (state)).merge(gemini_router(state))`) without either owning the other's
/// routes; the `/v1beta/*` prefix never collides with the OpenAI or Anthropic
/// `/v1/*` routes.
///
/// The model directory (`GET /v1beta/models`, `GET /v1beta/models/<model>`)
/// shares the single-model path with the `POST` generation call, so the two
/// methods register on one route.
pub fn gemini_router(state: Arc<OpenAiState>) -> Router {
    Router::new()
        .route("/v1beta/models", get(list_models))
        .route(
            "/v1beta/models/:model_action",
            get(get_model).post(generate_content),
        )
        .with_state(state)
}

/// The query parameters a Gemini client may attach. The API key rides here
/// (`?key=...`) as an alternative to the `x-goog-api-key` header, exactly as
/// Google's REST surface accepts it. `alt=sse` selects server-sent events for
/// a streaming call — the framing the Google GenAI SDKs use — over the default
/// streamed JSON array.
#[derive(Debug, Deserialize)]
struct GeminiParams {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    alt: Option<String>,
}

/// One `generateContent` request as a Gemini client sends it. The turns ride
/// `contents`, a system prompt rides the top-level `systemInstruction`, and
/// the sampling knobs sit under `generationConfig` — all camelCase, Google's
/// convention.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest {
    #[serde(default)]
    contents: Vec<GeminiContent>,
    #[serde(default)]
    system_instruction: Option<GeminiContent>,
    #[serde(default)]
    generation_config: Option<GenerationConfig>,
    #[serde(default)]
    tools: Vec<GeminiTool>,
    #[serde(default)]
    tool_config: Option<ToolConfig>,
    #[serde(default)]
    safety_settings: Vec<SafetySetting>,
    #[serde(default)]
    cached_content: Option<String>,
}

/// One entry of Gemini's `tools` array. Only the function-declaration form is
/// served. A Gemini `tools` entry is a discriminated union — one of
/// `functionDeclarations`, `googleSearch`, `codeExecution`, `urlContext`, and
/// their kin — so any other key names a tool Google hosts in its own
/// infrastructure. Those keys are captured, not dropped, so a hosted tool is
/// refused whether it stands alone or rides alongside a function tool, rather
/// than the buyer's grounding request vanishing from a served completion.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    #[serde(default)]
    function_declarations: Vec<FunctionDeclaration>,
    #[serde(flatten)]
    hosted: BTreeMap<String, Value>,
}

/// One function the model may call: Gemini's `{name, description, parameters}`,
/// where `parameters` is an OpenAPI-subset JSON schema.
#[derive(Debug, Deserialize)]
struct FunctionDeclaration {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<Value>,
}

/// Gemini's `toolConfig`: how the model may use the offered tools.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolConfig {
    #[serde(default)]
    function_calling_config: Option<FunctionCallingConfig>,
}

/// `functionCallingConfig`: `mode` is `AUTO` (the model decides), `ANY` (it must
/// call a tool), or `NONE` (it may not); `allowedFunctionNames` narrows `ANY` to
/// a named tool.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FunctionCallingConfig {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    allowed_function_names: Option<Vec<String>>,
}

/// One `safetySettings` entry: a harm `category` and the `threshold` the client
/// wants enforced on it. This network relays to an operator's backend and
/// applies none of Gemini's configurable harm-category filters, so the threshold
/// is read to tell a request that turns blocking off (which the network already
/// does) from one that asks it to block at a level it cannot enforce.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SafetySetting {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    threshold: Option<String>,
}

impl SafetySetting {
    /// True when the entry names a threshold that blocks at some level.
    /// `BLOCK_NONE` and `OFF` turn blocking off; `HARM_BLOCK_THRESHOLD_UNSPECIFIED`
    /// or an absent threshold names no level. None of those ask for enforcement
    /// this network lacks.
    fn requests_blocking(&self) -> bool {
        self.threshold.as_deref().is_some_and(|threshold| {
            let threshold = threshold.trim();
            !threshold.is_empty()
                && !threshold.eq_ignore_ascii_case("BLOCK_NONE")
                && !threshold.eq_ignore_ascii_case("OFF")
                && !threshold.eq_ignore_ascii_case("HARM_BLOCK_THRESHOLD_UNSPECIFIED")
        })
    }
}

/// One turn: a role (`user` or `model`) and its ordered parts. A
/// `systemInstruction` reuses this shape with no role.
#[derive(Debug, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

/// One part of a turn: text, an inline base64 image (`inlineData`), a tool call
/// the model made (`functionCall`), or a tool result fed back
/// (`functionResponse`). A remote `fileData` reference deserializes as none of
/// these and earns a clear refusal rather than being silently dropped.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    inline_data: Option<InlineData>,
    #[serde(default)]
    function_call: Option<GeminiFunctionCall>,
    #[serde(default)]
    function_response: Option<GeminiFunctionResponse>,
}

/// A tool call the model made in a prior `model` turn, replayed in history:
/// `{name, args}` where `args` is the object the model produced.
#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

/// A tool's result fed back in a later turn: `{name, response}` where
/// `response` is the object the agent's tool returned. Gemini correlates a
/// result to its call by function name, carrying no call id.
#[derive(Debug, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    #[serde(default)]
    response: Value,
}

/// An inline image part: base64 bytes the request carries directly. Only the
/// `data` is read — the network relays the bytes to the operator's backend,
/// which sniffs the format — so a `mimeType` is accepted and ignored. A remote
/// `fileData` reference is not modelled: the network never reaches out to fetch
/// a buyer's image, matching the SSRF-closed posture of the other doors.
#[derive(Debug, Deserialize)]
struct InlineData {
    #[serde(default)]
    data: String,
}

/// Gemini's `generationConfig`. `topK`, an enabling `thinkingConfig`, and a
/// non-text `responseModalities` are read so a request that sets a control this
/// network can't honor earns a refusal, not a completion quietly sampled
/// without it; `responseMimeType` with either `responseSchema` or
/// `responseJsonSchema` maps to structured output.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<u32>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    seed: Option<i64>,
    #[serde(default)]
    response_logprobs: Option<bool>,
    #[serde(default)]
    logprobs: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    candidate_count: Option<u32>,
    #[serde(default)]
    response_mime_type: Option<String>,
    #[serde(default)]
    response_schema: Option<Value>,
    #[serde(default)]
    response_json_schema: Option<Value>,
    #[serde(default)]
    response_modalities: Option<Vec<String>>,
    #[serde(default)]
    thinking_config: Option<ThinkingConfig>,
}

/// Gemini's `thinkingConfig`: a thinking-token budget and an
/// `includeThoughts` flag. This network's serving path returns a final answer
/// only, with no separate reasoning stream, so a request that enables thinking
/// earns a refusal rather than a completion silently missing the reasoning the
/// buyer asked (and would pay) for. A zero budget with no thought stream is the
/// client opting out and rides through.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThinkingConfig {
    #[serde(default)]
    thinking_budget: Option<i32>,
    #[serde(default)]
    include_thoughts: Option<bool>,
}

impl ThinkingConfig {
    /// `Ok(())` when the client opted out of thinking; `Err(msg)` when it asked
    /// for a reasoning stream this network's serving path does not produce. A
    /// negative budget is Gemini's "dynamic thinking", still an enabling value.
    fn ensure_supported(&self) -> Result<(), String> {
        let wants_thoughts = self.include_thoughts == Some(true);
        let wants_budget = self.thinking_budget.is_some_and(|b| b != 0);
        if !wants_thoughts && !wants_budget {
            return Ok(());
        }
        Err(
            "thinkingConfig is not supported: this network's serving path returns a final \
             answer only, with no separate reasoning stream; omit thinkingConfig or set \
             thinkingBudget to 0"
                .into(),
        )
    }
}

/// The sampling controls a request carries once narrowed to what the network
/// serves.
#[derive(Debug, Default, PartialEq)]
struct NarrowedConfig {
    temperature: Option<f64>,
    top_p: Option<f64>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    seed: Option<i64>,
    logprobs: Option<u32>,
    max_tokens: Option<u32>,
    stop: Option<Vec<String>>,
    response_format: Option<ResponseFormat>,
}

/// Narrows a `generationConfig` to the controls the serving path exposes, or
/// refuses one it cannot honor: `topK` (no top-k on this network) and
/// `candidateCount > 1` (one candidate per paid call). The structured-output
/// controls are mapped by [`parse_response_format`].
fn narrow_config(config: Option<&GenerationConfig>) -> Result<NarrowedConfig, String> {
    let Some(config) = config else {
        return Ok(NarrowedConfig::default());
    };
    if config.top_k.is_some() {
        return Err(
            "topK is not supported: this network's serving path exposes no top-k; \
                    use temperature or topP"
                .into(),
        );
    }
    if config.candidate_count.is_some_and(|n| n > 1) {
        return Err("candidateCount > 1 is not supported: request one candidate per call".into());
    }
    if let Some(thinking) = &config.thinking_config {
        thinking.ensure_supported()?;
    }
    if config.response_modalities.as_deref().is_some_and(|m| {
        m.iter()
            .any(|modality| !modality.eq_ignore_ascii_case("text"))
    }) {
        return Err(
            "responseModalities other than TEXT is not supported: this network's serving path \
             returns text; omit responseModalities or set it to [\"TEXT\"]"
                .into(),
        );
    }
    // Gemini couples the two logprobs knobs the way OpenAI couples
    // `logprobs`/`top_logprobs`: the `logprobs` count is only meaningful with
    // `responseLogprobs: true`. Fold them to the protocol's single knob, whose
    // presence means "on" and whose value is the number of alternatives.
    let logprobs = if config.response_logprobs == Some(true) {
        Some(config.logprobs.unwrap_or(0))
    } else if config.logprobs.is_some() {
        return Err(
            "logprobs requires responseLogprobs to be true: set responseLogprobs or drop logprobs"
                .into(),
        );
    } else {
        None
    };
    Ok(NarrowedConfig {
        temperature: config.temperature,
        top_p: config.top_p,
        presence_penalty: config.presence_penalty,
        frequency_penalty: config.frequency_penalty,
        seed: config.seed,
        logprobs,
        max_tokens: config.max_output_tokens,
        stop: config
            .stop_sequences
            .clone()
            .filter(|sequences| !sequences.is_empty()),
        response_format: parse_response_format(config)?,
    })
}

/// `Ok(())` when no `safetySettings` entry asks this network to block content,
/// `Err(msg)` when one does. The network relays to an operator's backend and
/// applies none of Gemini's harm-category thresholds, so a request that names a
/// real block level is refused rather than served as if the filter were in
/// force; the request would otherwise carry a moderation guarantee the network
/// never delivered. A threshold that turns blocking off (`BLOCK_NONE`, `OFF`) or
/// names no level asks for nothing the network lacks and rides through.
fn ensure_safety_supported(settings: &[SafetySetting]) -> Result<(), String> {
    let Some(blocking) = settings.iter().find(|s| s.requests_blocking()) else {
        return Ok(());
    };
    let category = blocking.category.as_deref().unwrap_or("a harm category");
    let threshold = blocking.threshold.as_deref().unwrap_or_default();
    Err(format!(
        "safetySettings threshold {threshold} for {category} is not supported: this network \
         relays to an operator's backend and applies none of Gemini's harm-category filters; \
         omit safetySettings or set every threshold to BLOCK_NONE or OFF"
    ))
}

/// Maps Gemini's `responseMimeType` and either schema field to the protocol's
/// structured-output control. `application/json` on its own is JSON mode; with
/// a `responseSchema` (the OpenAPI-3.0 subset) or `responseJsonSchema` (full
/// JSON Schema) it constrains the reply to that schema. The two schema fields
/// are alternatives, so setting both is ambiguous and refused. `text/plain` (or
/// an absent mime) is free-form. Any other MIME — Gemini's `text/x.enum` enum
/// mode, an XML request — is refused rather than sampled without the constraint
/// the buyer asked for. Gemini has no schema name; a fixed `response` label is
/// stamped so the protocol's named-schema contract holds.
fn parse_response_format(config: &GenerationConfig) -> Result<Option<ResponseFormat>, String> {
    let (schema, field) = match (&config.response_schema, &config.response_json_schema) {
        (Some(_), Some(_)) => {
            return Err(
                "responseSchema and responseJsonSchema are mutually exclusive: set only one".into(),
            )
        }
        (Some(schema), None) => (Some(schema), "responseSchema"),
        (None, Some(schema)) => (Some(schema), "responseJsonSchema"),
        (None, None) => (None, ""),
    };
    match config.response_mime_type.as_deref().unwrap_or("text/plain") {
        "text/plain" => {
            if schema.is_some() {
                return Err(format!(
                    "{field} requires responseMimeType application/json"
                ));
            }
            Ok(None)
        }
        "application/json" => match schema {
            Some(schema) => Ok(Some(ResponseFormat::JsonSchema {
                name: "response".into(),
                schema: schema.clone(),
                strict: None,
            })),
            None => Ok(Some(ResponseFormat::JsonObject)),
        },
        other => Err(format!(
            "responseMimeType '{other}' is not supported: use application/json for structured \
             output, or omit it for text"
        )),
    }
}

/// Splits a turn's parts into its joined prose and its inline images. A part
/// that is neither text nor `inlineData` (a tool part, a remote `fileData`
/// reference) is refused rather than silently dropped.
fn classify_parts(parts: &[GeminiPart]) -> Result<ClassifiedParts<'_>, String> {
    let mut out = ClassifiedParts::default();
    for part in parts {
        if let Some(chunk) = part.text.as_deref() {
            out.text.push_str(chunk);
        } else if let Some(inline) = &part.inline_data {
            out.images.push(decode_inline_image(inline)?);
        } else if let Some(call) = &part.function_call {
            out.calls.push(call);
        } else if let Some(response) = &part.function_response {
            out.responses.push(response);
        } else {
            return Err("a part must carry text, inlineData, functionCall, or \
                        functionResponse: this door does not accept remote fileData references"
                .into());
        }
    }
    Ok(out)
}

/// A turn's parts split by kind. Holds references into the request so no part
/// is cloned before it is known to be used.
#[derive(Default)]
struct ClassifiedParts<'a> {
    text: String,
    images: Vec<String>,
    calls: Vec<&'a GeminiFunctionCall>,
    responses: Vec<&'a GeminiFunctionResponse>,
}

/// Correlates a replayed tool result to the call it answers. Gemini carries no
/// call id — a `functionResponse` names only the function — so as each
/// `functionCall` is replayed it is assigned a positional id and queued by
/// name, and each `functionResponse` pops the oldest unanswered id for that
/// name. This gives the assistant tool call and its tool result the matching
/// ids the protocol's chat form (and an OpenAI-compatible backend) pair on.
#[derive(Default)]
struct ToolCallCorrelator {
    next: usize,
    pending: HashMap<String, VecDeque<String>>,
}

impl ToolCallCorrelator {
    fn assign(&mut self, call: &GeminiFunctionCall) -> ToolCall {
        let id = self.fresh_id();
        self.pending
            .entry(call.name.clone())
            .or_default()
            .push_back(id.clone());
        ToolCall {
            id,
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: call.name.clone(),
                arguments: call.args.to_string(),
            },
        }
    }

    fn resolve(&mut self, name: &str) -> String {
        self.pending
            .get_mut(name)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| self.fresh_id())
    }

    fn fresh_id(&mut self) -> String {
        let id = format!("call_{}", self.next);
        self.next += 1;
        id
    }
}

/// Pulls the base64 bytes out of an `inlineData` part, rejecting data that does
/// not decode here rather than paying for a job a backend fails. The bytes are
/// relayed as-is, the same base64 form the other doors carry into the signed
/// job.
fn decode_inline_image(inline: &InlineData) -> Result<String, String> {
    let data: String = inline.data.split_whitespace().collect();
    if data.is_empty() {
        return Err("an inlineData part carries no image bytes".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|_| "an inlineData part's data is not valid base64".to_string())?;
    Ok(data)
}

/// Narrows a Gemini request to the protocol's canonical message list. The
/// system instruction becomes a system turn; a `model` content becomes an
/// assistant turn carrying any `functionCall`s the model made; and a `user`
/// content becomes a user turn (with any inline images) plus one tool message
/// per `functionResponse` fed back, so an agent's tool loop replays intact.
/// Gemini names the assistant role `model` and sends tool results on a user
/// turn; an unknown role is refused rather than guessed, and a part on a turn
/// where no backend consumes it (an image on a system turn, a `functionCall` on
/// a user turn) is refused rather than paid for and dropped.
fn build_messages(
    system: Option<&GeminiContent>,
    contents: &[GeminiContent],
) -> Result<Vec<ChatMessage>, String> {
    let mut messages = Vec::with_capacity(contents.len() + 1);
    if let Some(system) = system {
        let parts = classify_parts(&system.parts)?;
        if !parts.images.is_empty() || !parts.calls.is_empty() || !parts.responses.is_empty() {
            return Err("a systemInstruction carries only text".into());
        }
        if !parts.text.is_empty() {
            messages.push(ChatMessage::system(parts.text));
        }
    }
    let mut correlator = ToolCallCorrelator::default();
    for content in contents {
        let parts = classify_parts(&content.parts)?;
        match content.role.as_deref().unwrap_or("user") {
            "model" => {
                if !parts.images.is_empty() {
                    return Err("an image part is only supported on a user turn".into());
                }
                if !parts.responses.is_empty() {
                    return Err(
                        "a functionResponse belongs on a user turn, not a model turn".into(),
                    );
                }
                let tool_calls: Vec<ToolCall> = parts
                    .calls
                    .iter()
                    .map(|call| correlator.assign(call))
                    .collect();
                if tool_calls.is_empty() {
                    messages.push(ChatMessage::assistant(parts.text));
                } else {
                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: parts.text,
                        images: Vec::new(),
                        tool_calls,
                        tool_call_id: None,
                    });
                }
            }
            // Gemini sends tool results on a user turn; some clients use a
            // `function`/`tool` role. All three carry a `functionResponse` the
            // same way.
            "user" | "function" | "tool" => {
                if !parts.calls.is_empty() {
                    return Err("a functionCall belongs on a model turn, not a user turn".into());
                }
                if !parts.text.is_empty() || !parts.images.is_empty() {
                    if parts.images.is_empty() {
                        messages.push(ChatMessage::user(parts.text));
                    } else {
                        messages.push(ChatMessage::user_with_images(parts.text, parts.images));
                    }
                }
                for response in &parts.responses {
                    let id = correlator.resolve(&response.name);
                    messages.push(ChatMessage::tool(id, response.response.to_string()));
                }
            }
            other => {
                return Err(format!(
                    "unknown content role '{other}': use 'user' or 'model'"
                ))
            }
        }
    }
    Ok(messages)
}

/// Flattens Gemini's `tools` (each an object holding `functionDeclarations`)
/// into the protocol's tool list. A `tools` entry carrying any other key names
/// a tool Google hosts (`googleSearch`, `codeExecution`, `urlContext`) that this
/// network does not host, so it is refused — alone or alongside a function tool
/// — rather than served as if the grounding it asked for were in force. An entry
/// with an empty `functionDeclarations` and nothing else offers no callable tool
/// and is likewise refused.
fn parse_tools(tools: &[GeminiTool]) -> Result<Vec<ToolDefinition>, String> {
    if tools.is_empty() {
        return Ok(Vec::new());
    }
    let mut defs = Vec::new();
    for tool in tools {
        if let Some(hosted) = tool.hosted.keys().next() {
            return Err(format!(
                "the hosted tool '{hosted}' is not supported: this network serves function tools \
                 the model calls and your agent runs, not tools Google hosts like googleSearch or \
                 codeExecution; offer only functionDeclarations"
            ));
        }
        for decl in &tool.function_declarations {
            defs.push(ToolDefinition {
                kind: ToolKind::Function,
                function: FunctionDefinition {
                    name: decl.name.clone(),
                    description: decl.description.clone(),
                    parameters: decl.parameters.clone(),
                },
            });
        }
    }
    if defs.is_empty() {
        return Err(
            "no functionDeclarations found: this network serves function tools the \
                    model calls and your agent runs, not hosted tools like googleSearch or \
                    codeExecution"
                .into(),
        );
    }
    Ok(defs)
}

/// Maps Gemini's `functionCallingConfig` to the protocol's tool choice: `AUTO`
/// lets the model decide, `ANY` forces a call, `NONE` forbids one, and an
/// `ANY` with a single `allowedFunctionNames` entry forces that named tool. A
/// multi-name allow-list is refused: the serving path can force any tool or one
/// named tool, not an arbitrary subset.
fn parse_tool_choice(config: &ToolConfig) -> Result<Option<ToolChoice>, String> {
    let Some(fcc) = &config.function_calling_config else {
        return Ok(None);
    };
    match fcc.mode.as_deref().unwrap_or("AUTO") {
        "AUTO" => Ok(Some(ToolChoice::Mode(ToolChoiceMode::Auto))),
        "NONE" => Ok(Some(ToolChoice::Mode(ToolChoiceMode::None))),
        "ANY" => match fcc.allowed_function_names.as_deref() {
            Some([name]) => Ok(Some(ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction { name: name.clone() },
            }))),
            Some(names) if names.len() > 1 => Err("allowedFunctionNames with more than one name \
                 is not supported: force any tool, or one named tool"
                .into()),
            _ => Ok(Some(ToolChoice::Mode(ToolChoiceMode::Required))),
        },
        other => Err(format!(
            "functionCallingConfig mode '{other}' is not supported: use AUTO, ANY, or NONE"
        )),
    }
}

async fn generate_content(
    State(state): State<Arc<OpenAiState>>,
    Path(model_action): Path<String>,
    Query(params): Query<GeminiParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(unauthorized) = gemini_authorize(&state, &headers, &params) {
        return unauthorized;
    }

    // Google addresses the model and the method in one segment,
    // `<model>:<method>`. A model id never contains a colon, so the last
    // colon splits the two.
    let Some((model, method)) = model_action.rsplit_once(':') else {
        return bad_request("the path must be /v1beta/models/<model>:generateContent");
    };
    let (streaming, sse) = match method {
        "generateContent" => (false, false),
        // `alt=sse` selects server-sent events (the SDK framing); the REST
        // default is a streamed JSON array.
        "streamGenerateContent" => (true, params.alt.as_deref() == Some("sse")),
        other => {
            return gemini_error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!(
                    "unknown method '{other}': this door serves generateContent and \
                     streamGenerateContent"
                ),
            )
        }
    };
    if model.trim().is_empty() {
        return bad_request("a model is required: /v1beta/models/<model>:generateContent");
    }
    let model = model.to_string();

    let req: GenerateContentRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => return bad_request(format!("could not parse the request body: {e}")),
    };
    if req.contents.is_empty() {
        return bad_request("contents must not be empty");
    }

    let config = match narrow_config(req.generation_config.as_ref()) {
        Ok(config) => config,
        Err(msg) => return bad_request(msg),
    };
    if let Err(msg) = ensure_safety_supported(&req.safety_settings) {
        return bad_request(msg);
    }
    // A `cachedContent` handle points at context held in a Gemini cache this
    // network cannot read. Serving the request would answer from `contents`
    // alone, silently missing the cached turns the buyer is paying to condition
    // on, so refuse rather than return a reply shaped by half the prompt.
    if req
        .cached_content
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty())
    {
        return bad_request(
            "cachedContent is not supported: this network cannot read a Gemini context cache, so \
             the cached turns would be missing from the prompt; inline that content in `contents` \
             instead",
        );
    }
    let messages = match build_messages(req.system_instruction.as_ref(), &req.contents) {
        Ok(messages) => messages,
        Err(msg) => return bad_request(msg),
    };
    let tools = match parse_tools(&req.tools) {
        Ok(tools) => tools,
        Err(msg) => return bad_request(msg),
    };
    let tool_choice = match req.tool_config.as_ref().map(parse_tool_choice).transpose() {
        Ok(choice) => choice.flatten(),
        Err(msg) => return bad_request(msg),
    };
    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return bad_request(msg),
    };

    // Pack the input through the same shared arguments every buyer surface
    // uses, so a request here means the same job as a chat on the CLI, the
    // MCP server, and the OpenAI and Anthropic front doors.
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
        temperature: config.temperature,
        top_p: config.top_p,
        max_tokens: config.max_tokens,
        seed: config.seed,
        presence_penalty: config.presence_penalty,
        frequency_penalty: config.frequency_penalty,
        logprobs: config.logprobs,
        stop: config.stop,
        response_format: config.response_format,
        // An empty tools list offers none; a forcing tool_choice with no tool
        // to bind is refused inside `InferArgs::input`, not billed as a plain
        // completion.
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice,
        idempotency_key: None,
        dry_run: false,
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return bad_request(e),
    };

    // Refuse a model the network is not serving before pricing it — the
    // SDK-correct 404, not a doomed submit that comes back as a bad gateway.
    if let Err(resp) = ensure_model_servable(&state, &model).await {
        return resp;
    }

    // Gemini requests carry no price. Offer the cheapest matching ask, held
    // under the per-call ceiling — the same default-price behaviour every
    // other price-less buyer surface takes.
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
    let reservation = match state.caps.try_reserve(price) {
        Ok(reservation) => reservation,
        Err(msg) => return reservation_refusal(msg),
    };
    if streaming {
        return stream_generate_content(state, model, request, reservation, sse).await;
    }
    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => Json(generate_content_response(&model, &outcome)).into_response(),
        Err(response) => response,
    }
}

/// The `parts` of a settled reply: the prose as a text part, then one
/// `functionCall` part per tool the model asked to call (its attested arguments
/// string parsed back to the object Gemini's `args` carries). A candidate
/// always holds at least one part, so an empty reply reads as one empty text
/// part.
fn reply_parts(reply: &AssistantReply) -> Vec<Value> {
    let mut parts = Vec::new();
    if !reply.text.is_empty() {
        parts.push(json!({ "text": reply.text }));
    }
    for call in &reply.tool_calls {
        let args: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
        parts.push(json!({
            "functionCall": { "name": call.function.name, "args": args },
        }));
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

/// Shapes a settled reply into Gemini's `GenerateContentResponse`: one
/// candidate carrying the model's prose and any tool calls, the metered token
/// counts, and the `covenant` proof a Covenant-aware client verifies.
fn generate_content_response(model: &str, outcome: &DispatchOutcome) -> Value {
    let receipt = &outcome.receipt.receipt;
    let reply = parse_assistant_output(&outcome.output);
    let parts = reply_parts(&reply);

    let finish_reason = receipt
        .meter
        .finish_reason
        .map(|reason| reason.as_gemini())
        .unwrap_or("STOP");
    let prompt_tokens = receipt.meter.tokens_in.unwrap_or(0);
    let output_tokens = receipt.meter.tokens_out.unwrap_or(0);

    json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish_reason,
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": prompt_tokens,
            "candidatesTokenCount": output_tokens,
            "totalTokenCount": prompt_tokens.saturating_add(output_tokens),
        },
        "modelVersion": model,
        "covenant": covenant_receipt(outcome),
    })
}

/// Streams a reply as Gemini's `streamGenerateContent`: a sequence of partial
/// `GenerateContentResponse` chunks, each carrying the next slice of text, then
/// a terminal chunk with the finish reason, the metered usage, and the
/// `covenant` proof. The live token feed is a best-effort preview; the signed
/// receipt is the paid artifact, so the terminal chunk carries the remainder
/// the verified output adds and is reconciled against it. Framed as server-sent
/// events when the client asked for `alt=sse` (the Google GenAI SDK framing),
/// else as the default streamed JSON array.
///
/// The job is submitted inside the spawned task, not awaited in this request
/// frame: a client disconnect during submit would otherwise cancel the handler
/// and drop the reservation while the coordinator already holds escrow, leaving
/// real spend uncounted against the session cap. Mirrors the OpenAI and
/// Anthropic doors' streaming handlers.
async fn stream_generate_content(
    state: Arc<OpenAiState>,
    model: String,
    request: JobRequest,
    reservation: SpendReservation,
    sse: bool,
) -> Response {
    let job_id = Uuid::new_v4();
    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), BuyerError>>();

    // The exact text the live feed delivered, so the terminal chunk sends only
    // the remainder the verified output adds rather than repeating it.
    let shown = Arc::new(Mutex::new(String::new()));
    let shown_feed = Arc::clone(&shown);
    let feed_model = model.clone();
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

        let delta_tx = tx.clone();
        let drained = stream_and_verify(
            &state.http,
            &state.buyer,
            &state.identity,
            envelope,
            move |delta| {
                if let Ok(mut feed) = shown_feed.lock() {
                    feed.push_str(delta);
                }
                let _ = delta_tx.send(delta_chunk(&feed_model, delta));
            },
        )
        .await;
        match drained {
            Ok(streamed) => {
                reservation.settle();
                let reply = parse_assistant_output(&streamed.outcome.output);
                // Send the remainder the verified output adds: when the feed is
                // a prefix (the common case, or a node that can't stream) only
                // the missing tail; when it diverged, the verified text
                // supersedes the preview.
                let shown = shown.lock().map(|feed| feed.clone()).unwrap_or_default();
                let missing = match reply.text.strip_prefix(shown.as_str()) {
                    Some(tail) => tail,
                    None => reply.text.as_str(),
                };
                let _ = tx.send(terminal_chunk(&model, missing, &reply, &streamed.outcome));
            }
            Err(e) => {
                // The buy never settled — release the hold. The stream is
                // already a 200, so the failure rides back as a terminal error
                // chunk the client reads, the shape Google's own stream uses.
                drop(reservation);
                let _ = tx.send(json!({
                    "error": {
                        "code": StatusCode::BAD_GATEWAY.as_u16(),
                        "message": e.to_string(),
                        "status": "UNAVAILABLE",
                    },
                }));
            }
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {
            if sse {
                let stream = futures::stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|chunk| {
                        (
                            Ok::<Event, Infallible>(Event::default().data(chunk.to_string())),
                            rx,
                        )
                    })
                });
                Sse::new(stream).into_response()
            } else {
                json_array_response(rx)
            }
        }
        Ok(Err(e)) => map_dispatch_error(e),
        Err(_) => gemini_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "the streaming dispatch task did not start",
        ),
    }
}

/// One streamed chunk carrying the next slice of generated text.
fn delta_chunk(model: &str, text: &str) -> Value {
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": text }] },
            "index": 0,
        }],
        "modelVersion": model,
    })
}

/// The terminal streamed chunk: the remainder the verified output adds (often
/// empty when the live feed already delivered it), any tool calls (which never
/// ride the live feed), the finish reason, the metered usage, and the
/// `covenant` proof.
fn terminal_chunk(
    model: &str,
    missing: &str,
    reply: &AssistantReply,
    outcome: &DispatchOutcome,
) -> Value {
    let receipt = &outcome.receipt.receipt;
    let finish_reason = receipt
        .meter
        .finish_reason
        .map(|reason| reason.as_gemini())
        .unwrap_or("STOP");
    let prompt_tokens = receipt.meter.tokens_in.unwrap_or(0);
    let output_tokens = receipt.meter.tokens_out.unwrap_or(0);

    // The tail text and any tool calls, sharing `reply_parts`' shaping — the
    // deltas already carried the text prefix, so only the remainder rides here.
    // Tool calls never stream, so a pure tool-call turn's whole call set lands
    // in this terminal chunk.
    let parts = reply_parts(&AssistantReply {
        text: missing.to_string(),
        tool_calls: reply.tool_calls.clone(),
        logprobs: None,
    });

    json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish_reason,
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": prompt_tokens,
            "candidatesTokenCount": output_tokens,
            "totalTokenCount": prompt_tokens.saturating_add(output_tokens),
        },
        "modelVersion": model,
        "covenant": covenant_receipt(outcome),
    })
}

/// One phase of the default streamed-JSON-array framing.
enum ArrayPhase {
    Open(mpsc::UnboundedReceiver<Value>),
    Body {
        rx: mpsc::UnboundedReceiver<Value>,
        first: bool,
    },
    Done,
}

/// Frames the chunk channel as Gemini's default streamed JSON array — `[`, each
/// chunk comma-separated, then `]` — for a `streamGenerateContent` call that
/// did not ask for `alt=sse`.
fn json_array_response(rx: mpsc::UnboundedReceiver<Value>) -> Response {
    let stream = futures::stream::unfold(ArrayPhase::Open(rx), |phase| async move {
        match phase {
            ArrayPhase::Open(rx) => Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(b"[")),
                ArrayPhase::Body { rx, first: true },
            )),
            ArrayPhase::Body { mut rx, first } => match rx.recv().await {
                Some(chunk) => {
                    let mut piece = String::new();
                    if !first {
                        piece.push(',');
                    }
                    piece.push_str(&chunk.to_string());
                    Some((
                        Ok(Bytes::from(piece)),
                        ArrayPhase::Body { rx, first: false },
                    ))
                }
                None => Some((Ok(Bytes::from_static(b"]")), ArrayPhase::Done)),
            },
            ArrayPhase::Done => None,
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .expect("a streamed json-array response builds")
}

/// Dispatch a paid job on a detached task so its settlement survives a client
/// disconnect. Mirrors the OpenAI front door's `dispatch_settling`, shaped to
/// Gemini's error body.
async fn dispatch_settling(
    state: Arc<OpenAiState>,
    request: JobRequest,
    reservation: SpendReservation,
) -> Result<DispatchOutcome, Response> {
    let joined = tokio::spawn(async move {
        let result = dispatch_and_verify(&state.http, &state.buyer, &state.identity, request).await;
        match &result {
            Ok(_) => reservation.settle(),
            Err(_) => drop(reservation),
        }
        result
    })
    .await;
    match joined {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(e)) => Err(map_dispatch_error(e)),
        Err(_) => Err(gemini_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "the dispatch task did not complete",
        )),
    }
}

/// Whether the network is serving `model` for an `inference_call`; returns the
/// SDK-correct 404 otherwise.
async fn ensure_model_servable(state: &OpenAiState, model: &str) -> Result<(), Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    if model_is_served(&view, model, JobKind::InferenceCall) {
        Ok(())
    } else {
        Err(gemini_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("model '{model}' is not being served by any operator on the network"),
        ))
    }
}

/// The concrete `inference_call` model ids the network is serving right now,
/// sorted and de-duplicated. The wildcard `any` node advertises no concrete id,
/// so it contributes nothing; only models a `generateContent` call can name are
/// listed.
async fn served_model_ids(state: &OpenAiState) -> Result<Vec<String>, Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    let mut ids: Vec<String> = view
        .entries
        .iter()
        .filter(|entry| entry.kind == JobKind::InferenceCall && entry.model != "any")
        .map(|entry| entry.model.clone())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// One Gemini `Model` object. The network carries no curated label or token
/// limits for an operator's model, so `displayName` is the served id itself and
/// only the fields the directory can honestly fill are set.
fn model_object(id: &str) -> Value {
    json!({
        "name": format!("models/{id}"),
        "displayName": id,
        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
    })
}

/// `GET /v1beta/models` — the model directory in Gemini's shape, listing the
/// chat models the network is serving now.
async fn list_models(
    State(state): State<Arc<OpenAiState>>,
    Query(params): Query<GeminiParams>,
    headers: HeaderMap,
) -> Response {
    if let Some(unauthorized) = gemini_authorize(&state, &headers, &params) {
        return unauthorized;
    }
    let ids = match served_model_ids(&state).await {
        Ok(ids) => ids,
        Err(resp) => return resp,
    };
    let models: Vec<Value> = ids.iter().map(|id| model_object(id)).collect();
    Json(json!({ "models": models })).into_response()
}

/// `GET /v1beta/models/<model>` — one model's directory entry, or the
/// SDK-correct 404 when the network is not serving it. A `models/` name prefix
/// (the form the directory returns) is accepted as well as the bare id.
async fn get_model(
    State(state): State<Arc<OpenAiState>>,
    Path(model): Path<String>,
    Query(params): Query<GeminiParams>,
    headers: HeaderMap,
) -> Response {
    if let Some(unauthorized) = gemini_authorize(&state, &headers, &params) {
        return unauthorized;
    }
    let model = model.strip_prefix("models/").unwrap_or(&model);
    if let Err(resp) = ensure_model_servable(&state, model).await {
        return resp;
    }
    Json(model_object(model)).into_response()
}

/// Maps a dispatch failure onto a Gemini-shaped error with a fitting status:
/// an underfunded buyer reads as a resource-exhausted quota limit, a timeout
/// or upstream fault as a server-side error.
fn map_dispatch_error(e: BuyerError) -> Response {
    let (status, gstatus) = match &e {
        _ if e.is_underfunded() => (StatusCode::TOO_MANY_REQUESTS, "RESOURCE_EXHAUSTED"),
        BuyerError::ReceiptTimeout(_) => (StatusCode::GATEWAY_TIMEOUT, "DEADLINE_EXCEEDED"),
        BuyerError::Rpc(_) | BuyerError::Protocol(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
        }
        BuyerError::SubmitRefused { .. }
        | BuyerError::NotServed { .. }
        | BuyerError::Coordinator(_)
        | BuyerError::Unreachable { .. }
        | BuyerError::Verification(_) => (StatusCode::BAD_GATEWAY, "UNAVAILABLE"),
    };
    gemini_error(status, gstatus, e.to_string())
}

/// Shapes a spend-cap refusal into the Gemini error a client expects: an
/// over-ceiling offer is a bad request, a full session cap is a
/// resource-exhausted quota limit.
fn reservation_refusal(msg: String) -> Response {
    if msg.contains("per-call ceiling") {
        bad_request(msg)
    } else {
        gemini_error(StatusCode::TOO_MANY_REQUESTS, "RESOURCE_EXHAUSTED", msg)
    }
}

/// `None` when the request may proceed; `Some(response)` is the ready 401 when
/// a configured key is missing or wrong. Gemini clients present the key as the
/// `x-goog-api-key` header or a `?key=` query parameter; both are accepted.
fn gemini_authorize(
    state: &OpenAiState,
    headers: &HeaderMap,
    params: &GeminiParams,
) -> Option<Response> {
    let expected = state.api_key.as_deref()?;
    let presented = goog_api_key(headers)
        .or_else(|| params.key.clone())
        .unwrap_or_default();
    if presented.as_bytes().ct_eq(expected.as_bytes()).into() {
        None
    } else {
        Some(gemini_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "missing or invalid API key: pass it as the x-goog-api-key header or the ?key= \
             query parameter",
        ))
    }
}

fn goog_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-goog-api-key")?
        .to_str()
        .ok()
        .map(|value| value.trim().to_string())
}

fn bad_request(message: impl Into<String>) -> Response {
    gemini_error(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", message)
}

/// Gemini's error envelope: `{"error":{"code","message","status"}}`, where
/// `status` is the canonical Google API status name (`INVALID_ARGUMENT`,
/// `NOT_FOUND`, …) and `code` is the HTTP status.
fn gemini_error(status: StatusCode, gstatus: &'static str, message: impl Into<String>) -> Response {
    let body = json!({
        "error": {
            "code": status.as_u16(),
            "message": message.into(),
            "status": gstatus,
        },
    });
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::ChatRole;

    fn text_part(text: &str) -> GeminiPart {
        GeminiPart {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    fn image_part(base64: &str) -> GeminiPart {
        GeminiPart {
            inline_data: Some(InlineData {
                data: base64.into(),
            }),
            ..Default::default()
        }
    }

    fn call_part(name: &str, args: Value) -> GeminiPart {
        GeminiPart {
            function_call: Some(GeminiFunctionCall {
                name: name.into(),
                args,
            }),
            ..Default::default()
        }
    }

    fn response_part(name: &str, response: Value) -> GeminiPart {
        GeminiPart {
            function_response: Some(GeminiFunctionResponse {
                name: name.into(),
                response,
            }),
            ..Default::default()
        }
    }

    fn content(role: Option<&str>, text: &str) -> GeminiContent {
        GeminiContent {
            role: role.map(Into::into),
            parts: vec![text_part(text)],
        }
    }

    #[test]
    fn a_user_and_model_turn_narrow_to_user_and_assistant() {
        let contents = vec![
            content(Some("user"), "hello"),
            content(Some("model"), "hi there"),
        ];
        let messages = build_messages(None, &contents).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, ChatRole::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, ChatRole::Assistant);
        assert_eq!(messages[1].content, "hi there");
    }

    #[test]
    fn a_missing_role_defaults_to_user() {
        let contents = vec![content(None, "hello")];
        let messages = build_messages(None, &contents).unwrap();
        assert_eq!(messages[0].role, ChatRole::User);
    }

    #[test]
    fn a_system_instruction_leads_as_a_system_turn() {
        let system = content(None, "be terse");
        let contents = vec![content(Some("user"), "hello")];
        let messages = build_messages(Some(&system), &contents).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[0].content, "be terse");
        assert_eq!(messages[1].role, ChatRole::User);
    }

    #[test]
    fn multiple_text_parts_concatenate() {
        let content = GeminiContent {
            role: Some("user".into()),
            parts: vec![text_part("foo"), text_part("bar")],
        };
        let messages = build_messages(None, &[content]).unwrap();
        assert_eq!(messages[0].content, "foobar");
    }

    #[test]
    fn an_unknown_role_is_refused() {
        let contents = vec![content(Some("assistant"), "hi")];
        let err = build_messages(None, &contents).unwrap_err();
        assert!(err.contains("unknown content role 'assistant'"), "{err}");
    }

    #[test]
    fn a_part_with_no_known_kind_is_refused() {
        let content = GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart::default()],
        };
        let err = build_messages(None, &[content]).unwrap_err();
        assert!(err.contains("must carry text"), "{err}");
    }

    #[test]
    fn a_tool_loop_round_trips_call_and_response() {
        // model calls a tool, the app feeds the result back on a user turn.
        let contents = vec![
            content(Some("user"), "weather in Paris?"),
            GeminiContent {
                role: Some("model".into()),
                parts: vec![call_part("get_weather", json!({ "city": "Paris" }))],
            },
            GeminiContent {
                role: Some("user".into()),
                parts: vec![response_part("get_weather", json!({ "tempC": 14 }))],
            },
        ];
        let messages = build_messages(None, &contents).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, ChatRole::User);

        // The model turn is an assistant message carrying the tool call.
        assert_eq!(messages[1].role, ChatRole::Assistant);
        assert_eq!(messages[1].tool_calls.len(), 1);
        assert_eq!(messages[1].tool_calls[0].function.name, "get_weather");
        assert_eq!(
            messages[1].tool_calls[0].function.arguments,
            r#"{"city":"Paris"}"#
        );

        // The result is a tool message whose id matches the call's id.
        assert_eq!(messages[2].role, ChatRole::Tool);
        assert_eq!(
            messages[2].tool_call_id.as_deref(),
            Some(messages[1].tool_calls[0].id.as_str())
        );
        assert_eq!(messages[2].content, r#"{"tempC":14}"#);
    }

    #[test]
    fn parallel_calls_of_one_function_pair_by_position() {
        // A model can call the same function twice in one turn (weather for two
        // cities). A Gemini functionResponse names only the function and carries
        // no call id, so the two results must bind to the two calls in order —
        // the per-name FIFO the correlator keeps. Collapsing that queue to a
        // single id per name, or popping it LIFO, would silently cross the wires
        // and feed each tool its sibling's result.
        let contents = vec![
            GeminiContent {
                role: Some("model".into()),
                parts: vec![
                    call_part("get_weather", json!({ "city": "Paris" })),
                    call_part("get_weather", json!({ "city": "London" })),
                ],
            },
            GeminiContent {
                role: Some("user".into()),
                parts: vec![
                    response_part("get_weather", json!({ "city": "Paris", "tempC": 14 })),
                    response_part("get_weather", json!({ "city": "London", "tempC": 11 })),
                ],
            },
        ];
        let messages = build_messages(None, &contents).unwrap();
        assert_eq!(messages.len(), 3);

        let calls = &messages[0].tool_calls;
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].id, calls[1].id, "each call gets its own id");

        assert_eq!(messages[1].role, ChatRole::Tool);
        assert_eq!(
            messages[1].tool_call_id.as_deref(),
            Some(calls[0].id.as_str())
        );
        assert!(
            messages[1].content.contains("Paris"),
            "{}",
            messages[1].content
        );

        assert_eq!(messages[2].role, ChatRole::Tool);
        assert_eq!(
            messages[2].tool_call_id.as_deref(),
            Some(calls[1].id.as_str())
        );
        assert!(
            messages[2].content.contains("London"),
            "{}",
            messages[2].content
        );
    }

    #[test]
    fn a_function_call_on_a_user_turn_is_refused() {
        let contents = vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![call_part("x", json!({}))],
        }];
        let err = build_messages(None, &contents).unwrap_err();
        assert!(
            err.contains("functionCall belongs on a model turn"),
            "{err}"
        );
    }

    #[test]
    fn a_tool_role_carries_a_function_response() {
        // Some clients label the tool-result turn `function`/`tool`; both work.
        let contents = vec![
            GeminiContent {
                role: Some("model".into()),
                parts: vec![call_part("ping", json!({}))],
            },
            GeminiContent {
                role: Some("function".into()),
                parts: vec![response_part("ping", json!({ "ok": true }))],
            },
        ];
        let messages = build_messages(None, &contents).unwrap();
        assert_eq!(messages[1].role, ChatRole::Tool);
        assert_eq!(
            messages[1].tool_call_id.as_deref(),
            Some(messages[0].tool_calls[0].id.as_str())
        );
    }

    #[test]
    fn an_inline_image_becomes_a_user_turn_with_the_image() {
        // "AQID" is base64 for the bytes 01 02 03.
        let content = GeminiContent {
            role: Some("user".into()),
            parts: vec![text_part("what is this?"), image_part("AQID")],
        };
        let messages = build_messages(None, &[content]).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::User);
        assert_eq!(messages[0].content, "what is this?");
        assert_eq!(messages[0].images, vec!["AQID".to_string()]);
    }

    #[test]
    fn invalid_inline_image_base64_is_refused() {
        let content = GeminiContent {
            role: Some("user".into()),
            parts: vec![image_part("not base64!!")],
        };
        let err = build_messages(None, &[content]).unwrap_err();
        assert!(err.contains("not valid base64"), "{err}");
    }

    #[test]
    fn an_image_on_a_model_turn_is_refused() {
        let content = GeminiContent {
            role: Some("model".into()),
            parts: vec![image_part("AQID")],
        };
        let err = build_messages(None, &[content]).unwrap_err();
        assert!(err.contains("only supported on a user turn"), "{err}");
    }

    #[test]
    fn generation_config_narrows_the_sampling_knobs() {
        let config = GenerationConfig {
            temperature: Some(0.4),
            top_p: Some(0.9),
            presence_penalty: Some(0.5),
            frequency_penalty: Some(-0.5),
            seed: Some(42),
            max_output_tokens: Some(256),
            stop_sequences: Some(vec!["STOP".into()]),
            candidate_count: Some(1),
            response_mime_type: Some("text/plain".into()),
            ..Default::default()
        };
        let narrowed = narrow_config(Some(&config)).unwrap();
        assert_eq!(
            narrowed,
            NarrowedConfig {
                temperature: Some(0.4),
                top_p: Some(0.9),
                presence_penalty: Some(0.5),
                frequency_penalty: Some(-0.5),
                seed: Some(42),
                logprobs: None,
                max_tokens: Some(256),
                stop: Some(vec!["STOP".into()]),
                response_format: None,
            }
        );
    }

    #[test]
    fn an_absent_generation_config_is_all_defaults() {
        assert_eq!(narrow_config(None).unwrap(), NarrowedConfig::default());
    }

    #[test]
    fn top_k_is_refused() {
        let config = GenerationConfig {
            top_k: Some(40),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("topK is not supported"), "{err}");
    }

    #[test]
    fn more_than_one_candidate_is_refused() {
        let config = GenerationConfig {
            candidate_count: Some(2),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("candidateCount > 1"), "{err}");
    }

    #[test]
    fn response_logprobs_folds_to_a_count() {
        // On with a count asks for that many alternatives; on without a count
        // asks for the chosen token alone.
        let with_count = GenerationConfig {
            response_logprobs: Some(true),
            logprobs: Some(5),
            ..Default::default()
        };
        assert_eq!(narrow_config(Some(&with_count)).unwrap().logprobs, Some(5));
        let bare = GenerationConfig {
            response_logprobs: Some(true),
            ..Default::default()
        };
        assert_eq!(narrow_config(Some(&bare)).unwrap().logprobs, Some(0));
    }

    #[test]
    fn a_logprobs_count_without_the_switch_is_refused() {
        // A count with responseLogprobs off would be silently ignored by
        // Gemini, so refuse it rather than let the buyer pay for logprobs they
        // asked for and would not get.
        let config = GenerationConfig {
            logprobs: Some(5),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("responseLogprobs"), "{err}");
    }

    #[test]
    fn an_enabling_thinking_budget_is_refused() {
        // A thinking budget (a positive count, or -1 for Gemini's dynamic
        // thinking) would bill for a reasoning stream this network does not
        // return, so refuse it rather than sample without it.
        for budget in [1024, -1] {
            let config = GenerationConfig {
                thinking_config: Some(ThinkingConfig {
                    thinking_budget: Some(budget),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let err = narrow_config(Some(&config)).unwrap_err();
            assert!(err.contains("thinkingConfig"), "{err}");
        }
    }

    #[test]
    fn asking_for_thoughts_is_refused() {
        let config = GenerationConfig {
            thinking_config: Some(ThinkingConfig {
                include_thoughts: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("thinkingConfig"), "{err}");
    }

    #[test]
    fn an_opted_out_thinking_config_rides_through() {
        // A zero budget with no thought stream is the client disabling thinking;
        // honor it as a no-op rather than refusing.
        let config = GenerationConfig {
            thinking_config: Some(ThinkingConfig {
                thinking_budget: Some(0),
                include_thoughts: Some(false),
            }),
            ..Default::default()
        };
        assert!(narrow_config(Some(&config)).is_ok());
    }

    #[test]
    fn a_non_text_response_modality_is_refused() {
        // Asking for image or audio output would bill for a modality this
        // network does not produce, so refuse it rather than return text.
        for modalities in [
            vec!["IMAGE".to_string()],
            vec!["TEXT".into(), "IMAGE".into()],
        ] {
            let config = GenerationConfig {
                response_modalities: Some(modalities),
                ..Default::default()
            };
            let err = narrow_config(Some(&config)).unwrap_err();
            assert!(err.contains("responseModalities"), "{err}");
        }
    }

    #[test]
    fn a_text_response_modality_rides_through() {
        let config = GenerationConfig {
            response_modalities: Some(vec!["TEXT".into()]),
            ..Default::default()
        };
        assert!(narrow_config(Some(&config)).is_ok());
    }

    fn safety(threshold: &str) -> SafetySetting {
        SafetySetting {
            category: Some("HARM_CATEGORY_DANGEROUS_CONTENT".into()),
            threshold: Some(threshold.into()),
        }
    }

    #[test]
    fn a_blocking_safety_threshold_is_refused() {
        // Naming a real block level would let the buyer believe the reply was
        // moderated to it; the network applies no such filter, so refuse and
        // name the category rather than pretend.
        for threshold in [
            "BLOCK_LOW_AND_ABOVE",
            "BLOCK_MEDIUM_AND_ABOVE",
            "BLOCK_ONLY_HIGH",
        ] {
            let err = ensure_safety_supported(&[safety(threshold)]).unwrap_err();
            assert!(err.contains("safetySettings"), "{err}");
            assert!(err.contains("HARM_CATEGORY_DANGEROUS_CONTENT"), "{err}");
        }
    }

    #[test]
    fn a_permissive_safety_threshold_rides_through() {
        // Turning blocking off, leaving it unspecified, or an empty list all ask
        // for nothing the network lacks.
        assert!(ensure_safety_supported(&[]).is_ok());
        for threshold in ["BLOCK_NONE", "off", "HARM_BLOCK_THRESHOLD_UNSPECIFIED", ""] {
            assert!(
                ensure_safety_supported(&[safety(threshold)]).is_ok(),
                "threshold {threshold} should ride through"
            );
        }
    }

    #[test]
    fn one_blocking_entry_among_permissive_ones_is_refused() {
        let settings = [
            safety("BLOCK_NONE"),
            safety("BLOCK_ONLY_HIGH"),
            safety("OFF"),
        ];
        assert!(ensure_safety_supported(&settings).is_err());
    }

    #[test]
    fn application_json_is_json_mode() {
        let config = GenerationConfig {
            response_mime_type: Some("application/json".into()),
            ..Default::default()
        };
        assert_eq!(
            narrow_config(Some(&config)).unwrap().response_format,
            Some(ResponseFormat::JsonObject)
        );
    }

    #[test]
    fn application_json_with_a_schema_constrains_the_reply() {
        let config = GenerationConfig {
            response_mime_type: Some("application/json".into()),
            response_schema: Some(json!({ "type": "object" })),
            ..Default::default()
        };
        assert_eq!(
            narrow_config(Some(&config)).unwrap().response_format,
            Some(ResponseFormat::JsonSchema {
                name: "response".into(),
                schema: json!({ "type": "object" }),
                strict: None,
            })
        );
    }

    #[test]
    fn response_json_schema_constrains_the_reply() {
        // Gemini's newer full-JSON-Schema field carries the same constraint as
        // the OpenAPI-subset responseSchema and maps to the same protocol form.
        let config = GenerationConfig {
            response_mime_type: Some("application/json".into()),
            response_json_schema: Some(json!({ "type": "object" })),
            ..Default::default()
        };
        assert_eq!(
            narrow_config(Some(&config)).unwrap().response_format,
            Some(ResponseFormat::JsonSchema {
                name: "response".into(),
                schema: json!({ "type": "object" }),
                strict: None,
            })
        );
    }

    #[test]
    fn setting_both_schema_fields_is_refused() {
        let config = GenerationConfig {
            response_mime_type: Some("application/json".into()),
            response_schema: Some(json!({ "type": "object" })),
            response_json_schema: Some(json!({ "type": "object" })),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn a_json_schema_without_the_json_mime_is_refused() {
        let config = GenerationConfig {
            response_json_schema: Some(json!({ "type": "object" })),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("responseJsonSchema requires"), "{err}");
    }

    #[test]
    fn an_enum_or_xml_response_mime_type_is_refused() {
        let config = GenerationConfig {
            response_mime_type: Some("text/x.enum".into()),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("responseMimeType 'text/x.enum'"), "{err}");
    }

    #[test]
    fn a_schema_without_the_json_mime_is_refused() {
        let config = GenerationConfig {
            response_schema: Some(json!({ "type": "object" })),
            ..Default::default()
        };
        let err = narrow_config(Some(&config)).unwrap_err();
        assert!(err.contains("responseSchema requires"), "{err}");
    }

    #[test]
    fn an_empty_stop_sequence_list_narrows_to_none() {
        let config = GenerationConfig {
            stop_sequences: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(narrow_config(Some(&config)).unwrap().stop, None);
    }

    #[test]
    fn the_request_parses_gemini_camel_case() {
        let body = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }],
            "systemInstruction": { "parts": [{ "text": "be terse" }] },
            "generationConfig": { "temperature": 0.2, "maxOutputTokens": 64 },
        });
        let req: GenerateContentRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.contents.len(), 1);
        assert!(req.system_instruction.is_some());
        let config = req.generation_config.unwrap();
        assert_eq!(config.temperature, Some(0.2));
        assert_eq!(config.max_output_tokens, Some(64));
    }

    fn tools_from(value: Value) -> Vec<GeminiTool> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parse_tools_flattens_function_declarations() {
        let tools = tools_from(json!([{
            "functionDeclarations": [
                { "name": "get_weather", "description": "look it up",
                  "parameters": { "type": "object" } },
                { "name": "get_time" },
            ]
        }]));
        let defs = parse_tools(&tools).unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].function.name, "get_weather");
        assert_eq!(defs[0].function.description.as_deref(), Some("look it up"));
        assert_eq!(
            defs[0].function.parameters,
            Some(json!({ "type": "object" }))
        );
        assert_eq!(defs[1].function.name, "get_time");
    }

    #[test]
    fn parse_tools_refuses_a_hosted_tool_by_name() {
        for hosted in [
            json!([{ "googleSearch": {} }]),
            json!([{ "codeExecution": {} }]),
            // Riding alongside a real function tool must not let the hosted
            // tool slip through as a silently ungrounded completion.
            json!([
                { "googleSearch": {} },
                { "functionDeclarations": [{ "name": "get_time" }] },
            ]),
        ] {
            let err = parse_tools(&tools_from(hosted)).unwrap_err();
            assert!(err.contains("hosted tool"), "{err}");
        }
    }

    #[test]
    fn parse_tools_refuses_an_empty_tool_entry() {
        // A tool object offering neither a function nor a hosted tool has
        // nothing callable to serve.
        let tools = tools_from(json!([{ "functionDeclarations": [] }]));
        let err = parse_tools(&tools).unwrap_err();
        assert!(err.contains("no functionDeclarations"), "{err}");
    }

    fn tool_config_from(value: Value) -> ToolConfig {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parse_tool_choice_maps_the_modes() {
        let auto = tool_config_from(json!({ "functionCallingConfig": { "mode": "AUTO" } }));
        assert_eq!(
            parse_tool_choice(&auto).unwrap(),
            Some(ToolChoice::Mode(ToolChoiceMode::Auto))
        );
        let any = tool_config_from(json!({ "functionCallingConfig": { "mode": "ANY" } }));
        assert_eq!(
            parse_tool_choice(&any).unwrap(),
            Some(ToolChoice::Mode(ToolChoiceMode::Required))
        );
        let none = tool_config_from(json!({ "functionCallingConfig": { "mode": "NONE" } }));
        assert_eq!(
            parse_tool_choice(&none).unwrap(),
            Some(ToolChoice::Mode(ToolChoiceMode::None))
        );
    }

    #[test]
    fn parse_tool_choice_any_with_one_name_forces_it() {
        let config = tool_config_from(json!({
            "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["get_weather"] }
        }));
        assert_eq!(
            parse_tool_choice(&config).unwrap(),
            Some(ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction {
                    name: "get_weather".into()
                },
            }))
        );
    }

    #[test]
    fn parse_tool_choice_refuses_a_multi_name_allow_list() {
        let config = tool_config_from(json!({
            "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["a", "b"] }
        }));
        let err = parse_tool_choice(&config).unwrap_err();
        assert!(err.contains("more than one name"), "{err}");
    }

    #[test]
    fn parse_tool_choice_refuses_an_unknown_mode() {
        let config = tool_config_from(json!({ "functionCallingConfig": { "mode": "SOMETIMES" } }));
        let err = parse_tool_choice(&config).unwrap_err();
        assert!(err.contains("mode 'SOMETIMES'"), "{err}");
    }

    #[test]
    fn reply_parts_renders_a_function_call() {
        use covenant_compute_protocol::{FunctionCall, ToolCall, ToolCallKind};
        let reply = AssistantReply {
            text: "let me check".into(),
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: r#"{"city":"Paris"}"#.into(),
                },
            }],
            logprobs: None,
        };
        let parts = reply_parts(&reply);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], json!({ "text": "let me check" }));
        assert_eq!(
            parts[1],
            json!({ "functionCall": { "name": "get_weather", "args": { "city": "Paris" } } })
        );
    }

    #[test]
    fn reply_parts_for_a_pure_tool_call_has_no_empty_text() {
        use covenant_compute_protocol::{FunctionCall, ToolCall, ToolCallKind};
        let reply = AssistantReply {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: "ping".into(),
                    arguments: "{}".into(),
                },
            }],
            logprobs: None,
        };
        let parts = reply_parts(&reply);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].get("functionCall").is_some());
    }
}
