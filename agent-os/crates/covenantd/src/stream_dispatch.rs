//! IPC v2 streaming dispatch helpers (ADR 0010 slice 4).
//!
//! ADR 0010 introduces a v2 server-streaming envelope: a `StreamBegin`
//! frame, zero or more `StreamChunk` frames carrying the per-chunk
//! payload, then exactly one `StreamEnd` (success) or `StreamError`
//! (failure). This module owns the pure frame-emission contract for
//! one streamable verb at a time. It does NOT touch capability
//! gating, peer auth, or the daemon's `Server::respond` dispatch —
//! callers do all of that, then hand a pre-computed page of records
//! to a helper here.
//!
//! Splitting the helper from the dispatch wiring keeps the diff
//! reviewable: this slice ships only the primitive, with unit tests
//! that drive the helper through a `Vec<u8>`-backed writer and decode
//! every frame back via [`covenant_ipc::read_frame`]. The wiring
//! slice (ipc-v2-stream-recent-memory-dispatch-wiring or similar)
//! integrates this helper into the `Server::handle` loop and
//! pre-allocates the stream_id so it can register the entry with
//! [`crate::stream_tracker::StreamTracker`] before the first frame
//! goes out.

use covenant_audit::AuditEvent;
use covenant_ipc::{write_frame, IpcError, StreamEnvelope};
use covenant_runtime::AgentResult;
use covenant_types::MemoryRecord;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Per-chunk schema string ADR 0010 names for streamed memory
/// records. Pinned here as a const so the emit helper, the tracker
/// register call in the streaming orchestrators, and any
/// fixture validator stay in sync. A rename here is a v2 break —
/// every v2-aware client that reads `stream_begin.schema` to route
/// the subsequent chunks would mis-dispatch.
pub const MEMORY_CHUNK_SCHEMA: &str = "covenant.ipc.v2.chunk.memory-record.v1";

/// Logical `response_kind` the daemon advertises in the
/// `StreamBegin` frame for a streamed memory page. ADR 0010 pins
/// this as the lowercase plural of the terminal `Response` variant
/// (`Response::Memories` → `"memories"`); a v2-aware client matches
/// on this literal to route the subsequent chunks to its memory-row
/// decoder. A rename breaks every v2 memory consumer at the routing
/// layer.
pub const MEMORY_RESPONSE_KIND: &str = "memories";

/// Per-chunk schema string ADR 0010 names for streamed audit events.
/// Symmetric with [`MEMORY_CHUNK_SCHEMA`]; pinned as a const so the
/// emit helper, future tracker register call, and any fixture
/// validator stay in sync. A rename here is a v2 break.
pub const AUDIT_CHUNK_SCHEMA: &str = "covenant.ipc.v2.chunk.audit-event.v1";

/// Logical `response_kind` for a streamed audit-events page. ADR
/// 0010 doesn't name this literal explicitly; the convention follows
/// the terminal `Response` variant name (`Response::AuditEvents`)
/// snake-cased to `"audit_events"`. A v2-aware audit consumer
/// matches on this literal to route the subsequent chunks.
pub const AUDIT_RESPONSE_KIND: &str = "audit_events";

/// Per-chunk schema string ADR 0010 names for streamed SubmitIntent
/// content. Symmetric with [`MEMORY_CHUNK_SCHEMA`] and
/// [`AUDIT_CHUNK_SCHEMA`]; pinned as a const so the emit helper,
/// future tracker register call, and any fixture validator stay in
/// sync. A rename here is a v2 break — every v2-aware client that
/// reads `stream_begin.schema` to route the subsequent chunks would
/// mis-dispatch.
pub const INTENT_RESULT_CHUNK_SCHEMA: &str = "covenant.ipc.v2.chunk.agent-result.v1";

/// Logical `response_kind` for a streamed SubmitIntent dispatch. ADR
/// 0010 doesn't name this literal explicitly; the convention follows
/// the terminal `Response` variant name (`Response::IntentResult`)
/// snake-cased to `"intent_result"`. A v2-aware intent consumer
/// matches on this literal to route the subsequent chunks to its
/// AgentResult decoder.
pub const INTENT_RESPONSE_KIND: &str = "intent_result";

/// Emit a v2 streaming-response sequence for one memory page.
///
/// Writes exactly:
///
/// 1. `StreamEnvelope::StreamBegin { stream_id, response_kind: "memories" }`
/// 2. `StreamEnvelope::StreamChunk { stream_id, sequence: i, chunk: <record> }`
///    for each `record` in `records`, with `sequence` counting from
///    `0` and incrementing by one per record.
/// 3. `StreamEnvelope::StreamEnd { stream_id, summary: None }`
///
/// An empty `records` slice still emits exactly one `StreamBegin`
/// and one `StreamEnd` — a stream that never opens is
/// indistinguishable from a dead daemon at the protocol layer, so
/// every call to this helper produces the begin+end pair.
///
/// `stream_id` is accepted as a parameter (not allocated inside the
/// helper) so the caller can pre-register the entry in
/// [`crate::stream_tracker::StreamTracker`] before the first frame
/// goes out — the tracker's snapshot stays consistent with the wire.
///
/// The chunk payload is `serde_json::to_value(record)` verbatim — no
/// transformation, no field renaming, no skipping. Per-chunk schema
/// is [`MEMORY_CHUNK_SCHEMA`]; consumers route via the
/// `stream_begin.response_kind` literal ([`MEMORY_RESPONSE_KIND`]),
/// not via the chunk schema, but the schema string remains the
/// durable contract for fixture validators and operator-facing
/// snapshot labels.
///
/// Capability gating happens BEFORE the helper is called. The
/// caller has already filtered `records` to the tier slice the peer
/// is authorized to read; this function trusts its input.
pub async fn emit_memory_stream<W>(
    writer: &mut W,
    stream_id: Uuid,
    records: &[MemoryRecord],
) -> Result<(), IpcError>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(
        writer,
        &StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: MEMORY_RESPONSE_KIND.to_string(),
        },
    )
    .await?;

    for (sequence, record) in records.iter().enumerate() {
        let chunk = serde_json::to_value(record).map_err(IpcError::Serde)?;
        write_frame(
            writer,
            &StreamEnvelope::StreamChunk {
                stream_id,
                sequence: sequence as u32,
                chunk,
            },
        )
        .await?;
    }

    write_frame(
        writer,
        &StreamEnvelope::StreamEnd {
            stream_id,
            summary: None,
        },
    )
    .await?;

    Ok(())
}

/// Emit a v2 streaming-response sequence for one audit-events page.
///
/// Symmetric with [`emit_memory_stream`]: writes exactly one
/// [`StreamEnvelope::StreamBegin`] (`response_kind = "audit_events"`),
/// one [`StreamEnvelope::StreamChunk`] per event (sequence counting
/// from `0`, chunk = `serde_json::to_value(event)` verbatim), then
/// one [`StreamEnvelope::StreamEnd`] with `summary: None`. Empty
/// `events` still emits the begin+end pair so a stream that never
/// opens is never confused with a dead daemon at the protocol layer.
///
/// `stream_id` is a parameter so the caller can pre-register the
/// entry in [`crate::stream_tracker::StreamTracker`] before the
/// first frame goes out. Capability gating happens BEFORE the
/// helper is called — the caller has already filtered `events` to
/// the audience the peer is authorized to read.
///
/// The per-chunk payload schema is [`AUDIT_CHUNK_SCHEMA`]; consumers
/// route via [`AUDIT_RESPONSE_KIND`] on the StreamBegin frame.
pub async fn emit_audit_stream<W>(
    writer: &mut W,
    stream_id: Uuid,
    events: &[AuditEvent],
) -> Result<(), IpcError>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(
        writer,
        &StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: AUDIT_RESPONSE_KIND.to_string(),
        },
    )
    .await?;

    for (sequence, event) in events.iter().enumerate() {
        let chunk = serde_json::to_value(event).map_err(IpcError::Serde)?;
        write_frame(
            writer,
            &StreamEnvelope::StreamChunk {
                stream_id,
                sequence: sequence as u32,
                chunk,
            },
        )
        .await?;
    }

    write_frame(
        writer,
        &StreamEnvelope::StreamEnd {
            stream_id,
            summary: None,
        },
    )
    .await?;

    Ok(())
}

/// Emit a v2 streaming-response sequence for one SubmitIntent
/// dispatch.
///
/// Structurally symmetric with [`emit_memory_stream`] and
/// [`emit_audit_stream`]: writes exactly one
/// [`StreamEnvelope::StreamBegin`]
/// (`response_kind = "intent_result"`), one
/// [`StreamEnvelope::StreamChunk`] per `AgentResult` in `results`
/// (sequence counting from `0`, chunk = `serde_json::to_value(result)`
/// verbatim), then one [`StreamEnvelope::StreamEnd`] carrying
/// `summary`. An empty `results` slice still emits the begin+end pair
/// so a stream that never opens is never confused with a dead daemon
/// at the protocol layer.
///
/// The `summary` parameter is the asymmetry with memory/audit. The
/// `Response::IntentResult` variant carries `intent_id`, `status`, and
/// `settlement` fields that aren't part of `AgentResult` and don't fit
/// in an AgentResult chunk. ADR 0010's `StreamEnvelope::StreamEnd`
/// already exposes a `summary: Option<serde_json::Value>` field for
/// exactly this kind of trailing metadata; passing `Some(value)` here
/// puts that value on the wire in the terminal frame. Passing `None`
/// matches the memory/audit terminal-frame shape. Slice 5.c — the
/// `Server::stream_submit_intent` orchestrator — will populate the
/// summary from the dispatched `Response::IntentResult`.
///
/// `stream_id` is a parameter so the caller can pre-register the
/// entry in [`crate::stream_tracker::StreamTracker`] before the first
/// frame goes out. Capability gating happens BEFORE the helper is
/// called — the caller has already authorized the peer to dispatch
/// the intent.
///
/// The per-chunk payload schema is [`INTENT_RESULT_CHUNK_SCHEMA`]
/// (`covenant.ipc.v2.chunk.agent-result.v1`); consumers route via
/// [`INTENT_RESPONSE_KIND`] on the StreamBegin frame.
///
/// Today the only caller will pass a single-element slice carrying
/// the final `AgentResult` once the synchronous runner returns; the
/// fallback shape is byte-equivalent to a v1 terminal response
/// wrapped in v2 frames with the IntentResult bookkeeping in
/// `summary`. The slice surface gives a future streaming runtime
/// extension a path to emit multiple partial `AgentResult` chunks
/// without changing this signature.
pub async fn emit_intent_stream<W>(
    writer: &mut W,
    stream_id: Uuid,
    results: &[AgentResult],
    summary: Option<serde_json::Value>,
) -> Result<(), IpcError>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(
        writer,
        &StreamEnvelope::StreamBegin {
            stream_id,
            response_kind: INTENT_RESPONSE_KIND.to_string(),
        },
    )
    .await?;

    for (sequence, result) in results.iter().enumerate() {
        let chunk = serde_json::to_value(result).map_err(IpcError::Serde)?;
        write_frame(
            writer,
            &StreamEnvelope::StreamChunk {
                stream_id,
                sequence: sequence as u32,
                chunk,
            },
        )
        .await?;
    }

    write_frame(writer, &StreamEnvelope::StreamEnd { stream_id, summary }).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_ipc::read_frame;
    use covenant_types::{AgentId, MemoryTier};
    use std::io::Cursor;

    fn fixture_record(seed: u8, text: &str) -> MemoryRecord {
        MemoryRecord {
            id: Uuid::from_bytes([seed; 16]),
            tier: MemoryTier::Working,
            owner: AgentId::new("test@local", [seed; 32]),
            text: text.into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: 1_700_000_000_000 + seed as u64,
            parent: None,
        }
    }

    async fn drain_envelopes(buf: &[u8]) -> Vec<StreamEnvelope> {
        let mut cursor = Cursor::new(buf);
        let mut out = Vec::new();
        // Loop until the cursor hits EOF; read_frame returns
        // UnexpectedEof on a drained reader.
        while let Ok(env) = read_frame::<_, StreamEnvelope>(&mut cursor).await {
            out.push(env);
        }
        out
    }

    #[tokio::test]
    async fn empty_records_emits_exactly_begin_then_end() {
        // Load-bearing failure mode: a stream that never opens is
        // indistinguishable from a dead daemon. Every helper call
        // must emit the begin+end pair regardless of record count.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_memory_stream(&mut buf, stream_id, &[]).await.unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(
            envelopes.len(),
            2,
            "empty page must emit exactly two frames"
        );
        match &envelopes[0] {
            StreamEnvelope::StreamBegin {
                stream_id: sid,
                response_kind,
            } => {
                assert_eq!(*sid, stream_id);
                assert_eq!(response_kind, MEMORY_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
        match &envelopes[1] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert!(summary.is_none());
            }
            other => panic!("frame 1 must be StreamEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn three_records_emits_begin_three_chunks_end_with_monotonic_sequence() {
        // ADR 0010 pins sequence as 'monotonically increasing index
        // within stream_id, starting at 0'. Asserting exact 0/1/2
        // catches a refactor that used a hash-based ordering or
        // skipped a sequence value.
        let stream_id = Uuid::new_v4();
        let records = vec![
            fixture_record(1, "alpha"),
            fixture_record(2, "beta"),
            fixture_record(3, "gamma"),
        ];
        let mut buf = Vec::new();
        emit_memory_stream(&mut buf, stream_id, &records)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(envelopes.len(), 5, "begin + 3 chunks + end = 5 frames");

        assert!(
            matches!(&envelopes[0], StreamEnvelope::StreamBegin { stream_id: sid, .. } if *sid == stream_id),
            "frame 0 must be StreamBegin with the caller's stream_id"
        );

        for (i, record) in records.iter().enumerate() {
            match &envelopes[1 + i] {
                StreamEnvelope::StreamChunk {
                    stream_id: sid,
                    sequence,
                    chunk,
                } => {
                    assert_eq!(*sid, stream_id, "chunk {i} stream_id must match begin");
                    assert_eq!(
                        *sequence, i as u32,
                        "chunk sequence must be monotonic from 0"
                    );
                    let expected = serde_json::to_value(record).unwrap();
                    assert_eq!(
                        *chunk, expected,
                        "chunk payload must equal serde_json::to_value(&record) verbatim"
                    );
                }
                other => panic!("frame {} must be StreamChunk, got {other:?}", 1 + i),
            }
        }

        match &envelopes[4] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert!(summary.is_none());
            }
            other => panic!("frame 4 must be StreamEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_id_is_constant_across_every_emitted_frame() {
        // ADR 0010 pins stream_id as carried by every envelope
        // variant so multiplexed connections can demultiplex chunks.
        // A refactor that allocated a fresh stream_id per chunk
        // (e.g., via Uuid::new_v4() inside the loop) would break
        // multiplexing silently — this assertion catches it.
        let stream_id = Uuid::new_v4();
        let records = vec![fixture_record(7, "one"), fixture_record(8, "two")];
        let mut buf = Vec::new();
        emit_memory_stream(&mut buf, stream_id, &records)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        for (i, env) in envelopes.iter().enumerate() {
            let sid = match env {
                StreamEnvelope::StreamBegin { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamChunk { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamEnd { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamError { stream_id, .. } => *stream_id,
            };
            assert_eq!(sid, stream_id, "frame {i} stream_id diverged from caller's");
        }
    }

    #[tokio::test]
    async fn response_kind_pins_memories_literal() {
        // ADR 0010 names the response_kind literal in the docs as
        // 'memories' (lowercase plural of Response::Memories). A
        // refactor that drifted it to 'memory' or 'recent_memory'
        // breaks every v2-aware memory consumer at the routing
        // layer; this pin catches the drift.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_memory_stream(&mut buf, stream_id, &[]).await.unwrap();
        let envelopes = drain_envelopes(&buf).await;
        let begin = &envelopes[0];
        match begin {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "memories");
                assert_eq!(response_kind, MEMORY_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
    }

    #[test]
    fn memory_chunk_schema_pins_adr_0010_literal() {
        // ADR 0010 names the per-chunk schema string. Even though
        // the schema is not emitted on the wire by emit_memory_stream
        // (the begin frame's schema field is response_kind, not
        // chunk schema), the const is the durable contract surface
        // for the future stream_tracker register call and any
        // fixture validator. A rename here is a v2 break — pin it.
        assert_eq!(
            MEMORY_CHUNK_SCHEMA,
            "covenant.ipc.v2.chunk.memory-record.v1"
        );
    }

    fn fixture_event(seed: u8) -> AuditEvent {
        AuditEvent {
            id: Uuid::from_bytes([seed; 16]),
            timestamp_ms: 1_700_000_000_000 + seed as u64,
            issuer: AgentId::new("test@local", [seed; 32]),
            kind: covenant_audit::AuditKind::IntentDispatched {
                intent_id: Uuid::from_bytes([seed.wrapping_add(1); 16]),
                intent_text: format!("intent {seed}"),
                matched_agent: Some("test-agent".into()),
                result_hash_hex: format!("{:064x}", seed as u64),
                status: "ok".into(),
            },
        }
    }

    #[tokio::test]
    async fn emit_audit_stream_empty_events_emits_exactly_begin_then_end() {
        // Symmetric to the memory empty-records pin. Every helper
        // call must emit begin+end regardless of event count so the
        // protocol layer can distinguish 'opened, no rows' from
        // 'never opened'.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_audit_stream(&mut buf, stream_id, &[]).await.unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(
            envelopes.len(),
            2,
            "empty page must emit exactly two frames"
        );
        match &envelopes[0] {
            StreamEnvelope::StreamBegin {
                stream_id: sid,
                response_kind,
            } => {
                assert_eq!(*sid, stream_id);
                assert_eq!(response_kind, AUDIT_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
        match &envelopes[1] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert!(summary.is_none());
            }
            other => panic!("frame 1 must be StreamEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_audit_stream_three_events_emits_begin_three_chunks_end_with_monotonic_sequence() {
        // Symmetric to the memory monotonic-sequence pin. ADR 0010
        // requires sequence to count from 0 by 1 — assert exact
        // values, not just distinct.
        let stream_id = Uuid::new_v4();
        let events = vec![fixture_event(1), fixture_event(2), fixture_event(3)];
        let mut buf = Vec::new();
        emit_audit_stream(&mut buf, stream_id, &events)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(envelopes.len(), 5, "begin + 3 chunks + end = 5 frames");

        assert!(
            matches!(&envelopes[0], StreamEnvelope::StreamBegin { stream_id: sid, .. } if *sid == stream_id)
        );

        for (i, event) in events.iter().enumerate() {
            match &envelopes[1 + i] {
                StreamEnvelope::StreamChunk {
                    stream_id: sid,
                    sequence,
                    chunk,
                } => {
                    assert_eq!(*sid, stream_id);
                    assert_eq!(*sequence, i as u32);
                    let expected = serde_json::to_value(event).unwrap();
                    assert_eq!(
                        *chunk, expected,
                        "chunk payload must equal serde_json::to_value(&event) verbatim"
                    );
                }
                other => panic!("frame {} must be StreamChunk, got {other:?}", 1 + i),
            }
        }

        assert!(matches!(
            &envelopes[4],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
    }

    #[tokio::test]
    async fn emit_audit_stream_id_constant_across_every_frame() {
        // Symmetric to the memory stream_id-constancy pin. A
        // refactor that re-allocated stream_id per chunk would
        // break multiplexing silently.
        let stream_id = Uuid::new_v4();
        let events = vec![fixture_event(5), fixture_event(6)];
        let mut buf = Vec::new();
        emit_audit_stream(&mut buf, stream_id, &events)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        for (i, env) in envelopes.iter().enumerate() {
            let sid = match env {
                StreamEnvelope::StreamBegin { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamChunk { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamEnd { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamError { stream_id, .. } => *stream_id,
            };
            assert_eq!(sid, stream_id, "frame {i} stream_id diverged");
        }
    }

    #[tokio::test]
    async fn emit_audit_stream_response_kind_pins_audit_events_literal() {
        // Response::AuditEvents → snake_case 'audit_events'. A
        // refactor that drifted to 'audit' or 'audit_log' would
        // break every v2-aware audit consumer at the routing layer.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_audit_stream(&mut buf, stream_id, &[]).await.unwrap();
        let envelopes = drain_envelopes(&buf).await;
        match &envelopes[0] {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "audit_events");
                assert_eq!(response_kind, AUDIT_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
    }

    #[test]
    fn audit_chunk_schema_pins_adr_0010_literal() {
        // Symmetric to the memory schema-const pin. ADR 0010 names
        // this literal; a rename here breaks every future fixture
        // validator and operator-facing snapshot label.
        assert_eq!(AUDIT_CHUNK_SCHEMA, "covenant.ipc.v2.chunk.audit-event.v1");
    }

    fn fixture_intent_result(seed: u8, text: &str) -> AgentResult {
        AgentResult {
            text: text.into(),
            sources: vec![format!("source-{seed}")],
            runtime_events: vec![covenant_runtime::RuntimeTrace::HermesToolInvoked {
                run_id: format!("run-{seed}"),
                tool: "terminal".into(),
                preview: format!("preview-{seed}"),
            }],
        }
    }

    #[tokio::test]
    async fn emit_intent_stream_empty_results_emits_exactly_begin_then_end() {
        // Symmetric to the memory/audit empty-page pins. Every
        // helper call must emit begin+end regardless of result count
        // so the protocol layer can distinguish 'opened, no rows'
        // from 'never opened' — a stream that never opens is
        // indistinguishable from a dead daemon.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &[], None)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(
            envelopes.len(),
            2,
            "empty page must emit exactly two frames"
        );
        match &envelopes[0] {
            StreamEnvelope::StreamBegin {
                stream_id: sid,
                response_kind,
            } => {
                assert_eq!(*sid, stream_id);
                assert_eq!(response_kind, INTENT_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
        match &envelopes[1] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert!(summary.is_none());
            }
            other => panic!("frame 1 must be StreamEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_intent_stream_three_results_emits_begin_three_chunks_end_with_monotonic_sequence()
    {
        // Symmetric to the memory/audit monotonic-sequence pins. ADR
        // 0010 requires sequence to count from 0 by 1; asserting
        // exact 0/1/2 catches a refactor that derived sequence from
        // a result field (e.g., a hash) rather than the iteration
        // index. Chunk payload byte-equality with
        // serde_json::to_value(&result) catches a hidden schema
        // transform (e.g., dropping runtime_events or renaming
        // fields).
        let stream_id = Uuid::new_v4();
        let results = vec![
            fixture_intent_result(1, "alpha"),
            fixture_intent_result(2, "beta"),
            fixture_intent_result(3, "gamma"),
        ];
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &results, None)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(envelopes.len(), 5, "begin + 3 chunks + end = 5 frames");

        assert!(
            matches!(&envelopes[0], StreamEnvelope::StreamBegin { stream_id: sid, .. } if *sid == stream_id)
        );

        for (i, result) in results.iter().enumerate() {
            match &envelopes[1 + i] {
                StreamEnvelope::StreamChunk {
                    stream_id: sid,
                    sequence,
                    chunk,
                } => {
                    assert_eq!(*sid, stream_id);
                    assert_eq!(
                        *sequence, i as u32,
                        "chunk sequence must be monotonic from 0"
                    );
                    let expected = serde_json::to_value(result).unwrap();
                    assert_eq!(
                        *chunk, expected,
                        "chunk payload must equal serde_json::to_value(&result) verbatim"
                    );
                }
                other => panic!("frame {} must be StreamChunk, got {other:?}", 1 + i),
            }
        }

        assert!(matches!(
            &envelopes[4],
            StreamEnvelope::StreamEnd { summary: None, .. }
        ));
    }

    #[tokio::test]
    async fn emit_intent_stream_id_constant_across_every_frame() {
        // Symmetric to the memory/audit stream_id-constancy pins. A
        // refactor that re-allocated stream_id per chunk would break
        // multiplexing silently.
        let stream_id = Uuid::new_v4();
        let results = vec![
            fixture_intent_result(5, "one"),
            fixture_intent_result(6, "two"),
        ];
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &results, None)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        for (i, env) in envelopes.iter().enumerate() {
            let sid = match env {
                StreamEnvelope::StreamBegin { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamChunk { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamEnd { stream_id, .. } => *stream_id,
                StreamEnvelope::StreamError { stream_id, .. } => *stream_id,
            };
            assert_eq!(sid, stream_id, "frame {i} stream_id diverged");
        }
    }

    #[tokio::test]
    async fn emit_intent_stream_response_kind_pins_intent_result_literal() {
        // Response::IntentResult → snake_case 'intent_result'. A
        // refactor that drifted to 'intent' or 'agent_result' would
        // break every v2-aware intent consumer at the routing layer.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &[], None)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        match &envelopes[0] {
            StreamEnvelope::StreamBegin { response_kind, .. } => {
                assert_eq!(response_kind, "intent_result");
                assert_eq!(response_kind, INTENT_RESPONSE_KIND);
            }
            other => panic!("frame 0 must be StreamBegin, got {other:?}"),
        }
    }

    #[test]
    fn intent_result_chunk_schema_pins_adr_0010_literal() {
        // Symmetric to the memory/audit schema-const pins. ADR 0010
        // names this literal; a rename here breaks every future
        // fixture validator and operator-facing snapshot label.
        assert_eq!(
            INTENT_RESULT_CHUNK_SCHEMA,
            "covenant.ipc.v2.chunk.agent-result.v1"
        );
    }

    #[tokio::test]
    async fn emit_intent_stream_with_summary_threads_value_to_stream_end() {
        // The IntentResult-only bookkeeping (intent_id, status,
        // settlement) doesn't fit inside an AgentResult chunk, so it
        // rides in StreamEnvelope::StreamEnd.summary. The orchestrator
        // (slice 5.c) will build this Value from the dispatched
        // Response::IntentResult; this test pins that the helper
        // threads whatever Value the caller passes verbatim to the
        // terminal frame.
        let stream_id = Uuid::new_v4();
        let summary = serde_json::json!({
            "intent_id": "00000000-0000-0000-0000-000000000001",
            "status": "ok",
            "settlement": serde_json::Value::Null,
        });
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &[], Some(summary.clone()))
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(envelopes.len(), 2, "empty results still emits begin+end");
        match &envelopes[1] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary: end_summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert_eq!(
                    end_summary.as_ref(),
                    Some(&summary),
                    "StreamEnd.summary must equal the caller's Value byte-for-byte"
                );
            }
            other => panic!("frame 1 must be StreamEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_intent_stream_with_summary_none_keeps_terminal_frame_unchanged() {
        // Passing None must produce StreamEnd.summary.is_none() — the
        // memory/audit-style terminal shape. The contract pin: None
        // and Some(Value::Null) are wire-distinct (Some(Null) would
        // serialize as "summary":null where None serializes as
        // "summary" absent or null depending on serde config). This
        // test exercises None specifically so the orchestrator slice
        // can opt out of the summary when there's nothing to report.
        let stream_id = Uuid::new_v4();
        let mut buf = Vec::new();
        emit_intent_stream(&mut buf, stream_id, &[], None)
            .await
            .unwrap();
        let envelopes = drain_envelopes(&buf).await;
        assert_eq!(envelopes.len(), 2);
        match &envelopes[1] {
            StreamEnvelope::StreamEnd {
                stream_id: sid,
                summary,
            } => {
                assert_eq!(*sid, stream_id);
                assert!(
                    summary.is_none(),
                    "passing None must produce summary:None terminal frame, got {summary:?}"
                );
            }
            other => panic!("frame 1 must be StreamEnd, got {other:?}"),
        }
    }
}
