import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = ["a2a-idempotency", "A2A idempotency policy", 'The idempotency metadata, retry gate, and receiver obligations for A2A task redelivery.'] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function A2AIdempotencyPolicyPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>A2A idempotency policy</h1>
      <p>
        Covenant A2A is a durable, explicitly leased queue. It does not
        automatically redeliver leased work after restart. Operators can repair
        stale leases explicitly, and a disabled-by-default retry scan can
        requeue only stale tasks that declare idempotent duplicate safety and
        carry a non-empty key.
      </p>

      <h2>Terms</h2>
      <ul>
        <li>
          <strong>Attempt.</strong> One lease and execution of a task.
        </li>
        <li>
          <strong>Duplicate execution.</strong> A task is executed more than
          once (for example, the receiver crashes after performing work but
          before posting a result).
        </li>
        <li>
          <strong>Retry.</strong> Requeueing a task for another attempt without
          changing the task id.
        </li>
      </ul>

      <h2>Policy goals</h2>
      <ol>
        <li>Make duplicate-work risk explicit and machine-checkable.</li>
        <li>Prevent silent duplicate side effects when automation requeues.</li>
        <li>Keep retries visible via attempt counters and audit rows.</li>
      </ol>

      <h2>Task metadata</h2>
      <p>
        Tasks may carry explicit idempotency metadata in the{" "}
        <code>A2ATask</code> envelope. The daemon validates that a present key
        is non-empty, persists the metadata, and returns it through
        queue/status surfaces.
      </p>

      <h3>Idempotency class</h3>
      <ul>
        <li>
          <code>idempotent</code>: executing the task multiple times with the
          same task id is safe. Any side effects must be keyed or conditional
          such that duplicates do not create new external effects.
        </li>
        <li>
          <code>unsafe</code>: duplicate execution may cause external effects.
          The system must not automatically requeue these tasks.
        </li>
      </ul>
      <p>
        Manual repair still uses <code>operator_accepted</code> as an explicit
        human posture. The automated retry gate only accepts task metadata
        marked <code>idempotent</code>.
      </p>

      <h3>Idempotency key</h3>
      <p>
        The idempotency key is a stable, caller-chosen key for the logical work
        unit. For tasks that call external systems that support explicit keys,
        senders should provide the same key so receivers can forward it
        consistently.
      </p>

      <h2>Receiver-side result cache</h2>
      <p>
        When an idempotent task posts a result, the mailbox stores a cached
        payload keyed by sender, recipient, current task kind, and idempotency
        key. A later task with the same cache key receives a replayed result
        immediately instead of being leased again.
      </p>
      <p>
        JSONL-backed mailboxes persist cache entries in the event log. Task
        compaction removes resolved task history but keeps cache entries, so
        future duplicates can still short-circuit after restart.
      </p>

      <h2>Explicit retry gate</h2>
      <p>
        <code>covenant a2a retry-stale</code> is disabled by default and reports
        what it would do unless the operator passes <code>--enable</code>.
      </p>
      <ol>
        <li>
          Never synthesize a new task id. Retries requeue the same task id and
          increment the attempt counter on the next lease.
        </li>
        <li>Retry only tasks marked <code>idempotent</code>.</li>
        <li>Skip tasks without a non-empty idempotency key.</li>
        <li>
          Make retry decisions observable via <code>auto_requeue</code> audit
          rows and skipped-task report entries.
        </li>
        <li>
          Bound retry behavior with explicit maximum attempts, maximum requeues,
          minimum lease age, and scan limits.
        </li>
      </ol>

      <h2>Periodic scheduler</h2>
      <p>
        The daemon can run the same retry gate on a timer through an explicit
        environment opt-in. It does not bypass the{" "}
        <code>a2a.repair.requeue</code> capability gate.
      </p>
      <pre>
        <code>{`COVENANT_A2A_AUTO_RETRY_SCHEDULER=1
COVENANT_A2A_AUTO_RETRY_INTERVAL_MS=60000
COVENANT_A2A_AUTO_RETRY_MIN_LEASE_AGE_MS=300000
COVENANT_A2A_AUTO_RETRY_MAX_ATTEMPTS=3
COVENANT_A2A_AUTO_RETRY_MAX_REQUEUES=1
COVENANT_A2A_AUTO_RETRY_SCAN_LIMIT=100`}</code>
      </pre>
      <p>
        Every scheduler pass records an{" "}
        <code>a2_a_auto_retry_scheduler_scan</code> audit summary. Actual
        mutations still produce per-task <code>auto_requeue</code> repair rows.
      </p>

      <h2>Receiver obligations</h2>
      <p>Receivers may claim <code>idempotent</code> only when:</p>
      <ul>
        <li>
          persistent writes are conditional on the task id (or explicit
          idempotency key) so replays do not create new records;
        </li>
        <li>
          external calls that support idempotency keys receive the key
          consistently across retries;
        </li>
        <li>
          results are safe to post multiple times (posting the same result twice
          must not corrupt mailbox state).
        </li>
      </ul>
      <p>
        If any step cannot be made idempotent, classify the task as{" "}
        <code>operator_accepted</code>.
      </p>

      <h2>Relationship to manual repair</h2>
      <p>
        Manual lease repair already requires an explicit duplicate-risk posture
        (<code>idempotent</code> vs <code>operator_accepted</code>). The retry
        gate is effectively a daemon-initiated requeue, so it must use task
        metadata and must never bypass this classification.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/a2a">Agent-to-agent</Link> — A2A envelopes and mailbox
          surface.
        </li>
        <li>
          <Link href="/live-coverage">Live coverage</Link> — boundary test
          inventory.
        </li>
      </ul>
    </>
  );
}
