//! An Anthropic-compatible front door for the compute network. A client
//! built for Anthropic's `POST /v1/messages` — the Anthropic SDKs, an
//! agent framework pointed at `ANTHROPIC_BASE_URL`, a bare `curl` — reaches
//! the same network the OpenAI front door serves, and every message is
//! bought on the network, paid, and returned with the operator's signed,
//! locally re-verified work receipt. No client code changes; the buyer's
//! spend stays under the same per-call and session caps every other buyer
//! surface enforces.
//!
//! Text, image content, a system prompt, tool use, and streaming all carry
//! across: a `tools` request rides the signed job, an assistant turn comes
//! back with `tool_use` blocks, a follow-up turn's `tool_result` blocks feed
//! back in, and `stream: true` returns Anthropic's server-sent event
//! sequence, so an agent's tool loop and a live UI both run end to end.
//!
//! The response is a standard Anthropic `message` object. It carries one
//! extra `covenant` field with the verified receipt — a plain Anthropic
//! client ignores it, a Covenant-aware one checks the job on-chain. This
//! is the Anthropic dialect of the same front door; the shared dispatch,
//! pricing, and receipt machinery live in [`crate::openai`] and [`crate`],
//! and only the request/response wire shapes differ.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use covenant_compute_protocol::{
    parse_assistant_output, AssistantReply, CapacityView, ChatMessage, ChatRole, FunctionCall,
    FunctionDefinition, JobKind, JobMeter, NamedFunction, NamedToolChoice, ToolCall, ToolCallKind,
    ToolChoice, ToolChoiceMode, ToolDefinition, ToolKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::openai::{
    bearer_token, covenant_receipt, model_is_served, reputation_floor, OpenAiState,
};
use crate::{
    capacity, cheapest_matching_ask, dispatch_and_verify, stream_and_verify, submit_streaming,
    BuyerError, DispatchOutcome, InferArgs, JobRequest, SpendReservation,
};

/// The Anthropic front-door router: `POST /v1/messages`. Kept separate from
/// [`crate::openai::openai_router`] so the binary can serve both dialects
/// on one address (`openai_router(state).merge(anthropic_router(state))`)
/// without either owning the other's routes.
///
/// The Models API (`GET /v1/models`, `/v1/models/:model`) is not registered
/// here: both dialects answer that one path with different bodies, so the
/// route lives on the OpenAI router and negotiates the shape per request via
/// [`wants_anthropic_dialect`], delegating to [`list_models`]/[`retrieve_model`]
/// below when the caller is an Anthropic client.
pub fn anthropic_router(state: Arc<OpenAiState>) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .with_state(state)
}

/// One `/v1/messages` request as an Anthropic client sends it. A `system`
/// prompt rides the top level, not the message list, and `max_tokens` is
/// required — both Anthropic conventions the OpenAI shape does not share.
#[derive(Debug, Clone, Deserialize)]
struct MessagesRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    system: Option<SystemPrompt>,
    /// Required by Anthropic: the ceiling on tokens to generate. Absent or
    /// zero earns a refusal rather than a silently unbounded completion the
    /// buyer pays for.
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    /// Anthropic's nucleus-by-count knob. This network's serving path
    /// exposes no `top_k`, so a request that sets it earns a refusal rather
    /// than a completion sampled without the control the buyer asked for.
    #[serde(default)]
    top_k: Option<u32>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    stream: bool,
    /// Functions the model may call, Anthropic's `{name, description,
    /// input_schema}` shape. Carried into the signed job so the reply may be
    /// tool calls the buyer runs and feeds back.
    #[serde(default)]
    tools: Vec<AnthropicTool>,
    #[serde(default)]
    tool_choice: Option<AnthropicToolChoice>,
    /// Anthropic's extended-thinking control. This network's serving path
    /// returns a final answer only, with no separate reasoning stream, so a
    /// request that enables thinking earns a refusal rather than a completion
    /// silently missing the reasoning the buyer asked (and would pay) for. An
    /// explicitly disabled block is the client opting out, and rides through.
    #[serde(default)]
    thinking: Option<ThinkingConfig>,
    /// Anthropic's per-request `metadata`, whose one documented field is a
    /// `user_id` abuse-detection hint. This network routes on capability and
    /// price, not caller identity, so it takes no action on `user_id`. It
    /// still validates the object against Anthropic's contract and refuses a
    /// malformed one, rather than accepting arbitrary JSON it would drop.
    #[serde(default)]
    metadata: Option<Value>,
}

/// Anthropic's `thinking` block: `{"type":"enabled","budget_tokens":N}` or
/// `{"type":"disabled"}`. The `type` is read as a free string so a future
/// mode earns a clear refusal naming it, not a bare serde error.
#[derive(Debug, Clone, Deserialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    kind: String,
}

impl ThinkingConfig {
    /// `Ok(())` when the client opted out of thinking; `Err(msg)` when it
    /// asked for a reasoning stream this network's serving path does not
    /// produce.
    fn ensure_supported(&self) -> Result<(), String> {
        if self.kind == "disabled" {
            return Ok(());
        }
        Err(format!(
            "thinking ('{}') is not supported: this network's serving path returns a final \
             answer only, with no separate reasoning stream; omit thinking or set it to \
             disabled",
            self.kind
        ))
    }
}

/// Validates Anthropic's per-request `metadata`. The network takes no action
/// on the `user_id` hint, but it accepts a well-formed object for drop-in
/// compatibility and refuses one real Anthropic would also reject: `metadata`
/// must be an object, and `user_id`, when present, a string of at most 256
/// characters.
fn ensure_metadata_supported(metadata: &Value) -> Result<(), String> {
    let Value::Object(fields) = metadata else {
        return Err("metadata must be an object".to_owned());
    };
    match fields.get("user_id") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(user_id)) if user_id.chars().count() <= 256 => Ok(()),
        Some(Value::String(_)) => Err("metadata.user_id must be at most 256 characters".to_owned()),
        Some(_) => Err("metadata.user_id must be a string".to_owned()),
    }
}

/// One tool an Anthropic client offers. A custom tool carries `{name,
/// description, input_schema}`, where `input_schema` is the JSON-Schema for
/// the call arguments (the protocol's `parameters` under Anthropic's name).
/// Server tools (web search, computer use, code execution) instead carry a
/// versioned `type` such as `web_search_20250305` and run inside Anthropic's
/// own infrastructure, which this network does not host.
#[derive(Debug, Clone, Deserialize)]
struct AnthropicTool {
    /// Absent (or `custom`) for a client-run function tool; a versioned
    /// identifier for a server tool. Read as a free string so a new server
    /// tool earns a clear refusal naming it, not a bare serde error.
    #[serde(rename = "type", default)]
    tool_type: Option<String>,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    input_schema: Option<Value>,
}

impl AnthropicTool {
    fn into_protocol(self) -> Result<ToolDefinition, String> {
        if let Some(kind) = self.tool_type.as_deref() {
            if kind != "custom" {
                return Err(format!(
                    "tool '{}' has type '{kind}': server tools run inside Anthropic's own \
                     infrastructure, which this network does not host; offer it as a custom \
                     tool ({{name, description, input_schema}}) the model calls and your agent runs",
                    self.name
                ));
            }
        }
        Ok(ToolDefinition {
            kind: ToolKind::Function,
            function: FunctionDefinition {
                name: self.name,
                description: self.description,
                parameters: self.input_schema,
            },
        })
    }
}

/// Anthropic's `tool_choice`. `auto` lets the model decide, `any` forces
/// some tool, `tool` forces a named one, `none` forbids them —
/// `disable_parallel_tool_use` is refused, since the network can't promise
/// one call per turn.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicToolChoice {
    Auto {
        #[serde(default)]
        disable_parallel_tool_use: bool,
    },
    Any {
        #[serde(default)]
        disable_parallel_tool_use: bool,
    },
    None,
    Tool {
        name: String,
        #[serde(default)]
        disable_parallel_tool_use: bool,
    },
}

impl AnthropicToolChoice {
    fn into_protocol(self) -> Result<ToolChoice, String> {
        let (choice, disable) = match self {
            AnthropicToolChoice::Auto {
                disable_parallel_tool_use,
            } => (
                ToolChoice::Mode(ToolChoiceMode::Auto),
                disable_parallel_tool_use,
            ),
            AnthropicToolChoice::Any {
                disable_parallel_tool_use,
            } => (
                ToolChoice::Mode(ToolChoiceMode::Required),
                disable_parallel_tool_use,
            ),
            AnthropicToolChoice::None => (ToolChoice::Mode(ToolChoiceMode::None), false),
            AnthropicToolChoice::Tool {
                name,
                disable_parallel_tool_use,
            } => (
                ToolChoice::Named(NamedToolChoice {
                    kind: ToolKind::Function,
                    function: NamedFunction { name },
                }),
                disable_parallel_tool_use,
            ),
        };
        if disable {
            return Err(
                "disable_parallel_tool_use is not supported: this network can't guarantee \
                        one tool call per turn; omit it to take the model's default"
                    .into(),
            );
        }
        Ok(choice)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: MessageContent,
}

/// A message's content: a plain string, or the array of typed blocks the
/// SDKs emit. Text blocks join into the turn's prose; image blocks
/// contribute their inline base64; tool blocks carry a call or its result.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// One content block. `type` is read as a free string so an unsupported
/// block earns a clear refusal naming it, rather than a bare serde error;
/// this mirrors [`crate::openai`]'s `ContentPart` handling. The tool fields
/// are populated only on `tool_use`/`tool_result` blocks.
#[derive(Debug, Clone, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    source: Option<ImageSource>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    content: Option<MessageContent>,
}

/// The `source` of an Anthropic image block. Only an inline base64 source
/// is served; a `url` source is refused, since the network never reaches
/// out to fetch a buyer's image, it relays inline bytes only.
#[derive(Debug, Clone, Deserialize)]
struct ImageSource {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: Option<String>,
}

/// Anthropic's `system` is a plain string or an array of text blocks.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Deserialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

impl SystemPrompt {
    /// Flatten to one system instruction. A non-text system block (Anthropic
    /// allows only text there) is refused rather than dropped.
    fn into_text(self) -> Result<String, String> {
        match self {
            SystemPrompt::Text(s) => Ok(s),
            SystemPrompt::Blocks(blocks) => {
                let mut text = String::new();
                for block in blocks {
                    if block.kind != "text" {
                        return Err(format!(
                            "system block of type '{}' is not supported: system may carry text only",
                            block.kind
                        ));
                    }
                    text.push_str(block.text.as_deref().unwrap_or_default());
                }
                Ok(text)
            }
        }
    }
}

impl MessageContent {
    /// Narrow an assistant turn to one protocol message: its text blocks
    /// joined, and each `tool_use` block a replayed tool call. An image or
    /// `tool_result` block does not belong on an assistant turn and is
    /// refused.
    fn into_assistant(self) -> Result<ChatMessage, String> {
        match self {
            MessageContent::Text(s) => Ok(ChatMessage::assistant(s)),
            MessageContent::Blocks(blocks) => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for block in blocks {
                    match block.kind.as_str() {
                        "text" => text.push_str(block.text.as_deref().unwrap_or_default()),
                        "tool_use" => tool_calls.push(tool_use_to_call(block)?),
                        "tool_result" => {
                            return Err("a tool_result block belongs to a user turn, \
                                        not an assistant turn"
                                .into())
                        }
                        "image" => {
                            return Err("an assistant turn carries text and tool_use blocks, \
                                        not images"
                                .into())
                        }
                        other => {
                            return Err(format!(
                                "assistant content block of type '{other}' is not supported"
                            ))
                        }
                    }
                }
                Ok(ChatMessage {
                    role: ChatRole::Assistant,
                    content: text,
                    images: Vec::new(),
                    tool_calls,
                    tool_call_id: None,
                })
            }
        }
    }

    /// Narrow a user turn to protocol messages. Text and image blocks make
    /// one user message; each `tool_result` block is its own tool message
    /// (the protocol carries one result per message), so a turn that answers
    /// several parallel calls fans out to several tool messages. A
    /// `tool_use` block does not belong on a user turn and is refused.
    fn into_user_messages(self) -> Result<Vec<ChatMessage>, String> {
        match self {
            MessageContent::Text(s) => Ok(vec![ChatMessage::user(s)]),
            MessageContent::Blocks(blocks) => {
                let mut out = Vec::new();
                let mut text = String::new();
                let mut images = Vec::new();
                for block in blocks {
                    match block.kind.as_str() {
                        "text" => text.push_str(block.text.as_deref().unwrap_or_default()),
                        "image" => images.push(image_block_base64(block.source)?),
                        "tool_result" => out.push(tool_result_to_message(block)?),
                        "tool_use" => {
                            return Err("a tool_use block belongs to an assistant turn, \
                                        not a user turn"
                                .into())
                        }
                        other => {
                            return Err(format!(
                                "user content block of type '{other}' is not supported: \
                                 this endpoint serves text, image, and tool_result content"
                            ))
                        }
                    }
                }
                if !text.is_empty() || !images.is_empty() {
                    out.push(ChatMessage {
                        role: ChatRole::User,
                        content: text,
                        images,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                }
                if out.is_empty() {
                    out.push(ChatMessage::user(String::new()));
                }
                Ok(out)
            }
        }
    }
}

/// Turn a `tool_use` block into a replayed protocol tool call. Its `input`
/// object serializes back to the arguments string the protocol carries.
fn tool_use_to_call(block: ContentBlock) -> Result<ToolCall, String> {
    let id = block.id.ok_or("a tool_use block carries no id")?;
    let name = block.name.ok_or("a tool_use block carries no name")?;
    let input = block.input.unwrap_or_else(|| json!({}));
    Ok(ToolCall {
        id,
        kind: ToolCallKind::Function,
        function: FunctionCall {
            name,
            arguments: input.to_string(),
        },
    })
}

/// Turn a `tool_result` block into a protocol tool message answering the
/// call it names.
fn tool_result_to_message(block: ContentBlock) -> Result<ChatMessage, String> {
    let tool_use_id = block
        .tool_use_id
        .ok_or("a tool_result block carries no tool_use_id")?;
    let content = match block.content {
        None => String::new(),
        Some(MessageContent::Text(s)) => s,
        Some(MessageContent::Blocks(blocks)) => {
            let mut text = String::new();
            for block in blocks {
                if block.kind != "text" {
                    return Err("a tool_result carries text content only".into());
                }
                text.push_str(block.text.as_deref().unwrap_or_default());
            }
            text
        }
    };
    Ok(ChatMessage::tool(tool_use_id, content))
}

/// Pull the raw base64 out of an Anthropic image block's source, the only
/// image form this endpoint serves. A `url` source is refused; a base64
/// source whose data does not decode is rejected here rather than paid for
/// and failed at a backend.
fn image_block_base64(source: Option<ImageSource>) -> Result<String, String> {
    let source = source.ok_or("an image block carries no source")?;
    if source.kind != "base64" {
        return Err(format!(
            "image source of type '{}' is not supported: inline the image as a base64 source",
            source.kind
        ));
    }
    let data = source
        .data
        .ok_or("an image block's base64 source carries no data")?;
    let data: String = data.split_whitespace().collect();
    if data.is_empty() {
        return Err("an image block carries no image bytes".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|_| "an image block's source is not valid base64".to_string())?;
    Ok(data)
}

impl AnthropicMessage {
    /// Narrow to protocol messages. Anthropic carries only `user` and
    /// `assistant` roles in the message list (system is top-level), so any
    /// other role is refused; a user turn may fan out to several messages
    /// (see [`MessageContent::into_user_messages`]).
    fn into_protocol_messages(self) -> Result<Vec<ChatMessage>, String> {
        match self.role.as_str() {
            "assistant" => Ok(vec![self.content.into_assistant()?]),
            "user" => self.content.into_user_messages(),
            other => Err(format!(
                "message role '{other}' is not supported: /v1/messages carries user and \
                 assistant turns (put a system prompt in the top-level system field)"
            )),
        }
    }
}

async fn messages(
    State(state): State<Arc<OpenAiState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    let req: MessagesRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("could not parse the request body: {e}"),
            )
        }
    };

    if req.model.trim().is_empty() {
        return bad_request("model is required");
    }
    if req.messages.is_empty() {
        return bad_request("messages must not be empty");
    }
    let max_tokens = match req.max_tokens {
        Some(0) | None => return bad_request("max_tokens is required and must be at least 1"),
        Some(n) => n,
    };
    if req.top_k.is_some() {
        return bad_request(
            "top_k is not supported: this network's serving path exposes no top_k; \
             use temperature or top_p",
        );
    }
    if let Some(thinking) = &req.thinking {
        if let Err(msg) = thinking.ensure_supported() {
            return bad_request(msg);
        }
    }
    if let Some(metadata) = &req.metadata {
        if let Err(msg) = ensure_metadata_supported(metadata) {
            return bad_request(msg);
        }
    }

    let min_reputation_bps = match reputation_floor(&headers) {
        Ok(floor) => floor,
        Err(msg) => return bad_request(msg),
    };
    let stream = req.stream;
    let model = req.model.clone();

    // Prepend the system prompt as a system turn, then narrow each message
    // to the protocol's canonical form, before anything is priced or signed.
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = req.system {
        match system.into_text() {
            Ok(text) if !text.is_empty() => messages.push(ChatMessage::system(text)),
            Ok(_) => {}
            Err(msg) => return bad_request(msg),
        }
    }
    for message in req.messages {
        match message.into_protocol_messages() {
            Ok(mut turns) => messages.append(&mut turns),
            Err(msg) => return bad_request(msg),
        }
    }

    let tools = if req.tools.is_empty() {
        None
    } else {
        match req
            .tools
            .into_iter()
            .map(AnthropicTool::into_protocol)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(defs) => Some(defs),
            Err(msg) => return bad_request(msg),
        }
    };
    let tool_choice = match req.tool_choice {
        Some(tc) => match tc.into_protocol() {
            Ok(choice) => Some(choice),
            Err(msg) => return bad_request(msg),
        },
        None => None,
    };

    // Pack the input through the same shared arguments every buyer surface
    // uses, so a message here means the same job as a chat on the CLI, the
    // MCP server, and the OpenAI front door.
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
        max_tokens: Some(max_tokens),
        seed: None,
        presence_penalty: None,
        frequency_penalty: None,
        logprobs: None,
        stop: req.stop_sequences,
        response_format: None,
        tools,
        tool_choice,
        idempotency_key: None,
        dry_run: false,
    };
    let input = match args.input() {
        Ok(input) => input,
        Err(e) => return bad_request(e),
    };

    // Refuse a model the network is not serving before pricing it — the
    // SDK-correct 404, not a doomed submit that comes back as a bad gateway
    // naming an internal job.
    if let Err(resp) = ensure_model_servable(&state, &model).await {
        return resp;
    }

    // Anthropic requests carry no price. Offer the cheapest matching ask,
    // held under the per-call ceiling — the same default-price behaviour
    // every other price-less buyer surface takes.
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
    if stream {
        return stream_messages(state, model, request, reservation).await;
    }
    match dispatch_settling(state, request, reservation).await {
        Ok(outcome) => Json(message_response(&model, &outcome)).into_response(),
        Err(response) => response,
    }
}

/// The streaming variant of a message: Anthropic's server-sent event
/// sequence (`message_start`, a text `content_block`, then `message_delta`
/// and `message_stop`) as the operator relays tokens. The signed receipt
/// is still verified before the reservation settles — the live feed is a
/// preview, the receipt over the final output is the paid artifact, exactly
/// as the non-streaming path treats it.
///
/// The job is submitted inside the spawned task, not awaited in this
/// request frame: a client disconnect during submit would otherwise cancel
/// the handler and drop the reservation while the coordinator already holds
/// escrow, leaving real spend uncounted against the session cap. Spawning
/// first gives the stream the same cancellation-safety `dispatch_settling`
/// gives the non-streaming path. Mirrors the OpenAI door's `stream_chat`.
async fn stream_messages(
    state: Arc<OpenAiState>,
    model: String,
    request: JobRequest,
    reservation: SpendReservation,
) -> Response {
    let job_id = Uuid::new_v4();
    let id = format!("msg_{}", job_id.simple());
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), BuyerError>>();

    // The exact text the live feed delivered. The feed is a best-effort
    // preview; the signed receipt is the paid artifact, so it is reconciled
    // against the verified output below rather than passed off as complete.
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

        // The message opens with no content blocks. The text block is opened
        // lazily — on the first live delta here, or from the verified output
        // below — so a turn that is pure tool calls carries no empty leading
        // text block, matching Anthropic's own stream. Real input_tokens are
        // known only from the receipt (the network meters post-hoc), so
        // message_start reports zero and the closing message_delta carries the
        // metered counts.
        let _ = tx.send(named_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": feed_model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        ));

        let text_open = Arc::new(AtomicBool::new(false));
        let delta_open = Arc::clone(&text_open);
        let delta_tx = tx.clone();
        let drained = stream_and_verify(
            &state.http,
            &state.buyer,
            &state.identity,
            envelope,
            move |delta| {
                if !delta_open.swap(true, Ordering::Relaxed) {
                    let _ = delta_tx.send(text_block_start_event());
                }
                if let Ok(mut feed) = shown_feed.lock() {
                    feed.push_str(delta);
                }
                let _ = delta_tx.send(text_delta_event(delta));
            },
        )
        .await;
        match drained {
            Ok(streamed) => {
                reservation.settle();
                let reply = parse_assistant_output(&streamed.outcome.output);
                // Complete the text block from the verified output: when the
                // feed is a prefix (the common lost-tail or can't-stream case)
                // only the missing remainder is sent; when it diverged, the
                // verified text supersedes the preview. A block the live feed
                // already opened is closed even if the verified text is empty;
                // a block never opened is opened now only when there is text to
                // carry, so a pure tool-call turn emits no text block at all.
                let shown = shown.lock().map(|feed| feed.clone()).unwrap_or_default();
                let missing = match reply.text.strip_prefix(shown.as_str()) {
                    Some(tail) => tail,
                    None => reply.text.as_str(),
                };
                let mut text_opened = text_open.load(Ordering::Relaxed);
                if !text_opened && !reply.text.is_empty() {
                    let _ = tx.send(text_block_start_event());
                    text_opened = true;
                }
                if text_opened {
                    if !missing.is_empty() {
                        let _ = tx.send(text_delta_event(missing));
                    }
                    let _ = tx.send(named_event(
                        "content_block_stop",
                        json!({ "type": "content_block_stop", "index": 0 }),
                    ));
                }
                // Tool calls never ride the live feed (a tools job runs the
                // backend non-streaming), so each is emitted whole from the
                // verified output as its own tool_use block. They follow the
                // text block when there is one, else open at index 0.
                let base = if text_opened { 1 } else { 0 };
                for (offset, call) in reply.tool_calls.iter().enumerate() {
                    let index = base + offset;
                    let args: Value = serde_json::from_str(&call.function.arguments)
                        .unwrap_or_else(|_| json!({}));
                    let _ = tx.send(named_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.function.name,
                                "input": {},
                            },
                        }),
                    ));
                    let _ = tx.send(named_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "input_json_delta", "partial_json": args.to_string() },
                        }),
                    ));
                    let _ = tx.send(named_event(
                        "content_block_stop",
                        json!({ "type": "content_block_stop", "index": index }),
                    ));
                }
                let _ = tx.send(message_delta_event(&reply, &streamed.outcome));
                let _ = tx.send(named_event(
                    "message_stop",
                    json!({ "type": "message_stop" }),
                ));
            }
            Err(e) => {
                // The buy never settled — release the hold. The stream is
                // already a 200, so the failure rides back as a terminal
                // error event the client reads, the shape Anthropic's own API
                // uses mid-stream.
                drop(reservation);
                let _ = tx.send(named_event(
                    "error",
                    json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": e.to_string() },
                    }),
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
        Err(_) => anthropic_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "the streaming dispatch task did not start",
        ),
    }
}

/// One named Anthropic SSE frame: the `event:` line the SDK dispatches on,
/// carrying its typed `data:` payload.
fn named_event(name: &str, data: Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

/// The `content_block_start` frame that opens the message's leading text
/// block (index 0). Emitted lazily — on the first live delta or from the
/// verified output — so a pure tool-call turn never opens it.
fn text_block_start_event() -> Event {
    named_event(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" },
        }),
    )
}

/// A `content_block_delta` frame carrying one text delta on the message's
/// first block.
fn text_delta_event(text: &str) -> Event {
    named_event(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": text },
        }),
    )
}

/// The Anthropic `stop_reason` for a settled turn. The operator's signed
/// `finish_reason` is authoritative: a tool call the backend cut off at
/// `length` or stopped for `content_filter` reports `max_tokens` /
/// `refusal`, never a clean `tool_use` — the same truncation the node's
/// `for_tool_turn` preserves, so a partial tool call is never handed to the
/// client as one that ended cleanly. Only when the backend reported no
/// reason at all does the presence of tool calls choose between `tool_use`
/// and `end_turn`.
fn anthropic_stop_reason(meter: &JobMeter, has_tool_calls: bool) -> &'static str {
    match meter.finish_reason {
        Some(reason) => reason.as_anthropic(),
        None if has_tool_calls => "tool_use",
        None => "end_turn",
    }
}

/// The closing `message_delta` frame: the final stop reason, the metered
/// token counts (including the input count message_start could not yet
/// know), and the `covenant` proof a Covenant-aware client verifies.
fn message_delta_event(reply: &AssistantReply, outcome: &DispatchOutcome) -> Event {
    let meter: &JobMeter = &outcome.receipt.receipt.meter;
    let stop_reason = anthropic_stop_reason(meter, !reply.tool_calls.is_empty());
    named_event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason, "stop_sequence": Value::Null },
            "usage": {
                "input_tokens": meter.tokens_in.unwrap_or(0),
                "output_tokens": meter.tokens_out.unwrap_or(0),
            },
            "covenant": covenant_receipt(outcome),
        }),
    )
}

/// Dispatch a paid job on a detached task so its settlement survives a
/// client disconnect. Once the envelope is submitted the coordinator holds
/// escrow and an operator can complete it whether or not the client waits,
/// so the reservation must settle on success and release only on a real
/// failure — letting the request future's cancellation drop it would leave
/// real spend uncounted against the session cap. This mirrors the OpenAI
/// front door's `dispatch_settling`, shaped to Anthropic's error body.
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
        Err(_) => Err(anthropic_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "the dispatch task did not complete",
        )),
    }
}

/// Whether the network is serving `model` for a message (an
/// `inference_call` job); returns the SDK-correct 404 otherwise.
async fn ensure_model_servable(state: &OpenAiState, model: &str) -> Result<(), Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    if model_is_served(&view, model, JobKind::InferenceCall) {
        Ok(())
    } else {
        Err(anthropic_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("model '{model}' is not being served by any operator on the network"),
        ))
    }
}

/// `POST /v1/messages/count_tokens` — Anthropic's pre-flight token count.
/// This network has no local tokenizer and meters tokens only on the
/// operator's signed receipt once a job runs, so it cannot return an honest
/// pre-flight count. It refuses with a clear message naming the reason rather
/// than a guessed number a buyer would price its budget against, or the bare
/// framework 404 an unregistered route would give a real SDK caller.
async fn count_tokens(State(state): State<Arc<OpenAiState>>, headers: HeaderMap) -> Response {
    if let Some(unauthorized) = authorize(&state, &headers) {
        return unauthorized;
    }
    anthropic_error(
        StatusCode::NOT_FOUND,
        "not_found_error",
        "this network does not offer a pre-flight token count: it has no local tokenizer and \
         meters tokens on the operator's signed receipt once a job runs",
    )
}

/// Whether a request to the shared Models API path wants the Anthropic
/// dialect rather than the OpenAI one. The two APIs collide on `/v1/models`,
/// so the OpenAI router owns the route and calls this to pick the body shape.
/// An Anthropic client is identified by a header the OpenAI SDK never sends:
/// `anthropic-version` (which every Anthropic SDK sets) or the Anthropic-only
/// `x-api-key` credential.
pub(crate) fn wants_anthropic_dialect(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version") || headers.contains_key("x-api-key")
}

/// The concrete chat model ids the network is serving right now. Reads the
/// coordinator's live capacity directory, then narrows it with
/// [`chat_model_ids`].
async fn chat_models(state: &OpenAiState) -> Result<Vec<String>, Response> {
    let view = capacity(&state.http, &state.buyer)
        .await
        .map_err(map_dispatch_error)?;
    Ok(chat_model_ids(&view))
}

/// The concrete chat model ids in `view` — the `inference_call` models a
/// client can name in a message — sorted and de-duplicated. Only that one
/// kind appears: the Anthropic front door serves a single endpoint,
/// `/v1/messages`, so listing an embedding, transcription, or speech model
/// would advertise something no `/v1/messages` call can use. The wildcard
/// `any` node advertises no concrete id, so it contributes nothing.
fn chat_model_ids(view: &CapacityView) -> Vec<String> {
    let mut ids: Vec<String> = view
        .entries
        .iter()
        .filter(|e| e.kind == JobKind::InferenceCall && e.model != "any")
        .map(|e| e.model.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// One Anthropic `model` object. `display_name` is the served id itself —
/// the network carries no curated label for an operator's model, and
/// inventing one would misdescribe it — and `created_at` is the Unix epoch,
/// the same "unknown" marker the OpenAI door stamps, since the network does
/// not track when a model was released.
fn model_object(id: &str) -> Value {
    json!({
        "type": "model",
        "id": id,
        "display_name": id,
        "created_at": "1970-01-01T00:00:00Z",
    })
}

/// `GET /v1/models` in the Anthropic dialect — the list half of Anthropic's
/// Models API, what `client.models.list()` reads to discover the network's
/// chat models. Reached from the OpenAI router when [`wants_anthropic_dialect`]
/// holds. The page carries every served model at once: the network's
/// directory is small and unpaginated, so `has_more` is always false and the
/// cursor ids bound this single page (both null when nothing is being served).
pub(crate) async fn list_models(state: &OpenAiState, headers: &HeaderMap) -> Response {
    if let Some(unauthorized) = authorize(state, headers) {
        return unauthorized;
    }
    let ids = match chat_models(state).await {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let data: Vec<Value> = ids.iter().map(|id| model_object(id)).collect();
    Json(json!({
        "data": data,
        "first_id": ids.first(),
        "has_more": false,
        "last_id": ids.last(),
    }))
    .into_response()
}

/// `GET /v1/models/:model` in the Anthropic dialect — the retrieve half, what
/// `client.models.retrieve` reads. Returns the model object when the network
/// is serving it for messages, else Anthropic's `not_found_error`. Like the
/// list, only a concretely-advertised chat model resolves; a model reachable
/// solely through a wildcard node is not a named catalog entry.
pub(crate) async fn retrieve_model(
    state: &OpenAiState,
    model: &str,
    headers: &HeaderMap,
) -> Response {
    if let Some(unauthorized) = authorize(state, headers) {
        return unauthorized;
    }
    let ids = match chat_models(state).await {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    if ids.iter().any(|id| id == model) {
        Json(model_object(model)).into_response()
    } else {
        anthropic_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("model '{model}' is not being served by any operator on the network"),
        )
    }
}

/// The signed, re-verified receipt shaped into an Anthropic `message`, plus
/// a `covenant` field carrying the proof an Anthropic client ignores and a
/// Covenant-aware one verifies on-chain. Prose becomes a `text` block and
/// each attested tool call a `tool_use` block; a turn that called a tool
/// stops for `tool_use` so the client runs it and continues — unless the
/// backend cut that turn off, whose signed `length`/`content_filter`
/// carries through as `max_tokens`/`refusal` so a truncated tool call is
/// never read as a clean one.
fn message_response(model: &str, outcome: &DispatchOutcome) -> Value {
    let receipt = &outcome.receipt.receipt;
    let reply = parse_assistant_output(&outcome.output);

    let mut content = Vec::new();
    if !reply.text.is_empty() {
        content.push(json!({ "type": "text", "text": reply.text }));
    }
    for call in &reply.tool_calls {
        // The attested arguments are a JSON string; Anthropic's `input` is
        // the object itself. A real backend's arguments parse to an object;
        // anything that does not is surfaced as an empty object rather than
        // a malformed block.
        let args: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
        content.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.function.name,
            "input": args,
        }));
    }
    // Anthropic always returns at least one content block.
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let stop_reason = anthropic_stop_reason(&receipt.meter, !reply.tool_calls.is_empty());

    json!({
        "id": format!("msg_{}", receipt.job_id.simple()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": receipt.meter.tokens_in.unwrap_or(0),
            "output_tokens": receipt.meter.tokens_out.unwrap_or(0),
        },
        "covenant": covenant_receipt(outcome),
    })
}

/// Maps a dispatch failure onto an Anthropic-shaped error with a fitting
/// status: an underfunded buyer reads as a rate limit (Anthropic's quota
/// shape has no 402), a timeout or upstream fault as a server-side error.
fn map_dispatch_error(e: BuyerError) -> Response {
    let (status, kind) = match &e {
        _ if e.is_underfunded() => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        BuyerError::ReceiptTimeout(_) => (StatusCode::GATEWAY_TIMEOUT, "api_error"),
        BuyerError::Rpc(_) | BuyerError::Protocol(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "api_error")
        }
        BuyerError::SubmitRefused { .. }
        | BuyerError::NotServed { .. }
        | BuyerError::Coordinator(_)
        | BuyerError::Unreachable { .. }
        | BuyerError::Verification(_) => (StatusCode::BAD_GATEWAY, "api_error"),
    };
    anthropic_error(status, kind, e.to_string())
}

/// Shapes a spend-cap refusal into the Anthropic error a client expects: an
/// over-ceiling offer is a bad request, a full session cap is a rate limit.
fn reservation_refusal(msg: String) -> Response {
    if msg.contains("per-call ceiling") {
        bad_request(msg)
    } else {
        anthropic_error(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", msg)
    }
}

/// `None` when the request may proceed; `Some(response)` is the ready 401
/// when a configured key is missing or wrong. Anthropic clients send
/// `x-api-key`; an `Authorization: Bearer` is also accepted so the two front
/// doors take the same credential.
fn authorize(state: &OpenAiState, headers: &HeaderMap) -> Option<Response> {
    let expected = state.api_key.as_deref()?;
    let presented = api_key_header(headers)
        .or_else(|| bearer_token(headers))
        .unwrap_or_default();
    if presented.as_bytes().ct_eq(expected.as_bytes()).into() {
        None
    } else {
        Some(anthropic_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing or invalid x-api-key",
        ))
    }
}

fn api_key_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-api-key")?
        .to_str()
        .ok()
        .map(|v| v.trim().to_string())
}

fn bad_request(message: impl Into<String>) -> Response {
    anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", message)
}

/// Anthropic's error envelope: `{"type":"error","error":{"type","message"}}`.
fn anthropic_error(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Response {
    let body = json!({
        "type": "error",
        "error": { "type": kind, "message": message.into() },
    });
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reason_trusts_the_signed_finish_reason_over_the_tool_call_heuristic() {
        use covenant_compute_protocol::FinishReason;
        let meter = |finish_reason| JobMeter {
            wall_ms: 1,
            tokens_in: None,
            tokens_out: None,
            gpu_seconds: None,
            finish_reason,
        };

        // A tool call the backend cut off carries its truncation through as
        // `max_tokens`/`refusal`, never a clean `tool_use`: the node's
        // `for_tool_turn` keeps the `length`/`content_filter`, and the front
        // door must not paper over it.
        assert_eq!(
            anthropic_stop_reason(&meter(Some(FinishReason::Length)), true),
            "max_tokens"
        );
        assert_eq!(
            anthropic_stop_reason(&meter(Some(FinishReason::ContentFilter)), true),
            "refusal"
        );
        // A clean tool turn still stops for `tool_use`.
        assert_eq!(
            anthropic_stop_reason(&meter(Some(FinishReason::ToolCalls)), true),
            "tool_use"
        );
        // No backend reason at all: the shape of the reply decides.
        assert_eq!(anthropic_stop_reason(&meter(None), true), "tool_use");
        assert_eq!(anthropic_stop_reason(&meter(None), false), "end_turn");
        // A prose turn reports its signed reason verbatim.
        assert_eq!(
            anthropic_stop_reason(&meter(Some(FinishReason::Stop)), false),
            "end_turn"
        );
        assert_eq!(
            anthropic_stop_reason(&meter(Some(FinishReason::Length)), false),
            "max_tokens"
        );
    }

    #[test]
    fn a_truncated_tool_call_reports_max_tokens_not_a_clean_tool_use() {
        use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
        use covenant_compute_protocol::{
            CapabilityRequirement, FinishReason, JobEnvelopePayload, SignedJobEnvelope,
            SignedWorkReceipt, WorkReceiptPayload,
        };
        use covenant_identity::LocalIdentity;
        use covenant_mcp::Content;

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
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "truncated-tool"),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        };
        let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();

        // The model began a tool call and the backend hit the token budget
        // mid-arguments, so the receipt is signed `length` even though a tool
        // call is present in the output.
        let output = vec![Content::json(json!({
            "tool_calls": [{
                "id": "toolu_1",
                "type": "function",
                "function": { "name": "get_weather", "arguments": "{\"city\":\"Par" },
            }]
        }))];
        let receipt = SignedWorkReceipt::sign(
            WorkReceiptPayload {
                job_id,
                operator: operator.agent_id(),
                job_hash_hex: "00".repeat(32),
                result_hash_hex: "11".repeat(32),
                meter: JobMeter {
                    wall_ms: 5,
                    tokens_in: Some(3),
                    tokens_out: Some(16),
                    gpu_seconds: None,
                    finish_reason: Some(FinishReason::Length),
                },
                price_micro_usdc: 10_000,
                status: A2ATaskStatus::Ok,
                executed_at_ms: 2,
                node_audit_root_hex: "cc".repeat(32),
            },
            &operator,
        )
        .unwrap();
        let outcome = DispatchOutcome {
            envelope,
            receipt,
            output,
            payout: None,
        };

        let rendered = message_response("claude-compat", &outcome);
        assert_eq!(
            rendered["stop_reason"], "max_tokens",
            "a tool call the backend truncated must not be reported as a clean tool_use"
        );
        // The partial tool call is still surfaced so the client can see what
        // was being attempted when the budget ran out.
        assert_eq!(rendered["content"][0]["type"], "tool_use");
    }

    #[test]
    fn a_string_content_message_narrows_to_a_user_turn() {
        let msg = AnthropicMessage {
            role: "user".into(),
            content: MessageContent::Text("hi".into()),
        };
        let turns = msg.into_protocol_messages().expect("narrow");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, ChatRole::User);
        assert_eq!(turns[0].content, "hi");
        assert!(turns[0].images.is_empty());
    }

    #[test]
    fn text_and_image_blocks_split_into_prose_and_base64() {
        let clip = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        let body = format!(
            r#"{{"role":"user","content":[
                {{"type":"text","text":"look:"}},
                {{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{clip}"}}}}
            ]}}"#
        );
        let msg: AnthropicMessage = serde_json::from_str(&body).expect("parse");
        let turns = msg.into_protocol_messages().expect("narrow");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].content, "look:");
        assert_eq!(turns[0].images, vec![clip]);
    }

    #[test]
    fn a_remote_image_source_is_refused() {
        let body = r#"{"role":"user","content":[
            {"type":"image","source":{"type":"url","url":"https://example.com/x.png"}}
        ]}"#;
        let msg: AnthropicMessage = serde_json::from_str(body).expect("parse");
        let err = msg.into_protocol_messages().expect_err("refuse url source");
        assert!(err.contains("base64"), "{err}");
    }

    #[test]
    fn an_assistant_tool_use_block_becomes_a_replayed_call() {
        let body = r#"{"role":"assistant","content":[
            {"type":"text","text":"let me check"},
            {"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Paris"}}
        ]}"#;
        let msg: AnthropicMessage = serde_json::from_str(body).expect("parse");
        let turns = msg.into_protocol_messages().expect("narrow");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, ChatRole::Assistant);
        assert_eq!(turns[0].content, "let me check");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].id, "toolu_1");
        assert_eq!(turns[0].tool_calls[0].function.name, "get_weather");
        assert_eq!(
            turns[0].tool_calls[0].function.arguments,
            r#"{"city":"Paris"}"#
        );
    }

    #[test]
    fn a_user_turn_with_two_tool_results_fans_out_to_two_tool_messages() {
        let body = r#"{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","content":"18C"},
            {"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"text","text":"clear"}]},
            {"type":"text","text":"thanks"}
        ]}"#;
        let msg: AnthropicMessage = serde_json::from_str(body).expect("parse");
        let turns = msg.into_protocol_messages().expect("narrow");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, ChatRole::Tool);
        assert_eq!(turns[0].tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(turns[0].content, "18C");
        assert_eq!(turns[1].role, ChatRole::Tool);
        assert_eq!(turns[1].tool_call_id.as_deref(), Some("toolu_2"));
        assert_eq!(turns[1].content, "clear");
        // The trailing prose is its own user turn, after the results.
        assert_eq!(turns[2].role, ChatRole::User);
        assert_eq!(turns[2].content, "thanks");
    }

    #[test]
    fn a_tool_use_on_a_user_turn_is_refused() {
        let body = r#"{"role":"user","content":[
            {"type":"tool_use","id":"toolu_1","name":"x","input":{}}
        ]}"#;
        let msg: AnthropicMessage = serde_json::from_str(body).expect("parse");
        let err = msg
            .into_protocol_messages()
            .expect_err("refuse tool_use on user");
        assert!(err.contains("assistant turn"), "{err}");
    }

    #[test]
    fn a_system_role_in_the_message_list_is_refused() {
        let msg = AnthropicMessage {
            role: "system".into(),
            content: MessageContent::Text("be terse".into()),
        };
        let err = msg
            .into_protocol_messages()
            .expect_err("refuse system role");
        assert!(err.contains("system field"), "{err}");
    }

    #[test]
    fn tool_choice_modes_map_to_the_protocol() {
        let auto: AnthropicToolChoice = serde_json::from_str(r#"{"type":"auto"}"#).unwrap();
        assert_eq!(
            auto.into_protocol().unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Auto)
        );
        let any: AnthropicToolChoice = serde_json::from_str(r#"{"type":"any"}"#).unwrap();
        assert_eq!(
            any.into_protocol().unwrap(),
            ToolChoice::Mode(ToolChoiceMode::Required)
        );
        let named: AnthropicToolChoice =
            serde_json::from_str(r#"{"type":"tool","name":"get_weather"}"#).unwrap();
        assert_eq!(
            named.into_protocol().unwrap(),
            ToolChoice::Named(NamedToolChoice {
                kind: ToolKind::Function,
                function: NamedFunction {
                    name: "get_weather".into()
                },
            })
        );
    }

    #[test]
    fn disable_parallel_tool_use_is_refused() {
        let choice: AnthropicToolChoice =
            serde_json::from_str(r#"{"type":"auto","disable_parallel_tool_use":true}"#).unwrap();
        let err = choice.into_protocol().expect_err("refuse the guarantee");
        assert!(err.contains("disable_parallel_tool_use"), "{err}");
    }

    #[test]
    fn a_tool_maps_input_schema_to_parameters() {
        let tool: AnthropicTool = serde_json::from_str(
            r#"{"name":"get_weather","description":"look up weather","input_schema":{"type":"object"}}"#,
        )
        .unwrap();
        let def = tool.into_protocol().expect("a custom tool maps cleanly");
        assert_eq!(def.function.name, "get_weather");
        assert_eq!(def.function.description.as_deref(), Some("look up weather"));
        assert_eq!(def.function.parameters, Some(json!({ "type": "object" })));
    }

    #[test]
    fn an_explicit_custom_type_still_maps() {
        let tool: AnthropicTool = serde_json::from_str(
            r#"{"type":"custom","name":"get_weather","input_schema":{"type":"object"}}"#,
        )
        .unwrap();
        let def = tool
            .into_protocol()
            .expect("type:custom is a client-run function tool");
        assert_eq!(def.function.name, "get_weather");
    }

    #[test]
    fn a_server_tool_is_refused_naming_its_type() {
        let tool: AnthropicTool =
            serde_json::from_str(r#"{"type":"web_search_20250305","name":"web_search"}"#).unwrap();
        let err = tool
            .into_protocol()
            .expect_err("a server tool this network can't host is refused");
        assert!(err.contains("web_search_20250305"), "{err}");
        assert!(err.contains("web_search"), "{err}");
    }

    #[test]
    fn enabled_thinking_is_refused_naming_it() {
        let cfg: ThinkingConfig =
            serde_json::from_str(r#"{"type":"enabled","budget_tokens":512}"#).unwrap();
        let err = cfg
            .ensure_supported()
            .expect_err("there is no reasoning stream to hand back");
        assert!(err.contains("thinking"), "{err}");
        assert!(err.contains("enabled"), "{err}");
    }

    #[test]
    fn disabled_thinking_rides_through() {
        let cfg: ThinkingConfig = serde_json::from_str(r#"{"type":"disabled"}"#).unwrap();
        cfg.ensure_supported().expect("an opt-out is honored");
    }

    #[test]
    fn a_well_formed_metadata_rides_through() {
        let metadata: Value = serde_json::from_str(r#"{"user_id":"a1b2c3"}"#).unwrap();
        ensure_metadata_supported(&metadata).expect("a valid user_id is honored");
        let empty: Value = serde_json::from_str(r#"{}"#).unwrap();
        ensure_metadata_supported(&empty).expect("an empty metadata object is fine");
        let null_id: Value = serde_json::from_str(r#"{"user_id":null}"#).unwrap();
        ensure_metadata_supported(&null_id).expect("a null user_id is fine");
    }

    #[test]
    fn a_non_object_metadata_is_refused() {
        let metadata: Value = serde_json::from_str(r#""just-a-string""#).unwrap();
        let err =
            ensure_metadata_supported(&metadata).expect_err("a non-object metadata is refused");
        assert!(err.contains("metadata must be an object"), "{err}");
    }

    #[test]
    fn a_non_string_user_id_is_refused() {
        let metadata: Value = serde_json::from_str(r#"{"user_id":42}"#).unwrap();
        let err = ensure_metadata_supported(&metadata).expect_err("a numeric user_id is refused");
        assert!(err.contains("user_id"), "{err}");
        assert!(err.contains("string"), "{err}");
    }

    #[test]
    fn an_overlong_user_id_is_refused() {
        let body = format!(r#"{{"user_id":"{}"}}"#, "x".repeat(257));
        let metadata: Value = serde_json::from_str(&body).unwrap();
        let err = ensure_metadata_supported(&metadata)
            .expect_err("a user_id past 256 characters is refused");
        assert!(err.contains("256"), "{err}");

        let at_limit = format!(r#"{{"user_id":"{}"}}"#, "x".repeat(256));
        let ok: Value = serde_json::from_str(&at_limit).unwrap();
        ensure_metadata_supported(&ok).expect("a user_id of exactly 256 characters is honored");
    }

    #[test]
    fn a_system_prompt_flattens_text_blocks_and_refuses_other_kinds() {
        let text = SystemPrompt::Blocks(vec![
            SystemBlock {
                kind: "text".into(),
                text: Some("a".into()),
            },
            SystemBlock {
                kind: "text".into(),
                text: Some("b".into()),
            },
        ])
        .into_text()
        .expect("flatten");
        assert_eq!(text, "ab");

        let err = SystemPrompt::Blocks(vec![SystemBlock {
            kind: "image".into(),
            text: None,
        }])
        .into_text()
        .expect_err("refuse non-text system block");
        assert!(err.contains("text only"), "{err}");
    }

    #[test]
    fn an_image_block_with_no_source_or_no_data_is_refused() {
        assert!(image_block_base64(None).is_err());
        assert!(image_block_base64(Some(ImageSource {
            kind: "base64".into(),
            data: None,
        }))
        .is_err());
        assert!(image_block_base64(Some(ImageSource {
            kind: "base64".into(),
            data: Some("!!!not base64!!!".into()),
        }))
        .is_err());
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
    fn chat_model_ids_lists_only_inference_models_sorted_and_deduped() {
        let view = capacity_of(&[
            (JobKind::InferenceCall, "qwen2.5:0.5b"),
            (JobKind::InferenceCall, "llama3.2"),
            // A second operator serving the same chat model — one entry, not two.
            (JobKind::InferenceCall, "qwen2.5:0.5b"),
            // Other kinds a `/v1/messages` call cannot use are left out.
            (JobKind::Embedding, "nomic-embed-text"),
            (JobKind::Transcription, "whisper-1"),
            (JobKind::SpeechSynthesis, "say-1"),
            // A wildcard node advertises no concrete id.
            (JobKind::InferenceCall, "any"),
        ]);
        assert_eq!(chat_model_ids(&view), vec!["llama3.2", "qwen2.5:0.5b"]);
    }

    #[test]
    fn chat_model_ids_is_empty_when_no_chat_model_is_served() {
        let view = capacity_of(&[(JobKind::Embedding, "nomic-embed-text")]);
        assert!(chat_model_ids(&view).is_empty());
    }

    #[test]
    fn a_model_object_is_the_anthropic_shape() {
        let m = model_object("qwen2.5:0.5b");
        assert_eq!(m["type"], "model");
        assert_eq!(m["id"], "qwen2.5:0.5b");
        assert_eq!(m["display_name"], "qwen2.5:0.5b");
        assert_eq!(m["created_at"], "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_dialect_is_anthropic_only_for_an_anthropic_header() {
        let mut anthropic_version = HeaderMap::new();
        anthropic_version.insert("anthropic-version", "2023-06-01".parse().unwrap());
        assert!(wants_anthropic_dialect(&anthropic_version));

        let mut x_api_key = HeaderMap::new();
        x_api_key.insert("x-api-key", "sk-test".parse().unwrap());
        assert!(wants_anthropic_dialect(&x_api_key));

        // A bearer-only request is the OpenAI dialect on the shared path.
        let mut bearer = HeaderMap::new();
        bearer.insert("authorization", "Bearer sk-test".parse().unwrap());
        assert!(!wants_anthropic_dialect(&bearer));
        assert!(!wants_anthropic_dialect(&HeaderMap::new()));
    }
}
