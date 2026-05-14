import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("ipc", "Local IPC", "Length-prefixed JSON IPC protocol on the daemon's Unix socket.");

export default function IpcPage() {
  return (
    <>
      <h1>Local IPC</h1>
      <p>
        The daemon&apos;s canonical wire protocol. Clients on the same
        host — the CLI, operator UIs, third-party tooling — communicate
        with the daemon over length-prefixed JSON on a Unix socket at{" "}
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
        single connection or open one per request.
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
  "text": "…" }

{ "kind": "recent_memory",
  "tier": "working" | "episodic" | "longterm" | null,
  "limit": 10 }

{ "kind": "search_memory",
  "query": "…",
  "tier":  "working" | "episodic" | "longterm" | null,
  "limit": 10 }

{ "kind": "purge_memory",
  "tier": "working" | "episodic" | "longterm" | null,
  "before_ms": 1714938000000 }

{ "kind": "recent_receipts",
  "limit": 10 }

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
  "limit": 20 }

{ "kind": "verify_audit_integrity" }

{ "kind": "send_a2a_task",      "task":   { ... } }
{ "kind": "try_recv_a2a_task" }
{ "kind": "a2a_queue",          "limit":  20 }

{ "kind": "post_a2a_result",    "result": { ... } }
{ "kind": "try_recv_a2a_result" }`}</code>
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
    "max_supported": 1
  } }

{ "kind": "authenticated",
  "display": "operator@local" }

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
{ "kind": "a2a_task_queued",   "task_id": "uuid" }
{ "kind": "a2a_task_opt",      "task":    null | { ... } }
{ "kind": "a2a_result_posted", "task_id": "uuid" }
{ "kind": "a2a_result_opt",    "result":  null | { ... } }
{ "kind": "a2a_queue",         "tasks":   [ ... ], "results": [ ... ] }

{ "kind": "error", "message": "…" }`}</code>
      </pre>

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
          intentionally minimal and treated as stable for protocol
          version 1. Clients should ignore unknown fields; adding new
          required fields implies a protocol version bump.
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
