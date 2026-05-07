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
        Agent-to-agent (A2A) is the surface one Covenant agent uses to
        send a task to another, get the result back, and reconstruct a
        task graph across many such exchanges. The wire types are
        small; the storage and routing are intentionally pluggable.
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
        The blocking <code>recv_*</code> variants suit in-process
        agents that idle on a long-lived connection; the non-blocking{" "}
        <code>try_recv_*</code> variants suit RPC-style callers that
        prefer to poll over a single round-trip; the non-consuming{" "}
        <code>recent_*</code> variants suit operator dashboards that
        need to inspect the queue without draining it.
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
        <Link href="/docs/ipc">Local IPC</Link> for the full request/
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
          <code>PostA2AResult</code> requires <code>a2a.respond</code>.
          The audit row carries scope id{" "}
          <code>a2a-respond:&lt;task_id&gt;</code>.
        </li>
      </ul>
      <p>
        Read paths (<code>TryRecv*</code>, <code>Recent*</code>) are
        not gated. Drain operations on the operator&apos;s own daemon
        are treated as a local-trust action.
      </p>

      <h2>Orchestration patterns</h2>
      <h3>Fan-out</h3>
      <p>
        An orchestrator receives a root intent, generates several
        child <code>A2ATask</code> envelopes (each with{" "}
        <code>parent = root_intent.id</code>), sends them via{" "}
        <code>POST /a2a/tasks</code>, and polls{" "}
        <code>GET /a2a/results/next</code> until it has results for
        every dispatched child.
      </p>

      <h3>Pipeline</h3>
      <p>
        Two agents form a producer-consumer pipeline. The producer
        sends tasks tagged for the consumer&apos;s{" "}
        <code>recipient</code>; the consumer pulls them off the
        mailbox via <code>recv_task</code> and posts results back.
      </p>

      <h2>Implementation notes</h2>
      <ul>
        <li>
          <strong>Persistence.</strong> The default mailbox is in
          memory. A daemon restart drops every queued task and result.
          A disk-backed mailbox is on the roadmap.
        </li>
        <li>
          <strong>Routing.</strong> The default mailbox is global FIFO
          — every <code>recv_task</code> caller pulls from the same
          queue regardless of <code>recipient</code>. Per-recipient
          routing is on the roadmap.
        </li>
        <li>
          <strong>Authentication.</strong> Both write paths are gated
          by capability tokens (<code>a2a.send.&lt;recipient&gt;</code>{" "}
          and <code>a2a.respond</code>) checked against the
          daemon&apos;s local identity. The cap is not yet bound to
          the calling HTTP/IPC peer — closing that gap requires
          per-call peer authentication, which is a separate piece of
          work.
        </li>
        <li>
          <strong>Result attribution.</strong> The current{" "}
          <code>a2a.respond</code> capability is coarse: a holder can
          respond to any queued <code>task_id</code>. Narrowing to{" "}
          <code>a2a.respond.&lt;sender&gt;</code> requires the mailbox
          to track <code>(task_id → sender)</code>; on the roadmap.
        </li>
      </ul>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/docs/concepts">Concepts</Link> — agents in
          context.
        </li>
        <li>
          <Link href="/docs/mcp">MCP integration</Link> — the
          companion surface for tools.
        </li>
      </ul>
    </>
  );
}
