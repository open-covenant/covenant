import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("audit", "Audit log", 'JSONL audit log: event variants, integrity sidecar, schema, and how to read it.');

export default function AuditPage() {
  return (
    <>
      <h1>Audit log</h1>
      <p>
        Every state-changing surface in Covenant emits an{" "}
        <code>AuditEvent</code> to the append-only log at{" "}
        <code>$COVENANT_HOME/audit/events.jsonl</code>. The log is the
        ground truth — operators read it directly,{" "}
        <code>covenant verify</code> cross-checks it against the other
        state files, <code>covenant audit verify</code> checks the local
        hash-chain sidecar, and the <code>covenant audit/recent</code>{" "}
        route reads from the same file.
      </p>

      <h2>Event envelope</h2>
      <pre>
        <code>{`AuditEvent {
  id:           uuid,                  // unique per event
  timestamp_ms: u64,                   // unix milliseconds
  issuer:       AgentId,               // the daemon's local identity
  kind:         AuditKind              // tagged variant — see below
}`}</code>
      </pre>

      <h2>Variants</h2>

      <h3>
        <code>IntentDispatched</code>
      </h3>
      <pre>
        <code>{`{
  "kind":           "intent_dispatched",
  "intent_id":      "uuid",
  "intent_text":    "…",
  "matched_agent":  "research@local" | null,
  "result_hash_hex": "…",
  "status":         "ok"
}`}</code>
      </pre>

      <h3>
        <code>IntentIgnored</code>
      </h3>
      <pre>
        <code>{`{
  "kind":            "intent_ignored",
  "intent_id":       "uuid",
  "intent_text":     "…",
  "matched_pattern": "**/*.pem"
}`}</code>
      </pre>

      <h3>
        <code>CapabilityCheck</code>
      </h3>
      <pre>
        <code>{`{
  "kind":              "capability_check",
  "agent_id":          "research@local" | "tool:echo",
  "required_actions":  ["tool.web_search"],
  "missing_actions":   [],
  "passed":            true
}`}</code>
      </pre>

      <h3>
        <code>CapabilityGranted</code>
      </h3>
      <pre>
        <code>{`{
  "kind":               "capability_granted",
  "subject_display":    "user@local",
  "action":             "tool.web_search",
  "granted_by_display": "user@local",
  "signature_b58":      "4qXP…8tF1"
}`}</code>
      </pre>

      <h2>Properties</h2>
      <ul>
        <li>
          <strong>Append-only during normal writes.</strong> The file is
          opened for append on event record. Operator-driven retention
          purge rewrites the retained rows and the sidecar together.
        </li>
        <li>
          <strong>Locally chained.</strong> The daemon writes{" "}
          <code>$COVENANT_HOME/audit/events.chain.jsonl</code> with a
          SHA-256 hash chain over retained event rows.
        </li>
        <li>
          <strong>One event per line.</strong> Compatible with{" "}
          <code>tail -F</code>, <code>jq</code>, and other JSONL-aware
          tooling.
        </li>
        <li>
          <strong>Deterministic schema.</strong> Each variant
          serialises with a stable <code>kind</code> tag plus its
          payload. Adding new variants is a backward-compatible
          schema change.
        </li>
        <li>
          <strong>Cross-checked.</strong>{" "}
          <code>covenant verify</code> runs four audits over a
          rolling window:
          <ul>
            <li>
              memory ↔ audit — every memory record has a matching{" "}
              <code>IntentDispatched</code>.
            </li>
            <li>
              memory parent references — every parent id resolves in
              the memory store.
            </li>
            <li>
              capability ↔ audit — every granted capability has a
              matching <code>CapabilityGranted</code>.
            </li>
            <li>
              memory ↔ receipts — memory writes and settlement
              receipts pair by <code>memory_record_id</code>, with
              legacy count fallback.
            </li>
          </ul>
        </li>
      </ul>

      <h2>Reading the log</h2>

      <h3>Last few events</h3>
      <pre>
        <code>{`covenant audit recent --limit 5 --json
# Or via HTTP:
curl -s 127.0.0.1:8421/audit/recent?limit=5&since_ms=1714938000000 | jq`}</code>
      </pre>

      <h3>Verify local chain</h3>
      <pre>
        <code>{`covenant audit verify
curl -s 127.0.0.1:8421/audit/verify \\
  -H "Authorization: Bearer $COVENANT_OPERATOR_TOKEN" | jq`}</code>
      </pre>

      <h3>Filter for capability checks that failed</h3>
      <pre>
        <code>{`tail -F ~/.covenant/audit/events.jsonl \\
  | jq -c 'select(.kind.kind == "capability_check" and .kind.passed == false)'`}</code>
      </pre>

      <h3>Find every dispatch for a specific agent</h3>
      <pre>
        <code>{`jq -c 'select(.kind.kind == "intent_dispatched"
              and .kind.matched_agent == "research@local")' \\
  ~/.covenant/audit/events.jsonl`}</code>
      </pre>

      <h2>Trust model</h2>
      <p>
        The audit log is local. A user with write access to{" "}
        <code>$COVENANT_HOME</code> can rewrite history. The local
        hash-chain detects retained-row edits and sidecar mismatch after
        anchoring, and <code>covenant verify</code> surfaces
        cross-reference drift. This is not public signing or immutable
        storage.
      </p>
      <p>
        Deployments where the operator is not the sole writer to the host
        should either sign individual events or stream the log to an
        append-only system with the appropriate trust model. Both
        approaches integrate against the existing <code>AuditLog</code>{" "}
        trait.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/audit-integrity">Audit integrity</Link> — local
          hash-chain verification and its limits.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          where grants and checks originate.
        </li>
        <li>
          <Link href="/cli">CLI</Link> — <code>verify</code> and
          its drift-check rules.
        </li>
        <li>
          <Link href="/security">Security model</Link> — what the
          local-trust assumption costs you.
        </li>
      </ul>
    </>
  );
}
