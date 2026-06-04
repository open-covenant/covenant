// Bridges Hatcher Agent-Mesh tasks <-> the Covenant A2A mailbox (dev ask #4).
// Daemon-facing calls are verified against covenantd (routes + shapes in daemon.ts);
// the mesh-facing half stays behind the same inferred frame contract as the rest of
// the connector. This module exposes the two primitives the transport wires to mesh
// frames: push a mesh task into the local mailbox, and drain local results back out.

import type { AgentRef, A2ATask, A2ATaskResult, A2ADaemon } from './daemon.js';

export interface MeshTask {
  // The Hatcher mesh task id — used as the A2A idempotency key so a redelivered
  // dispatch (reconnect/retry) does not double-execute.
  mesh_task_id: string;
  // The local Covenant agent to run the task ({display, pubkey} — pubkey base58).
  recipient: AgentRef;
  intent_text: string;
  task_kind?: string;
  deadline_ms?: number;
}

export interface A2ABridgeOptions {
  idGen?: () => string;
  // A2ADuplicateSafety enum variant (snake_case): 'unsafe' | 'idempotent' (verified
  // covenant-a2a/src/lib.rs:50). Omit to send no idempotency block.
  duplicateSafety?: 'unsafe' | 'idempotent';
}

export class A2ABridge {
  private readonly idGen: () => string;

  constructor(
    private readonly daemon: A2ADaemon,
    // The connector's own AgentId, used as A2ATask.sender (the daemon asserts
    // sender == authenticated caller). Discovered at pairing.
    private readonly self: AgentRef,
    private readonly opts: A2ABridgeOptions = {},
  ) {
    this.idGen = opts.idGen ?? (() => crypto.randomUUID());
  }

  // Mesh -> Covenant: enqueue a local A2A task. Idempotent on the mesh task id when a
  // duplicate-safety variant is configured.
  async dispatchToLocal(t: MeshTask): Promise<{ task_id: string }> {
    const task: A2ATask = {
      id: this.idGen(),
      sender: this.self,
      recipient: t.recipient,
      intent_text: t.intent_text,
      ...(t.task_kind ? { task_kind: t.task_kind } : {}),
      ...(t.deadline_ms ? { deadline_ms: t.deadline_ms } : {}),
      ...(this.opts.duplicateSafety
        ? { idempotency: { duplicate_safety: this.opts.duplicateSafety, key: t.mesh_task_id } }
        : {}),
    };
    await this.daemon.sendA2ATask(task);
    return { task_id: task.id };
  }

  // Covenant -> Mesh: drain ready results so the caller can forward each one upward.
  // Bounded so a flooded mailbox can't starve the event loop in one tick.
  async drainResults(max = 32): Promise<A2ATaskResult[]> {
    const out: A2ATaskResult[] = [];
    for (let i = 0; i < max; i++) {
      const r = await this.daemon.recvA2AResult();
      if (!r) break;
      out.push(r);
    }
    return out;
  }
}
