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

  async fn send_result(&self, result: A2ATaskResult)   -> Result<()>;
  async fn recv_result(&self)                          -> Result<A2ATaskResult>;
  async fn try_recv_result(&self)                      -> Result<Option<A2ATaskResult>>;
}`}</code>
      </pre>

      <p>
        The blocking <code>recv_*</code> variants suit in-process
        agents that idle on a long-lived connection; the non-blocking{" "}
        <code>try_recv_*</code> variants suit RPC-style callers that
        prefer to poll over a single round-trip.
      </p>

      <h2>Daemon-mediated flow</h2>
      <pre>
        <code>{`POST /a2a/tasks            # body: A2ATask JSON
  → 200 { "kind": "a2a_task_queued", "task_id": "uuid" }

GET  /a2a/tasks/next       # next queued task or null
  → 200 { "kind": "a2a_task_opt", "task": { ... } | null }

POST /a2a/results          # body: A2ATaskResult JSON
  → 200 { "kind": "a2a_result_posted", "task_id": "uuid" }

GET  /a2a/results/next     # next queued result or null
  → 200 { "kind": "a2a_result_opt", "result": { ... } | null }`}</code>
      </pre>

      <p>
        Equivalent IPC variants exist: <code>SendA2ATask</code>,{" "}
        <code>TryRecvA2ATask</code>, <code>PostA2AResult</code>,{" "}
        <code>TryRecvA2AResult</code>. See{" "}
        <Link href="/docs/ipc">Local IPC</Link> for the full request/
        response shapes.
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
          <strong>Authentication.</strong> The HTTP routes accept any
          well-formed JSON; the daemon does not currently verify that
          the caller is the named <code>sender</code>. Capability
          gating for A2A is on the roadmap.
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
