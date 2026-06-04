import { describe, it, expect } from 'vitest';
import { A2ABridge } from './a2a.js';
import type { A2ATask, A2ATaskResult, A2ADaemon, AgentRef } from './daemon.js';

const SELF: AgentRef = { display: 'hatcher-connector', pubkey: 'CONNECTORPK' };
const RECIP: AgentRef = { display: 'repo-doctor', pubkey: 'AGENTPK' };

class FakeA2A implements A2ADaemon {
  sent: A2ATask[] = [];
  results: A2ATaskResult[] = [];
  async sendA2ATask(task: A2ATask) {
    this.sent.push(task);
    return { kind: 'a2_a_task_sent', task_id: task.id };
  }
  async recvA2ATask() {
    return null;
  }
  async postA2AResult() {
    return { kind: 'a2_a_result_posted' };
  }
  async recvA2AResult() {
    return this.results.shift() ?? null;
  }
}

let n = 0;
const idGen = () => `id-${++n}`;

describe('A2ABridge.dispatchToLocal', () => {
  it('builds a verified A2ATask with sender=self, an object recipient, and a generated id', async () => {
    n = 0;
    const daemon = new FakeA2A();
    const bridge = new A2ABridge(daemon, SELF, { idGen });
    const { task_id } = await bridge.dispatchToLocal({ mesh_task_id: 'mesh-1', recipient: RECIP, intent_text: 'run tests' });

    expect(task_id).toBe('id-1');
    expect(daemon.sent).toHaveLength(1);
    const t = daemon.sent[0]!;
    expect(t.sender).toEqual(SELF);
    expect(t.recipient).toEqual(RECIP); // {display, pubkey} object on the wire, not a bare string
    expect(t.intent_text).toBe('run tests');
    expect(t.idempotency).toBeUndefined(); // omitted unless a duplicate-safety variant is configured
  });

  it('attaches idempotency keyed on the mesh task id when duplicateSafety is configured', async () => {
    n = 0;
    const daemon = new FakeA2A();
    const bridge = new A2ABridge(daemon, SELF, { idGen, duplicateSafety: 'idempotent' });
    await bridge.dispatchToLocal({ mesh_task_id: 'mesh-9', recipient: RECIP, intent_text: 'x' });

    expect(daemon.sent[0]!.idempotency).toEqual({ duplicate_safety: 'idempotent', key: 'mesh-9' });
  });
});

describe('A2ABridge.drainResults', () => {
  it('drains ready results until the mailbox is empty', async () => {
    const daemon = new FakeA2A();
    daemon.results = [
      { task_id: 't1', status: 'ok', content: [{ type: 'text', text: 'done' }] },
      { task_id: 't2', status: 'error', content: [], error_message: 'boom' },
    ];
    const bridge = new A2ABridge(daemon, SELF, { idGen });
    const out = await bridge.drainResults();

    expect(out.map((r) => r.task_id)).toEqual(['t1', 't2']);
    expect(out[1]!.error_message).toBe('boom');
  });
});
