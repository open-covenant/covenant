import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "conformance",
  "Conformance",
  "Golden-vector suites that freeze Covenant's wire and record contracts, and how an external implementation proves it conforms.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function ConformancePage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Conformance</h1>
      <p>
        Covenant freezes its public wire and record contracts as committed,
        byte-exact <em>golden vectors</em>. A golden vector is the canonical
        serialized form of one message or record, checked into the repository,
        that an external implementation can encode and decode against without
        reading the Rust source. If your implementation reproduces every
        committed vector byte-for-byte and recomputes every committed hash, it
        speaks the same protocol Covenant does.
      </p>

      <h2>The suites</h2>
      <ul>
        <li>
          <strong>IPC requests and responses.</strong> One canonical frame per
          daemon <code>Request</code> and <code>Response</code> variant, plus
          the protocol-info and v2 streaming envelopes.
        </li>
        <li>
          <strong>Capability grammar.</strong> One <code>Capability</code> grant
          per scope namespace — the dotted{" "}
          <code>namespace.method[.sub]</code> action grammar.
        </li>
        <li>
          <strong>Audit record kinds.</strong> Every audit record variant&apos;s
          wire form and its SHA-256 event hash.
        </li>
        <li>
          <strong>Audit provenance chain.</strong> The capability-family
          provenance records and the genesis-seeded chain root fold.
        </li>
      </ul>
      <p>
        The wire form is what the bytes mean, not how they are pretty-printed.
        The audit suites pin the <em>compact</em> JSON string because that is the
        exact byte sequence hashed into the tamper-evident chain; the IPC and
        capability suites pin the pretty-printed form because those vectors exist
        to be read and diffed, and JSON object key order — not whitespace — is
        the contract.
      </p>

      <h2>Running the suite</h2>
      <p>One command runs every suite:</p>
      <pre>
        <code>{`node agent-os/scripts/conformance.mjs`}</code>
      </pre>
      <p>
        It executes each golden runner and requires every suite to run at least
        one test and report zero failures, so a renamed or deleted suite
        surfaces as a hard error rather than a silently shrinking run. Two
        read-only modes need no Rust toolchain:
      </p>
      <pre>
        <code>{`node agent-os/scripts/conformance.mjs --list    # print the registered suites
node agent-os/scripts/conformance.mjs --check   # static integrity, no cargo`}</code>
      </pre>
      <p>
        <code>--check</code> discovers the golden runners on disk and fails if
        any one is not registered in the runner, so a new conformance suite can
        never be added without the runner learning about it.
      </p>

      <h2>Proving conformance from another implementation</h2>
      <p>
        You do not need the runner — or Rust — to verify conformance. The
        committed vectors are plain JSON. For a wire suite, construct the
        message, serialize it, compare against the committed vector (object key
        order and field presence are part of the contract; insignificant
        whitespace is not), and confirm it round-trips back to the same value.
      </p>
      <p>For the audit record suites, the hash is the contract:</p>
      <pre>
        <code>{`canonical_json   = compact JSON of the AuditEvent
event_hash_hex   = sha256(canonical_json)
chain_hash       = sha256(previous_hash + "\\n" + event_hash)   // per record, in order
                   previous_hash seeds with 64 hex zeros
chain_root       = the final chain_hash`}</code>
      </pre>

      <h2>Changing a frozen contract</h2>
      <p>
        Golden vectors are never regenerated blindly to make a failing test
        pass — a silent regenerate is exactly how a downstream verifier breaks.
        Each suite detects drift by re-serializing the in-code value and
        asserting byte-for-byte equality, and keeps the corpus exhaustive with a
        compile-time match over the underlying enum, so a new variant fails the
        build until a vector is added. When a wire shape genuinely changes,
        regenerate deliberately and review the resulting diff as the record of
        the change; bump the <code>.v&lt;n&gt;</code> suffix on a fixture for an
        incompatible record shape so existing verifiers can pin the version they
        support.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/ipc">Local IPC</Link>: the request and response surface.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link>: the action
          grammar.
        </li>
        <li>
          <Link href="/audit-integrity">Audit integrity</Link>: the chain
          construction.
        </li>
      </ul>
    </>
  );
}
