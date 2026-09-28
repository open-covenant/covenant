//! An OpenAI Responses API front door for the compute network. A client
//! built for `POST /v1/responses` — the modern `openai` SDK's
//! `client.responses.create(...)`, the OpenAI Agents SDK — points its base
//! URL here, and every response is bought on the compute network, paid, and
//! returned with the operator's signed, locally re-verified work receipt. No
//! client code changes; the buyer's spend stays under the same per-call and
//! session caps every other buyer surface enforces.
//!
//! The Responses API is a different request and response shape from
//! `/v1/chat/completions` — an `input` that is a string or a list of typed
//! items, a top-level `instructions`, and an `output` array of content items
//! rather than a `choices` list — but the same underlying job: one
//! `InferenceCall` dispatched, metered, and receipted. This module shapes the
//! Responses wire form over that shared path, reusing the OpenAI door's auth,
//! pricing, model-servability check, and dispatch settling verbatim.
//!
//! The response carries one extra `covenant` field with the verified receipt,
//! exactly as the chat endpoint does — a plain OpenAI client ignores it, a
//! Covenant-aware one holds the job to its on-chain money trail.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use covenant_compute_protocol::{
    parse_assistant_output, AssistantReply, ChatMessage, ChatRole, FunctionCall,
    FunctionDefinition, JobKind, JobMeter, NamedFunction, NamedToolChoice, ResponseFormat,
    ToolCall, ToolCallKind, ToolChoice, ToolChoiceMode, ToolDefinition, ToolKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::openai::{
    api_error, authorize, covenant_receipt, dispatch_settling, ensure_model_servable, epoch_secs,
    image_data_uri_bytes, map_dispatch_error, openai_finish_reason, reputation_floor,
    reservation_refusal, OpenAiState,
};
use crate::{
    cheapest_matching_ask, stream_and_verify, submit_streaming, BuyerError, InferArgs, JobRequest,
    SpendReservation,
};

#[derive(Debug, Deserialize)]
struct ResponsesRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    input: Option<Value>,
    /// The system/developer instruction inserted ahead of the input. In the
    /// Responses API this is a top-level string, not a message.
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    /// Per-token log-probability count, and the `include` list that opts the
    /// response into carrying them. The network's responses serving path emits
    /// no logprobs items, so a request for them is refused rather than accepted
    /// and silently dropped — the chat completions endpoint serves logprobs.
    #[serde(default)]
    top_logprobs: Option<u32>,
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    stream: bool,
    /// Server-side conversation chaining. The network stores no response, so
    /// this is refused rather than silently ignored — a caller relying on it
    /// would otherwise get wrong context with no signal.
    #[serde(default)]
    previous_response_id: Option<String>,
    /// Whether to run the request asynchronously and poll for it later. The
    /// network has no background queue to poll, so it is refused.
    #[serde(default)]
    background: bool,
    /// Reasoning-model controls. The network serves ordinary models and
    /// produces no reasoning items, so a present `reasoning` block is refused
    /// rather than accepted and dropped.
    #[serde(default)]
    reasoning: Option<Value>,
    /// How the network handles an input that overflows the model's context
    /// window. `disabled` (the API default) fails the request, which is what
    /// this network already does, so it passes and is echoed back honestly.
    /// `auto` asks the network to silently drop input items to fit; the wire
    /// carries no such control, so it is refused rather than accepted, dropped,
    /// and then falsely echoed as `disabled`.
    #[serde(default)]
    truncation: Option<String>,
    /// Output-format controls (`{"format": {...}}`). The default text format
    /// leaves the reply free-form, while a `json_schema` or `json_object`
    /// format constrains it, the structured output the Agents SDK's
    /// `output_type` uses.
    #[serde(default)]
    text: Option<Value>,
    /// Functions the model may call. In the Responses API a function tool
    /// flattens `name`/`description`/`parameters` at the top level rather than
    /// nesting them under a `function` object the way `/v1/chat/completions`
    /// does; a hosted/server tool carries another `type` this network can't
    /// run and is refused naming it.
    #[serde(default)]
    tools: Vec<Value>,
    /// Whether and which tool the model may or must call.
    #[serde(default)]
    tool_choice: Option<Value>,
    /// Whether the model may emit several tool calls in one turn. The network
    /// can't promise exactly one per turn, so a `false` is refused when tools
    /// are in play rather than accepted and broken.
    #[serde(default)]
    parallel_tool_calls: Option<bool>,
    /// A ceiling on how many tool calls the model may make in a turn. This
    /// network runs a single inference and can't bound the count the backend
    /// emits, so a cap is refused when tools are in play rather than accepted
    /// and silently exceeded.
    #[serde(default)]
    max_tool_calls: Option<Value>,
    /// Client-attached key-value pairs the response carries back unchanged, so
    /// an SDK can correlate a reply to its request. The network stores nothing,
    /// so these live only for the round trip.
    #[serde(default)]
    metadata: Option<Value>,
}

/// One entry in a Responses `tools` array, before it is narrowed to the
/// protocol's [`ToolDefinition`]. A function tool flattens its fields at the
/// top level; a hosted tool (`web_search`, `file_search`, `code_interpreter`,
/// `computer_use_preview`, `mcp`, …) carries a `type` the network can't
/// execute.
#[derive(Debug, Deserialize)]
struct ResponsesTool {
    #[serde(rename = "type", default)]
    tool_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<Value>,
}

impl ResponsesTool {
    fn into_protocol(self) -> Result<ToolDefinition, String> {
        match self.tool_type.as_deref() {
            None | Some("function") => {}
            Some(other) => {
                return Err(format!(
                    "tool type '{other}' is a hosted tool that runs inside OpenAI's own \
                     service, so this network can't execute it; offer a function tool \
                     ({{\"type\":\"function\",\"name\":...,\"parameters\":...}}) the model \
                     calls and your agent runs"
                ))
            }
        }
        let name = self
            .name
            .filter(|n| !n.trim().is_empty())
            .ok_or("a function tool needs a name")?;
        Ok(ToolDefinition {
            kind: ToolKind::Function,
            function: FunctionDefinition {
                name,
                description: self.description,
                parameters: self.parameters,
            },
        })
    }
}

/// Narrow a Responses `tools` array to the protocol's tool definitions,
/// refusing a hosted tool before any spend.
fn parse_tools(tools: &[Value]) -> Result<Vec<ToolDefinition>, String> {
    tools
        .iter()
        .map(|tool| {
            let parsed: ResponsesTool = serde_json::from_value(tool.clone())
                .map_err(|e| format!("could not parse a tool: {e}"))?;
            parsed.into_protocol()
        })
        .collect()
}

/// Narrow a Responses `tool_choice` to the protocol's [`ToolChoice`]. The
/// Responses forced-function form names the function at the top level
/// (`{"type":"function","name":...}`), unlike the chat door's nesting under a
/// `function` object; a hosted-tool choice is refused.
fn parse_tool_choice(value: &Value) -> Result<ToolChoice, String> {
    match value {
        Value::String(mode) => match mode.as_str() {
            "auto" => Ok(ToolChoice::Mode(ToolChoiceMode::Auto)),
            "none" => Ok(ToolChoice::Mode(ToolChoiceMode::None)),
            "required" => Ok(ToolChoice::Mode(ToolChoiceMode::Required)),
            other => Err(format!(
                "tool_choice '{other}' is not supported; use auto, none, required, or a \
                 function"
            )),
        },
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
            Some("function") => {
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                    .ok_or("a function tool_choice needs a name")?;
                Ok(ToolChoice::Named(NamedToolChoice {
                    kind: ToolKind::Function,
                    function: NamedFunction {
                        name: name.to_string(),
                    },
                }))
            }
            Some(other) => Err(format!(
                "tool_choice type '{other}' selects a hosted tool this network can't run; \
                 force a function by name instead"
            )),
            None => Err("a tool_choice object needs a type".into()),
        },
        _ => Err("tool_choice must be a string or an object".into()),
    }
}

fn bad_request(msg: impl Into<String>) -> Response {
    api_error(StatusCode::BAD_REQUEST, "invalid_request_error", msg)
}

pub(crate) async fn create_response(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let req: ResponsesRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => return bad_request(format!("could not parse the request body: {e}")),
    };

    if req.previous_response_id.is_some() {
        return bad_request(
            "previous_response_id is not supported: the network stores no response; send the \
             full conversation as input each call",
        );
    }
    if req.background {
        return bad_request("background responses are not supported; omit background");
    }
    if req.reasoning.is_some() {
        return bad_request(
            "reasoning is not supported: the network serves ordinary models and returns no \
             reasoning items; omit reasoning",
        );
    }
    if req.truncation.as_deref().is_some_and(|t| t != "disabled") {
        return bad_request(
            "truncation other than 'disabled' is not supported: this network can't drop input \
             items to fit an overflowing context; omit truncation or set it to disabled",
        );
    }
    if req.top_logprobs.is_some()
        || req
            .include
            .as_deref()
            .is_some_and(|entries| entries.iter().any(|e| e.contains("logprobs")))
    {
        return bad_request(
            "logprobs are not supported on the responses endpoint: this network returns no \
             logprobs on this path; omit top_logprobs and the logprobs include, or use the \
             chat completions endpoint",
        );
    }
    let response_format = match parse_text_format(req.text.as_ref()) {
        Ok(format) => format,
        Err(msg) => return bad_request(msg),
    };
    let tools = match parse_tools(&req.tools) {
        Ok(tools) => tools,
        Err(msg) => return bad_request(msg),
    };
    let tool_choice = match req.tool_choice.as_ref().map(parse_tool_choice).transpose() {
        Ok(choice) => choice,
        Err(msg) => return bad_request(msg),
    };
    if req.parallel_tool_calls == Some(false) && !tools.is_empty() {
        return bad_request(
            "parallel_tool_calls=false is not supported: this network can't guarantee one \
             tool call per turn; omit it to take the model's default",
        );
    }
    if req.max_tool_calls.as_ref().is_some_and(|v| !v.is_null()) && !tools.is_empty() {
        return bad_request(
            "max_tool_calls is not supported: this network runs a single inference and can't \
             bound how many tool calls the model makes; omit max_tool_calls",
        );
    }
    if req.model.trim().is_empty() {
        return bad_request("model is required");
    }
    let metadata = match req.metadata.as_ref() {
        None => json!({}),
        Some(value @ Value::Object(_)) => value.clone(),
        Some(_) => return bad_request("metadata must be an object of key-value pairs"),
    };
    let echo = ResponseEcho::from_request(&req, &response_format, &tools, &tool_choice, metadata);

    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return bad_request(msg),
    };

    let messages = match build_messages(req.instructions.as_deref(), req.input.as_ref()) {
        Ok(messages) => messages,
        Err(msg) => return bad_request(msg),
    };

    let model = req.model.clone();
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
        max_tokens: req.max_output_tokens,
        seed: None,
        presence_penalty: None,
        frequency_penalty: None,
        logprobs: None,
        stop: None,
        response_format,
        // An empty tools list offers none; a forcing tool_choice with no tool
        // to bind is refused inside `InferArgs::input`, not billed as a plain
        // completion.
        tools: if tools.is_empty() {
            None
        } else {
            Some(tools.clone())
        },
        tool_choice: tool_choice.clone(),
        idempotency_key: None,
        dry_run: false,
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return bad_request(e),
    };

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

    let reservation = match state.caps.try_reserve(price) {
        Ok(reservation) => reservation,
        Err(msg) => return reservation_refusal(msg),
    };
    if req.stream {
        return stream_response(state, model, echo, request, reservation).await;
    }
    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => Json(response_object(&model, &echo, &outcome)).into_response(),
        Err(response) => response,
    }
}

/// The Responses `text.format` object, before it is narrowed to the
/// protocol's [`ResponseFormat`]. Unlike the chat door's `response_format`,
/// the `json_schema` fields sit at the format level rather than nested under
/// a `json_schema` object.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponsesTextFormat {
    Text,
    JsonObject,
    JsonSchema {
        #[serde(default)]
        name: String,
        #[serde(default)]
        schema: Option<Value>,
        #[serde(default)]
        strict: Option<bool>,
    },
}

/// The structured-output constraint a `text.format` asks for, narrowed to the
/// protocol's [`ResponseFormat`]. An absent `text`, an absent `format`, or the
/// default text format leaves the reply free-form (`None`); a `json_schema`
/// with no schema is refused before any spend, the way the chat door refuses
/// it. An unknown format type fails the deserialize loudly rather than being
/// dropped. A `text.verbosity` is refused rather than dropped: it shapes the
/// answer's length, and a network of ordinary models has no such control to
/// honor it with.
fn parse_text_format(text: Option<&Value>) -> Result<Option<ResponseFormat>, String> {
    let Some(text) = text else { return Ok(None) };
    if text.get("verbosity").is_some_and(|v| !v.is_null()) {
        return Err(
            "text.verbosity is not supported: this network serves ordinary models \
                    with no output-verbosity control; omit verbosity"
                .into(),
        );
    }
    let Some(format) = text.get("format") else {
        return Ok(None);
    };
    let parsed: ResponsesTextFormat = serde_json::from_value(format.clone())
        .map_err(|e| format!("could not parse text.format: {e}"))?;
    match parsed {
        ResponsesTextFormat::Text => Ok(None),
        ResponsesTextFormat::JsonObject => Ok(Some(ResponseFormat::JsonObject)),
        ResponsesTextFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let schema = schema.ok_or("text.format json_schema requires a schema")?;
            Ok(Some(ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            }))
        }
    }
}

/// The `text` object a response echoes back, rebuilt from the resolved
/// constraint so a strict SDK reads the format it asked for.
fn text_format_echo(format: &Option<ResponseFormat>) -> Value {
    match format {
        None => json!({ "format": { "type": "text" } }),
        Some(ResponseFormat::JsonObject) => json!({ "format": { "type": "json_object" } }),
        Some(ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        }) => json!({
            "format": {
                "type": "json_schema",
                "name": name,
                "schema": schema,
                "strict": strict,
            }
        }),
    }
}

/// The conversation an `input` describes, with `instructions` inserted ahead
/// of it as a system turn. `input` is either a bare string (one user turn) or
/// an array of typed input items; anything else, or an item shape this slice
/// does not serve, is a request error refused before any spend.
fn build_messages(
    instructions: Option<&str>,
    input: Option<&Value>,
) -> Result<Vec<ChatMessage>, String> {
    let mut messages = Vec::new();
    if let Some(instructions) = instructions {
        if !instructions.is_empty() {
            messages.push(ChatMessage::system(instructions));
        }
    }

    match input {
        None => return Err("input is required".into()),
        Some(Value::String(text)) => messages.push(ChatMessage::user(text.clone())),
        Some(Value::Array(items)) => {
            for item in items {
                messages.push(input_item_message(item)?);
            }
        }
        Some(_) => return Err("input must be a string or an array of input items".into()),
    }

    if messages.iter().all(|m| m.role == ChatRole::System) {
        return Err("input must carry at least one user or assistant turn".into());
    }
    Ok(messages)
}

/// One input array item narrowed to a chat turn. A `message` item becomes an
/// ordinary turn; a `function_call` item replays a prior tool call as an
/// assistant turn; a `function_call_output` item feeds a tool result back;
/// and an unknown item type fails loudly rather than being dropped.
fn input_item_message(item: &Value) -> Result<ChatMessage, String> {
    let obj = item
        .as_object()
        .ok_or("each input item must be an object")?;
    match obj.get("type").and_then(Value::as_str) {
        None | Some("message") => message_input_item(obj),
        Some("function_call") => function_call_item(obj),
        Some("function_call_output") => function_call_output_item(obj),
        Some(other) => Err(format!("input item type {other:?} is not supported")),
    }
}

/// A `message` input item narrowed to a chat turn.
fn message_input_item(obj: &serde_json::Map<String, Value>) -> Result<ChatMessage, String> {
    let role = match obj.get("role").and_then(Value::as_str) {
        Some("user") => ChatRole::User,
        Some("assistant") => ChatRole::Assistant,
        // "developer" is the Responses API's name for the system role.
        Some("system") | Some("developer") => ChatRole::System,
        Some(other) => return Err(format!("input message role {other:?} is not supported")),
        None => return Err("an input message needs a role".into()),
    };

    let content = obj.get("content").ok_or("an input message needs content")?;
    let (text, images) = message_content(content)?;
    Ok(ChatMessage {
        role,
        content: text,
        images,
        tool_calls: Vec::new(),
        tool_call_id: None,
    })
}

/// A `function_call` input item — the assistant's prior request to call a
/// tool — replayed as an assistant turn carrying that one call. `call_id` is
/// the handle a later `function_call_output` references, so it becomes the
/// protocol tool call's id.
fn function_call_item(obj: &serde_json::Map<String, Value>) -> Result<ChatMessage, String> {
    let call_id = obj
        .get("call_id")
        .and_then(Value::as_str)
        .ok_or("a function_call item needs a call_id")?;
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a function_call item needs a name")?;
    let arguments = obj
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(ChatMessage {
        role: ChatRole::Assistant,
        content: String::new(),
        images: Vec::new(),
        tool_calls: vec![ToolCall {
            id: call_id.to_string(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: name.to_string(),
                arguments,
            },
        }],
        tool_call_id: None,
    })
}

/// A `function_call_output` input item — a tool's result — fed back as a tool
/// turn answering the call it names. A string output rides through as-is; a
/// structured output is serialized to the JSON text the conversation carries.
fn function_call_output_item(obj: &serde_json::Map<String, Value>) -> Result<ChatMessage, String> {
    let call_id = obj
        .get("call_id")
        .and_then(Value::as_str)
        .ok_or("a function_call_output item needs a call_id")?;
    let output = match obj.get("output") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => return Err("a function_call_output item needs an output".into()),
    };
    Ok(ChatMessage::tool(call_id, output))
}

/// The text and inline images of a message's content: a bare string (no
/// images), or the concatenation of the text parts and the base64 of the image
/// parts in a content array. An `input_image` carries its bytes inline as a
/// `data:` URI; a remote URL or a `file_id` reference is refused rather than
/// silently dropped.
fn message_content(content: &Value) -> Result<(String, Vec<String>), String> {
    match content {
        Value::String(text) => Ok((text.clone(), Vec::new())),
        Value::Array(parts) => {
            let mut text = String::new();
            let mut images = Vec::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str);
                match kind {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        let t = part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or("a text content part needs a text field")?;
                        text.push_str(t);
                    }
                    Some("input_image") | Some("output_image") => {
                        images.push(input_image_base64(part)?);
                    }
                    Some(other) => {
                        return Err(format!("content part type {other:?} is not supported"))
                    }
                    None => return Err("a content part needs a type".into()),
                }
            }
            Ok((text, images))
        }
        _ => Err("message content must be a string or an array of content parts".into()),
    }
}

/// Pull the inline base64 out of a Responses `input_image` part. The image
/// travels inline as a `data:` URI (`image_url` is the URI string itself, not
/// a nested object as in the chat door); a remote URL is refused because the
/// network never fetches a buyer's image, and a `file_id` has no store to
/// resolve against.
fn input_image_base64(part: &Value) -> Result<String, String> {
    if let Some(url) = part.get("image_url").and_then(Value::as_str) {
        return image_data_uri_bytes(url);
    }
    if part.get("file_id").is_some() {
        return Err(
            "input_image by file_id is not supported: inline the image as a base64 data uri".into(),
        );
    }
    Err("an input_image part needs an image_url".into())
}

/// Whether a finished job ran to a natural stop or was cut off, and the
/// Responses API `status`/`incomplete_details` that reports it. A `length`
/// finish is `incomplete` with reason `max_output_tokens`; a `content_filter`
/// finish is `incomplete` with reason `content_filter`, so a partial answer
/// the backend's policy cut short is not reported as a clean `completed`.
/// Every other finish is `completed`.
fn status_for(meter: &JobMeter) -> (&'static str, Value) {
    match openai_finish_reason(meter) {
        "length" => ("incomplete", json!({ "reason": "max_output_tokens" })),
        "content_filter" => ("incomplete", json!({ "reason": "content_filter" })),
        _ => ("completed", Value::Null),
    }
}

/// The assistant turn shaped into a Responses `output` message item: one
/// `output_text` content part carrying the reply's prose. A turn that also
/// calls tools carries [`output_function_call_item`]s alongside this in the
/// output array (see [`output_items`]).
fn output_message_item(item_id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": item_id,
        "status": status,
        "role": "assistant",
        "content": [
            { "type": "output_text", "text": text, "annotations": [] }
        ],
    })
}

/// One tool call shaped into a Responses `output` `function_call` item, keyed
/// by the item's own handle `id`. The attested call's id is the `call_id` a
/// follow-up `function_call_output` references. Arguments ride through as the
/// JSON string the protocol carries, the form the SDK re-parses.
fn output_function_call_item(id: &str, call: &ToolCall, status: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": call.id,
        "name": call.function.name,
        "arguments": call.function.arguments,
        "status": status,
    })
}

/// The full `output` array for a settled reply: a message item when there is
/// prose, then one `function_call` item per tool call. A reply with neither
/// (an empty completion) still returns a single empty message item, the shape
/// a Responses client always expects at least one of.
fn output_items(reply: &AssistantReply, status: &str) -> Value {
    let mut items = Vec::new();
    if !reply.text.is_empty() {
        let item_id = format!("msg_{}", Uuid::new_v4().simple());
        items.push(output_message_item(&item_id, &reply.text, status));
    }
    for call in &reply.tool_calls {
        let fc_id = format!("fc_{}", Uuid::new_v4().simple());
        items.push(output_function_call_item(&fc_id, call, status));
    }
    if items.is_empty() {
        let item_id = format!("msg_{}", Uuid::new_v4().simple());
        items.push(output_message_item(&item_id, "", status));
    }
    Value::Array(items)
}

/// The request knobs a Response echoes back, owned so a streamed response can
/// carry them into the `response.created` and terminal frames from a detached
/// task.
struct ResponseEcho {
    instructions: Option<String>,
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    /// The `text` object to echo, rebuilt from the resolved output-format
    /// constraint (`{"format":{"type":"text"}}` when the reply is free-form).
    text: Value,
    /// The `tools` the job offered, re-rendered in Responses shape, and the
    /// resolved `tool_choice` (`"auto"` when the request left it default).
    tools: Value,
    tool_choice: Value,
    /// The request's `metadata` object, carried back unchanged (`{}` when the
    /// request set none).
    metadata: Value,
}

impl ResponseEcho {
    fn from_request(
        req: &ResponsesRequest,
        format: &Option<ResponseFormat>,
        tools: &[ToolDefinition],
        tool_choice: &Option<ToolChoice>,
        metadata: Value,
    ) -> Self {
        Self {
            instructions: req.instructions.clone(),
            max_output_tokens: req.max_output_tokens,
            temperature: req.temperature,
            top_p: req.top_p,
            text: text_format_echo(format),
            tools: tools_echo(tools),
            tool_choice: tool_choice_echo(tool_choice),
            metadata,
        }
    }
}

/// The `tools` a response echoes back: each offered function re-rendered in
/// the Responses flat shape a strict SDK reads (`{type, name, description,
/// parameters, strict}`). Empty when the request offered none.
fn tools_echo(tools: &[ToolDefinition]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "parameters": tool.function.parameters,
                    "strict": Value::Null,
                })
            })
            .collect(),
    )
}

/// The `tool_choice` a response echoes back, resolved from the request: the
/// default is `"auto"`, and a forced function names itself the Responses way.
fn tool_choice_echo(choice: &Option<ToolChoice>) -> Value {
    match choice {
        None | Some(ToolChoice::Mode(ToolChoiceMode::Auto)) => json!("auto"),
        Some(ToolChoice::Mode(ToolChoiceMode::None)) => json!("none"),
        Some(ToolChoice::Mode(ToolChoiceMode::Required)) => json!("required"),
        Some(ToolChoice::Named(named)) => {
            json!({ "type": "function", "name": named.function.name })
        }
    }
}

/// The Responses `usage` block from the verified receipt's meter. The network
/// meters post-hoc, so a backend that reported no counts reads back as zero.
fn usage_block(meter: &JobMeter) -> Value {
    let input = meter.tokens_in.unwrap_or(0);
    let output = meter.tokens_out.unwrap_or(0);
    json!({
        "input_tokens": input,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": output,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": input.saturating_add(output),
    })
}

/// A full Responses `response` object with the fixed field set a strict SDK
/// parse resolves, parameterized by the parts that differ between an
/// in-progress stream open, a settled result, and a failure. The `covenant`
/// proof extension rides alongside, ignored by a plain client.
#[allow(clippy::too_many_arguments)]
fn render_response(
    id: &str,
    model: &str,
    echo: &ResponseEcho,
    created_at: u64,
    status: &str,
    error: Value,
    incomplete_details: Value,
    output: Value,
    usage: Value,
    covenant: Value,
) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "error": error,
        "incomplete_details": incomplete_details,
        "instructions": echo.instructions,
        "max_output_tokens": echo.max_output_tokens,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": Value::Null,
        "reasoning": Value::Null,
        // The network stores no response, so this is honestly false whatever
        // the request asked.
        "store": false,
        "temperature": echo.temperature,
        "text": echo.text.clone(),
        "tool_choice": echo.tool_choice.clone(),
        "tools": echo.tools.clone(),
        "top_p": echo.top_p,
        "truncation": "disabled",
        "usage": usage,
        "user": Value::Null,
        "metadata": echo.metadata.clone(),
        "covenant": covenant,
    })
}

/// The signed, re-verified receipt shaped into a Responses API `response`
/// object for the non-streaming path.
fn response_object(model: &str, echo: &ResponseEcho, outcome: &crate::DispatchOutcome) -> Value {
    let receipt = &outcome.receipt.receipt;
    let reply = parse_assistant_output(&outcome.output);
    let (status, incomplete_details) = status_for(&receipt.meter);
    render_response(
        &format!("resp_{}", receipt.job_id.simple()),
        model,
        echo,
        epoch_secs(),
        status,
        Value::Null,
        incomplete_details,
        output_items(&reply, status),
        usage_block(&receipt.meter),
        covenant_receipt(outcome),
    )
}

/// One named Responses SSE frame: the `event:` line the SDK dispatches on,
/// carrying its typed `data:` payload with the monotonic `sequence_number`
/// every Responses event reports.
fn response_event(name: &str, seq: &AtomicU64, mut data: Value) -> Event {
    let n = seq.fetch_add(1, Ordering::Relaxed);
    if let Some(obj) = data.as_object_mut() {
        obj.insert("sequence_number".into(), json!(n));
    }
    Event::default().event(name).data(data.to_string())
}

/// The streaming variant of a response: the Responses event sequence
/// (`response.created`, the message item and its text part opened, then
/// `response.output_text.delta` frames) as the operator relays tokens, closed
/// by `response.completed`, or `response.incomplete` at the token cap. The
/// signed receipt is still verified before the reservation settles: the live
/// feed is a preview, the receipt over the final output is the paid artifact,
/// so the feed is reconciled against the verified output rather than passed
/// off as complete. Mirrors the OpenAI door's `stream_chat`.
///
/// The job is submitted inside the spawned task, not awaited in this request
/// frame, so a client disconnect during submit cannot cancel the handler and
/// drop the reservation while the coordinator already holds escrow.
async fn stream_response(
    state: Arc<OpenAiState>,
    model: String,
    echo: ResponseEcho,
    request: JobRequest,
    reservation: SpendReservation,
) -> Response {
    let job_id = Uuid::new_v4();
    let response_id = format!("resp_{}", job_id.simple());
    let item_id = format!("msg_{}", Uuid::new_v4().simple());
    let created = epoch_secs();
    let seq = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), BuyerError>>();

    // The exact text the live feed delivered, reconciled against the signed
    // receipt below rather than passed off as complete.
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

        // Open the response and its single text item before any token, the
        // frames a Responses client reads to set up its output buffer. Usage
        // and the receipt are unknown until the job settles, so they are null
        // here and carried on the terminal frame.
        let opening = render_response(
            &response_id,
            &model,
            &echo,
            created,
            "in_progress",
            Value::Null,
            Value::Null,
            json!([]),
            Value::Null,
            Value::Null,
        );
        let _ = tx.send(response_event(
            "response.created",
            &seq,
            json!({ "type": "response.created", "response": opening.clone() }),
        ));
        let _ = tx.send(response_event(
            "response.in_progress",
            &seq,
            json!({ "type": "response.in_progress", "response": opening }),
        ));
        let _ = tx.send(response_event(
            "response.output_item.added",
            &seq,
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": item_id.as_str(),
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        ));
        let _ = tx.send(response_event(
            "response.content_part.added",
            &seq,
            json!({
                "type": "response.content_part.added",
                "item_id": item_id.as_str(),
                "output_index": 0,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] },
            }),
        ));

        let delta_tx = tx.clone();
        let delta_seq = Arc::clone(&seq);
        let delta_item = item_id.clone();
        let drained = stream_and_verify(
            &state.http,
            &state.buyer,
            &state.identity,
            envelope,
            move |delta| {
                if let Ok(mut feed) = shown_feed.lock() {
                    feed.push_str(delta);
                }
                let _ = delta_tx.send(response_event(
                    "response.output_text.delta",
                    &delta_seq,
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": delta_item.as_str(),
                        "output_index": 0,
                        "content_index": 0,
                        "delta": delta,
                    }),
                ));
            },
        )
        .await;

        match drained {
            Ok(streamed) => {
                reservation.settle();
                let reply = parse_assistant_output(&streamed.outcome.output);
                // Complete the text from the verified output: when the feed is
                // a prefix (the common lost-tail or can't-stream case) only the
                // missing remainder is sent; when it diverged, the verified
                // text supersedes the preview.
                let shown = shown.lock().map(|feed| feed.clone()).unwrap_or_default();
                let missing = match reply.text.strip_prefix(shown.as_str()) {
                    Some(tail) => tail,
                    None => reply.text.as_str(),
                };
                if !missing.is_empty() {
                    let _ = tx.send(response_event(
                        "response.output_text.delta",
                        &seq,
                        json!({
                            "type": "response.output_text.delta",
                            "item_id": item_id.as_str(),
                            "output_index": 0,
                            "content_index": 0,
                            "delta": missing,
                        }),
                    ));
                }
                let (status, incomplete_details) =
                    status_for(&streamed.outcome.receipt.receipt.meter);
                let _ = tx.send(response_event(
                    "response.output_text.done",
                    &seq,
                    json!({
                        "type": "response.output_text.done",
                        "item_id": item_id.as_str(),
                        "output_index": 0,
                        "content_index": 0,
                        "text": reply.text,
                    }),
                ));
                let _ = tx.send(response_event(
                    "response.content_part.done",
                    &seq,
                    json!({
                        "type": "response.content_part.done",
                        "item_id": item_id.as_str(),
                        "output_index": 0,
                        "content_index": 0,
                        "part": { "type": "output_text", "text": reply.text, "annotations": [] },
                    }),
                ));
                let _ = tx.send(response_event(
                    "response.output_item.done",
                    &seq,
                    json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": output_message_item(&item_id, &reply.text, status),
                    }),
                ));

                // Tool calls never ride the live feed (a tools job runs the
                // backend non-streaming), so each is emitted whole from the
                // verified output as its own function_call item after the
                // message: the item opens, its arguments arrive in one delta,
                // then it closes. The message holds output index 0, so the
                // calls follow at 1, 2, … and the terminal frame carries the
                // same items the client just saw stream, ids and all.
                let mut output = vec![output_message_item(&item_id, &reply.text, status)];
                for (offset, call) in reply.tool_calls.iter().enumerate() {
                    let output_index = offset + 1;
                    let fc_id = format!("fc_{}", Uuid::new_v4().simple());
                    let _ = tx.send(response_event(
                        "response.output_item.added",
                        &seq,
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {
                                "type": "function_call",
                                "id": fc_id.as_str(),
                                "call_id": call.id,
                                "name": call.function.name,
                                "arguments": "",
                                "status": "in_progress",
                            },
                        }),
                    ));
                    let _ = tx.send(response_event(
                        "response.function_call_arguments.delta",
                        &seq,
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": fc_id.as_str(),
                            "output_index": output_index,
                            "delta": call.function.arguments,
                        }),
                    ));
                    let _ = tx.send(response_event(
                        "response.function_call_arguments.done",
                        &seq,
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": fc_id.as_str(),
                            "output_index": output_index,
                            "arguments": call.function.arguments,
                        }),
                    ));
                    let item = output_function_call_item(&fc_id, call, status);
                    let _ = tx.send(response_event(
                        "response.output_item.done",
                        &seq,
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": item.clone(),
                        }),
                    ));
                    output.push(item);
                }

                let final_obj = render_response(
                    &response_id,
                    &model,
                    &echo,
                    created,
                    status,
                    Value::Null,
                    incomplete_details,
                    Value::Array(output),
                    usage_block(&streamed.outcome.receipt.receipt.meter),
                    covenant_receipt(&streamed.outcome),
                );
                let terminal = if status == "incomplete" {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                let _ = tx.send(response_event(
                    terminal,
                    &seq,
                    json!({ "type": terminal, "response": final_obj }),
                ));
            }
            Err(e) => {
                // The buy never settled — release the hold. The stream is
                // already a 200, so the failure rides back as a terminal
                // `response.failed` frame the client reads.
                drop(reservation);
                let failed = render_response(
                    &response_id,
                    &model,
                    &echo,
                    created,
                    "failed",
                    json!({ "code": "server_error", "message": e.to_string() }),
                    Value::Null,
                    json!([]),
                    Value::Null,
                    Value::Null,
                );
                let _ = tx.send(response_event(
                    "response.failed",
                    &seq,
                    json!({ "type": "response.failed", "response": failed }),
                ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_input_is_one_user_turn() {
        let messages = build_messages(None, Some(&json!("hello"))).expect("valid");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::User);
        assert_eq!(messages[0].content, "hello");
    }

    #[test]
    fn instructions_lead_as_a_system_turn() {
        let messages = build_messages(Some("be terse"), Some(&json!("hi"))).expect("valid");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[0].content, "be terse");
        assert_eq!(messages[1].role, ChatRole::User);
    }

    #[test]
    fn empty_instructions_add_no_turn() {
        let messages = build_messages(Some(""), Some(&json!("hi"))).expect("valid");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::User);
    }

    #[test]
    fn an_array_of_messages_maps_roles() {
        let input = json!([
            { "role": "developer", "content": "be terse" },
            { "role": "user", "content": "hi" },
            { "type": "message", "role": "assistant", "content": "hello" },
            { "role": "user", "content": "again" },
        ]);
        let messages = build_messages(None, Some(&input)).expect("valid");
        let roles: Vec<ChatRole> = messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                ChatRole::System,
                ChatRole::User,
                ChatRole::Assistant,
                ChatRole::User
            ]
        );
    }

    #[test]
    fn content_parts_concatenate_text() {
        let input = json!([{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "a" },
                { "type": "input_text", "text": "b" },
            ],
        }]);
        let messages = build_messages(None, Some(&input)).expect("valid");
        assert_eq!(messages[0].content, "ab");
    }

    #[test]
    fn missing_input_is_refused() {
        let err = build_messages(Some("sys"), None).unwrap_err();
        assert!(err.contains("input is required"), "{err}");
    }

    #[test]
    fn the_logprobs_controls_are_modelled_so_the_handler_can_refuse_them() {
        // Captured, not dropped: a client asking for logprobs on the responses
        // path — which produces none — is refused by the handler rather than
        // billed for a response silently missing the logprobs it paid for.
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "m",
            "input": "hi",
            "top_logprobs": 5,
            "include": ["message.output_text.logprobs"],
        }))
        .expect("the logprobs controls are modelled");
        assert_eq!(req.top_logprobs, Some(5));
        assert!(req
            .include
            .as_deref()
            .is_some_and(|e| e.iter().any(|s| s.contains("logprobs"))));
    }

    #[test]
    fn max_tool_calls_is_modelled_so_the_handler_can_refuse_it() {
        // Captured, not dropped: a client capping the tool-call count on a
        // network that runs a single inference and can't bound it is refused
        // by the handler rather than billed for a reply that ignored the cap.
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "m",
            "input": "hi",
            "tools": [{ "type": "function", "name": "f", "parameters": {} }],
            "max_tool_calls": 1,
        }))
        .expect("max_tool_calls is modelled");
        assert!(req.max_tool_calls.as_ref().is_some_and(|v| !v.is_null()));
    }

    #[test]
    fn only_instructions_leaves_no_turn_to_answer() {
        let err = build_messages(Some("sys"), Some(&json!([]))).unwrap_err();
        assert!(err.contains("at least one user or assistant turn"), "{err}");
    }

    #[test]
    fn an_input_image_attaches_its_inline_base64() {
        let (text, images) = message_content(&json!([
            { "type": "input_text", "text": "what is this?" },
            { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
        ]))
        .expect("valid");
        assert_eq!(text, "what is this?");
        assert_eq!(images, vec!["AAAA".to_string()]);
    }

    #[test]
    fn a_remote_input_image_url_is_refused() {
        let err = message_content(&json!([
            { "type": "input_image", "image_url": "https://example.com/cat.png" }
        ]))
        .unwrap_err();
        assert!(err.contains("remote image url"), "{err}");
    }

    #[test]
    fn an_input_image_by_file_id_is_refused() {
        let err = message_content(&json!([
            { "type": "input_image", "file_id": "file_123" }
        ]))
        .unwrap_err();
        assert!(err.contains("file_id"), "{err}");
    }

    #[test]
    fn an_unknown_item_type_is_refused() {
        let err = input_item_message(&json!({ "type": "reasoning", "id": "r1" })).unwrap_err();
        assert!(err.contains("not supported"), "{err}");
    }

    #[test]
    fn the_default_text_format_leaves_the_reply_free_form() {
        assert_eq!(
            parse_text_format(Some(&json!({ "format": { "type": "text" } }))).unwrap(),
            None
        );
        assert_eq!(parse_text_format(None).unwrap(), None);
        assert_eq!(parse_text_format(Some(&json!({}))).unwrap(), None);
    }

    #[test]
    fn a_json_object_format_maps_to_json_mode() {
        assert_eq!(
            parse_text_format(Some(&json!({ "format": { "type": "json_object" } }))).unwrap(),
            Some(ResponseFormat::JsonObject)
        );
    }

    #[test]
    fn a_json_schema_format_flattens_to_the_protocol_constraint() {
        let format = parse_text_format(Some(&json!({
            "format": {
                "type": "json_schema",
                "name": "colors",
                "schema": { "type": "object" },
                "strict": true,
            }
        })))
        .unwrap();
        assert_eq!(
            format,
            Some(ResponseFormat::JsonSchema {
                name: "colors".into(),
                schema: json!({ "type": "object" }),
                strict: Some(true),
            })
        );
    }

    #[test]
    fn a_json_schema_without_a_schema_is_refused() {
        let err = parse_text_format(Some(&json!({
            "format": { "type": "json_schema", "name": "colors" }
        })))
        .unwrap_err();
        assert!(err.contains("requires a schema"), "{err}");
    }

    #[test]
    fn an_unknown_text_format_is_refused() {
        let err =
            parse_text_format(Some(&json!({ "format": { "type": "handwriting" } }))).unwrap_err();
        assert!(err.contains("text.format"), "{err}");
    }

    #[test]
    fn a_text_verbosity_is_refused_not_dropped() {
        // It shapes the answer's length, and a network of ordinary models has
        // no control to honor it, so it is refused rather than billed for a
        // default-length reply — even with no format alongside it.
        let err = parse_text_format(Some(&json!({ "verbosity": "low" }))).unwrap_err();
        assert!(err.contains("verbosity"), "{err}");
        let err = parse_text_format(Some(&json!({
            "format": { "type": "text" },
            "verbosity": "high"
        })))
        .unwrap_err();
        assert!(err.contains("verbosity"), "{err}");
        // An explicit null asks for nothing and passes, the way an absent one does.
        assert_eq!(
            parse_text_format(Some(&json!({ "verbosity": null }))).unwrap(),
            None
        );
    }

    #[test]
    fn a_function_tool_narrows_to_the_protocol_definition() {
        let tools = parse_tools(&[json!({
            "type": "function",
            "name": "get_weather",
            "description": "look up the weather",
            "parameters": { "type": "object" }
        })])
        .expect("valid");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "get_weather");
        assert_eq!(
            tools[0].function.description.as_deref(),
            Some("look up the weather")
        );
    }

    #[test]
    fn a_tool_with_no_type_reads_as_a_function() {
        let tools = parse_tools(&[json!({ "name": "f", "parameters": {} })]).expect("valid");
        assert_eq!(tools[0].function.name, "f");
    }

    #[test]
    fn a_hosted_tool_is_refused_naming_it() {
        let err = parse_tools(&[json!({ "type": "web_search" })]).unwrap_err();
        assert!(err.contains("hosted tool"), "{err}");
        assert!(err.contains("web_search"), "{err}");
    }

    #[test]
    fn a_function_tool_without_a_name_is_refused() {
        let err = parse_tools(&[json!({ "type": "function", "parameters": {} })]).unwrap_err();
        assert!(err.contains("needs a name"), "{err}");
    }

    #[test]
    fn tool_choice_modes_map_to_the_protocol() {
        assert_eq!(
            parse_tool_choice(&json!("auto")).unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Auto)
        );
        assert_eq!(
            parse_tool_choice(&json!("none")).unwrap(),
            ToolChoice::Mode(ToolChoiceMode::None)
        );
        assert_eq!(
            parse_tool_choice(&json!("required")).unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Required)
        );
    }

    #[test]
    fn a_named_tool_choice_forces_the_function() {
        let choice =
            parse_tool_choice(&json!({ "type": "function", "name": "get_weather" })).unwrap();
        assert_eq!(
            choice,
            ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction {
                    name: "get_weather".into()
                },
            })
        );
    }

    #[test]
    fn a_hosted_tool_choice_is_refused() {
        let err = parse_tool_choice(&json!({ "type": "web_search" })).unwrap_err();
        assert!(err.contains("hosted tool"), "{err}");
    }

    #[test]
    fn an_unknown_tool_choice_mode_is_refused() {
        let err = parse_tool_choice(&json!("whenever")).unwrap_err();
        assert!(err.contains("not supported"), "{err}");
    }

    #[test]
    fn a_function_call_item_replays_as_an_assistant_turn() {
        let msg = input_item_message(&json!({
            "type": "function_call",
            "call_id": "call_7",
            "name": "get_weather",
            "arguments": "{\"city\":\"Paris\"}"
        }))
        .expect("valid");
        assert_eq!(msg.role, ChatRole::Assistant);
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].id, "call_7");
        assert_eq!(msg.tool_calls[0].function.name, "get_weather");
        assert_eq!(msg.tool_calls[0].function.arguments, r#"{"city":"Paris"}"#);
    }

    #[test]
    fn a_function_call_output_item_feeds_a_tool_result_back() {
        let msg = input_item_message(&json!({
            "type": "function_call_output",
            "call_id": "call_7",
            "output": "18C and clear"
        }))
        .expect("valid");
        assert_eq!(msg.role, ChatRole::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_7"));
        assert_eq!(msg.content, "18C and clear");
    }

    #[test]
    fn a_function_call_output_needs_an_output() {
        let err = input_item_message(&json!({
            "type": "function_call_output",
            "call_id": "call_7"
        }))
        .unwrap_err();
        assert!(err.contains("needs an output"), "{err}");
    }

    #[test]
    fn the_tool_choice_echo_defaults_to_auto() {
        assert_eq!(tool_choice_echo(&None), json!("auto"));
        assert_eq!(
            tool_choice_echo(&Some(ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction { name: "f".into() },
            }))),
            json!({ "type": "function", "name": "f" })
        );
    }

    #[test]
    fn output_items_pair_a_message_with_each_function_call() {
        let reply = AssistantReply {
            text: "on it".into(),
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
        let items = output_items(&reply, "completed");
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["content"][0]["text"], "on it");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_0");
        assert_eq!(items[1]["name"], "get_weather");
        assert_eq!(items[1]["arguments"], r#"{"city":"Paris"}"#);
    }

    #[test]
    fn output_items_of_a_pure_tool_call_carry_no_message() {
        let reply = AssistantReply {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            }],
            logprobs: None,
        };
        let items = output_items(&reply, "completed");
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "function_call");
    }

    #[test]
    fn a_content_filtered_finish_is_incomplete_not_completed() {
        use covenant_compute_protocol::FinishReason;
        let meter = |fr: Option<FinishReason>| JobMeter {
            wall_ms: 1,
            tokens_in: None,
            tokens_out: None,
            gpu_seconds: None,
            finish_reason: fr,
        };
        // A content-policy cut-off is incomplete with the Responses API's own
        // reason, the same way a token cut-off is — not a clean completed
        // over a partial answer the buyer would read as whole.
        assert_eq!(
            status_for(&meter(Some(FinishReason::ContentFilter))),
            ("incomplete", json!({ "reason": "content_filter" }))
        );
        assert_eq!(
            status_for(&meter(Some(FinishReason::Length))),
            ("incomplete", json!({ "reason": "max_output_tokens" }))
        );
        assert_eq!(status_for(&meter(Some(FinishReason::Stop))).0, "completed");
        assert_eq!(status_for(&meter(None)).0, "completed");
    }
}
