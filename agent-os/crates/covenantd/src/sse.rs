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

use axum::http::header::{HeaderName, HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
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

/// Classify an incoming HTTP request's `Accept` header as
/// SSE-opt-in or not.
///
/// ADR 0010 names the explicit opt-in: "A client sets `Accept:
/// text/event-stream`; the gateway responds with `Content-Type:
/// text/event-stream` ... Clients without SSE Accept get the
/// accumulated terminal response." This helper is the gate. Returns
/// `true` only when the Accept value contains `text/event-stream`
/// (case-insensitive, q-values stripped) as one of its media-type
/// entries.
///
/// The non-acceptance of `*/*` and `text/*` is deliberate. SSE and
/// the buffered-JSON fallback have different content types, framing,
/// and connection lifetimes; a browser or CLI that didn't explicitly
/// ask for SSE must not be flipped onto the long-lived `text/event-
/// stream` path. The buffered-fallback path is the safe default.
///
/// `accept` is `Option<&str>` so the caller can pass the raw header
/// value from any header map (axum, hyper, reqwest test client)
/// without coupling this helper to a specific HTTP type. `None` and
/// the empty/whitespace-only string both return `false`.
pub fn wants_event_stream(accept: Option<&str>) -> bool {
    let Some(value) = accept else { return false };
    value
        .split(',')
        .map(str::trim)
        .map(|part| part.split(';').next().unwrap_or("").trim())
        .any(|media_type| media_type.eq_ignore_ascii_case("text/event-stream"))
}

/// The pinned `(name, value)` header set an SSE response must carry.
///
/// ADR 0010 names `Content-Type: text/event-stream` as the framing
/// contract; the other two headers are universally load-bearing for
/// SSE deployments and are pinned here so a future contributor doesn't
/// drop them as "redundant":
///
/// - `Content-Type: text/event-stream` — bare media type, no
///   `charset=utf-8` suffix. Strict `EventSource` implementations
///   reject the suffix; intermediate proxies sometimes normalize it
///   away anyway, so the bare form is the safer wire shape.
/// - `Cache-Control: no-cache` — intermediate caches treat this as
///   "revalidate before reuse", which on an open streaming connection
///   means "forward every byte". `no-store` would let proxies decide
///   whether to keep partial streams; `private` would skip shared
///   caches but not handle the streaming case; neither matches the
///   contract that every chunk must reach the client now.
/// - `X-Accel-Buffering: no` — nginx-specific. Default nginx buffers
///   chunked responses and holds every chunk until the connection
///   closes, which defeats the streaming purpose. The header is a
///   no-op on non-nginx proxies and load-bearing on nginx-fronted
///   deployments.
///
/// Returned as a fixed-size array so the call site is allocation-free
/// and the count of headers is part of the type signature. The
/// buffered-fallback path (Accept header doesn't request
/// `text/event-stream`) uses axum's default JSON response shape; this
/// helper applies only to the SSE branch.
pub fn sse_response_headers() -> [(HeaderName, HeaderValue); 3] {
    [
        (CONTENT_TYPE, HeaderValue::from_static("text/event-stream")),
        (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        (
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        ),
    ]
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

    #[test]
    fn wants_event_stream_returns_false_for_none() {
        assert!(!wants_event_stream(None));
    }

    #[test]
    fn wants_event_stream_returns_false_for_empty_or_whitespace() {
        assert!(!wants_event_stream(Some("")));
        assert!(!wants_event_stream(Some("   ")));
    }

    #[test]
    fn wants_event_stream_returns_true_for_exact_match() {
        assert!(wants_event_stream(Some("text/event-stream")));
    }

    #[test]
    fn wants_event_stream_is_case_insensitive() {
        assert!(wants_event_stream(Some("TEXT/EVENT-STREAM")));
        assert!(wants_event_stream(Some("Text/Event-Stream")));
    }

    #[test]
    fn wants_event_stream_returns_true_when_in_comma_list() {
        assert!(wants_event_stream(Some(
            "application/json, text/event-stream"
        )));
        assert!(wants_event_stream(Some(
            "text/event-stream, application/json"
        )));
    }

    #[test]
    fn wants_event_stream_strips_q_values() {
        assert!(wants_event_stream(Some("text/event-stream; q=1.0")));
        assert!(wants_event_stream(Some(
            "application/json; q=1.0, text/event-stream; q=0.5"
        )));
    }

    #[test]
    fn wants_event_stream_rejects_wildcards() {
        assert!(!wants_event_stream(Some("*/*")));
        assert!(!wants_event_stream(Some("text/*")));
    }

    #[test]
    fn wants_event_stream_handles_whitespace_padding() {
        assert!(wants_event_stream(Some(
            "  text/event-stream  ; q=1.0  , application/json"
        )));
    }

    #[test]
    fn wants_event_stream_returns_false_for_plain_json() {
        assert!(!wants_event_stream(Some("application/json")));
    }

    #[test]
    fn sse_response_headers_returns_three_entries() {
        assert_eq!(sse_response_headers().len(), 3);
    }

    #[test]
    fn sse_response_headers_pins_content_type_text_event_stream() {
        let headers = sse_response_headers();
        assert_eq!(headers[0].0, CONTENT_TYPE);
        assert_eq!(headers[0].1, HeaderValue::from_static("text/event-stream"));
    }

    #[test]
    fn sse_response_headers_pins_cache_control_no_cache() {
        let headers = sse_response_headers();
        assert_eq!(headers[1].0, CACHE_CONTROL);
        assert_eq!(headers[1].1, HeaderValue::from_static("no-cache"));
    }

    #[test]
    fn sse_response_headers_pins_x_accel_buffering_no() {
        let headers = sse_response_headers();
        assert_eq!(headers[2].0.as_str(), "x-accel-buffering");
        assert_eq!(headers[2].1, HeaderValue::from_static("no"));
    }

    #[test]
    fn sse_response_headers_content_type_has_no_charset_suffix() {
        let headers = sse_response_headers();
        let content_type = headers[0].1.to_str().expect("ASCII header value");
        assert_eq!(
            content_type, "text/event-stream",
            "content-type must be the bare media type; strict EventSource implementations reject 'charset=utf-8' suffixes"
        );
    }
}
