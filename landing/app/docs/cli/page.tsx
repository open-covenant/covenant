import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("cli", "Command-line interface", 'Every covenant subcommand, with arguments and exit codes.');

export default function CliPage() {
  return (
    <>
      <h1>Command-line interface</h1>
      <p>
        The <code>covenant</code> CLI communicates with a running daemon
        over the Unix socket at <code>$COVENANT_HOME/sock</code>. Each
        subcommand is a single round-trip; the CLI performs no caching
        and holds no state of its own.
      </p>

      <h2>Synopsis</h2>
      <pre>
        <code>{`covenant <subcommand> [args]

  bootstrap [--json]                 Grant the union of every loaded
                                     agent's required capabilities (plus
                                     memory.write); idempotent — already
                                     granted actions are skipped.
  intent [--json] <text>             Submit an intent and print the result.
  intents resume (<intent-id>|latest)
        [--json]                     Re-dispatch a budget-rejected intent.
  ping [--json]                      Check the daemon is responsive.
  version                            Print daemon protocol metadata as JSON
                                     (pre-auth; no operator token required).

  memory recent [--tier T] [-n N]
        [--json]                     List recent memory records.
  memory search <query>
        [--tier T] [-n N] [--json]   Cosine-similarity search via embeddings.
  memory purge [--tier T]
        (--before-ms M
         | --older-than-ms D) [--json]
                                     Delete records older than the threshold.
  memory compact --reason TEXT [--apply]
        [--detach-stale-parents]
        [tier deletion flags] [--json] Apply bounded multi-tier compaction.
  memory plan-compaction --reason TEXT
        [--detach-stale-parents]
        [tier deletion flags] [--json] Preview the compaction plan without mutating.
  memory plan-receipt-backfill
        [-n N] [--json]                Read-only plan for legacy uncorrelated receipts.
  memory repair detach-parent <id>
        --reason TEXT
        [--expected-parent UUID] [--apply]
                                     Clear a stale parent reference.
  memory repair delete <id>
        --reason TEXT [--apply]      Delete a confirmed invalid record.
  memory repair backfill-provenance <id>
        --reason TEXT --provenance JSON [--apply]
                                     Write provenance evidence under metadata.

  capabilities recent [-n N] [--json]
                                     List recent capability tokens.
  capabilities grant <action>
        [--scope <json>]
        [--expires-at <ms>] [--json] Sign and persist a new capability.
  capabilities revoke <signature-b58>
        [--json]
                                     Tombstone a previously granted token.
  capabilities purge
        (--before-ms M
         | --older-than-ms D) [--json]
                                     Delete old revoked capability tokens.

  receipts recent [-n N] [--json]    List recent settlement receipts.
  chain status [--json]               Print configured chain settlement state.
  chain flush-receipts [-n N] [--json]
                                     Batch local receipts into a receipt root.
  chain receipt-batches [-n N] [--json]
                                     List local receipt batches.

  a2a status [-n N] [--min-lease-age-ms N] [--json]
                                     Inspect queued tasks, in-flight leases,
                                     and pending results.
  a2a requeue <task-id>
        --reason <text>
        --duplicate-risk <idempotent|operator-accepted>
        [--lease-id <uuid>]          Return an in-flight task to queued.
  a2a force-error <task-id>
        --reason <text>
        --message <text>
        [--lease-id <uuid>]          Resolve an in-flight task as failed.
  a2a retry-stale [--enable]
        [--min-lease-age-ms N]
        [--max-attempts N]
        [--max-requeues N]
        [--scan-limit N]
        [--json]                     Scan stale leases; mutate only with --enable.
  a2a compact [--json]              Drop fully resolved A2A event rows.

  verify [--window N] [--json]       Cross-check audit log vs other state.

  audit recent [-n N] [--json]       List recent audit events as JSONL
                                     or one JSON envelope.
  audit verify                       Verify the local audit hash-chain.
  audit purge
        (--before-ms M
         | --older-than-ms D) [--json]
                                     Delete audit events older than the threshold.

  ignore check [--json] <text>       Report whether text matches the
                                     .covenantignore rules.

  tools list [--json]                List registered tools.
  tools call <name> [--args <json>] [--json]
                                      Invoke a registered tool.

  peers purge
        (--before-ms M
         | --older-than-ms D) [--json]
                                     Delete old revoked peer tombstones.
  peers rotate [--json]              Rotate the operator peer token.
  peers list [-n N] [--prefix B58] [--json]
                                     List peer registry summaries.
  peers revoke <token-prefix> [--json]
                                     Revoke a peer token by prefix.
`}</code>
      </pre>

      <h2>Conventions</h2>
      <ul>
        <li>
          <code>--tier T</code> accepts <code>working</code>,{" "}
          <code>episodic</code>, or <code>longterm</code> (also{" "}
          <code>long-term</code>, <code>long_term</code>).
        </li>
        <li>
          <code>-n N</code> sets the result count. Defaults to 10.
        </li>
        <li>
          Time values are Unix milliseconds.{" "}
          <code>--before-ms</code> is an absolute epoch;{" "}
          <code>--older-than-ms</code> is a relative offset (now minus
          duration).
        </li>
        <li>
          Daemon errors print to stderr and exit non-zero.
        </li>
      </ul>

      <h2>Exit codes</h2>
      <table>
        <thead>
          <tr>
            <th>Code</th>
            <th>Meaning</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>0</code>
            </td>
            <td>Success.</td>
          </tr>
          <tr>
            <td>
              <code>1</code>
            </td>
            <td>
              The daemon returned an error response, or a downstream
              call (e.g. socket connect) failed.
            </td>
          </tr>
          <tr>
            <td>
              <code>2</code>
            </td>
            <td>
              Usage error — bad subcommand, missing argument, malformed
              flag value.
            </td>
          </tr>
        </tbody>
      </table>

      <p>
        <code>covenant verify</code> is the one exception: a non-zero exit
        signals drift between state files even when the call itself
        succeeded.
      </p>

      <h2>Examples</h2>

      <h3>Submit an intent</h3>
      <pre>
        <code>{`$ covenant intent "summarise recent work on agent memory"
echo (no agent matched): summarise recent work on agent memory`}</code>
      </pre>
      <pre>
        <code>{`$ covenant intent --json "summarise recent work on agent memory"
{"kind":"intent_result","intent_id":"...","status":"ok","text":"...","sources":[],"settlement":null}`}</code>
      </pre>

      <h3>Resume a budget-rejected intent</h3>
      <pre>
        <code>{`$ covenant intents resume latest --json
{"kind":"intent_resume","resumed_intent_id":"...","status":"error","result":null,"message":"budget exhausted: ..."}`}</code>
      </pre>

      <h3>Inspect daemon protocol metadata</h3>
      <pre>
        <code>{`$ covenant version
{"protocol":"covenant.ipc","version":1,"min_supported":1,"max_supported":1}`}</code>
      </pre>

      <h3>Probe daemon health</h3>
      <pre>
        <code>{`$ covenant ping --json
{"kind":"daemon_ping","status":"ok"}`}</code>
      </pre>

      <h3>Inspect recent memory</h3>
      <pre>
        <code>{`$ covenant memory recent -n 3
[1714938191234] working: echo (no agent matched): summarise...
[1714938018993] working: echo (no agent matched): index the...
[1714937883112] working: echo (no agent matched): list any open...`}</code>
      </pre>
      <pre>
        <code>{`$ covenant memory recent -n 3 --json
{"kind":"memory_read","mode":"recent","tier":null,"limit":3,"query":null,"records":[...]}`}</code>
      </pre>

      <h3>Semantic search across all tiers</h3>
      <pre>
        <code>{`$ covenant memory search "agent memory" -n 5
# (records ordered by cosine similarity, descending)`}</code>
      </pre>
      <pre>
        <code>{`$ covenant memory search "agent memory" -n 5 --json
{"kind":"memory_read","mode":"search","tier":null,"limit":5,"query":"agent memory","records":[...]}`}</code>
      </pre>

      <h3>Purge old memory records</h3>
      <pre>
        <code>{`$ covenant memory purge --tier working --before-ms 1714938191234 --json
{"kind":"memory_purged","tier":"working","before_ms":1714938191234,"purged":0}`}</code>
      </pre>

      <h3>Compact memory</h3>
      <pre>
        <code>{`$ covenant memory compact --delete-working-before-ms 1714938191234 --reason "maintenance window" --json
{"kind":"memory_compacted","outcome":{"mode":"dry_run","would_change":true,"changed":false,"deleted":[],"stale_marked":[],"parents_detached":[]}}`}</code>
      </pre>

      <h3>Grant and revoke a capability</h3>
      <pre>
        <code>{`$ covenant capabilities grant tool.web_search
granted: user@local → tool.web_search
signature: 4qXP...8tF1

$ covenant capabilities revoke 4qXP...8tF1
revoked: 4qXP...8tF1`}</code>
      </pre>
      <pre>
        <code>{`$ covenant capabilities revoke 4qXP...8tF1 --json
{"kind":"capability_revoked","signature_b58":"4qXP...8tF1","removed":true}`}</code>
      </pre>

      <h3>Grant a scoped capability</h3>
      <pre>
        <code>{`$ covenant capabilities grant memory.write --scope '{"version":1,"tiers":["working"],"apply":true}'
granted: user@local → memory.write
signature: 4qXP...8tF1`}</code>
      </pre>
      <pre>
        <code>{`$ covenant capabilities grant memory.write --scope '{"version":1,"tiers":["working"],"apply":true}' --json
{"kind":"capability_granted","subject_display":"user@local","action":"memory.write","signature_b58":"...","scope":{"version":1,"tiers":["working"],"apply":true},"expires_at":null}`}</code>
      </pre>

      <h3>Inspect active capabilities as JSON</h3>
      <pre>
        <code>{`$ covenant capabilities recent --limit 5 --json
{"kind":"capability_list","limit":5,"capabilities":[...]}`}</code>
      </pre>

      <h3>Purge old capability tombstones</h3>
      <pre>
        <code>{`$ covenant capabilities purge --before-ms 1714938191234 --json
{"kind":"capabilities_purged","before_ms":1714938191234,"purged":0}`}</code>
      </pre>

      <h3>Verify state</h3>
      <pre>
        <code>{`$ covenant verify --window 100
verify (last 100 records):
  ✓ memory ↔ audit — 0 memory orphan(s), 0 audit orphan(s)
  ✓ memory parent references — 0 stale parent reference(s)
  ✓ capability ↔ audit — 0 capabilit(ies) without matching grant audit event
  ✓ memory ↔ receipts — 20 memory record(s) vs 20 receipt(s); count diff = 0; exact drift = 0; legacy fallback = 0
orphans total: 0`}</code>
      </pre>

      <pre>
        <code>{`$ covenant verify --window 100 --json
{"kind":"verify_report","window":100,"checks":[],"drift":[],"orphans_total":0}`}</code>
      </pre>

      <h3>Verify the audit chain</h3>
      <pre>
        <code>{`$ covenant audit verify
{"events":42,"anchors":42,"valid":true,"root_hash_hex":"...","failures":[]}`}</code>
      </pre>

      <pre>
        <code>{`$ covenant audit verify --json
{"kind":"audit_integrity","report":{"events":42,"anchors":42,"valid":true,"root_hash_hex":"...","failures":[]}}`}</code>
      </pre>

      <h3>Read the audit feed</h3>
      <pre>
        <code>{`$ covenant audit recent --limit 5 --json
{"kind":"audit_recent","limit":5,"events":[...]}`}</code>
      </pre>

      <h3>Purge old audit events</h3>
      <pre>
        <code>{`$ covenant audit purge --before-ms 1714938191234 --json
{"kind":"audit_purged","before_ms":1714938191234,"purged":0}`}</code>
      </pre>

      <h3>Check ignore rules</h3>
      <pre>
        <code>{`$ covenant ignore check --json "summarise ~/.ssh/id_rsa"
{"kind":"ignore_report","ignored":true,"matched_pattern":"id_rsa","rules_loaded":5}`}</code>
      </pre>

      <h3>Purge old peer tombstones</h3>
      <pre>
        <code>{`$ covenant peers purge --before-ms 1714938191234 --json
{"kind":"peers_purged","before_ms":1714938191234,"purged":0}`}</code>
      </pre>

      <h3>Rotate the operator peer token</h3>
      <pre>
        <code>{`$ covenant peers rotate --json
{"kind":"peer_token_rotated","token_b58":"..."}`}</code>
      </pre>

      <h3>Inspect the A2A queue</h3>
      <pre>
        <code>{`$ covenant a2a status --min-lease-age-ms 300000 --json
{"kind":"a2a_status","limit":10,"min_lease_age_ms":300000,"tasks":[],"results":[]}`}</code>
      </pre>

      <h3>Scan stale A2A leases</h3>
      <pre>
        <code>{`$ covenant a2a retry-stale --json
{"kind":"a2a_auto_retry","report":{"policy":{"enabled":false,...},"considered":0,"requeued":[],"skipped":[]}}`}</code>
      </pre>

      <h3>Compact resolved A2A events</h3>
      <pre>
        <code>{`$ covenant a2a compact --json
{"kind":"a2a_compacted","dropped":0}`}</code>
      </pre>

      <h3>Invoke a tool</h3>
      <pre>
        <code>{`$ covenant tools list --json
{"kind":"tool_list","tools":[...]}

$ covenant capabilities grant tool.call.echo
$ covenant tools call echo --args '{"text":"hello"}'
hello

$ covenant tools call echo --args '{"text":"hello"}' --json
{"kind":"tool_result","name":"echo","content":[{"type":"text","text":"hello"}],"is_error":false}`}</code>
      </pre>

      <h2>Environment</h2>
      <table>
        <thead>
          <tr>
            <th>Variable</th>
            <th>Purpose</th>
            <th>Default</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>COVENANT_HOME</code>
            </td>
            <td>
              Root of all on-disk state — socket, identity, memory,
              receipts, audit, capabilities, agents.
            </td>
            <td>
              <code>$HOME/.covenant</code>
            </td>
          </tr>
          <tr>
            <td>
              <code>COVENANT_HTTP_PORT</code>
            </td>
            <td>
              Port the daemon binds for the HTTP gateway. The CLI
              itself does not use HTTP.
            </td>
            <td>
              <code>8421</code>
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/http-api">HTTP API</Link> — same surface
          over HTTP, suitable for browser-facing UIs.
        </li>
        <li>
          <Link href="/ipc">Local IPC</Link> — the wire protocol
          underneath the CLI.
        </li>
        <li>
          <Link href="/audit-integrity">Audit integrity</Link> — what{" "}
          <code>audit verify</code> checks.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          what <code>capabilities grant</code>/<code>revoke</code>{" "}
          actually mints.
        </li>
      </ul>
    </>
  );
}
