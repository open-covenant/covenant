//! Tool/function-calling wire types over the envelope's `Vec<Content>`.
//!
//! The buyer's tool definitions and choice travel as one `Content::Json`
//! block shaped `{"tools": [...], "tool_choice": ...}` alongside the
//! packed chat, so the envelope's wire form (and every existing
//! signature) is untouched and the block is signed like the rest of the
//! paid input — the same pattern [`crate::chat`] and [`crate::generation`]
//! use. The executing node reads the definitions with [`parse_tools_input`]
//! and maps them onto its backend; the assistant's tool-call result rides
//! back in the attested output via [`assistant_output`] /
//! [`parse_assistant_output`], so [`crate::receipt::output_hash_hex`]
//! commits the operator to the calls it returned exactly as it does the
//! prose.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::ToolCall;
use crate::logprobs::TokenLogprob;
use crate::sign::ProtocolError;

/// The most tools one request may offer. A real agent exposes a handful;
/// a huge list is a buyer burning paid operator context, so an oversized
/// set is refused before the job is signed.
pub const MAX_TOOLS: usize = 128;

/// The kind of a tool the buyer offers. Only function tools are served;
/// the field exists so the wire form matches OpenAI's
/// `{"type":"function",…}` and an unmodelled kind fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Function,
}

/// One function the buyer makes available to the model. `parameters` is a
/// JSON-Schema object describing the call arguments; `None` means the
/// function takes none. The schema is passed through to the backend
/// unshaped — it is the buyer's contract with the model, not this layer's
/// to interpret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// One entry in a request's `tools` list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: FunctionDefinition,
}

/// The bare `tool_choice` modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    /// The model decides whether to call a tool (the backend default).
    Auto,
    /// The model must not call a tool.
    None,
    /// The model must call at least one tool.
    Required,
}

/// Force one named function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedToolChoice {
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: NamedFunction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedFunction {
    pub name: String,
}

/// Which tool the model may or must call. The wire form is OpenAI's: a
/// bare string mode (`"auto"`, `"none"`, `"required"`) or an object
/// naming one function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolChoiceMode),
    Named(NamedToolChoice),
}

/// The tools a request carries, read back off the job input.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestTools {
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

/// Rejects an unknown field rather than dropping it: a typo'd `tool_choice`
/// on paid input must fail loudly, not let the model skip the tool the buyer
/// paid to force, the same posture [`crate::generation`] takes.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsBlock {
    tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
}

fn validate_tools(
    tools: &[ToolDefinition],
    choice: Option<&ToolChoice>,
) -> Result<(), ProtocolError> {
    if tools.is_empty() {
        return Err(ProtocolError::Invalid(
            "tools list is empty; omit tools rather than sending none".into(),
        ));
    }
    if tools.len() > MAX_TOOLS {
        return Err(ProtocolError::Invalid(format!(
            "tools list carries {} functions; at most {MAX_TOOLS} are served per call",
            tools.len()
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for tool in tools {
        let name = tool.function.name.trim();
        if name.is_empty() {
            return Err(ProtocolError::Invalid(
                "a tool function has an empty name".into(),
            ));
        }
        if !seen.insert(name) {
            return Err(ProtocolError::Invalid(format!(
                "tool function '{name}' is defined more than once"
            )));
        }
    }
    if let Some(ToolChoice::Named(named)) = choice {
        let wanted = named.function.name.trim();
        if !tools.iter().any(|t| t.function.name.trim() == wanted) {
            return Err(ProtocolError::Invalid(format!(
                "tool_choice names function '{wanted}', which is not in the tools list"
            )));
        }
    }
    Ok(())
}

/// Packs a request's tool definitions and choice into one job-input
/// block. Rejects an empty or oversized list, a nameless or duplicate
/// function, and a `tool_choice` naming a function the list does not
/// offer, so an executor only ever sees a coherent tool set.
pub fn tools_input(
    tools: Vec<ToolDefinition>,
    tool_choice: Option<ToolChoice>,
) -> Result<Content, ProtocolError> {
    validate_tools(&tools, tool_choice.as_ref())?;
    let value = serde_json::to_value(ToolsBlock { tools, tool_choice })
        .expect("tool definitions serialize infallibly");
    Ok(Content::json(value))
}

/// Reads a request's tools back out of job input.
///
/// `Ok(None)` means no tools block is present (the ordinary case).
/// `Err` means a tools block that is malformed or fails the same
/// coherence checks [`tools_input`] enforces: the job fails loudly
/// rather than the executor calling a model with a broken tool set on
/// paid input.
pub fn parse_tools_input(input: &[Content]) -> Result<Option<RequestTools>, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("tools").is_none() {
            continue;
        }
        let block: ToolsBlock = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("tools input: {e}")))?;
        validate_tools(&block.tools, block.tool_choice.as_ref())?;
        return Ok(Some(RequestTools {
            tools: block.tools,
            tool_choice: block.tool_choice,
        }));
    }
    Ok(None)
}

#[derive(Serialize, Deserialize)]
struct ToolCallsBlock {
    tool_calls: Vec<ToolCall>,
}

/// An assistant's reply, split into its prose, the tools it asked to
/// call, and the per-token log probabilities when the buyer requested
/// them (`None` otherwise).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantReply {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub logprobs: Option<Vec<TokenLogprob>>,
}

/// The canonical form of a tool call's `arguments`, so the same logical
/// call attests the same bytes whichever backend served it. An object or
/// array is re-serialized through serde_json's sorted-key, whitespace-free
/// form: Ollama returns a parsed object whose key order it does not fix,
/// while an OpenAI-compatible backend returns a ready string with its own
/// key order and spacing, and the receipt hash can only be executor-
/// independent if both reduce to one form. Arguments that are not a JSON
/// object or array (a bare value, or a string that is not valid JSON) are
/// left exactly as the backend produced them.
fn canonical_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(value) if value.is_object() || value.is_array() => value.to_string(),
        _ => arguments.to_string(),
    }
}

/// Packs an assistant reply into the attested job output.
///
/// With no tool calls the shape is one text block — byte-for-byte what a
/// plain completion produced before tool calling existed, so an ordinary
/// job's output hashes and settles exactly as before. Tool calls add one
/// JSON block; empty prose is dropped, so a pure tool-call turn carries
/// only the calls. Each call's id is renumbered to its position and its
/// arguments are canonicalized on the way in (see [`canonical_arguments`])
/// so the attested hash commits to the logical call, not to which backend
/// produced it.
pub fn assistant_output(text: String, tool_calls: Vec<ToolCall>) -> Vec<Content> {
    if tool_calls.is_empty() {
        return vec![Content::text(text)];
    }
    let tool_calls = tool_calls
        .into_iter()
        .enumerate()
        .map(|(position, mut call)| {
            // The id is only the client's handle for correlating a tool
            // result back in a later turn; its exact value carries no
            // meaning. An OpenAI-compatible backend mints a fresh random
            // one per request while Ollama already numbers them by
            // position, so renumbering to the position lets the attested
            // output commit to the logical call rather than a backend's
            // random handle — otherwise two honest operators on such a
            // backend hash the same deterministic tool-call job to
            // different bytes and the redundancy sampler faults the
            // minority for correct work.
            call.id = format!("call_{position}");
            call.function.arguments = canonical_arguments(&call.function.arguments);
            call
        })
        .collect();
    let mut output = Vec::with_capacity(2);
    if !text.is_empty() {
        output.push(Content::text(text));
    }
    let value = serde_json::to_value(ToolCallsBlock { tool_calls })
        .expect("tool calls serialize infallibly");
    output.push(Content::json(value));
    output
}

/// Reads an assistant reply back out of attested output: every text
/// block concatenated, any tool calls carried in a JSON block, and the
/// per-token log probabilities when present. A JSON block that is neither
/// a tool-calls nor a logprobs block is ignored, so this stays correct
/// alongside other structured output.
pub fn parse_assistant_output(output: &[Content]) -> AssistantReply {
    let mut reply = AssistantReply::default();
    for content in output {
        match content {
            Content::Text { text } => reply.text.push_str(text),
            Content::Json { value } => {
                if value.get("tool_calls").is_some() {
                    if let Ok(block) = serde_json::from_value::<ToolCallsBlock>(value.clone()) {
                        reply.tool_calls.extend(block.tool_calls);
                    }
                }
            }
        }
    }
    reply.logprobs = crate::logprobs::parse_logprobs_output(output);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{FunctionCall, ToolCallKind};

    fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            kind: ToolKind::Function,
            function: FunctionDefinition {
                name: "get_weather".into(),
                description: Some("look up the weather".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                })),
            },
        }
    }

    fn a_call() -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Paris"}"#.into(),
            },
        }
    }

    #[test]
    fn tools_round_trip_through_parse() {
        let choice = Some(ToolChoice::Mode(ToolChoiceMode::Auto));
        let block = tools_input(vec![weather_tool()], choice.clone()).expect("valid");
        let parsed = parse_tools_input(&[block])
            .expect("well-formed")
            .expect("tools present");
        assert_eq!(parsed.tools, vec![weather_tool()]);
        assert_eq!(parsed.tool_choice, choice);
    }

    #[test]
    fn input_without_a_tools_block_parses_as_none() {
        let input = vec![Content::text("hi")];
        assert_eq!(parse_tools_input(&input).expect("well-formed"), None);
        let input = vec![Content::json(
            serde_json::json!({"generation": {"seed": 1}}),
        )];
        assert_eq!(parse_tools_input(&input).expect("well-formed"), None);
    }

    #[test]
    fn an_empty_tools_list_is_refused() {
        let err = tools_input(vec![], None).expect_err("empty");
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn a_duplicate_function_name_is_refused() {
        let err = tools_input(vec![weather_tool(), weather_tool()], None).expect_err("dup");
        assert!(err.to_string().contains("more than once"), "got: {err}");
    }

    #[test]
    fn tool_choice_naming_an_absent_function_is_refused() {
        let choice = ToolChoice::Named(NamedToolChoice {
            kind: ToolKind::Function,
            function: NamedFunction {
                name: "not_offered".into(),
            },
        });
        let err = tools_input(vec![weather_tool()], Some(choice)).expect_err("absent");
        assert!(
            err.to_string().contains("not in the tools list"),
            "got: {err}"
        );
    }

    #[test]
    fn a_typoed_steering_key_is_refused_not_dropped() {
        // `tool_choce` is not `tool_choice`; dropping it silently would let
        // the model skip the tool the buyer paid to force. The paid input
        // fails instead of running unsteered.
        let value = serde_json::json!({
            "tools": [weather_tool()],
            "tool_choce": "required",
        });
        let err = parse_tools_input(&[Content::json(value)]).expect_err("a typoed key must fail");
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn tool_choice_string_and_object_forms_both_parse() {
        for raw in [
            serde_json::json!("auto"),
            serde_json::json!("required"),
            serde_json::json!({"type": "function", "function": {"name": "get_weather"}}),
        ] {
            let value = serde_json::json!({"tools": [weather_tool()], "tool_choice": raw});
            parse_tools_input(&[Content::json(value)])
                .expect("well-formed")
                .expect("tools present");
        }
    }

    #[test]
    fn a_plain_reply_output_is_one_text_block_unchanged() {
        let output = assistant_output("blue".into(), Vec::new());
        assert_eq!(output, vec![Content::text("blue")]);
        let reply = parse_assistant_output(&output);
        assert_eq!(reply.text, "blue");
        assert!(reply.tool_calls.is_empty());
    }

    #[test]
    fn a_tool_call_reply_round_trips_and_drops_empty_prose() {
        let output = assistant_output(String::new(), vec![a_call()]);
        assert_eq!(output.len(), 1, "no empty text block");
        let reply = parse_assistant_output(&output);
        assert_eq!(reply.text, "");
        // The id is renumbered to its position; name and arguments carry through.
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].id, "call_0");
        assert_eq!(reply.tool_calls[0].function, a_call().function);
    }

    #[test]
    fn prose_and_a_tool_call_together_round_trip() {
        let output = assistant_output("let me check".into(), vec![a_call()]);
        assert_eq!(output.len(), 2);
        let reply = parse_assistant_output(&output);
        assert_eq!(reply.text, "let me check");
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].id, "call_0");
        assert_eq!(reply.tool_calls[0].function, a_call().function);
        assert_eq!(reply.logprobs, None);
    }

    fn call_with_arguments(arguments: &str) -> ToolCall {
        ToolCall {
            id: "call_0".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: arguments.into(),
            },
        }
    }

    #[test]
    fn tool_call_arguments_are_canonicalized_in_the_attested_output() {
        // A backend that emits the arguments with its own key order and
        // spacing still attests the canonical form, so the reply reads back
        // the same however it was served.
        let output = assistant_output(
            String::new(),
            vec![call_with_arguments(r#"{ "units": "C", "city": "Paris" }"#)],
        );
        let reply = parse_assistant_output(&output);
        assert_eq!(
            reply.tool_calls[0].function.arguments,
            r#"{"city":"Paris","units":"C"}"#
        );
    }

    #[test]
    fn the_same_call_hashes_identically_whatever_the_backend_formatting() {
        // The receipt commits to the logical call, not a backend's spacing
        // or key order: a compact Ollama-shaped object and a spaced
        // OpenAI-shaped one with the keys reversed attest identical bytes,
        // so a redundant re-run on a differently configured node can't fault
        // an honest operator over formatting alone.
        let compact = assistant_output(
            String::new(),
            vec![call_with_arguments(r#"{"city":"Paris","units":"C"}"#)],
        );
        let spaced = assistant_output(
            String::new(),
            vec![call_with_arguments(r#"{"units": "C", "city": "Paris"}"#)],
        );
        assert_eq!(
            crate::receipt::output_hash_hex(&compact),
            crate::receipt::output_hash_hex(&spaced),
        );
    }

    #[test]
    fn arguments_that_are_not_a_json_object_are_left_untouched() {
        // Malformed or bare-value arguments are carried through verbatim
        // rather than dropped or mangled by the canonicalizer.
        let reply = parse_assistant_output(&assistant_output(
            String::new(),
            vec![call_with_arguments("not json")],
        ));
        assert_eq!(reply.tool_calls[0].function.arguments, "not json");
    }

    #[test]
    fn tool_call_ids_are_renumbered_to_their_position() {
        // A backend that mints a fresh random id per request (the
        // OpenAI-compatible shape) attests positional ids, so the same
        // logical calls hash identically however they were served and the
        // redundancy sampler cannot fault the minority over a random
        // handle. Renumbering preserves order: id N names the Nth call.
        let calls = vec![
            ToolCall {
                id: "call_9fA2xytZ".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "call_ZZ7qWpLm".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: "get_time".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let reply = parse_assistant_output(&assistant_output(String::new(), calls));
        assert_eq!(reply.tool_calls[0].id, "call_0");
        assert_eq!(reply.tool_calls[0].function.name, "get_weather");
        assert_eq!(reply.tool_calls[1].id, "call_1");
        assert_eq!(reply.tool_calls[1].function.name, "get_time");
    }

    #[test]
    fn parse_surfaces_a_logprobs_block_alongside_prose() {
        let mut output = assistant_output("hi".into(), Vec::new());
        output.push(crate::logprobs::logprobs_block(vec![
            crate::logprobs::TokenLogprob {
                token: "hi".into(),
                logprob: -0.5,
                bytes: None,
                top_logprobs: Vec::new(),
            },
        ]));
        let reply = parse_assistant_output(&output);
        assert_eq!(reply.text, "hi");
        assert!(reply.tool_calls.is_empty());
        let logprobs = reply.logprobs.expect("logprobs surfaced");
        assert_eq!(logprobs.len(), 1);
        assert_eq!(logprobs[0].token, "hi");
    }
}
