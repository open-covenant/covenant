import Link from "next/link";

export const metadata = {
  title: "A2A idempotency policy",
  description:
    "The idempotency metadata and operator expectations required before any automatic A2A retry is implemented.",
};

export default function A2AIdempotencyPolicyPage() {
  return (
    <>
      <h1>A2A idempotency policy</h1>
      <p>
        Covenant A2A is a durable, explicitly leased queue. It does not
        automatically redeliver leased work after restart; operators repair
        stale leases explicitly. Automatic retry can only be added once tasks
        can declare duplicate-work safety in a stable, audited way.
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

      <h2>Required task metadata (planned)</h2>
      <p>
        Automatic retry requires that each task carry explicit idempotency
        metadata. The current <code>A2ATask</code> envelope does not include
        these fields yet; implementing them is tracked as follow-up work.
      </p>

      <h3>Idempotency class</h3>
      <ul>
        <li>
          <code>idempotent</code>: executing the task multiple times with the
          same task id is safe. Any side effects must be keyed or conditional
          such that duplicates do not create new external effects.
        </li>
        <li>
          <code>operator_accepted</code>: duplicates may cause external effects.
          The system must never auto-retry these tasks; the only way to requeue
          them is an explicit operator repair action that records the accepted
          risk.
        </li>
      </ul>
      <p>
        The default for tasks without explicit metadata is{" "}
        <code>operator_accepted</code>.
      </p>

      <h3>Idempotency key</h3>
      <p>
        Automatic retry requeues the same task id; the task id is therefore the
        default idempotency key. For tasks that call external systems that
        support explicit idempotency keys, senders should provide an explicit{" "}
        <code>idempotency_key</code> so receivers can forward it consistently.
      </p>

      <h2>Automatic retry rules (planned)</h2>
      <ol>
        <li>
          Never synthesize a new task id. Retries requeue the same task id and
          increment the attempt counter on the next lease.
        </li>
        <li>Retry only tasks marked <code>idempotent</code>.</li>
        <li>
          Make retry decisions observable via audit rows including task id,
          attempt, and reason.
        </li>
        <li>
          Bound retry behavior with explicit maximum attempts and backoff; keep
          defaults documented and surfaced in operator tooling.
        </li>
        <li>
          If a task cannot be classified as <code>idempotent</code>, stop and
          require an operator decision rather than guessing.
        </li>
      </ol>

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
        (<code>idempotent</code> vs <code>operator_accepted</code>). Automatic
        retry is effectively a daemon-initiated requeue, so it must use the
        same posture and must never bypass this classification.
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

