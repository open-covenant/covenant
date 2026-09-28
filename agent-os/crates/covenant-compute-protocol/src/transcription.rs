//! Typed speech-to-text input/output over the envelope's `Vec<Content>`.
//!
//! A [`JobKind::Transcription`](crate::JobKind::Transcription) job carries
//! one audio clip in and one transcript out. The clip travels as base64
//! inside a single `Content::Json` block — the same wrap-don't-embed
//! posture [`crate::chat`] takes for a conversation and its images, so the
//! envelope's wire form and every existing signature are untouched. The
//! buyer packs with [`transcription_input`], the executing node reads with
//! [`parse_transcription_input`] and answers with [`transcription_output`],
//! and because both sides live here they cannot drift on what a
//! transcription job means — the contract [`crate::embedding`] keeps for a
//! vector, in the shape audio needs.

use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::sign::ProtocolError;

/// Cap on the base64 audio payload in one transcription request. Audio
/// dominates the request's size, so bounding it keeps the signed envelope
/// clear of the 8 MiB IPC/HTTP frame cap (`covenant-ipc`'s `MAX_FRAME`)
/// with room for the rest of the payload. Measured on the base64 text the
/// request carries, not the decoded bytes — the network relays the string
/// the backend decodes, never the audio. Longer media belongs in
/// content-addressed blob storage referenced by hash (an open seam,
/// build-notes-phase1-foundation.md), not inlined here.
pub const MAX_AUDIO_B64_BYTES: usize = 6 * 1024 * 1024;

/// A speech-to-text request: the audio to transcribe and how to read it.
///
/// Rejects an unknown field rather than dropping it: a typo'd `translate`
/// or `language` on paid input must fail loudly, not return a transcript in
/// the wrong language the buyer then pays for, the same posture
/// [`crate::generation`] takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionInput {
    /// Base64-encoded audio bytes. A 16 kHz mono WAV is the form every
    /// backend decodes; other containers depend on the operator's build,
    /// so a node that cannot decode the clip fails the job rather than
    /// guess.
    pub audio_base64: String,
    /// A hint at the container (`"wav"`, `"mp3"`), for the operator's log
    /// and backend selection. Advisory: the backend reads the bytes
    /// themselves, so a wrong or absent hint never changes the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// The spoken language as an ISO-639-1 code (`"en"`), or `None` to let
    /// the backend detect it. Skipped on the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Translate the speech into English instead of transcribing it in the
    /// source language. Skipped on the wire when false so a plain
    /// transcription request keeps the bytes it had before translation was
    /// an option.
    #[serde(default, skip_serializing_if = "is_false")]
    pub translate: bool,
    /// Ask the backend to return per-segment timestamps alongside the
    /// transcript, for captioning and alignment. Skipped on the wire when
    /// false, so a plain transcription request keeps the bytes it had
    /// before timestamps were an option. A backend that produces them
    /// fills [`TranscriptionResult::segments`]; the transcript itself is
    /// unchanged either way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub timestamps: bool,
}

impl TranscriptionInput {
    /// A plain transcription of `audio_base64`, in its own language, with
    /// no format hint — the common case.
    pub fn new(audio_base64: impl Into<String>) -> Self {
        Self {
            audio_base64: audio_base64.into(),
            format: None,
            language: None,
            translate: false,
            timestamps: false,
        }
    }
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Packs a transcription request into job input: one JSON block carrying
/// the audio and its options.
pub fn transcription_input(request: TranscriptionInput) -> Vec<Content> {
    let value = serde_json::to_value(request)
        .expect("a transcription request serializes to JSON infallibly");
    vec![Content::json(value)]
}

/// Reads a transcription request back out of job input.
///
/// `Err` means the input is not transcription-shaped (no JSON block
/// carrying `audio_base64`), or is malformed — an empty clip, or one past
/// [`MAX_AUDIO_B64_BYTES`]. A transcription job with nothing to transcribe
/// is the buyer's error; the coordinator refuses it at submission and the
/// operator at admission, so it never reaches an executor that would fail
/// the job — and take the reputation fault — on input the buyer controls.
pub fn parse_transcription_input(input: &[Content]) -> Result<TranscriptionInput, ProtocolError> {
    for content in input {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("audio_base64").is_none() {
            continue;
        }
        let request: TranscriptionInput = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("transcription input: {e}")))?;
        if request.audio_base64.is_empty() {
            return Err(ProtocolError::Invalid(
                "transcription input carries no audio".into(),
            ));
        }
        if request.audio_base64.len() > MAX_AUDIO_B64_BYTES {
            return Err(ProtocolError::Invalid(format!(
                "transcription audio is {} base64 bytes, over the {MAX_AUDIO_B64_BYTES} cap",
                request.audio_base64.len()
            )));
        }
        return Ok(request);
    }
    Err(ProtocolError::Invalid(
        "job input carries no transcription block".into(),
    ))
}

/// One timed slice of a transcript: the recognized `text` and the
/// millisecond offsets it spans, measured from the start of the clip.
/// Integer milliseconds, never floats, so a segmented transcript keeps
/// the byte-stable receipt hashing a plain one has — the OpenAI front
/// door converts to that API's float seconds only when it renders the
/// reply, off the settlement path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    /// Offset of the segment's first word from the start of the clip.
    pub start_ms: u64,
    /// Offset of the segment's end from the start of the clip. Never
    /// before `start_ms`.
    pub end_ms: u64,
    pub text: String,
}

/// A completed transcription: the recognized text, the model that produced
/// it, and the language the backend read. One `Content::Json` block of
/// this shape is the whole output — [`transcription_output`] packs it, a
/// buyer reads it with [`parse_transcription_output`]. Integers and
/// strings only, no floats, so the block's JSON re-serializes
/// byte-for-byte after the wire and the receipt's `result_hash_hex`
/// re-checks equal — the stability an embedding block of floats needs
/// `float_roundtrip` to reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub model: String,
    pub transcript: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Per-segment timings, present only when the buyer asked for them
    /// ([`TranscriptionInput::timestamps`]) and the backend produced
    /// them. Skipped on the wire when absent, so a plain transcript keeps
    /// the bytes and the hash it had before timestamps were an option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<TranscriptionSegment>>,
}

/// Packs a transcription result into job output: one JSON block. Pass
/// `segments` only when the buyer asked for timestamps and the backend
/// produced them; `None` keeps the output byte-identical to a plain
/// transcript.
pub fn transcription_output(
    model: impl Into<String>,
    transcript: impl Into<String>,
    language: Option<String>,
    segments: Option<Vec<TranscriptionSegment>>,
) -> Content {
    let result = TranscriptionResult {
        model: model.into(),
        transcript: transcript.into(),
        language,
        segments,
    };
    let value =
        serde_json::to_value(result).expect("a transcription result serializes to JSON infallibly");
    Content::json(value)
}

/// Reads a transcription result back out of job output.
///
/// `Err` means the output is not transcription-shaped (no JSON block
/// carrying `transcript`) or is malformed — a buyer that asked for a
/// transcription and got a text completion instead should see that
/// plainly, not a silently empty string.
pub fn parse_transcription_output(
    output: &[Content],
) -> Result<TranscriptionResult, ProtocolError> {
    for content in output {
        let Content::Json { value } = content else {
            continue;
        };
        if value.get("transcript").is_none() {
            continue;
        }
        let result: TranscriptionResult = serde_json::from_value(value.clone())
            .map_err(|e| ProtocolError::Invalid(format!("transcription output: {e}")))?;
        return Ok(result);
    }
    Err(ProtocolError::Invalid(
        "job output carries no transcription block".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_round_trips_through_parse() {
        let request = TranscriptionInput {
            audio_base64: "UklGR... (base64 audio)".into(),
            format: Some("wav".into()),
            language: Some("en".into()),
            translate: true,
            timestamps: true,
        };
        let input = transcription_input(request.clone());
        let parsed = parse_transcription_input(&input).expect("transcription-shaped");
        assert_eq!(parsed, request);
    }

    #[test]
    fn a_plain_request_skips_its_default_fields_on_the_wire() {
        // A plain transcription must not carry `translate`, `language`, or
        // `format` keys it never set — the wire form stays minimal and a
        // future field can be added without disturbing these bytes.
        let input = transcription_input(TranscriptionInput::new("YWJj"));
        let Content::Json { value } = &input[0] else {
            panic!("packed as a JSON block");
        };
        assert!(value.get("translate").is_none(), "{value}");
        assert!(value.get("language").is_none(), "{value}");
        assert!(value.get("format").is_none(), "{value}");
        assert!(value.get("timestamps").is_none(), "{value}");
    }

    #[test]
    fn plain_text_input_is_not_transcription_shaped() {
        let input = vec![Content::text("transcribe this")];
        let err = parse_transcription_input(&input).expect_err("no audio block");
        assert!(err.to_string().contains("no transcription block"), "{err}");
    }

    #[test]
    fn a_json_block_without_audio_is_not_transcription_shaped() {
        let input = vec![Content::json(serde_json::json!({ "messages": [] }))];
        let err = parse_transcription_input(&input).expect_err("no audio field");
        assert!(err.to_string().contains("no transcription block"), "{err}");
    }

    #[test]
    fn a_typoed_field_is_refused_rather_than_run_at_the_default() {
        // `translte` is not `translate`; dropping it silently would return a
        // source-language transcript the buyer asked to have in English, at
        // the same price. The paid input fails instead.
        let input = vec![Content::json(serde_json::json!({
            "audio_base64": "AAAA",
            "translte": true,
        }))];
        let err = parse_transcription_input(&input).expect_err("a typoed knob must fail");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn an_empty_clip_fails_loudly() {
        let input = transcription_input(TranscriptionInput::new(""));
        let err = parse_transcription_input(&input).expect_err("nothing to transcribe");
        assert!(err.to_string().contains("no audio"), "{err}");
    }

    #[test]
    fn an_oversized_clip_is_rejected() {
        let input =
            transcription_input(TranscriptionInput::new("A".repeat(MAX_AUDIO_B64_BYTES + 1)));
        let err = parse_transcription_input(&input).expect_err("past the cap");
        assert!(err.to_string().contains("over the"), "{err}");
        // The bound is inclusive: a clip right at the cap still parses.
        let ok = transcription_input(TranscriptionInput::new("A".repeat(MAX_AUDIO_B64_BYTES)));
        assert!(parse_transcription_input(&ok).is_ok());
    }

    #[test]
    fn output_round_trips() {
        let output = vec![transcription_output(
            "whisper-base.en",
            "and so my fellow americans",
            Some("en".into()),
            None,
        )];
        let result = parse_transcription_output(&output).expect("transcription-shaped");
        assert_eq!(result.model, "whisper-base.en");
        assert_eq!(result.transcript, "and so my fellow americans");
        assert_eq!(result.language.as_deref(), Some("en"));
        assert!(result.segments.is_none());
    }

    #[test]
    fn a_plain_output_carries_no_segments_key_on_the_wire() {
        // A transcription without timestamps must not sprout a `segments`
        // key it never had — the wire bytes, and so the receipt hash, stay
        // exactly what they were before timestamps existed.
        let output = transcription_output("whisper-1", "hello", None, None);
        let Content::Json { value } = &output else {
            panic!("packed as a JSON block");
        };
        assert!(value.get("segments").is_none(), "{value}");
        assert!(value.get("language").is_none(), "{value}");
    }

    #[test]
    fn segments_round_trip_and_hash_stably() {
        let segments = vec![
            TranscriptionSegment {
                start_ms: 0,
                end_ms: 1_500,
                text: "covenant".into(),
            },
            TranscriptionSegment {
                start_ms: 1_500,
                end_ms: 3_200,
                text: "compute".into(),
            },
        ];
        let output = vec![transcription_output(
            "whisper-1",
            "covenant compute",
            Some("en".into()),
            Some(segments.clone()),
        )];
        let result = parse_transcription_output(&output).expect("transcription-shaped");
        assert_eq!(result.segments.as_deref(), Some(segments.as_slice()));

        // Integer-ms segments carry no floats, so the segmented block
        // hashes identically after a wire round-trip — the same settlement
        // stability a plain transcript has.
        let on_the_wire = serde_json::to_string(&output).unwrap();
        let received: Vec<Content> = serde_json::from_str(&on_the_wire).unwrap();
        assert_eq!(
            crate::output_hash_hex(&output),
            crate::output_hash_hex(&received),
            "a segmented transcript must hash the same across the wire"
        );
    }

    #[test]
    fn a_text_completion_is_not_a_transcription() {
        let output = vec![Content::text("the answer is blue")];
        let err = parse_transcription_output(&output).expect_err("not transcription-shaped");
        assert!(err.to_string().contains("no transcription block"), "{err}");
    }

    // The output is strings only, so the block a node hashes into its
    // receipt re-serializes to identical bytes after the wire and the
    // coordinator's re-hash matches — the settlement regression embeddings
    // needed `float_roundtrip` to avoid, which a text transcript sidesteps
    // by carrying no floats at all.
    #[test]
    fn a_transcription_block_hashes_the_same_after_a_wire_round_trip() {
        let output = vec![transcription_output(
            "whisper-base.en",
            "ask not what your country can do for you",
            Some("en".into()),
            None,
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
