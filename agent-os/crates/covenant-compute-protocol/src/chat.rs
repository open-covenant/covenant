//! Typed chat-conversation input over the envelope's `Vec<Content>`.
//!
//! A conversation travels as one `Content::Json` block shaped
//! `{"messages": [{"role", "content"}, …]}`, so the envelope's wire
//! form (and every existing signature) is untouched — the buyer packs
//! with [`chat_input`], the executing node reads with
//! [`parse_chat_input`], and because both live here the two sides
//! cannot drift on what a chat job means.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    /// The result of a tool the assistant asked to call, fed back into
    /// the conversation. Carries a [`ChatMessage::tool_call_id`] naming
    /// which call it answers.
    Tool,
}

/// The kind of a tool call. Only function tools are served; the field
/// exists so the wire form matches OpenAI's `{"type":"function",…}`
/// exactly and an unmodelled kind fails loudly rather than silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    Function,
}

/// One function the model asked to call. `arguments` is a JSON string,
/// the OpenAI convention: it is the model's own text, passed through
/// unparsed so a malformed argument object is the caller's to handle
/// rather than silently reshaped here. An object-valued backend (Ollama)
/// is normalized to this string form at the executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A single tool call in an assistant turn. `id` is the handle a later
/// `tool` message references via [`ChatMessage::tool_call_id`]; a backend
/// that omits it (Ollama) has one synthesized at the executor so a
/// multi-turn caller always has a stable reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ToolCallKind,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    /// Absent content reads as empty — an assistant turn that only calls
    /// tools carries no prose.
    #[serde(default)]
    pub content: String,
    /// Base64-encoded images attached to this message, carried in the
    /// wire form the model backend consumes directly: Ollama reads a
    /// message-level `images` array as-is, and the OpenAI-compatible
    /// executor rewrites each entry into an `image_url` content part.
    /// Empty on every text-only message, and skipped on the wire so a
    /// non-vision job signs and hashes exactly as it did before image
    /// input existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// The tool calls an assistant turn requested. Empty on every
    /// ordinary message, and skipped on the wire so a non-tool job's
    /// input signs and hashes exactly as it did before tool calling
    /// existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on a `tool` message: which assistant tool call this result
    /// answers. Skipped on the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A user turn carrying one or more base64-encoded images alongside
    /// its prompt — the shape a vision job travels in.
    pub fn user_with_images(content: impl Into<String>, images: Vec<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            images,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A `tool` message: the result of one tool call, naming the call it
    /// answers so the model can pair it back to its request.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// Cap on the total base64 image payload in one conversation. Images
/// dominate a vision request's size, so bounding their sum keeps the
/// signed envelope clear of the 8 MiB IPC/HTTP frame cap with room to
/// spare for the prompt and generation block. Measured on the base64
/// text the message carries, not decoded bytes — the network never
/// decodes an image, it relays the string the backend decodes.
pub const MAX_IMAGE_B64_BYTES: usize = 5 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct ChatInputBlock {
    messages: Vec<ChatMessage>,
}

/// Packs a conversation into job input: one JSON block carrying the
/// full message list.
pub fn chat_input(messages: Vec<ChatMessage>) -> Vec<Content> {
    let value = serde_json::to_value(ChatInputBlock { messages })
        .expect("chat messages serialize infallibly");
    vec![Content::json(value)]
}

/// Reads a conversation back out of job input.
///
/// `Ok(None)` means the input is not chat-shaped (no JSON block
/// carrying `messages`) — callers fall back to their raw-prompt path.
/// `Err` means chat-shaped but malformed (unknown role, wrong types,
/// an empty conversation): the job fails loudly instead of the
/// executor guessing at paid input.
pub fn parse_chat_input(input: &[Content]) -> Result<Option<Vec<ChatMessage>>, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("messages").is_none() {
            continue;
        }
        let block: ChatInputBlock = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("chat input: {e}")))?;
        if block.messages.is_empty() {
            return Err(ProtocolError::Invalid(
                "chat input carries an empty messages list".into(),
            ));
        }
        let mut image_bytes = 0usize;
        for message in &block.messages {
            for image in &message.images {
                if image.is_empty() {
                    return Err(ProtocolError::Invalid(
                        "chat input carries an empty image".into(),
                    ));
                }
                image_bytes = image_bytes.saturating_add(image.len());
            }
        }
        if image_bytes > MAX_IMAGE_B64_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "chat input images total {image_bytes} base64 bytes, over the \
                 {MAX_IMAGE_B64_BYTES} cap"
            )));
        }
        return Ok(Some(block.messages));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_input_round_trips_through_parse() {
        let messages = vec![
            ChatMessage::system("answer in one word"),
            ChatMessage::user("what color is the sky?"),
            ChatMessage::assistant("blue"),
            ChatMessage::user("and grass?"),
        ];
        let input = chat_input(messages.clone());
        let parsed = parse_chat_input(&input)
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(parsed, messages);
    }

    #[test]
    fn plain_text_input_is_not_chat_shaped() {
        let input = vec![Content::text("echo hello")];
        assert_eq!(parse_chat_input(&input).expect("well-formed"), None);
    }

    #[test]
    fn a_json_block_without_messages_is_not_chat_shaped() {
        let input = vec![Content::json(serde_json::json!({"config": {"seed": 7}}))];
        assert_eq!(parse_chat_input(&input).expect("well-formed"), None);
    }

    #[test]
    fn an_unknown_role_fails_instead_of_falling_back() {
        let input = vec![Content::json(serde_json::json!({
            "messages": [{"role": "operator", "content": "hi"}]
        }))];
        let err = parse_chat_input(&input).expect_err("unknown role");
        assert!(err.to_string().contains("chat input"), "got: {err}");
    }

    #[test]
    fn an_empty_conversation_fails_instead_of_falling_back() {
        let input = vec![Content::json(serde_json::json!({ "messages": [] }))];
        let err = parse_chat_input(&input).expect_err("empty conversation");
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn roles_serialize_lowercase_for_the_model_backend() {
        let value = serde_json::to_value(ChatMessage::system("s")).unwrap();
        assert_eq!(value["role"], "system");
    }

    #[test]
    fn a_plain_message_carries_no_tool_fields_on_the_wire() {
        let value = serde_json::to_value(ChatMessage::user("hi")).unwrap();
        assert_eq!(value, serde_json::json!({"role": "user", "content": "hi"}));
    }

    #[test]
    fn a_pre_tool_message_still_decodes() {
        let msg: ChatMessage =
            serde_json::from_value(serde_json::json!({"role": "assistant", "content": "blue"}))
                .expect("old shape decodes");
        assert_eq!(msg, ChatMessage::assistant("blue"));
        assert!(msg.tool_calls.is_empty());
        assert_eq!(msg.tool_call_id, None);
    }

    #[test]
    fn a_tool_call_turn_and_its_result_round_trip() {
        let call = ToolCall {
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Paris"}"#.into(),
            },
        };
        let messages = vec![
            ChatMessage::user("weather in Paris?"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: String::new(),
                images: Vec::new(),
                tool_calls: vec![call.clone()],
                tool_call_id: None,
            },
            ChatMessage::tool("call_1", "18C and clear"),
        ];
        let parsed = parse_chat_input(&chat_input(messages.clone()))
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(parsed, messages);
        let tool_msg = &parsed[2];
        assert_eq!(tool_msg.role, ChatRole::Tool);
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn a_user_message_carries_its_images_through_pack_and_parse() {
        let messages = vec![ChatMessage::user_with_images(
            "what is in this image?",
            vec!["aGVsbG8=".into()],
        )];
        let parsed = parse_chat_input(&chat_input(messages.clone()))
            .expect("well-formed")
            .expect("chat-shaped");
        assert_eq!(parsed, messages);
        assert_eq!(parsed[0].images, vec!["aGVsbG8=".to_string()]);
    }

    #[test]
    fn a_message_without_images_omits_the_field_on_the_wire() {
        // Byte-identical to a pre-vision message: the empty images vec is
        // skipped, so an old signature over this input still verifies.
        let value = serde_json::to_value(ChatMessage::user("hi")).unwrap();
        assert_eq!(value, serde_json::json!({"role": "user", "content": "hi"}));
    }

    #[test]
    fn an_image_serializes_at_the_message_level_for_the_backend() {
        let value = serde_json::to_value(ChatMessage::user_with_images(
            "caption",
            vec!["Zm9v".into()],
        ))
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"role": "user", "content": "caption", "images": ["Zm9v"]})
        );
    }

    #[test]
    fn an_empty_image_string_fails_instead_of_reaching_a_backend() {
        let input = vec![Content::json(serde_json::json!({
            "messages": [{"role": "user", "content": "hi", "images": [""]}]
        }))];
        let err = parse_chat_input(&input).expect_err("empty image");
        assert!(err.to_string().contains("empty image"), "got: {err}");
    }

    #[test]
    fn images_over_the_size_cap_are_refused() {
        let oversized = "A".repeat(MAX_IMAGE_B64_BYTES + 1);
        let input = chat_input(vec![ChatMessage::user_with_images("x", vec![oversized])]);
        let err = parse_chat_input(&input).expect_err("oversized image");
        assert!(err.to_string().contains("over the"), "got: {err}");
    }

    #[test]
    fn images_at_the_size_cap_are_accepted() {
        let at_cap = "A".repeat(MAX_IMAGE_B64_BYTES);
        let input = chat_input(vec![ChatMessage::user_with_images("x", vec![at_cap])]);
        assert!(parse_chat_input(&input).expect("at cap is fine").is_some());
    }

    #[test]
    fn a_tool_call_serializes_in_openai_shape() {
        let call = ToolCall {
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: FunctionCall {
                name: "f".into(),
                arguments: "{}".into(),
            },
        };
        let value = serde_json::to_value(call).unwrap();
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "f");
        assert_eq!(value["function"]["arguments"], "{}");
    }
}
