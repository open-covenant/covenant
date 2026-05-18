//! IPC v2 streaming → Server-Sent Events encoder (ADR 0010 slice 6.a).
//!
//! ADR 0010 introduces a v2 server-streaming envelope over the Unix IPC
//! socket: [`covenant_ipc::StreamEnvelope`] is serialized as a single
//! length-prefixed JSON frame per `StreamBegin` / `StreamChunk` /
//! `StreamEnd` / `StreamError`. The same logical sequence has to reach
//! browser clients over the HTTP gateway, which framing it with the
//! Server-Sent Events (SSE) `text/event-stream` content type so that the
//! standard `EventSource` API can demultiplex it.
//!
//! This module owns the pure transformation that maps one
//! [`StreamEnvelope`] to one SSE event block. The on-wire shape is:
//!
//! ```text
//! event: <kind>\ndata: <single-line JSON of the envelope>\n\n
//! ```
//!
//! `<kind>` mirrors the envelope's `kind` discriminator
//! (`stream_begin`, `stream_chunk`, `stream_end`, `stream_error`). The
//! JSON `data:` line carries the full envelope including the `kind`
//! field — the redundancy is intentional so a consumer that reads only
//! `data:` (ignoring the `event:` line) still has the discriminator
//! available.
//!
//! Slice 6.a deliberately stops here. No axum, no async, no I/O. The
//! response-stream wiring, `Accept: text/event-stream` detection,
//! capability gating, [`crate::stream_tracker::StreamTracker`]
//! integration, and buffered-fallback path for non-SSE clients are
//! explicit follow-up sub-slices of ADR 0010 slice 6.

use covenant_ipc::StreamEnvelope;

/// Encode one [`StreamEnvelope`] as a single SSE event block.
///
/// Returns the exact bytes the HTTP gateway must emit for one frame:
/// an `event: <kind>` line, a `data: <json>` line, and a trailing
/// blank line. The trailing `\n\n` is load-bearing — SSE frames are
/// delimited by the blank line, and omitting it makes the browser
/// `EventSource` accumulate the chunk indefinitely without
/// dispatching it.
///
/// The JSON `data:` line is the compact form of the envelope
/// (`serde_json::to_string`); pretty-printing would embed real
/// newlines and split one envelope across multiple SSE `data:` lines
/// in the consumer.
///
/// The envelope is borrowed (`&StreamEnvelope`) so the future SSE
/// response-stream caller can keep ownership while emitting many
/// chunks from the same allocation.
pub fn encode_stream_envelope_as_sse(env: &StreamEnvelope) -> Result<String, serde_json::Error> {
    let event_name = match env {
        StreamEnvelope::StreamBegin { .. } => "stream_begin",
        StreamEnvelope::StreamChunk { .. } => "stream_chunk",
        StreamEnvelope::StreamEnd { .. } => "stream_end",
        StreamEnvelope::StreamError { .. } => "stream_error",
    };
    let json = serde_json::to_string(env)?;
    Ok(format!("event: {event_name}\ndata: {json}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use uuid::Uuid;

    fn parse_data_line(encoded: &str) -> (&str, &str) {
        let lines: Vec<&str> = encoded.split('\n').collect();
        assert!(
            lines.len() >= 4,
            "encoded block must have event line, data line, and trailing blank line; got {lines:?}"
        );
        assert!(
            lines[0].starts_with("event: "),
            "line 0 must start with 'event: '; got {:?}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("data: "),
            "line 1 must start with 'data: '; got {:?}",
            lines[1]
        );
        (&lines[0][7..], &lines[1][6..])
    }

    #[test]
    fn encode_stream_envelope_as_sse_begin_pins_event_name_and_round_trips() {
        let stream_id = Uuid::nil();
        let env = StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: "memories".to_string(),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let (event, data) = parse_data_line(&encoded);
        assert_eq!(event, "stream_begin");
        let parsed: StreamEnvelope =
            serde_json::from_str(data).expect("data line is valid JSON envelope");
        assert_eq!(parsed, env);
    }

    #[test]
    fn encode_stream_envelope_as_sse_chunk_pins_event_name_and_round_trips() {
        let stream_id = Uuid::from_u128(0x42);
        let env = StreamEnvelope::StreamChunk {
            stream_id,
            sequence: 7,
            chunk: json!({"id": "abc", "tier": "working"}),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let (event, data) = parse_data_line(&encoded);
        assert_eq!(event, "stream_chunk");
        let parsed: StreamEnvelope =
            serde_json::from_str(data).expect("data line is valid JSON envelope");
        assert_eq!(parsed, env);
    }

    #[test]
    fn encode_stream_envelope_as_sse_end_without_summary_omits_summary_key() {
        let env = StreamEnvelope::StreamEnd {
            stream_id: Uuid::nil(),
            summary: None,
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let (event, data) = parse_data_line(&encoded);
        assert_eq!(event, "stream_end");
        let parsed_value: Value = serde_json::from_str(data).expect("valid JSON");
        assert!(
            parsed_value.get("summary").is_none(),
            "skip_serializing_if must drop summary from the wire when None; got {parsed_value}"
        );
        let parsed: StreamEnvelope =
            serde_json::from_str(data).expect("data line is valid JSON envelope");
        assert_eq!(parsed, env);
    }

    #[test]
    fn encode_stream_envelope_as_sse_end_with_summary_round_trips() {
        let summary = json!({"intent_id": "00000000-0000-0000-0000-000000000001", "status": "ok"});
        let env = StreamEnvelope::StreamEnd {
            stream_id: Uuid::nil(),
            summary: Some(summary.clone()),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let (event, data) = parse_data_line(&encoded);
        assert_eq!(event, "stream_end");
        let parsed_value: Value = serde_json::from_str(data).expect("valid JSON");
        assert_eq!(parsed_value.get("summary"), Some(&summary));
        let parsed: StreamEnvelope =
            serde_json::from_str(data).expect("data line is valid JSON envelope");
        assert_eq!(parsed, env);
    }

    #[test]
    fn encode_stream_envelope_as_sse_error_pins_event_name_and_round_trips() {
        let env = StreamEnvelope::StreamError {
            stream_id: Uuid::nil(),
            message: "boom".to_string(),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let (event, data) = parse_data_line(&encoded);
        assert_eq!(event, "stream_error");
        let parsed: StreamEnvelope =
            serde_json::from_str(data).expect("data line is valid JSON envelope");
        assert_eq!(parsed, env);
    }

    #[test]
    fn encode_stream_envelope_as_sse_terminates_with_blank_line() {
        let env = StreamEnvelope::StreamBegin {
            stream_id: Uuid::nil(),
            response_kind: "memories".to_string(),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        assert!(
            encoded.ends_with("\n\n"),
            "SSE frame must end with blank-line terminator; got {encoded:?}"
        );
        assert!(
            !encoded.ends_with("\n\n\n"),
            "trailing terminator must be exactly one blank line; got {encoded:?}"
        );
    }

    #[test]
    fn encode_stream_envelope_as_sse_data_line_is_single_line() {
        let env = StreamEnvelope::StreamChunk {
            stream_id: Uuid::nil(),
            sequence: 0,
            chunk: json!({"text": "line1\nline2"}),
        };
        let encoded = encode_stream_envelope_as_sse(&env).expect("encode");
        let body = encoded
            .strip_suffix("\n\n")
            .expect("frame ends with blank line");
        let newlines_in_body = body.bytes().filter(|b| *b == b'\n').count();
        assert_eq!(
            newlines_in_body, 1,
            "frame body must contain exactly one newline (between event and data lines); embedded JSON newlines must be escaped. got body={body:?}"
        );
    }
}
