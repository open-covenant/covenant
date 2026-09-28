//! Buyer-chosen generation parameters over the envelope's `Vec<Content>`.
//!
//! Sampling knobs (temperature, seed, output cap, stop sequences)
//! travel as one `Content::Json` block shaped `{"generation": {…}}`
//! alongside the prompt or packed chat, so the envelope's wire form
//! (and every existing signature) is untouched and the block is signed
//! like the rest of the paid input. The buyer packs with
//! [`generation_input`], the executing node reads with
//! [`parse_generation_params`], and because both live here the two
//! sides cannot drift on what a knob means.
//!
//! Advisory for executors that don't sample: a batch node ignores the
//! block the same way a non-streaming executor ignores the envelope's
//! `stream` flag. Inference executors map every present field onto
//! their backend; a field the buyer left `None` stays the backend's
//! default.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// OpenAI's documented cap, the tightest across the backend class.
pub const MAX_STOP_SEQUENCES: usize = 4;
/// Real stop sequences are short delimiters; anything longer is a
/// buyer burning operator CPU on substring scans.
pub const MAX_STOP_SEQUENCE_BYTES: usize = 64;
/// Largest a structured-output schema name may be — OpenAI's own cap.
pub const MAX_SCHEMA_NAME_BYTES: usize = 64;
/// Largest a structured-output JSON schema may serialize to. Generous
/// for a real schema, bounded so paid input can't hand an operator an
/// unbounded document to compile into a decoding grammar.
pub const MAX_SCHEMA_BYTES: usize = 16 * 1024;
/// Most alternative tokens a buyer may ask for per generated token —
/// OpenAI's own `top_logprobs` ceiling, the tightest across the backend
/// class.
pub const MAX_TOP_LOGPROBS: u32 = 20;

/// A buyer's constraint on the shape of generated output. Left `None` on
/// [`GenerationParams`], the backend returns free-form text; set, it maps
/// to the backend's native structured-output control — Ollama's `format`,
/// an OpenAI-compatible backend's `response_format`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Any syntactically valid JSON, with no fixed schema.
    JsonObject,
    /// Output constrained to a named JSON schema (structured outputs).
    /// `strict` asks a backend that supports it to guarantee adherence; a
    /// backend without a strict mode still constrains to the schema.
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

impl ResponseFormat {
    /// A schema must be a named, non-empty JSON object within the size
    /// bound; `json_object` mode is always meaningful. Enforced on both
    /// pack and parse, like the sampling knobs.
    fn validate(&self) -> Result<(), ProtocolError> {
        let ResponseFormat::JsonSchema { name, schema, .. } = self else {
            return Ok(());
        };
        if name.trim().is_empty() {
            return Err(ProtocolError::Invalid(
                "response_format json schema has an empty name".into(),
            ));
        }
        if name.len() > MAX_SCHEMA_NAME_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "response_format schema name of {} bytes exceeds the {}-byte cap",
                name.len(),
                MAX_SCHEMA_NAME_BYTES
            )));
        }
        if !schema.is_object() {
            return Err(ProtocolError::Invalid(
                "response_format json schema must be a JSON object".into(),
            ));
        }
        let encoded = serde_json::to_string(schema)
            .map_err(|e| ProtocolError::Invalid(format!("response_format schema: {e}")))?;
        if encoded.len() > MAX_SCHEMA_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "response_format schema of {} bytes exceeds the {}-byte cap",
                encoded.len(),
                MAX_SCHEMA_BYTES
            )));
        }
        Ok(())
    }
}

/// Every field optional: `None` means "the backend's default", and a
/// block with nothing set is rejected rather than dispatched as paid
/// noise. Unknown fields are rejected too — a typo'd knob on paid
/// input must fail loudly, not run the job at defaults.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationParams {
    /// In `0..=2` — the range the whole backend class accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// In `(0, 1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Output cap in tokens — the buyer's cost/latency bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// With `temperature` 0 this makes a run repeatable on the same
    /// backend + hardware; it does not promise identical output across
    /// different machines or quantizations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// In `-2..=2`. Positive values discourage reusing tokens already
    /// present at all, nudging the model toward new subjects; negative
    /// values encourage repetition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// In `-2..=2`. Positive values scale the penalty with how often a
    /// token has already appeared, damping verbatim repetition; negative
    /// values reinforce it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// At most [`MAX_STOP_SEQUENCES`] sequences of 1 to
    /// [`MAX_STOP_SEQUENCE_BYTES`] bytes each.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Constrain the shape of generated output (JSON mode or a named
    /// schema). `None` leaves the backend returning free-form text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Ask the backend to report the log probability of each generated
    /// token. `None` leaves them off; `Some(n)` returns them, with the `n`
    /// most-likely alternatives per token (`0..=`[`MAX_TOP_LOGPROBS`];
    /// `Some(0)` is the sampled token's own logprob with no alternatives).
    /// The reported probabilities ride the attested output, so an operator
    /// commits to them exactly as it does the tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<u32>,
}

impl GenerationParams {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Meaningfulness checks shared by pack and parse — enforced on
    /// both sides so a hand-rolled buyer bypassing [`generation_input`]
    /// still can't reach an executor with knobs no backend can honor.
    /// The ranges are the intersection every supported backend accepts:
    /// a knob one backend tolerates but another 400s on would fail the
    /// job — and fault the operator — on input only the buyer controls.
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.is_empty() {
            return Err(ProtocolError::Invalid(
                "generation block sets no parameters".into(),
            ));
        }
        if let Some(t) = self.temperature {
            if !t.is_finite() || !(0.0..=2.0).contains(&t) {
                return Err(ProtocolError::Invalid(format!(
                    "generation temperature {t} is outside 0..=2"
                )));
            }
        }
        if let Some(p) = self.top_p {
            if !p.is_finite() || p <= 0.0 || p > 1.0 {
                return Err(ProtocolError::Invalid(format!(
                    "generation top_p {p} is outside (0, 1]"
                )));
            }
        }
        if let Some(pp) = self.presence_penalty {
            if !pp.is_finite() || !(-2.0..=2.0).contains(&pp) {
                return Err(ProtocolError::Invalid(format!(
                    "generation presence_penalty {pp} is outside -2..=2"
                )));
            }
        }
        if let Some(fp) = self.frequency_penalty {
            if !fp.is_finite() || !(-2.0..=2.0).contains(&fp) {
                return Err(ProtocolError::Invalid(format!(
                    "generation frequency_penalty {fp} is outside -2..=2"
                )));
            }
        }
        if self.max_tokens == Some(0) {
            return Err(ProtocolError::Invalid(
                "generation max_tokens 0 asks for no output".into(),
            ));
        }
        if let Some(stop) = &self.stop {
            if stop.is_empty() {
                return Err(ProtocolError::Invalid(
                    "generation stop list is empty".into(),
                ));
            }
            if stop.len() > MAX_STOP_SEQUENCES {
                return Err(ProtocolError::Invalid(format!(
                    "generation stop list carries {} sequences; backends honor at most {}",
                    stop.len(),
                    MAX_STOP_SEQUENCES
                )));
            }
            if stop.iter().any(|s| s.is_empty()) {
                return Err(ProtocolError::Invalid(
                    "generation stop list carries an empty sequence".into(),
                ));
            }
            if let Some(oversized) = stop.iter().find(|s| s.len() > MAX_STOP_SEQUENCE_BYTES) {
                return Err(ProtocolError::Invalid(format!(
                    "generation stop sequence of {} bytes exceeds the {}-byte cap",
                    oversized.len(),
                    MAX_STOP_SEQUENCE_BYTES
                )));
            }
        }
        if let Some(response_format) = &self.response_format {
            response_format.validate()?;
        }
        if let Some(n) = self.logprobs {
            if n > MAX_TOP_LOGPROBS {
                return Err(ProtocolError::Invalid(format!(
                    "generation logprobs asks for {n} alternatives; backends return at most {MAX_TOP_LOGPROBS}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct GenerationInputBlock {
    generation: GenerationParams,
}

/// Packs generation parameters into the JSON block that rides in job
/// input next to the prompt or conversation. Fails on a meaningless
/// block so the buyer is refused before anything is signed or held.
pub fn generation_input(params: GenerationParams) -> Result<Content, ProtocolError> {
    params.validate()?;
    let value = serde_json::to_value(GenerationInputBlock { generation: params })
        .expect("generation params serialize infallibly");
    Ok(Content::json(value))
}

/// Reads generation parameters back out of job input.
///
/// `Ok(None)` means the input carries no generation block — the
/// executor runs at backend defaults. `Err` means a block is present
/// but malformed or meaningless: the job fails loudly instead of the
/// executor guessing at paid input.
pub fn parse_generation_params(
    input: &[Content],
) -> Result<Option<GenerationParams>, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("generation").is_none() {
            continue;
        }
        let block: GenerationInputBlock = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("generation input: {e}")))?;
        block.generation.validate()?;
        return Ok(Some(block.generation));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{chat_input, parse_chat_input, ChatMessage};

    fn params() -> GenerationParams {
        GenerationParams {
            temperature: Some(0.0),
            top_p: Some(0.9),
            max_tokens: Some(64),
            seed: Some(7),
            presence_penalty: Some(0.5),
            frequency_penalty: Some(-0.5),
            stop: Some(vec!["\n\n".into()]),
            response_format: None,
            logprobs: Some(5),
        }
    }

    #[test]
    fn generation_input_round_trips_through_parse() {
        let input = vec![generation_input(params()).expect("valid")];
        let parsed = parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed, params());
    }

    #[test]
    fn input_without_a_generation_block_parses_to_none() {
        let input = vec![Content::text("summarize this")];
        assert_eq!(parse_generation_params(&input).expect("well-formed"), None);
        let chat = chat_input(vec![ChatMessage::user("hi")]);
        assert_eq!(parse_generation_params(&chat).expect("well-formed"), None);
    }

    #[test]
    fn a_generation_block_composes_with_a_packed_conversation() {
        let mut input = chat_input(vec![ChatMessage::user("what color is the sky?")]);
        input.push(generation_input(params()).expect("valid"));
        let messages = parse_chat_input(&input)
            .expect("well-formed")
            .expect("chat");
        assert_eq!(messages.len(), 1);
        let parsed = parse_generation_params(&input)
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed.seed, Some(7));
    }

    #[test]
    fn an_empty_block_is_refused_on_both_sides() {
        let err = generation_input(GenerationParams::default()).expect_err("empty");
        assert!(err.to_string().contains("no parameters"), "got: {err}");
        let input = vec![Content::json(serde_json::json!({ "generation": {} }))];
        let err = parse_generation_params(&input).expect_err("empty");
        assert!(err.to_string().contains("no parameters"), "got: {err}");
    }

    #[test]
    fn an_out_of_range_knob_is_refused_at_parse_too() {
        let input = vec![Content::json(serde_json::json!({
            "generation": { "temperature": 3.0 }
        }))];
        let err = parse_generation_params(&input).expect_err("out of range");
        assert!(err.to_string().contains("outside 0..=2"), "got: {err}");
    }

    #[test]
    fn a_typoed_knob_fails_instead_of_running_at_defaults() {
        let input = vec![Content::json(serde_json::json!({
            "generation": { "temprature": 1.9 }
        }))];
        let err = parse_generation_params(&input).expect_err("unknown field");
        assert!(err.to_string().contains("generation input"), "got: {err}");
    }

    #[test]
    fn meaningless_knob_values_are_refused() {
        for (params, needle) in [
            (
                GenerationParams {
                    temperature: Some(-0.1),
                    ..Default::default()
                },
                "outside 0..=2",
            ),
            (
                GenerationParams {
                    temperature: Some(2.5),
                    ..Default::default()
                },
                "outside 0..=2",
            ),
            (
                GenerationParams {
                    temperature: Some(f64::NAN),
                    ..Default::default()
                },
                "outside 0..=2",
            ),
            (
                GenerationParams {
                    temperature: Some(f64::INFINITY),
                    ..Default::default()
                },
                "outside 0..=2",
            ),
            (
                GenerationParams {
                    top_p: Some(1.5),
                    ..Default::default()
                },
                "outside",
            ),
            (
                GenerationParams {
                    top_p: Some(f64::NAN),
                    ..Default::default()
                },
                "outside",
            ),
            (
                GenerationParams {
                    presence_penalty: Some(2.5),
                    ..Default::default()
                },
                "presence_penalty 2.5 is outside -2..=2",
            ),
            (
                GenerationParams {
                    presence_penalty: Some(f64::INFINITY),
                    ..Default::default()
                },
                "presence_penalty",
            ),
            (
                GenerationParams {
                    frequency_penalty: Some(-2.5),
                    ..Default::default()
                },
                "frequency_penalty -2.5 is outside -2..=2",
            ),
            (
                GenerationParams {
                    frequency_penalty: Some(f64::NAN),
                    ..Default::default()
                },
                "frequency_penalty",
            ),
            (
                GenerationParams {
                    max_tokens: Some(0),
                    ..Default::default()
                },
                "no output",
            ),
            (
                GenerationParams {
                    stop: Some(vec![]),
                    ..Default::default()
                },
                "empty",
            ),
            (
                GenerationParams {
                    stop: Some(vec!["ok".into(), String::new()]),
                    ..Default::default()
                },
                "empty sequence",
            ),
            (
                GenerationParams {
                    stop: Some(vec![
                        "a".into(),
                        "b".into(),
                        "c".into(),
                        "d".into(),
                        "e".into(),
                    ]),
                    ..Default::default()
                },
                "at most 4",
            ),
            (
                GenerationParams {
                    stop: Some(vec!["x".repeat(MAX_STOP_SEQUENCE_BYTES + 1)]),
                    ..Default::default()
                },
                "exceeds the 64-byte cap",
            ),
        ] {
            let err = generation_input(params.clone()).expect_err("meaningless");
            assert!(err.to_string().contains(needle), "{params:?} got: {err}");
        }
    }

    #[test]
    fn the_portable_boundary_values_are_honored() {
        let block = generation_input(GenerationParams {
            temperature: Some(2.0),
            top_p: Some(1.0),
            max_tokens: Some(1),
            seed: Some(i64::MIN),
            presence_penalty: Some(-2.0),
            frequency_penalty: Some(2.0),
            stop: Some(vec![
                "x".repeat(MAX_STOP_SEQUENCE_BYTES);
                MAX_STOP_SEQUENCES
            ]),
            response_format: None,
            logprobs: Some(MAX_TOP_LOGPROBS),
        })
        .expect("boundary values are valid");
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed.temperature, Some(2.0));
        assert_eq!(parsed.presence_penalty, Some(-2.0));
        assert_eq!(parsed.frequency_penalty, Some(2.0));
        assert_eq!(parsed.stop.as_ref().map(Vec::len), Some(MAX_STOP_SEQUENCES));
        assert_eq!(parsed.logprobs, Some(MAX_TOP_LOGPROBS));
    }

    #[test]
    fn none_fields_stay_off_the_wire() {
        let block = generation_input(GenerationParams {
            seed: Some(42),
            ..Default::default()
        })
        .expect("valid");
        let Content::Json { value } = &block else {
            panic!("generation packs as a JSON block");
        };
        assert_eq!(
            value.to_string(),
            r#"{"generation":{"seed":42}}"#,
            "unset knobs must not appear as nulls"
        );
    }

    #[test]
    fn a_json_block_without_generation_is_not_a_params_block() {
        let input = vec![Content::json(serde_json::json!({"config": {"seed": 7}}))];
        assert_eq!(parse_generation_params(&input).expect("well-formed"), None);
    }

    #[test]
    fn json_object_mode_round_trips_and_carries_its_type_tag() {
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonObject),
            ..Default::default()
        };
        let block = generation_input(params.clone()).expect("valid");
        let Content::Json { value } = &block else {
            panic!("generation packs as a JSON block");
        };
        assert_eq!(
            value.to_string(),
            r#"{"generation":{"response_format":{"type":"json_object"}}}"#
        );
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed, params);
    }

    #[test]
    fn a_json_schema_round_trips_with_name_and_strict() {
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "weather".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"],
                }),
                strict: Some(true),
            }),
            ..Default::default()
        };
        let block = generation_input(params.clone()).expect("valid");
        let parsed = parse_generation_params(&[block])
            .expect("well-formed")
            .expect("present");
        assert_eq!(parsed, params);
    }

    #[test]
    fn a_json_schema_that_is_not_an_object_is_refused() {
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "bad".into(),
                schema: serde_json::json!("not an object"),
                strict: None,
            }),
            ..Default::default()
        };
        let err = generation_input(params).expect_err("non-object schema");
        assert!(
            err.to_string().contains("must be a JSON object"),
            "got: {err}"
        );
    }

    #[test]
    fn a_json_schema_without_a_name_is_refused() {
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "  ".into(),
                schema: serde_json::json!({ "type": "object" }),
                strict: None,
            }),
            ..Default::default()
        };
        let err = generation_input(params).expect_err("empty name");
        assert!(err.to_string().contains("empty name"), "got: {err}");
    }

    #[test]
    fn an_oversized_schema_is_refused() {
        let bloated = serde_json::json!({
            "type": "object",
            "description": "x".repeat(MAX_SCHEMA_BYTES),
        });
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "big".into(),
                schema: bloated,
                strict: None,
            }),
            ..Default::default()
        };
        let err = generation_input(params).expect_err("oversized");
        assert!(err.to_string().contains("exceeds"), "got: {err}");
    }

    #[test]
    fn a_response_format_alone_is_a_meaningful_block() {
        let params = GenerationParams {
            response_format: Some(ResponseFormat::JsonObject),
            ..Default::default()
        };
        assert!(!params.is_empty());
        generation_input(params).expect("a response_format alone is enough to pack");
    }

    #[test]
    fn a_penalty_alone_is_a_meaningful_block() {
        let params = GenerationParams {
            frequency_penalty: Some(0.3),
            ..Default::default()
        };
        assert!(!params.is_empty());
        let block = generation_input(params).expect("a penalty alone is enough to pack");
        let Content::Json { value } = &block else {
            panic!("generation packs as a JSON block");
        };
        assert_eq!(
            value.to_string(),
            r#"{"generation":{"frequency_penalty":0.3}}"#,
            "only the set penalty is on the wire"
        );
    }

    #[test]
    fn an_out_of_range_penalty_is_refused_at_parse_too() {
        let input = vec![Content::json(serde_json::json!({
            "generation": { "presence_penalty": 3.0 }
        }))];
        let err = parse_generation_params(&input).expect_err("out of range");
        assert!(err.to_string().contains("outside -2..=2"), "got: {err}");
    }

    #[test]
    fn logprobs_alone_is_a_meaningful_block_and_zero_is_valid() {
        let params = GenerationParams {
            logprobs: Some(0),
            ..Default::default()
        };
        assert!(!params.is_empty());
        let block = generation_input(params).expect("logprobs alone is enough to pack");
        let Content::Json { value } = &block else {
            panic!("generation packs as a JSON block");
        };
        assert_eq!(
            value.to_string(),
            r#"{"generation":{"logprobs":0}}"#,
            "only the set knob is on the wire"
        );
    }

    #[test]
    fn too_many_logprobs_is_refused_on_both_sides() {
        let err = generation_input(GenerationParams {
            logprobs: Some(MAX_TOP_LOGPROBS + 1),
            ..Default::default()
        })
        .expect_err("over the cap");
        assert!(err.to_string().contains("at most 20"), "got: {err}");

        let input = vec![Content::json(serde_json::json!({
            "generation": { "logprobs": 21 }
        }))];
        let err = parse_generation_params(&input).expect_err("over the cap at parse");
        assert!(err.to_string().contains("at most 20"), "got: {err}");
    }
}
