import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("http-api", "HTTP API", 'Local HTTP gateway routes, request bodies, and responses.');

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
          Validation-level conditions (missing capability, no agent
          matched) return <code>200 OK</code> with{" "}
          <code>{"{ \"kind\": \"error\", \"message\": \"…\" }"}</code>,
          matching IPC behavior so that callers parse a single response
          shape.
        </li>
        <li>
          Internal errors (panic, I/O fault) return{" "}
          <code>500 Internal Server Error</code> with{" "}
          <code>{"{ \"error\": \"…\" }"}</code>.
        </li>
        <li>
          CORS uses an explicit origin allow-list. Default is{" "}
          <code>http://localhost:3000</code>; override with{" "}
          <code>COVENANT_HTTP_ORIGINS</code>.
        </li>
      </ul>

      <h2>Routes</h2>

      <h3>Health</h3>
      <pre>
        <code>{`GET /health
→ 200 { "status": "ok" }

GET /version
→ 200 {
    "kind": "protocol_info",
    "info": {
      "protocol": "covenant.ipc",
      "version": 1,
      "min_supported": 1,
      "max_supported": 1
    }
  }`}</code>
      </pre>
      <p>
        <code>/version</code> mirrors the unauthenticated{" "}
        <code>protocol_info</code> IPC probe. The response is intentionally
        minimal and stable for protocol version 1; adding required fields
        implies a protocol version bump.
      </p>

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
POST /memory/repair
  Body: MemoryRepairRequest    # see /memory for command shape
POST /memory/compact
  Body: MemoryCompactionRequest

→ 200 { "kind": "memories", "records": [ ... ] }
   or  { "kind": "memory_purged", "purged": 42 }
   or  { "kind": "memory_repair_applied", ... }
   or  { "kind": "memory_compacted", ... }`}</code>
      </pre>

      <h3>Receipts</h3>
      <pre>
        <code>{`GET /receipts/recent?limit=10
→ 200 { "kind": "receipts", "receipts": [ ... ] }`}</code>
      </pre>

      <h3>Chain</h3>
      <pre>
        <code>{`GET /chain/status
→ 200 {
    "kind": "chain_status",
    "status": {
      "chain":       "solana",
      "cluster":     "devnet",
      "rpc_url":     "…" | null,
      "ws_url":      "…" | null,
      "program_id":  "…" | null,
      "covnt_mint":  "…" | null,
      "ready":       false,
      "missing":     ["rpc_url", "program_id"]
    }
  }

GET /chain/receipt-batches?limit=10
→ 200 {
    "kind": "receipt_batches",
    "batches": [
      {
        "batch_id":      "…",
        "merkle_root":   "…",
        "receipt_count": 20,
        "tx_sig":        "…" | null,
        "slot":          123 | null
      }
    ]
  }

POST /chain/flush-receipts
  Body: { "limit": 10 }
→ 200 {
    "kind": "receipt_batch_flushed",
    "batch": { ... },
    "receipts_updated": 20
  }`}</code>
      </pre>
      <p>
        <code>/chain/status</code> reports the configured settlement chain
        and the names of any missing endpoints, mint, or program fields.
        <code>/chain/flush-receipts</code> batches unsettled local receipts
        into one <code>ReceiptBatchSummary</code> and stamps the receipts
        with the batch identifier; on-chain submission and{" "}
        <code>tx_sig</code> population follow once the chain configuration
        is complete.
      </p>

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
POST /capabilities/purge
  Body: { "before_ms": 1714938000000 }
     or { "older_than_ms": 86400000 }

→ 200 {
    "kind": "capability_granted",
    "signature_b58": "…",
    "subject_display": "user@local",
    "action": "tool.web_search"
  }
   or  { "kind": "capability_revoked", "signature_b58": "…", "removed": true }
   or  { "kind": "capabilities_purged", "purged": 7 }`}</code>
      </pre>

      <h3>Verify</h3>
      <pre>
        <code>{`GET /verify?window=100
→ 200 {
    "kind": "verify_report",
    "window": 100,
    "checks": [
      { "name": "memory ↔ audit",     "passed": true,  "message": "…" },
      { "name": "memory parent references", "passed": true, "message": "…" },
      { "name": "capability ↔ audit", "passed": true,  "message": "…" },
      { "name": "memory ↔ receipts",  "passed": true,  "message": "…" }
    ],
    "drift": [
      {
        "kind": "memory_stale_parent",
        "id": "uuid",
        "message": "…",
        "repair": "…"
      }
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
→ 200 { "kind": "audit_events", "events": [ ... ] }

GET /audit/verify
→ 200 {
    "kind": "audit_integrity",
    "report": {
      "events": 42,
      "anchors": 42,
      "valid": true,
      "root_hash_hex": "…",
      "failures": []
    }
  }

POST /audit/purge
  Body: { "before_ms": 1714938000000 }
     or { "older_than_ms": 86400000 }
→ 200 { "kind": "audit_purged", "purged": 42 }`}</code>
      </pre>

      <h3>Agent-to-agent</h3>
      <pre>
        <code>{`POST /a2a/tasks                   # body: A2ATask JSON
GET  /a2a/tasks/next              # leases the next queued task
GET  /a2a/tasks/recent?limit=N    # non-consuming snapshot

POST /a2a/results                 # body: A2ATaskResult JSON
GET  /a2a/results/next            # drains the next queued result
GET  /a2a/results/recent?limit=N  # non-consuming snapshot
GET  /a2a/queue?limit=N           # queued tasks, in-flight leases, pending results`}</code>
      </pre>
      <p>
        Write paths (<code>POST</code>) require capability tokens —
        see <Link href="/a2a">Agent-to-agent</Link> for the
        exact actions.
      </p>

      <h2>Authentication</h2>
      <p>
        Every route except <code>/health</code> and{" "}
        <code>/version</code> requires{" "}
        <code>Authorization: Bearer &lt;token&gt;</code>. The token must
        resolve to a live peer in the daemon registry, matching the
        Unix-socket authentication model. The gateway still binds to
        loopback by default and should not be exposed directly to an
        untrusted network.
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
