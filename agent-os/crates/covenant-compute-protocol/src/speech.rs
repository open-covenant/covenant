//! Typed text-to-speech input/output over the envelope's `Vec<Content>`.
//!
//! A [`JobKind::SpeechSynthesis`](crate::JobKind::SpeechSynthesis) job
//! carries one line of text in and one synthesized audio clip out — the
//! mirror of [`crate::transcription`], which runs the other direction. The
//! text and its options travel in a single `Content::Json` block, and the
//! audio comes back base64 inside another, the same wrap-don't-embed
//! posture the rest of this crate keeps, so the envelope's wire form and
//! every existing signature are untouched. The buyer packs with
//! [`speech_input`], the executing node reads with [`parse_speech_input`]
//! and answers with [`speech_output`], and because both sides live here
//! they cannot drift on what a speech job means.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// Cap on the text one synthesis request may carry, in characters. Speech
/// is billed by the envelope, but the text bounds the audio the operator
/// returns: longer text is a longer clip, and a clip near the 8 MiB
/// IPC/HTTP frame cap (`covenant-ipc`'s `MAX_FRAME`) cannot ride back
/// inline. This matches OpenAI's own 4096-character ceiling on
/// `/v1/audio/speech`, which keeps even uncompressed PCM well under the
/// frame. Longer copy belongs split across requests, or in a
/// content-addressed blob path (an open seam,
/// build-notes-phase1-foundation.md), not one inlined clip.
pub const MAX_SPEECH_TEXT_CHARS: usize = 4096;

/// The slowest and fastest playback the network accepts, as a multiple of
/// the voice's natural rate. OpenAI's `/v1/audio/speech` bounds `speed` to
/// this same window; a value outside it is the buyer's error, refused
/// before the job is priced rather than clamped silently to something they
/// did not ask for.
pub const MIN_SPEECH_SPEED: f32 = 0.25;
pub const MAX_SPEECH_SPEED: f32 = 4.0;

/// A text-to-speech request: the words to speak and how to voice them.
///
/// Rejects an unknown field rather than dropping it: a typo'd `speed` or
/// `voice` on paid input must fail loudly, not synthesize at the default
/// the buyer did not ask for, the same posture [`crate::generation`] takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechInput {
    /// The text to synthesize. UTF-8, at most [`MAX_SPEECH_TEXT_CHARS`]
    /// characters — the words the operator's speech model reads aloud.
    pub text: String,
    /// A named voice for the backend to use (`"Alex"`, `"alloy"`).
    /// Advisory and backend-specific: a node that does not know the voice
    /// falls back to its default rather than failing the job, so a plain
    /// request that names none still speaks. Skipped on the wire when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// The container the buyer wants back (`"wav"`, `"aiff"`). A node that
    /// cannot produce the named container fails the job rather than return
    /// a different one under the buyer's chosen name. Skipped on the wire
    /// when absent; the node then picks its own default (WAV, the form
    /// every client decodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Playback rate as a multiple of the voice's natural speed
    /// (`1.0` = normal), bounded to [`MIN_SPEECH_SPEED`]..=[`MAX_SPEECH_SPEED`].
    /// Skipped on the wire when absent, so a plain request keeps the bytes
    /// it had before speed was an option. This field rides in the signed
    /// envelope verbatim, so its float value never re-serializes for the
    /// receipt hash — the output, which must, carries no floats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

impl SpeechInput {
    /// A plain synthesis of `text` in the node's default voice and
    /// container at natural speed — the common case.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: None,
            format: None,
            speed: None,
        }
    }
}

/// Packs a synthesis request into job input: one JSON block carrying the
/// text and its options.
pub fn speech_input(request: SpeechInput) -> Vec<Content> {
    let value =
        serde_json::to_value(request).expect("a speech request serializes to JSON infallibly");
    vec![Content::json(value)]
}

/// Reads a synthesis request back out of job input.
///
/// `Err` means the input is not speech-shaped (no JSON block carrying
/// `text`), or is malformed — empty text, text past [`MAX_SPEECH_TEXT_CHARS`],
/// or a `speed` outside the accepted window. A speech job with nothing to
/// say is the buyer's error; the coordinator refuses it at submission and
/// the operator at admission, so it never reaches an executor that would
/// fail the job — and take the reputation fault — on input the buyer
/// controls.
pub fn parse_speech_input(input: &[Content]) -> Result<SpeechInput, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("text").is_none() {
            continue;
        }
        let request: SpeechInput = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("speech input: {e}")))?;
        if request.text.trim().is_empty() {
            return Err(ProtocolError::Invalid(
                "speech input carries no text to speak".into(),
            ));
        }
        let chars = request.text.chars().count();
        if chars > MAX_SPEECH_TEXT_CHARS {
            return Err(ProtocolError::Invalid(format!(
                "speech text is {chars} characters, over the {MAX_SPEECH_TEXT_CHARS} cap"
            )));
        }
        if let Some(speed) = request.speed {
            if !(MIN_SPEECH_SPEED..=MAX_SPEECH_SPEED).contains(&speed) {
                return Err(ProtocolError::Invalid(format!(
                    "speech speed {speed} is outside {MIN_SPEECH_SPEED}..={MAX_SPEECH_SPEED}"
                )));
            }
        }
        return Ok(request);
    }
    Err(ProtocolError::Invalid(
        "job input carries no speech block".into(),
    ))
}

/// A completed synthesis: the audio, the model that produced it, and how to
/// read the bytes. One `Content::Json` block of this shape is the whole
/// output — [`speech_output`] packs it, a buyer reads it with
/// [`parse_speech_output`]. Integers and strings only, no floats, so the
/// block's JSON re-serializes byte-for-byte after the wire and the
/// receipt's `result_hash_hex` re-checks equal — the stability an
/// embedding block of floats needs `float_roundtrip` to reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechResult {
    pub model: String,
    /// Base64-encoded audio bytes in the stated [`format`](Self::format).
    pub audio_base64: String,
    /// The container the audio is in (`"wav"`, `"aiff"`), so a client
    /// decodes it without guessing.
    pub format: String,
    /// The clip's sample rate in hertz, when the backend reports one.
    /// Skipped on the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate_hz: Option<u32>,
}

/// Packs a synthesis result into job output: one JSON block.
pub fn speech_output(
    model: impl Into<String>,
    audio_base64: impl Into<String>,
    format: impl Into<String>,
    sample_rate_hz: Option<u32>,
) -> Content {
    let result = SpeechResult {
        model: model.into(),
        audio_base64: audio_base64.into(),
        format: format.into(),
        sample_rate_hz,
    };
    let value =
        serde_json::to_value(result).expect("a speech result serializes to JSON infallibly");
    Content::json(value)
}

/// Reads a synthesis result back out of job output.
///
/// `Err` means the output is not speech-shaped (no JSON block carrying
/// `audio_base64`) or is malformed — a buyer that asked for speech and got
/// a text completion instead should see that plainly, not a silently empty
/// clip.
pub fn parse_speech_output(output: &[Content]) -> Result<SpeechResult, ProtocolError> {
    for content in output {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("audio_base64").is_none() {
            continue;
        }
        let result: SpeechResult = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("speech output: {e}")))?;
        return Ok(result);
    }
    Err(ProtocolError::Invalid(
        "job output carries no speech block".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_round_trips_through_parse() {
        let request = SpeechInput {
            text: "covenant compute speaks".into(),
            voice: Some("Alex".into()),
            format: Some("wav".into()),
            speed: Some(1.25),
        };
        let input = speech_input(request.clone());
        let parsed = parse_speech_input(&input).expect("speech-shaped");
        assert_eq!(parsed, request);
    }

    #[test]
    fn a_plain_request_skips_its_default_fields_on_the_wire() {
        // A plain synthesis must not carry `voice`, `format`, or `speed`
        // keys it never set — the wire form stays minimal and a future
        // field can be added without disturbing these bytes.
        let input = speech_input(SpeechInput::new("hello"));
        let Content::Json { value } = &input[0] else {
            panic!("packed as a JSON block");
        };
        assert!(value.get("voice").is_none(), "{value}");
        assert!(value.get("format").is_none(), "{value}");
        assert!(value.get("speed").is_none(), "{value}");
    }

    #[test]
    fn plain_text_content_is_not_speech_shaped() {
        // A bare text block is not a speech request: the request rides a
        // JSON block, not the loose prompt shape a chat job uses.
        let input = vec![Content::text("say this")];
        let err = parse_speech_input(&input).expect_err("no speech block");
        assert!(err.to_string().contains("no speech block"), "{err}");
    }

    #[test]
    fn a_json_block_without_text_is_not_speech_shaped() {
        let input = vec![Content::json(serde_json::json!({ "audio_base64": "AAAA" }))];
        let err = parse_speech_input(&input).expect_err("no text field");
        assert!(err.to_string().contains("no speech block"), "{err}");
    }

    #[test]
    fn a_typoed_field_is_refused_rather_than_run_at_the_default() {
        // `spede` is not `speed`; dropping it silently would synthesize at
        // the natural speed the buyer never asked for. The paid input fails.
        let input = vec![Content::json(serde_json::json!({
            "text": "hello",
            "spede": 2.0,
        }))];
        let err = parse_speech_input(&input).expect_err("a typoed knob must fail");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn empty_text_fails_loudly() {
        let input = speech_input(SpeechInput::new("   \n "));
        let err = parse_speech_input(&input).expect_err("nothing to speak");
        assert!(err.to_string().contains("no text"), "{err}");
    }

    #[test]
    fn text_past_the_cap_is_rejected() {
        let input = speech_input(SpeechInput::new("a".repeat(MAX_SPEECH_TEXT_CHARS + 1)));
        let err = parse_speech_input(&input).expect_err("past the cap");
        assert!(err.to_string().contains("over the"), "{err}");
        // The bound is inclusive: text right at the cap still parses.
        let ok = speech_input(SpeechInput::new("a".repeat(MAX_SPEECH_TEXT_CHARS)));
        assert!(parse_speech_input(&ok).is_ok());
    }

    #[test]
    fn a_speed_outside_the_window_is_rejected() {
        let mut req = SpeechInput::new("hi");
        req.speed = Some(10.0);
        let err = parse_speech_input(&speech_input(req)).expect_err("too fast");
        assert!(err.to_string().contains("outside"), "{err}");

        let mut req = SpeechInput::new("hi");
        req.speed = Some(0.1);
        let err = parse_speech_input(&speech_input(req)).expect_err("too slow");
        assert!(err.to_string().contains("outside"), "{err}");

        // The bounds are inclusive.
        for good in [MIN_SPEECH_SPEED, 1.0, MAX_SPEECH_SPEED] {
            let mut req = SpeechInput::new("hi");
            req.speed = Some(good);
            assert!(parse_speech_input(&speech_input(req)).is_ok(), "{good}");
        }
    }

    #[test]
    fn output_round_trips() {
        let output = vec![speech_output("say-1", "UklGRiQ", "wav", Some(22_050))];
        let result = parse_speech_output(&output).expect("speech-shaped");
        assert_eq!(result.model, "say-1");
        assert_eq!(result.audio_base64, "UklGRiQ");
        assert_eq!(result.format, "wav");
        assert_eq!(result.sample_rate_hz, Some(22_050));
    }

    #[test]
    fn a_result_without_a_sample_rate_skips_the_key() {
        let output = speech_output("say-1", "UklGRiQ", "wav", None);
        let Content::Json { value } = &output else {
            panic!("packed as a JSON block");
        };
        assert!(value.get("sample_rate_hz").is_none(), "{value}");
    }

    #[test]
    fn a_text_completion_is_not_speech() {
        let output = vec![Content::text("the answer is blue")];
        let err = parse_speech_output(&output).expect_err("not speech-shaped");
        assert!(err.to_string().contains("no speech block"), "{err}");
    }

    // The output is strings and integers only, so the block a node hashes
    // into its receipt re-serializes to identical bytes after the wire and
    // the coordinator's re-hash matches — the settlement regression
    // embeddings needed `float_roundtrip` to avoid, which audio bytes
    // sidestep by carrying no floats at all.
    #[test]
    fn a_speech_block_hashes_the_same_after_a_wire_round_trip() {
        let output = vec![speech_output(
            "say-1",
            "UklGRiQAAABXQVZFZm10IBAAAAABAAEA",
            "wav",
            Some(22_050),
        )];
        let on_the_wire = serde_json::to_string(&output).unwrap();
        let received: Vec<Content> = serde_json::from_str(&on_the_wire).unwrap();
        assert_eq!(
            crate::output_hash_hex(&output),
            crate::output_hash_hex(&received),
            "the wire round-trip changed the output hash, so settlement would reject it"
        );
    }
}
