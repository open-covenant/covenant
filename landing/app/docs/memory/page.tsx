import Link from "next/link";

export const metadata = {
  title: "Memory tiers",
  description:
    "Three-tier memory store, embeddings, semantic search, and the .covenantignore allow/deny list.",
};

export default function MemoryPage() {
  return (
    <>
      <h1>Memory tiers</h1>
      <p>
        Covenant&apos;s memory is a single store partitioned into three
        tiers. Tiers are represented as a column on a shared schema rather
        than as separate databases; records can be promoted between tiers
        without migration.
      </p>

      <h2>Tiers</h2>
      <table>
        <thead>
          <tr>
            <th>Tier</th>
            <th>Lifespan</th>
            <th>Typical use</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>working</code>
            </td>
            <td>per-task scratch; cleared explicitly</td>
            <td>
              Intermediate results during an intent run. Scratch
              memory the agent uses to reason. Default tier for
              automatic memory writes.
            </td>
          </tr>
          <tr>
            <td>
              <code>episodic</code>
            </td>
            <td>task-grained, durable</td>
            <td>
              Records of completed tasks retained across daemon
              restarts; the basis for queries about prior task
              outcomes.
            </td>
          </tr>
          <tr>
            <td>
              <code>longterm</code>
            </td>
            <td>intentionally retained context</td>
            <td>
              Curated reference material, persistent personal
              context, anything that should influence future routing
              and answer composition.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Record shape</h2>
      <pre>
        <code>{`MemoryRecord {
  id:          uuid,
  tier:        "working" | "episodic" | "longterm",
  owner:       AgentId,
  text:        "the result text",
  embedding:   [f32; N],          // empty if no embedder configured
  metadata:    JSON,              // free-form, e.g. { "intent_text": "…" }
  created_at:  u64,               // unix milliseconds
  parent:      uuid | null
}`}</code>
      </pre>

      <h2>Storage</h2>
      <p>
        The persistent backend is SQLite at{" "}
        <code>$COVENANT_HOME/memory.db</code>. The schema is one row
        per record; embeddings are stored as raw <code>f32</code>{" "}
        bytes for cheap cosine math. An in-memory backend covers
        tests; both implement the same <code>MemoryStore</code> trait.
      </p>

      <p>
        The daemon does not maintain a separate vector index; cosine
        search executes against the rows in the queried tier. The
        default store performs adequately for tens of thousands of
        records. Deployments expecting larger row counts should plan
        for a dedicated vector index.
      </p>

      <h2>Embedding</h2>
      <p>
        The daemon embeds memory writes if an embedder is configured.
        With no embedder configured, records are written without an
        embedding and will not appear in similarity search; they
        still surface via <code>recent</code> queries.
      </p>

      <p>
        Configure an embedder in <code>~/.covenant/secrets.toml</code>:
      </p>

      <pre>
        <code>{`[embed]
provider = "ollama"
model    = "nomic-embed-text"`}</code>
      </pre>

      <p>
        With the above, every memory write embeds via Ollama on{" "}
        <code>http://localhost:11434</code>. The embedder is shared
        across writes and search queries, so query and document
        vectors are guaranteed to be in the same space.
      </p>

      <h2>Reading recent memory</h2>
      <p>
        Recent reads and semantic search require <code>memory.read</code>{" "}
        or a tier-specific <code>memory.read.&lt;tier&gt;</code>{" "}
        capability. Results are filtered to the authenticated owner and
        the signed memory scope before they are returned.
      </p>
      <pre>
        <code>{`covenant memory recent
covenant memory recent --tier longterm
covenant memory recent --tier episodic --limit 50
covenant memory recent --tier working --limit 10 --json

# Or via HTTP:
curl -s '127.0.0.1:8421/memory/recent?tier=longterm&limit=50'`}</code>
      </pre>

      <h2>Semantic search</h2>
      <p>
        Search runs cosine similarity over stored vectors. The query
        is embedded with the same provider used for writes; results
        are returned in descending similarity order.
      </p>

      <pre>
        <code>{`covenant memory search "agent memory"
covenant memory search "agent memory" --tier longterm --limit 5
covenant memory search "agent memory" --tier working --limit 5 --json

# Or via HTTP:
curl -s '127.0.0.1:8421/memory/search?q=agent+memory&tier=longterm&limit=5'`}</code>
      </pre>

      <h2>Garbage collection</h2>
      <p>
        Working-tier records grow unboundedly without explicit
        cleanup. The daemon exposes a purge primitive that removes
        records below a timestamp threshold:
      </p>

      <pre>
        <code>{`# Purge working-tier records older than 24 hours.
covenant memory purge --tier working --older-than-ms 86400000 --json

# Purge anything before an explicit epoch.
covenant memory purge --before-ms 1714938000000

# Purge all tiers older than seven days.
covenant memory purge --older-than-ms $((7*24*60*60*1000))

# Machine-readable output:
{"kind":"memory_purged","tier":"working","before_ms":1714938000000,"purged":0}`}</code>
      </pre>

      <p>
        Episodic and long-term records are typically curated rather than
        auto-purged. The same primitive applies to those tiers; operators
        should select retention thresholds deliberately.
      </p>

      <h2>Drift reports</h2>
      <p>
        <code>covenant verify</code> runs read-only consistency checks
        across memory, audit, capability, and settlement state. The
        response includes aggregate checks plus machine-readable drift
        items that future repair commands can consume.
      </p>

      <pre>
        <code>{`covenant verify --window 100 --json

# Drift kinds:
# memory_without_audit
# audit_without_memory
# memory_stale_parent
# capability_without_audit
# memory_receipt_mismatch`}</code>
      </pre>

      <p>
        The verifier does not delete, detach, or backfill records. It
        reports the repair posture for each drift item so an operator or
        future autonomous repair command can decide what to mutate.
      </p>

      <h2>Repair primitives</h2>
      <p>
        The memory crate defines explicit repair requests with{" "}
        <code>dry_run</code> and <code>apply</code> modes. Dry-run returns
        the exact before/after shape without mutating the store; apply
        performs the same checked operation.
      </p>

      <ul>
        <li>
          <code>detach_parent</code> clears a stale parent reference and
          can guard against a changed record with{" "}
          <code>expected_parent</code>.
        </li>
        <li>
          <code>delete_record</code> removes a confirmed invalid or unsafe
          memory record.
        </li>
        <li>
          <code>backfill_provenance</code> writes provenance evidence under{" "}
          <code>metadata.provenance</code> while preserving existing
          metadata.
        </li>
      </ul>

      <p>
        These primitives are implemented in the store layer. Daemon, CLI,
        and audit-log exposure remain pending.
      </p>

      <h2>The .covenantignore allow/deny list</h2>
      <p>
        Covenant supports a <code>.covenantignore</code> file at{" "}
        <code>$COVENANT_HOME/.covenantignore</code> with gitignore-
        style patterns. An intent whose text matches any of the
        active patterns is short-circuited before dispatch:
      </p>

      <ul>
        <li>
          The router is skipped — no agent runs.
        </li>
        <li>
          No memory record is written.
        </li>
        <li>
          No settlement receipt is written.
        </li>
        <li>
          An <code>IntentIgnored</code> audit event is recorded with
          the matching pattern.
        </li>
      </ul>

      <p>
        Pattern syntax: <code>*</code> matches any sequence within a
        single segment, <code>**</code> across segments,{" "}
        <code>?</code> matches a single character. <code>!</code>{" "}
        negates an earlier match. <code>/</code> at the start anchors
        to the path root. Last-rule-wins: a later rule supersedes an
        earlier match.
      </p>

      <p>
        The default seed list, created on first daemon start when no file
        is present, covers common credential filenames. Intents that
        reference paths such as <code>id_rsa</code>, <code>.env</code>,
        or <code>credentials.json</code> are dropped without persisting
        to memory or producing a settlement receipt.
      </p>

      <h2>Test the ignore set</h2>
      <pre>
        <code>{`covenant ignore check --json "summarise ~/.ssh/id_rsa"
{"kind":"ignore_report","ignored":true,"matched_pattern":"id_rsa","rules_loaded":5}`}</code>
      </pre>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/cli">CLI</Link> — every memory subcommand.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — where{" "}
          <code>IntentIgnored</code> lands.
        </li>
        <li>
          <Link href="/settlement">Settlement</Link> — memory
          writes pair 1:1 with receipts.
        </li>
      </ul>
    </>
  );
}
