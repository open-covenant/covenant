import Link from "next/link";

export const metadata = {
  title: "Agent-to-agent",
  description:
    "Task and result envelopes, the Mailbox trait, and how orchestrator agents fan tasks across child agents.",
};

export default function A2APage() {
  return (
    <>
      <h1>Agent-to-agent</h1>
      <p>
        Agent-to-agent (A2A) is the surface through which one Covenant
        agent dispatches a task to another, receives a result, and
        reconstructs a task graph across many such exchanges. The wire
        types are minimal; storage and routing are pluggable.
      </p>

      <h2>Wire types</h2>
      <p>
        A task is a request from one agent to another. A result is the
        response. Tasks form a tree via <code>parent</code> so an
        orchestrator can fan a root intent across child agents and
        reconstruct the result graph.
      </p>

      <pre>
        <code>{`A2ATask {
  id:          uuid,
  sender:      AgentId,
  recipient:   AgentId,
  intent_text: "do the thing",
  parent:      uuid | null,
  deadline_ms: u64 | null,
  idempotency: {
    duplicate_safety: "unsafe" | "idempotent",
    key:              string
  } | null
}

A2ATaskResult {
  task_id:       uuid,            // matches A2ATask.id
  status:        "ok" | "error" | "partial",
  content:       [ Content ],     // same Content blocks MCP uses
  error_message: string | null
}`}</code>
      </pre>

      <h2>Mailbox</h2>
      <p>
        The <code>Mailbox</code> trait abstracts the queue between
        agents. The daemon holds one mailbox; agents send and receive
        through the daemon&apos;s IPC or HTTP surface.
      </p>

      <pre>
        <code>{`trait Mailbox {
  async fn send_task(&self, task: A2ATask)         -> Result<()>;
  async fn recv_task(&self)                        -> Result<A2ATask>;
  async fn try_recv_task_for(&self, recipient)     -> Result<Option<A2ATask>>;
  async fn recent_tasks(&self, limit: usize)       -> Result<Vec<A2ATask>>;
  async fn task_queue(&self, limit: usize)         -> Result<Vec<A2ATaskQueueEntry>>;
  async fn repair_task(&self, request)             -> Result<A2ARepairOutcome>;

  async fn send_result(&self, result: A2ATaskResult)   -> Result<()>;
  async fn recv_result(&self)                          -> Result<A2ATaskResult>;
  async fn try_recv_result_for(&self, peer)             -> Result<Option<A2ATaskResult>>;
  async fn recent_results(&self, limit: usize)         -> Result<Vec<A2ATaskResult>>;
}`}</code>
      </pre>

      <p>
        The blocking <code>recv_*</code> variants are appropriate for
        in-process agents on long-lived connections. The non-blocking{" "}
        <code>try_recv_*</code> variants are appropriate for RPC-style
        callers that poll over a single round-trip. The non-consuming{" "}
        <code>recent_*</code> and <code>task_queue</code> variants support
        operator dashboards that inspect the queue without draining it.
      </p>

      <p>
        A received task becomes an explicit <code>in_flight</code> lease.
        Leases survive daemon restart and are not automatically
        redelivered; operators inspect them through the queue-status
        surface and repair them explicitly when needed. The CLI status
        surface also has a one-object JSON mode for supervisors that
        need a stable queue snapshot.
      </p>

      <h2>Lease repair</h2>
      <p>
        Repair is explicit and operator-controlled. The mailbox supports
        two repair commands for in-flight tasks:
      </p>
      <ul>
        <li>
          <code>requeue</code> returns a leased task to{" "}
          <code>queued</code>, preserves the last attempt counter, and
          requires an explicit duplicate-work posture:{" "}
          <code>idempotent</code> or <code>operator_accepted</code>.
        </li>
        <li>
          <code>force_error</code> clears the lease and posts an error
          result for the original sender to drain.
        </li>
      </ul>
      <p>
        Both commands require a non-empty reason and may include the
        observed <code>lease_id</code> as a guard against repairing a
        newer lease than the operator inspected. Daemon, HTTP, and CLI
        exposure exists for manual repair paths; automatic background retry
        remains disabled.
      </p>
      <p>
        Tasks may carry optional idempotency metadata: duplicate safety
        plus a stable key. Missing metadata is treated as unsafe. The
        daemon persists and validates the metadata. An explicit{" "}
        <code>retry-stale</code> scan can requeue only stale idempotent
        tasks when the operator passes <code>--enable</code>; skipped
        tasks stay visible in the report. See{" "}
        <Link href="/a2a-idempotency">A2A idempotency policy</Link>.
      </p>

      <h2>Daemon-mediated flow</h2>
      <pre>
        <code>{`POST /a2a/tasks                   # body: A2ATask JSON
  → 200 { "kind": "a2a_task_queued", "task_id": "uuid" }

GET  /a2a/tasks/next              # leases the next queued task
  → 200 { "kind": "a2a_task_opt", "task": { ... } | null }

GET  /a2a/tasks/recent?limit=N    # non-consuming snapshot
  → 200 { "kind": "a2a_tasks", "tasks": [ ... ] }

POST /a2a/results                 # body: A2ATaskResult JSON
  → 200 { "kind": "a2a_result_posted", "task_id": "uuid" }

GET  /a2a/results/next            # drains the next queued result
  → 200 { "kind": "a2a_result_opt", "result": { ... } | null }

GET  /a2a/results/recent?limit=N  # non-consuming snapshot
  → 200 { "kind": "a2a_results", "results": [ ... ] }

GET  /a2a/queue?limit=N           # queued tasks, in-flight leases, pending results
  → 200 { "kind": "a2a_queue", "tasks": [ ... ], "results": [ ... ] }`}</code>
      </pre>

      <pre>
        <code>{`covenant a2a status --min-lease-age-ms 300000 --json
  → { "kind": "a2a_status", "limit": 10, "min_lease_age_ms": 300000, "tasks": [ ... ], "results": [ ... ] }`}</code>
      </pre>

      <p>
        Equivalent IPC variants exist: <code>SendA2ATask</code>,{" "}
        <code>TryRecvA2ATask</code>, <code>RecentA2ATasks</code>,{" "}
        <code>PostA2AResult</code>, <code>TryRecvA2AResult</code>,{" "}
        <code>RecentA2AResults</code>, <code>A2AQueue</code>. See{" "}
        <Link href="/ipc">Local IPC</Link> for the full request/
        response shapes.
      </p>

      <h3>Capability gating</h3>
      <p>
        Both write paths are gated by capability tokens, audited via
        the standard <code>CapabilityCheck</code> event:
      </p>
      <ul>
        <li>
          <code>SendA2ATask</code> requires{" "}
          <code>a2a.send.&lt;recipient.display&gt;</code>. The audit
          row carries scope id{" "}
          <code>a2a-send:&lt;recipient&gt;</code>.
        </li>
        <li>
          <code>PostA2AResult</code> requires{" "}
          <code>a2a.respond.&lt;sender.display&gt;</code>, where{" "}
          <code>sender</code> is the original sender of the task
          identified by <code>result.task_id</code>. The daemon looks
          the sender up via the mailbox; results whose{" "}
          <code>task_id</code> was never dispatched through this
          daemon are rejected before the capability check, so the
          attacker cannot probe for granted caps with arbitrary task
          ids. The audit row carries scope id{" "}
          <code>a2a-respond:&lt;task_id&gt;</code>.
        </li>
      </ul>
      <p>
        Non-empty A2A scopes are enforced at dispatch.{" "}
        <code>peer_pubkey_b58</code> pins the counterparty,{" "}
        <code>task_id</code> pins one task, <code>lease_id</code> pins
        manual repair to one in-flight lease, and{" "}
        <code>duplicate_risk</code> pins requeue posture.
      </p>
      <p>
        Read paths (<code>TryRecv*</code>, <code>Recent*</code>) are not
        gated. Drain operations on the operator&apos;s own daemon are
        treated as local-trust actions.
      </p>

      <h2>Orchestration patterns</h2>
      <h3>Fan-out</h3>
      <p>
        An orchestrator receives a root intent, generates a set of child{" "}
        <code>A2ATask</code> envelopes (each with{" "}
        <code>parent = root_intent.id</code>), submits them via{" "}
        <code>POST /a2a/tasks</code>, and polls{" "}
        <code>GET /a2a/results/next</code> until results have been
        received for every dispatched child.
      </p>

      <h3>Pipeline</h3>
      <p>
        Two agents form a producer-consumer pipeline. The producer
        submits tasks tagged for the consumer&apos;s{" "}
        <code>recipient</code>; the consumer dequeues them via{" "}
        <code>recv_task</code> and posts results back.
      </p>

      <h2>Implementation notes</h2>
      <ul>
        <li>
          <strong>Persistence.</strong> The daemon uses an append-only
          JSONL mailbox. Queued tasks, in-flight leases, sender lookup
          state, and pending results replay after restart.
        </li>
        <li>
          <strong>Routing.</strong> Peer-scoped receives only drain
          tasks addressed to the authenticated recipient. Global FIFO
          receives remain available for in-process orchestrators.
        </li>
        <li>
          <strong>Authentication.</strong> Both write paths are gated by
          capability tokens (<code>a2a.send.&lt;recipient&gt;</code> and{" "}
          <code>a2a.respond.&lt;sender&gt;</code>) checked against the
          daemon&apos;s local identity. The capability is not yet bound
          to the calling HTTP/IPC peer; per-call peer authentication is
          tracked separately.
        </li>
        <li>
          <strong>Retry posture.</strong> A2A delivery avoids automatic
          redelivery of leased tasks after restart. A disabled-by-default
          retry gate can requeue stale in-flight tasks only when an
          operator enables a bounded scan and the task carries idempotent
          duplicate-safety metadata. Automatic background retry remains
          off.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/concepts">Concepts</Link> — agents in
          context.
        </li>
        <li>
          <Link href="/mcp">MCP integration</Link> — the
          companion surface for tools.
        </li>
      </ul>
    </>
  );
}
