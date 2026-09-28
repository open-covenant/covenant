//! Per-token log probabilities carried in the attested job output.
//!
//! When a buyer sets [`crate::GenerationParams::logprobs`], the executing
//! node asks its backend to report the log probability of each generated
//! token and packs the result into one `Content::Json` block shaped
//! `{"logprobs": [...]}`, appended to the assistant reply. Because the
//! block rides the output, [`crate::output_hash_hex`] commits the operator
//! to the reported probabilities exactly as it does the tokens — a buyer
//! who paid for logprobs can verify the ones they got were signed for.
//!
//! The shape mirrors OpenAI's chat `logprobs.content` entries, so a
//! demand front door serializes them straight back to a client.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

/// One alternative token the model weighed at a position, with its log
/// probability. `bytes` is the token's raw UTF-8, present when the backend
/// reports it (it lets a client reassemble characters a tokenizer split
/// across tokens).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopLogprob {
    pub token: String,
    pub logprob: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
}

/// The log probability of one generated token, plus the most-likely
/// alternatives the buyer asked for. `top_logprobs` is empty when the
/// buyer requested none (`logprobs: Some(0)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenLogprob {
    pub token: String,
    pub logprob: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_logprobs: Vec<TopLogprob>,
}

#[derive(Serialize, Deserialize)]
struct LogprobsBlock {
    logprobs: Vec<TokenLogprob>,
}

/// Packs reported token log probabilities into the JSON block the node
/// appends to the assistant reply. The node only emits this when the
/// buyer asked for logprobs, so a job that didn't request them produces
/// the same output it always has and hashes identically.
pub fn logprobs_block(logprobs: Vec<TokenLogprob>) -> Content {
    let value = serde_json::to_value(LogprobsBlock { logprobs })
        .expect("token logprobs serialize infallibly");
    Content::json(value)
}

/// Reads token log probabilities back out of attested output.
///
/// `None` means no logprobs block is present (the ordinary case — the
/// buyer didn't ask, or the backend didn't report). A JSON block whose
/// `logprobs` value doesn't decode is treated as absent rather than
/// fatal, so this stays correct alongside other structured output blocks
/// (tool calls) that also ride as JSON.
pub fn parse_logprobs_output(output: &[Content]) -> Option<Vec<TokenLogprob>> {
    for content in output {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("logprobs").is_none() {
            continue;
        }
        if let Ok(block) = serde_json::from_value::<LogprobsBlock>(value.clone()) {
            return Some(block.logprobs);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<TokenLogprob> {
        vec![
            TokenLogprob {
                token: "Hello".into(),
                logprob: -0.12768971920013428,
                bytes: Some(vec![72, 101, 108, 108, 111]),
                top_logprobs: vec![
                    TopLogprob {
                        token: "Hello".into(),
                        logprob: -0.12768971920013428,
                        bytes: Some(vec![72, 101, 108, 108, 111]),
                    },
                    TopLogprob {
                        token: "Hi".into(),
                        logprob: -2.55,
                        bytes: Some(vec![72, 105]),
                    },
                ],
            },
            TokenLogprob {
                token: "!".into(),
                logprob: -0.011264529079198837,
                bytes: Some(vec![33]),
                top_logprobs: Vec::new(),
            },
        ]
    }

    #[test]
    fn logprobs_round_trip_through_the_output_block() {
        let block = logprobs_block(sample());
        let parsed = parse_logprobs_output(&[block]).expect("present");
        assert_eq!(parsed, sample());
    }

    #[test]
    fn output_without_a_logprobs_block_parses_to_none() {
        assert_eq!(parse_logprobs_output(&[Content::text("hello")]), None);
        assert_eq!(
            parse_logprobs_output(&[Content::json(serde_json::json!({"tool_calls": []}))]),
            None
        );
    }

    #[test]
    fn a_token_with_no_alternatives_omits_the_array_on_the_wire() {
        let block = logprobs_block(vec![TokenLogprob {
            token: "x".into(),
            logprob: -1.5,
            bytes: None,
            top_logprobs: Vec::new(),
        }]);
        let Content::Json { value } = &block else {
            panic!("logprobs pack as a JSON block");
        };
        assert_eq!(
            value.to_string(),
            r#"{"logprobs":[{"logprob":-1.5,"token":"x"}]}"#,
            "an empty alternatives list and absent bytes stay off the wire \
             (keys sorted, as serde_json serializes an untagged object)"
        );
    }

    #[test]
    fn the_block_hashes_deterministically() {
        // The block rides output_hash_hex, so the same logprobs must
        // serialize to the same bytes every time — the float formatting is
        // stable, and re-parsing then re-packing yields identical output.
        let first = logprobs_block(sample());
        let reparsed = parse_logprobs_output(std::slice::from_ref(&first)).expect("present");
        let second = logprobs_block(reparsed);
        assert_eq!(
            serde_json::to_vec(&[first]).unwrap(),
            serde_json::to_vec(&[second]).unwrap()
        );
    }
}
