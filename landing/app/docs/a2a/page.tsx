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
  deadline_ms: u64 | null
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
  async fn try_recv_task(&self)                    -> Result<Option<A2ATask>>;
  async fn recent_tasks(&self, limit: usize)       -> Result<Vec<A2ATask>>;

  async fn send_result(&self, result: A2ATaskResult)   -> Result<()>;
  async fn recv_result(&self)                          -> Result<A2ATaskResult>;
  async fn try_recv_result(&self)                      -> Result<Option<A2ATaskResult>>;
  async fn recent_results(&self, limit: usize)         -> Result<Vec<A2ATaskResult>>;
}`}</code>
      </pre>

      <p>
        The blocking <code>recv_*</code> variants are appropriate for
        in-process agents on long-lived connections. The non-blocking{" "}
        <code>try_recv_*</code> variants are appropriate for RPC-style
        callers that poll over a single round-trip. The non-consuming{" "}
        <code>recent_*</code> variants support operator dashboards that
        inspect the queue without draining it.
      </p>

      <h2>Daemon-mediated flow</h2>
      <pre>
        <code>{`POST /a2a/tasks                   # body: A2ATask JSON
  → 200 { "kind": "a2a_task_queued", "task_id": "uuid" }

GET  /a2a/tasks/next              # consumes the next queued task
  → 200 { "kind": "a2a_task_opt", "task": { ... } | null }

GET  /a2a/tasks/recent?limit=N    # non-consuming snapshot
  → 200 { "kind": "a2a_tasks", "tasks": [ ... ] }

POST /a2a/results                 # body: A2ATaskResult JSON
  → 200 { "kind": "a2a_result_posted", "task_id": "uuid" }

GET  /a2a/results/next            # consumes the next queued result
  → 200 { "kind": "a2a_result_opt", "result": { ... } | null }

GET  /a2a/results/recent?limit=N  # non-consuming snapshot
  → 200 { "kind": "a2a_results", "results": [ ... ] }`}</code>
      </pre>

      <p>
        Equivalent IPC variants exist: <code>SendA2ATask</code>,{" "}
        <code>TryRecvA2ATask</code>, <code>RecentA2ATasks</code>,{" "}
        <code>PostA2AResult</code>, <code>TryRecvA2AResult</code>,{" "}
        <code>RecentA2AResults</code>. See{" "}
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
          <strong>Persistence.</strong> The default mailbox is
          in-memory; a daemon restart discards every queued task and
          result. A disk-backed mailbox is scheduled for a subsequent
          milestone.
        </li>
        <li>
          <strong>Routing.</strong> The default mailbox is global FIFO:
          every <code>recv_task</code> caller pulls from the same queue
          regardless of <code>recipient</code>. Per-recipient routing
          is scheduled for a subsequent milestone.
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
          <strong>Sender record.</strong> The mailbox retains a
          permanent record of the sender for every dispatched task so
          that respond capabilities remain sender-scoped after the task
          has been received. For long-running daemons this map grows
          unboundedly; a TTL or LRU policy is introduced with the
          disk-backed mailbox.
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
