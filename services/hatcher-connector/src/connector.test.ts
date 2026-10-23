import { describe, it, expect, vi } from 'vitest';
import { Connector } from './connector.js';
import { StubTransport } from './transport.js';
import { DaemonUnreachableError, type DaemonClient, type AgentEvent, type GrantRequest, type IntegrityReport, type IntentExecution } from './daemon.js';
import type { ManifestCapability } from './manifest.js';

class FakeDaemon implements DaemonClient {
  grants: GrantRequest[] = [];
  submitted: string[] = [];
  events: AgentEvent[] = [];
  failGrant = false;
  unreachable = false;
  status = 'running';
  failResult = false;
  resultStatus: unknown = 'ok'; // intentResult's reported status; set non-string to model a result without one
  executions: Array<IntentExecution | undefined> = [];
  stops: string[] = [];

  async health(): Promise<boolean> {
    return true;
  }
  async submitIntent(text: string, execution?: IntentExecution) {
    this.submitted.push(text);
    this.executions.push(execution);
    return { intent_id: 'int-1', status: this.status, text: '', sources: ['hermes'] };
  }
  async intentResult() {
    if (this.failResult) throw new Error('daemon /intents/int-1/result -> 404');
    return { kind: 'intent_outcome', intent_id: 'int-1', status: this.resultStatus };
  }
  async stopIntent(id: string) {
    this.stops.push(id);
    this.resultStatus = 'cancelled';
    return { accepted: true };
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

  it('finalizes via the poll watchdog when the SSE stream hangs', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'running';
    // Simulate a half-open SSE that never delivers the terminal close.
    daemon.streamEvents = () => new Promise<void>(() => {});
    const transport = new StubTransport();
    const connector = new Connector(daemon, transport, {
      maxConcurrentDispatch: 2,
      defaultDeadlineMs: 60_000,
      manifestCapabilities: CAPS,
      now: () => 1_000,
      terminalPollMs: 5,
    });
    await connector.start();
    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'hang', agent_id: 'PK', intent: { text: 'do work' } });
    await flush();
    expect(transport.sent.find((f) => f.type === 'result')).toBeUndefined();
    await new Promise((r) => setTimeout(r, 30));
    const result = transport.sent.find((f) => f.type === 'result');
    expect(result).toBeDefined();
    expect((result as { status: string }).status).toBe('success');
    expect(connector.inFlight).toBe(0);
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

  it('cancel emits one terminal cancelled result and the finalize guard suppresses a second', async () => {
    const daemon = new FakeDaemon();
    // A controllable live stream: yield one trace event, then block on a gate.
    // Releasing the gate resolves the stream NORMALLY (not an abort-throw), so
    // runTrace falls through to finalize — modelling the race where the run
    // finishes at the same tick a cancel arrives. That isolates finalize's
    // `!active.has` guard (the second defense) rather than runTrace's
    // abort early-return.
    let releaseStream!: () => void;
    const streamGate = new Promise<void>((r) => {
      releaseStream = r;
    });
    vi.spyOn(daemon, 'streamEvents').mockImplementation(async (_id, onEvent) => {
      onEvent({ type: 'tool_call', run_id: 'r1', tool: 'shell', preview: 'long task' });
      await streamGate;
    });
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'cx', agent_id: 'PK', intent: { text: 'long task' } });
    await flush();
    await flush();
    expect(connector.inFlight).toBe(1); // streaming, slot held

    transport.inject({ v: 1, type: 'cancel', dispatch_id: 'cx' });
    await flush();
    expect(connector.inFlight).toBe(0); // cancel aborted the stream and freed the slot

    // Let the stream finish normally; runTrace now reaches finalize, whose guard
    // must refuse to emit a second terminal result for the already-cancelled run.
    releaseStream();
    await flush();
    await flush();

    const results = transport.sent.filter((f) => f.type === 'result');
    expect(results).toHaveLength(1); // exactly one terminal — no double-result
    expect((results[0] as { status: string }).status).toBe('cancelled');
  });

  it('maps a daemon-reported failure status to a failed mesh result instead of silently succeeding', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok'; // terminal → finalize fast path
    daemon.resultStatus = 'error'; // the daemon reports the run failed
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'fx', agent_id: 'PK', intent: { text: 'do work' } });
    await flush();
    await flush();

    const result = transport.sent.at(-1) as { type: string; status: string };
    expect(result.type).toBe('result');
    // meshStatus default arm: any non-ok/cancelled daemon status surfaces as failed, not success.
    // Also proves daemonStatus carries the real value through rather than defaulting to 'ok'.
    expect(result.status).toBe('failed');
  });

  it('maps a daemon-reported cancelled status through finalize to a cancelled mesh result', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok';
    daemon.resultStatus = 'cancelled';
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'dc', agent_id: 'PK', intent: { text: 'do work' } });
    await flush();
    await flush();

    const result = transport.sent.at(-1) as { status: string };
    // meshStatus 'cancelled' arm — distinct from the cancel-handler's literal 'cancelled'.
    expect(result.status).toBe('cancelled');
  });

  it('keeps tracking when the daemon result carries no string status field', async () => {
    const daemon = new FakeDaemon();
    daemon.status = 'ok';
    daemon.resultStatus = undefined; // result without a usable status
    const { transport, connector } = build(daemon);
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'ns', agent_id: 'PK', intent: { text: 'do work' } });
    await flush();
    await flush();

    expect(transport.sent.some((f) => f.type === 'result')).toBe(false);
    expect(connector.inFlight).toBe(1);
    transport.inject({ v: 1, type: 'cancel', dispatch_id: 'ns' });
    await flush();
    expect(daemon.stops).toEqual(['int-1']);
    expect(connector.inFlight).toBe(0);
  });

  it('forwards the explicit GPU policy and enforced grant, with a stable replay key', async () => {
    const daemon = new FakeDaemon();
    const transport = new StubTransport();
    const connector = new Connector(daemon, transport, { maxConcurrentDispatch: 1, defaultDeadlineMs: 60_000, manifestCapabilities: [], agentId: 'full-agent-id', gpuEnabled: true });
    await connector.start();
    const frame = { v: 1 as const, type: 'dispatch' as const, dispatch_id: 'gpu-1', agent_id: 'full-agent-id', intent: { text: 'GPU smoke', context: { max_cost_usd: 0.50, gpu: { duration_secs: 300 } } }, grants: [{ scope: 'compute.gpu_workspace', constraints: { max_usdc_micros: 200_000, duration_secs: 300, max_launches: 1 } }] };
    transport.inject(frame);
    await flush();
    await flush();
    transport.inject(frame);
    await flush();
    await flush();
    expect(daemon.executions).toHaveLength(2);
    expect(daemon.executions[0]).toEqual(daemon.executions[1]);
    expect(daemon.executions[0]).toMatchObject({ run_key: expect.stringMatching(/^[a-f0-9]{64}$/), max_cost_usd: 0.50, gpu: frame.grants[0]!.constraints });
    expect(daemon.grants).toContainEqual(expect.objectContaining({ action: 'compute.gpu_workspace', scope: frame.grants[0]!.constraints }));
  });

  it('grants native coder capabilities only for the authorized GPU run key and deadline', async () => {
    const daemon = new FakeDaemon();
    const transport = new StubTransport();
    const connector = new Connector(daemon, transport, {
      maxConcurrentDispatch: 1, defaultDeadlineMs: 60_000, manifestCapabilities: [],
      agentId: 'full-agent-id', gpuEnabled: true, now: () => 1_000,
    });
    await connector.start();

    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'ordinary', agent_id: 'full-agent-id', intent: { text: 'ordinary work' }, deadline_ms: 5_000 });
    await flush();
    await flush();
    expect(daemon.grants).toEqual([{ action: 'memory.write', expires_at: 5_000 }]);
    const ordinaryRunKey = daemon.executions[0]!.run_key;

    daemon.grants = [];
    transport.inject({
      v: 1, type: 'dispatch', dispatch_id: 'authorized-gpu', agent_id: 'full-agent-id',
      intent: { text: 'GPU smoke', context: { max_cost_usd: 0.50 } }, deadline_ms: 5_000,
      grants: [{ scope: 'compute.gpu_workspace', constraints: { max_usdc_micros: 200_000, duration_secs: 300, max_launches: 1 } }],
    });
    await flush();
    await flush();
    const gpuRunKey = daemon.executions[1]!.run_key;
    expect(gpuRunKey).not.toBe(ordinaryRunKey);
    expect(daemon.grants.filter((grant) => ['intent.subscribe', 'tool.code'].includes(grant.action))).toEqual([
      { action: 'intent.subscribe', scope: { version: 1, run_key: gpuRunKey }, expires_at: 5_000 },
      { action: 'tool.code', scope: { version: 1, run_key: gpuRunKey }, expires_at: 5_000 },
    ]);
    expect(daemon.grants.filter((grant) => grant.action === 'memory.write')).toEqual([
      { action: 'memory.write', expires_at: 5_000 },
    ]);
  });

  it('does not admit GPU intent context without a consent grant', async () => {
    const daemon = new FakeDaemon();
    const { transport, connector } = build(daemon);
    await connector.start();
    transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'no-grant', agent_id: 'PK', intent: { text: 'GPU', context: { max_cost_usd: 0.50, gpu: { duration_secs: 300 } } } });
    await flush();
    expect(daemon.submitted).toEqual([]);
    expect(daemon.grants).toEqual([]);
    expect(transport.sent[0]).toMatchObject({ type: 'error', code: 'not_admitted' });
    expect(connector.inFlight).toBe(0);
  });

  it('keeps a stopping workload tracked after cancel, reports cleanup_pending, then relays confirmed cancellation', async () => {
    vi.useFakeTimers();
    try {
      const daemon = new FakeDaemon();
      daemon.resultStatus = 'running';
      daemon.streamEvents = () => new Promise<void>(() => {});
      const stop = vi.spyOn(daemon, 'stopIntent').mockImplementation(async () => ({ accepted: true }));
      const transport = new StubTransport();
      const connector = new Connector(daemon, transport, { maxConcurrentDispatch: 1, defaultDeadlineMs: 60_000, manifestCapabilities: [], terminalPollMs: 5, cancelTimeoutMs: 10 });
      await connector.start();
      transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'cancel-pending', agent_id: 'PK', intent: { text: 'work' } });
      await vi.advanceTimersByTimeAsync(1);
      transport.inject({ v: 1, type: 'cancel', dispatch_id: 'cancel-pending' });
      daemon.resultStatus = 'stopping';
      await vi.advanceTimersByTimeAsync(20);
      expect(stop).toHaveBeenCalledWith('int-1');
      expect(connector.inFlight).toBe(1);
      expect(transport.sent.some((f) => f.type === 'result')).toBe(false);
      expect(transport.sent).toContainEqual(expect.objectContaining({ type: 'error', code: 'cleanup_pending' }));
      daemon.resultStatus = 'cancelled';
      await vi.advanceTimersByTimeAsync(10);
      expect(connector.inFlight).toBe(0);
      expect(transport.sent.filter((f) => f.type === 'result')).toEqual([expect.objectContaining({ status: 'cancelled' })]);
    } finally { vi.useRealTimers(); }
  });

  it('requests workload stop at the dispatch deadline instead of discarding tracking', async () => {
    vi.useFakeTimers();
    try {
      const daemon = new FakeDaemon();
      daemon.resultStatus = 'running';
      daemon.streamEvents = () => new Promise<void>(() => {});
      const transport = new StubTransport();
      const connector = new Connector(daemon, transport, { maxConcurrentDispatch: 1, defaultDeadlineMs: 10, manifestCapabilities: [], terminalPollMs: 5 });
      await connector.start();
      transport.inject({ v: 1, type: 'dispatch', dispatch_id: 'timeout', agent_id: 'PK', intent: { text: 'work' } });
      await vi.advanceTimersByTimeAsync(20);
      expect(daemon.stops).toEqual(['int-1']);
      expect(transport.sent.at(-1)).toMatchObject({ type: 'result', status: 'cancelled' });
      expect(connector.inFlight).toBe(0);
    } finally { vi.useRealTimers(); }
  });
});
