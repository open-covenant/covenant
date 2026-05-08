import Link from "next/link";

export const metadata = {
  title: "HTTP API",
  description:
    "Local HTTP gateway routes, request bodies, and responses.",
};

export default function HttpApiPage() {
  return (
    <>
      <h1>HTTP API</h1>
      <p>
        The daemon exposes the same surface over HTTP that it does over
        the Unix socket. The HTTP gateway is suitable for browser-facing
        UIs and third-party tooling that cannot speak length-prefixed
        JSON IPC. Listening address is{" "}
        <code>127.0.0.1:8421</code> (loopback only) by default; override
        the port with the <code>COVENANT_HTTP_PORT</code> environment
        variable.
      </p>

      <h2>Conventions</h2>
      <ul>
        <li>
          Request bodies are JSON; responses are JSON.
        </li>
        <li>
          Validation-level problems (missing capability, no agent
          matched) come back as <code>200 OK</code> with{" "}
          <code>{"{ \"kind\": \"error\", \"message\": \"…\" }"}</code>. This
          mirrors the IPC behaviour and keeps callers reading a
          consistent shape.
        </li>
        <li>
          Internal errors (panic, I/O fault) come back as{" "}
          <code>500 Internal Server Error</code> with{" "}
          <code>{"{ \"error\": \"…\" }"}</code>.
        </li>
        <li>
          CORS is permissive on the gateway; treat it as a local-only
          surface and do not expose it beyond loopback.
        </li>
      </ul>

      <h2>Routes</h2>

      <h3>Health</h3>
      <pre>
        <code>{`GET /health
→ 200 { "status": "ok" }`}</code>
      </pre>

      <h3>Intents</h3>
      <pre>
        <code>{`POST /intent
Content-Type: application/json
Body: { "text": "summarise recent work on agent memory" }

→ 200 {
    "kind": "intent_result",
    "intent_id": "…",
    "status": "ok",
    "text": "…",
    "sources": ["…"],
    "settlement": null
  }`}</code>
      </pre>

      <h3>Memory</h3>
      <pre>
        <code>{`GET /memory/recent?tier=working&limit=10
GET /memory/search?q=agent+memory&tier=longterm&limit=5
POST /memory/purge
  Body: { "tier": "working", "before_ms": 1714938000000 }

→ 200 { "kind": "memories", "records": [ ... ] }
   or  { "kind": "memory_purged", "purged": 42 }`}</code>
      </pre>

      <h3>Receipts</h3>
      <pre>
        <code>{`GET /receipts/recent?limit=10
→ 200 { "kind": "receipts", "receipts": [ ... ] }`}</code>
      </pre>

      <h3>Capabilities</h3>
      <pre>
        <code>{`GET /capabilities/recent?limit=10
POST /capabilities/grant
  Body: {
    "action": "tool.web_search",
    "scope": null,
    "expires_at": null
  }
POST /capabilities/revoke
  Body: { "signature_b58": "4qXP…" }

→ 200 {
    "kind": "capability_granted",
    "signature_b58": "…",
    "subject_display": "user@local",
    "action": "tool.web_search"
  }
   or  { "kind": "capability_revoked", "signature_b58": "…", "removed": true }`}</code>
      </pre>

      <h3>Verify</h3>
      <pre>
        <code>{`GET /verify?window=100
→ 200 {
    "kind": "verify_report",
    "window": 100,
    "checks": [
      { "name": "memory ↔ audit",     "passed": true,  "message": "…" },
      { "name": "capability ↔ audit", "passed": true,  "message": "…" },
      { "name": "memory ↔ receipts",  "passed": true,  "message": "…" }
    ],
    "orphans_total": 0
  }`}</code>
      </pre>

      <h3>Tools</h3>
      <pre>
        <code>{`GET /tools
POST /tools/call
  Body: { "name": "echo", "arguments": { "text": "hi" } }

→ 200 { "kind": "tool_list", "tools": [ ... ] }
   or  { "kind": "tool_result", "content": [ ... ], "is_error": false }`}</code>
      </pre>

      <h3>Audit</h3>
      <pre>
        <code>{`GET /audit/recent?limit=20
→ 200 { "kind": "audit_events", "events": [ ... ] }`}</code>
      </pre>

      <h3>Agent-to-agent</h3>
      <pre>
        <code>{`POST /a2a/tasks                   # body: A2ATask JSON
GET  /a2a/tasks/next              # consumes the next queued task
GET  /a2a/tasks/recent?limit=N    # non-consuming snapshot

POST /a2a/results                 # body: A2ATaskResult JSON
GET  /a2a/results/next            # consumes the next queued result
GET  /a2a/results/recent?limit=N  # non-consuming snapshot`}</code>
      </pre>
      <p>
        Write paths (<code>POST</code>) require capability tokens —
        see <Link href="/a2a">Agent-to-agent</Link> for the
        exact actions.
      </p>

      <h2>Authentication</h2>
      <p>
        The HTTP gateway has no token authentication; reaching the
        loopback interface is the only credential. Treat the daemon as
        you would treat your shell — anything that can connect to{" "}
        <code>127.0.0.1:8421</code> can submit intents and grant
        capabilities. A future release will gate the HTTP surface
        behind capability tokens; until then, do not expose the
        gateway beyond loopback.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/cli">CLI</Link> — same surface, but talking
          to the Unix socket.
        </li>
        <li>
          <Link href="/ipc">Local IPC</Link> — the wire protocol
          on the Unix socket.
        </li>
        <li>
          <Link href="/security">Security model</Link> — what the
          loopback-only assumption costs you.
        </li>
      </ul>
    </>
  );
}
