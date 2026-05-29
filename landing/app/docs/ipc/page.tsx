import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("ipc", "Local IPC", "Length-prefixed JSON IPC protocol on the daemon's Unix socket.");

export default function IpcPage() {
  return (
    <>
      <h1>Local IPC</h1>
      <p>
        The daemon&apos;s canonical wire protocol. Clients on the same
        host — the CLI, the TUI, operator UIs, third-party tooling —
        communicate with the daemon over length-prefixed JSON on a Unix
        socket at{" "}
        <code>$COVENANT_HOME/sock</code>. The HTTP gateway is a thin
        adapter over the same surface.
      </p>

      <h2>Frame format</h2>
      <p>
        Each frame is a 4-byte big-endian unsigned integer length
        prefix followed by exactly that many bytes of UTF-8 JSON.
        Frames over <strong>8 MiB</strong> are rejected at the read
        boundary.
      </p>

      <pre>
        <code>{`+---------+---------+---------+---------+---------- … ----------+
| len[31..24] | len[23..16] | len[15..8] | len[7..0] | JSON payload |
+-------------+-------------+------------+-----------+--------------+
        4-byte big-endian length              up to 8 MiB`}</code>
      </pre>

      <p>
        The framing applies in both directions: each request frame is
        followed by exactly one response frame, and a long-lived
        connection can carry many request/response pairs in sequence.
        Connections are not pooled by the daemon; clients may reuse a
        single connection or open one per request. The IPC v2
        streaming opt-in (see{" "}
        <a href="#streaming-responses-ipc-v2">
          Streaming responses (IPC v2)
        </a>{" "}
        below) replaces the single response frame with a sequence of{" "}
        <code>StreamEnvelope</code> frames; the per-frame framing
        rules are unchanged.
      </p>

      <h2>Request shapes</h2>
      <p>
        A request is a JSON object tagged with <code>kind</code>. Core
        request kinds; the exhaustive enum lives in the{" "}
        <code>covenant-ipc</code> Rust crate.
      </p>

      <pre>
        <code>{`{ "kind": "ping" }

{ "kind": "protocol_info" }

{ "kind": "authenticate",
  "token_b58": "…" }

{ "kind": "submit_intent",
  "text":          "…",
  "prefer_stream": null | true | false }

{ "kind": "recent_memory",
  "tier":          "working" | "episodic" | "longterm" | null,
  "limit":         10,
  "prefer_stream": null | true | false }

{ "kind": "search_memory",
  "query":         "…",
  "tier":          "working" | "episodic" | "longterm" | null,
  "limit":         10,
  "min_relevance": null | 0.6 }

{ "kind": "purge_memory",
  "tier": "working" | "episodic" | "longterm" | null,
  "before_ms": 1714938000000 }

{ "kind": "purge_audit",        "before_ms": 1714938000000 }
{ "kind": "purge_capabilities", "before_ms": 1714938000000 }
{ "kind": "purge_peers",        "before_ms": 1714938000000 }

{ "kind": "recent_receipts",
  "limit":    10,
  "since_ms": null | 1714938000000 }

{ "kind": "recent_capabilities",
  "limit": 10 }

{ "kind": "grant_capability",
  "action": "tool.web_search",
  "scope": null | { ... },
  "expires_at": null | 1714938000000 }

{ "kind": "revoke_capability",
  "signature_b58": "…" }

{ "kind": "verify",
  "window": 100 }

{ "kind": "ignore_check",
  "text": "…" }

{ "kind": "list_tools" }

{ "kind": "call_tool",
  "name": "echo",
  "arguments": { ... } }

{ "kind": "recent_audit",
  "limit":         20,
  "since_ms":      null | 1714938000000,
  "prefer_stream": null | true | false }

{ "kind": "verify_audit_integrity" }

{ "kind": "send_a2_a_task",      "task":   { ... } }
{ "kind": "try_recv_a2_a_task" }
{ "kind": "a2_a_queue",
  "limit":              20,
  "min_lease_age_ms":   null | 5000,
  "deadline_within_ms": null | 60000,
  "state_filter":       null | "queued" | "in_flight" }

{ "kind": "post_a2_a_result",    "result": { ... } }
{ "kind": "try_recv_a2_a_result" }`}</code>
      </pre>

      <h2>Response shapes</h2>
      <p>
        Responses are also <code>kind</code>-tagged. One canonical
        response shape per request, plus a generic{" "}
        <code>error</code> for any handler-level failure.
      </p>

      <pre>
        <code>{`{ "kind": "pong" }

{ "kind": "protocol_info",
  "info": {
    "protocol": "covenant.ipc",
    "version": 1,
    "min_supported": 1,
    "max_supported": 2
  } }

{ "kind": "authenticated",
  "display": "user@local" }

{ "kind": "authentication_failed",
  "reason": "…" }

{ "kind": "intent_result",
  "intent_id": "uuid",
  "status": "ok" | "ignored",
  "text": "…",
  "sources": ["…"],
  "settlement": null | { ... } }

{ "kind": "memories",        "records":   [ ... ] }
{ "kind": "memory_purged",   "purged":    42 }
{ "kind": "audit_purged",    "purged":    42 }
{ "kind": "capabilities_purged", "purged": 42 }
{ "kind": "peers_purged",    "purged":    42 }
{ "kind": "receipts",        "receipts":  [ ... ] }
{ "kind": "capabilities",    "capabilities": [ ... ] }
{ "kind": "capability_granted",
  "signature_b58":   "…",
  "subject_display": "user@local",
  "action":          "tool.web_search" }
{ "kind": "capability_revoked",
  "signature_b58": "…",
  "removed":       true }
{ "kind": "verify_report",
  "window": 100,
  "checks": [ { "name": "…", "passed": true, "message": "…" } ],
  "drift":  [ { "kind": "…", "id": "…", "message": "…", "repair": "…" } ],
  "orphans_total": 0 }
{ "kind": "ignore_report",
  "ignored":         false,
  "matched_pattern": null,
  "rules_loaded":    3 }
{ "kind": "tool_list",       "tools":    [ ... ] }
{ "kind": "tool_result",     "content":  [ ... ], "is_error": false }
{ "kind": "audit_events",    "events":   [ ... ] }
{ "kind": "audit_integrity", "report": {
    "events": 42,
    "anchors": 42,
    "valid": true,
    "root_hash_hex": "…",
    "failures": []
  } }
{ "kind": "a2_a_task_queued",   "task_id": "uuid" }
{ "kind": "a2_a_task_opt",      "task":    null | { ... } }
{ "kind": "a2_a_result_posted", "task_id": "uuid" }
{ "kind": "a2_a_result_opt",    "result":  null | { ... } }
{ "kind": "a2_a_queue",         "tasks":   [ ... ], "results": [ ... ] }

{ "kind": "error", "message": "…" }`}</code>
      </pre>

      <h2 id="streaming-responses-ipc-v2">Streaming responses (IPC v2)</h2>
      <p>
        Three request kinds — <code>recent_memory</code>,{" "}
        <code>recent_audit</code>, and <code>submit_intent</code> —
        accept an optional <code>prefer_stream</code> boolean. When
        the client sets <code>prefer_stream: true</code>, the daemon
        replaces the single canonical response frame with a sequence
        of <code>StreamEnvelope</code> frames terminated by{" "}
        <code>stream_end</code> (or <code>stream_error</code> on a
        mid-flight failure). Omitted, <code>null</code>, or{" "}
        <code>false</code> keeps the v1 single-Response shape
        byte-identically; v1 clients are not affected.
      </p>
      <p>
        Each <code>StreamEnvelope</code> is one length-prefixed JSON
        frame using the same framing rules as v1 (4-byte big-endian
        length prefix, 8 MiB cap, UTF-8 JSON payload). The envelope
        is tagged with a <code>kind</code> discriminator:
      </p>
      <pre>
        <code>{`{ "kind":          "stream_begin",
  "stream_id":     "uuid",
  "response_kind": "memories" | "audit_events" | "intent_result" }

{ "kind":      "stream_chunk",
  "stream_id": "uuid",
  "sequence":  0,
  "chunk":     { … per-verb chunk payload … } }

{ "kind":      "stream_end",
  "stream_id": "uuid",
  "summary":   null | { … per-verb summary … } }

{ "kind":      "stream_error",
  "stream_id": "uuid",
  "message":   "…" }`}</code>
      </pre>
      <p>
        <code>stream_begin</code> opens the stream with a fresh{" "}
        <code>stream_id</code> and announces the buffered Response
        variant the stream materializes via <code>response_kind</code>:{" "}
        <code>memories</code> for <code>recent_memory</code>,{" "}
        <code>audit_events</code> for <code>recent_audit</code>, and{" "}
        <code>intent_result</code> for <code>submit_intent</code>.
      </p>
      <p>
        <code>stream_chunk</code> frames carry a <code>sequence</code>{" "}
        starting at 0 and incrementing by 1, plus a per-verb{" "}
        <code>chunk</code>: a memory record for <code>recent_memory</code>,
        an audit event for <code>recent_audit</code>, and an{" "}
        AgentResult-shaped object{" "}
        (<code>{"{ text, sources, runtime_events }"}</code>) for{" "}
        <code>submit_intent</code>. An empty result set still emits a{" "}
        <code>stream_begin</code> / <code>stream_end</code> pair so a
        dead daemon is never confused with an empty stream.
      </p>
      <p>
        <code>stream_end</code> closes the stream. For{" "}
        <code>submit_intent</code> the <code>summary</code> field
        carries <code>intent_id</code>, <code>status</code>, and{" "}
        <code>settlement</code> — IntentResult bookkeeping that does
        not fit in an AgentResult chunk. <code>recent_memory</code>{" "}
        and <code>recent_audit</code> omit <code>summary</code>.
      </p>
      <p>
        <code>stream_error</code> is reserved for streams that opened
        successfully and then failed mid-flight. <em>Streaming
        refused</em> — capability gate failure, ignore-rule match,
        budget exhaustion — is signaled by the daemon returning the
        v1 single-Response frame (typically{" "}
        <code>{"{ \"kind\": \"error\", \"message\": \"…\" }"}</code>)
        instead of opening a stream; the consumer disambiguates the
        two cases by reading the first frame&apos;s <code>kind</code>{" "}
        discriminator.
      </p>
      <p>
        The HTTP gateway exposes this same v2 surface as Server-Sent
        Events: see{" "}
        <Link href="/http-api#streaming-responses">
          Streaming responses
        </Link>{" "}
        on the HTTP API page for the per-frame SSE encoding.
      </p>

      <h2>Implementation notes</h2>
      <ul>
        <li>
          <strong>Backpressure.</strong> The daemon reads one frame at a
          time per connection; long-running operations hold the connection
          open until completion, so a slow handler delays the next request
          on that connection.
        </li>
        <li>
          <strong>Frame size.</strong> The 8 MiB cap applies in both
          directions. A memory record set exceeding the cap is rare under
          normal operation, but a verification window over millions of
          records can reach it. Use the available <code>limit</code>{" "}
          arguments to bound response sizes.
        </li>
        <li>
          <strong>Timeouts.</strong> The daemon does not impose a
          per-request timeout. Clients are responsible for their own
          timeouts.
        </li>
        <li>
          <strong>Authentication.</strong> The first frame must be{" "}
          <code>authenticate</code>, except for an optional{" "}
          <code>protocol_info</code> probe. The daemon answers protocol
          probes before authentication and then continues waiting for
          <code>authenticate</code>. After authentication, the resolved
          identity is bound to the connection.
        </li>
        <li>
          <strong>Compatibility.</strong> <code>protocol_info</code> is
          intentionally minimal and treated as stable across protocol
          versions 1 and 2 — the response shape did not change at the
          v2 bump, only <code>max_supported</code> advanced. Clients
          should ignore unknown fields; adding new required fields
          implies a protocol version bump.
        </li>
      </ul>

      <h2>Reference implementation</h2>
      <p>
        The <code>covenant-ipc</code> Rust crate provides{" "}
        <code>read_frame</code> and <code>write_frame</code> helpers
        alongside the <code>Request</code> and <code>Response</code> enums.
        Refer to the <Link href="/cli">CLI</Link> for an end-to-end
        example.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/http-api">HTTP API</Link> — the same
          surface for clients that prefer JSON over HTTP.
        </li>
        <li>
          <Link href="/security">Security model</Link> — what the
          socket-as-credential design costs you.
        </li>
      </ul>
    </>
  );
}
