import { describe, it, expect, vi } from 'vitest';
import { Connector } from './connector.js';
import { StubTransport } from './transport.js';
import { DaemonUnreachableError, type DaemonClient, type AgentEvent, type GrantRequest, type IntegrityReport } from './daemon.js';
import type { ManifestCapability } from './manifest.js';

class FakeDaemon implements DaemonClient {
  grants: GrantRequest[] = [];
  submitted: string[] = [];
  events: AgentEvent[] = [];
  failGrant = false;
  unreachable = false;
  status = 'running';
  failResult = false;

  async health(): Promise<boolean> {
    return true;
  }
  async submitIntent(text: string) {
    this.submitted.push(text);
    return { intent_id: 'int-1', status: this.status, text: '', sources: ['hermes'] };
  }
  async intentResult() {
    if (this.failResult) throw new Error('daemon /intents/int-1/result -> 404');
    return { kind: 'intent_outcome', intent_id: 'int-1', status: 'ok' };
  }
  async streamEvents(_id: string, onEvent: (e: AgentEvent) => void): Promise<void> {
    for (const e of this.events) onEvent(e);
  }
  async verify(): Promise<IntegrityReport> {
    return { events: 3, anchors: 0, valid: true, root_hash_hex: 'deadbeef', failures: [] };
  }
  async grant(req: GrantRequest): Promise<void> {
    if (this.unreachable) throw new DaemonUnreachableError('test: daemon down');
    if (this.failGrant) throw new Error('capability denied');
    this.grants.push(req);
  }
}

const CAPS: ManifestCapability[] = [
  { tool: 'filesystem', mode: 'read', paths: ['./'] },
  { tool: 'terminal', commands: ['pnpm test'] },
];

function build(daemon: DaemonClient, caps = CAPS) {
  const transport = new StubTransport();
  const connector = new Connector(daemon, transport, {
    maxConcurrentDispatch: 2,
    defaultDeadlineMs: 60_000,
    manifestCapabilities: caps,
    now: () => 1_000,
  });
  return { transport, connector };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe('Connector dispatch flow', () => {
  it('mints grants, dispatches the intent, relays trace, emits accepted+result+proof', async () => {
    const daemon = new FakeDaemon();
    daemon.events = [
      { type: 'tool_call', run_id: 'r1', tool: 'shell', preview: 'pnpm test' },
      { type: 'file_write', run_id: 'r1', path: 'REPORT.md', bytes: 12 },
    ];
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'd1', agent_id: 'PK', intent: { text: 'run the tests' }, deadline_ms: 5_000 });
    await flush();
    await flush();

    // baseline memory.write is always minted; filesystem/terminal are sandbox-policy, not grants
    expect(daemon.grants.map((g) => g.action)).toEqual(['memory.write']);
    expect(daemon.grants.every((g) => g.expires_at === 5_000)).toBe(true);
    // intent dispatched
    expect(daemon.submitted).toEqual(['run the tests']);

    const types = transport.sent.map((f) => f.type);
    expect(types).toEqual(['accepted', 'trace', 'trace', 'result']);

    const accepted = transport.sent[0] as { intent_id: string };
    expect(accepted.intent_id).toBe('int-1');

    const result = transport.sent.at(-1) as {
      type: 'result';
      status: string;
      proof: {
        audit_root: string;
        audit_verified: boolean;
        events: AgentEvent[];
        declared_policy: { fs: { read: string[] }; exec: { argv0Allow: string[] } };
      };
    };
    expect(result.status).toBe('success');
    expect(result.proof.audit_root).toBe('deadbeef');
    expect(result.proof.audit_verified).toBe(true);
    expect(result.proof.events).toHaveLength(2);
    // declared (non-token) policy travels in the proof so Hatcher renders honest consent
    expect(result.proof.declared_policy.fs.read).toEqual(['./']);
    expect(result.proof.declared_policy.exec.argv0Allow).toEqual(['pnpm']);
  });

  it('rejects dispatch with empty intent text and never calls the daemon', async () => {
    const daemon = new FakeDaemon();
    const submit = vi.spyOn(daemon, 'submitIntent');
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'd2', agent_id: 'PK', intent: { text: '   ' } });
    await flush();

    expect(submit).not.toHaveBeenCalled();
    expect(transport.sent).toEqual([{ v: 1, type: 'error', dispatch_id: 'd2', code: 'not_admitted', message: 'empty intent text' }]);
  });

  it('rejects a new dispatch when the concurrency cap is full (back-pressure)', async () => {
    const daemon = new FakeDaemon();
    // Hold the first dispatch in-flight: its grant never resolves, so the slot
    // stays reserved while a second, distinct dispatch arrives.
    vi.spyOn(daemon, 'grant').mockImplementation(() => new Promise<void>(() => {}));
    const transport = new StubTransport();
    const connector = new Connector(daemon, transport, {
      maxConcurrentDispatch: 1,
      defaultDeadlineMs: 60_000,
      manifestCapabilities: CAPS,
      now: () => 1_000,
    });
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'first', agent_id: 'PK', intent: { text: 'hold the slot' } });
    await flush();
    expect(connector.inFlight).toBe(1);

    // Valid text, so this can only be the concurrency arm — not the empty-text sibling above.
    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'second', agent_id: 'PK', intent: { text: 'also valid' } });
    await flush();

    expect(transport.sent).toEqual([
      { v: 1, type: 'error', dispatch_id: 'second', code: 'not_admitted', message: 'max concurrent dispatch reached' },
    ]);
    expect(daemon.submitted).toEqual([]); // neither dispatch reached intent submission
    expect(connector.inFlight).toBe(1); // the rejected dispatch never took a slot
  });

  it('surfaces capability_denied when a grant fails and does not dispatch', async () => {
    const daemon = new FakeDaemon();
    daemon.failGrant = true;
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'd3', agent_id: 'PK', intent: { text: 'do work' } });
    await flush();

    expect(daemon.submitted).toEqual([]);
    expect((transport.sent[0] as { code: string }).code).toBe('capability_denied');
  });

  it('reports daemon_unreachable (not capability_denied) when the daemon is down', async () => {
    const daemon = new FakeDaemon();
    daemon.unreachable = true;
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'down', agent_id: 'PK', intent: { text: 'x' } });
    await flush();

    expect(daemon.submitted).toEqual([]);
    expect((transport.sent[0] as { code: string }).code).toBe('daemon_unreachable');
  });

  it('is idempotent: a duplicate dispatch_id is ignored', async () => {
    const daemon = new FakeDaemon();
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'dup', agent_id: 'PK', intent: { text: 'x' } });
    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'dup', agent_id: 'PK', intent: { text: 'x' } });
    await flush();
    await flush();

    expect(daemon.submitted).toHaveLength(1);
  });

  it('answers ping with pong', async () => {
    const daemon = new FakeDaemon();
    const { transport, connector } = build(daemon);
    await connector.start();
    transport.inject({ v: 1, type: 'ping', ts: 42 });
    await flush();
    expect(transport.sent).toEqual([{ v: 1, type: 'pong', ts: 1_000 }]);
  });

  it('skips the SSE relay when the intent is already terminal (fast path)', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok'; // synchronously terminal — no live trace to stream
    const stream = vi.spyOn(daemon, 'streamEvents');
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'fast', agent_id: 'PK', intent: { text: 'echo' } });
    await flush();
    await flush();

    expect(stream).not.toHaveBeenCalled();
    expect(transport.sent.map((f) => f.type)).toEqual(['accepted', 'result']);
  });

  it('falls back to the submit-time result when the result re-fetch 404s (fast path)', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok';
    daemon.failResult = true;
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'fb', agent_id: 'PK', intent: { text: 'echo' } });
    await flush();
    await flush();

    const result = transport.sent.at(-1) as { type: string; proof: { result: { intent_id?: string } } };
    expect(result.type).toBe('result');
    expect(result.proof.result.intent_id).toBe('int-1'); // submit-time IntentResult, not null
  });

  it('mints grants from the dispatch frame (mesh grants), maps status, and enriches the proof', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok';
    const { transport, connector } = build(daemon, []); // empty manifest — grants come from the frame
    await connector.start();

    transport.inject({
      v: 1,
      type: 'dispatch',
      dispatch_id: 'mg',
      agent_id: 'PK',
      intent: { text: 'inspect the repo', context: { repo: 'o/r' } },
      grants: [
        { scope: 'github.read', constraints: { repo: 'o/r' } },
        { scope: 'filesystem.read', constraints: { paths: ['./'] } },
        { scope: 'terminal.exec', constraints: { approval: 'required', timeout_ms: 120000 } },
      ],
    });
    await flush();
    await flush();

    // baseline memory.write + tool.call.github (github is token-gated); fs/terminal -> policy
    expect(daemon.grants.map((g) => g.action)).toEqual(['memory.write', 'tool.call.github']);

    const result = transport.sent.at(-1) as {
      type: string;
      status: string;
      proof: {
        status: string;
        context: unknown;
        capability_grants: { action: string }[];
        declared_policy: { fs: { read: string[] }; exec: { approval?: string } };
        receipt_id: unknown;
        memory_ids: unknown[];
      };
    };
    expect(result.status).toBe('success'); // daemon "ok" -> mesh "success"
    expect(result.proof.context).toEqual({ repo: 'o/r' });
    expect(result.proof.capability_grants.map((g) => g.action)).toEqual(['memory.write', 'tool.call.github']);
    expect(result.proof.declared_policy.fs.read).toEqual(['./']);
    expect(result.proof.declared_policy.exec.approval).toBe('required');
    expect(result.proof.receipt_id).toBeNull();
    expect(result.proof.memory_ids).toEqual([]);
  });
});
