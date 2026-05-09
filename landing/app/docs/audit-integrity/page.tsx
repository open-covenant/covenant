import Link from "next/link";

export const metadata = {
  title: "Audit integrity",
  description:
    "Local hash-chain verification for Covenant audit JSONL logs.",
};

export default function AuditIntegrityPage() {
  return (
    <>
      <h1>Audit integrity</h1>
      <p>
        Covenant writes a local SHA-256 hash-chain sidecar next to the
        audit JSONL log. For <code>events.jsonl</code>, the sidecar is{" "}
        <code>events.chain.jsonl</code>. The primary log remains readable
        as one JSON event per line.
      </p>

      <h2>Sidecar entry</h2>
      <pre>
        <code>{`{
  "index": 0,
  "event_id": "uuid",
  "timestamp_ms": 1714938000000,
  "event_hash_hex": "sha256(event-json-line)",
  "previous_hash_hex": "0000…0000",
  "chain_hash_hex": "sha256(previous + '\\n' + event_hash)"
}`}</code>
      </pre>

      <h2>Verify</h2>
      <pre>
        <code>{`covenant audit verify

GET /audit/verify
Authorization: Bearer <operator-token>`}</code>
      </pre>
      <p>
        Both routes return an <code>AuditIntegrityReport</code> containing{" "}
        <code>events</code>, <code>anchors</code>, <code>valid</code>,{" "}
        <code>root_hash_hex</code>, and deterministic failure messages.
        The daemon restricts this report to the operator identity because
        it exposes global audit metadata.
      </p>

      <h2>Boundary</h2>
      <p>
        This is local tamper evidence. It detects retained-row edits,
        missing sidecar entries, and chain mismatches after local anchoring.
        It does not prevent deletion or replacement of both files by a
        host-level attacker, and it does not yet publish signed roots to a
        transparency log.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/audit">Audit log</Link> — event variants and JSONL
          reading.
        </li>
        <li>
          <Link href="/security">Security model</Link> — the local trust
          boundary.
        </li>
      </ul>
    </>
  );
}
