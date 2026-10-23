import { afterEach, describe, expect, it, vi } from 'vitest';
import { Connector } from './connector.js';
import type { AgentEvent, DaemonClient, IntegrityReport } from './daemon.js';
import { StubTransport, type InboundFrame, type OutboundFrame } from './transport.js';

type Result = Extract<OutboundFrame, { type: 'result' }>;
const audit: IntegrityReport = {
  events: 3,
  anchors: 0,
  valid: true,
  root_hash_hex: 'reconciled-audit-root',
  failures: [],
};
const gpu = { max_usdc_micros: 200_000, duration_secs: 300, max_launches: 1 };
const frame: Extract<InboundFrame, { type: 'dispatch' }> = {
  v: 1,
  type: 'dispatch',
  dispatch_id: 'gpu-cleanup-recovery',
  agent_id: 'full-agent-id',
  intent: {
    text: 'Run the authorized GPU smoke test',
    context: { max_cost_usd: 0.5, gpu: { duration_secs: 300 } },
  },
  grants: [{ scope: 'compute.gpu_workspace', constraints: gpu }],
};
const launchFailure: AgentEvent = {
  type: 'tool_result',
  run_id: 'gateway-run-1',
  tool: 'gpu_workspace',
  duration_ms: 10,
  error: true,
};

function terminalOutcome(status: 'error' | 'failed' | 'cancelled') {
  // Gateway receipts are part of the daemon's final output, distinct from its
  // settlement receipt_id in the connector proof envelope.
  const receipt = {
    job_id: 'original-compute-job',
    status: 'failed',
    receipt: { runtime_secs: 0, charged_usdc_micros: 0, refunded_usdc_micros: 200_000 },
  };
  return {
    kind: 'intent_outcome',
    intent_id: 'intent-1',
    status,
    text: `GPU command timed out.\n[gpu_workspace final receipts: ${JSON.stringify([receipt])}]\n[run accounting: {"total_usd":0.12}]`,
    result_hash_hex: 'final-output-hash',
  };
}

function setup() {
  vi.useFakeTimers();
  vi.setSystemTime(1_000);
  let outcome: unknown = { kind: 'intent_outcome', intent_id: 'intent-1', status: 'running' };
  let onEvent: (event: AgentEvent) => void = () => {};
  let closeStream!: () => void;
  let streamSignal: AbortSignal | undefined;
  const streamGate = new Promise<void>((resolve) => {
    closeStream = resolve;
  });
  const daemon = {
    health: vi.fn(async () => true),
    grant: vi.fn(async () => {}),
    submitIntent: vi.fn(async () => ({
      intent_id: 'intent-1',
      status: 'running',
      text: '',
      sources: ['hermes'],
    })),
    intentResult: vi.fn(async () => outcome),
    stopIntent: vi.fn(async () => ({ status: 'stopping', intent_id: 'intent-1' })),
    streamEvents: vi.fn(
      async (_id: string, callback: (event: AgentEvent) => void, signal: AbortSignal) => {
        onEvent = callback;
        streamSignal = signal;
        await streamGate;
      },
    ),
    verify: vi.fn(async () => audit),
  } satisfies DaemonClient;
  const transport = new StubTransport();
  const connector = new Connector(daemon, transport, {
    maxConcurrentDispatch: 1,
    defaultDeadlineMs: 20,
    manifestCapabilities: [],
    agentId: frame.agent_id,
    gpuEnabled: true,
    terminalPollMs: 5,
    cancelTimeoutMs: 10,
  });
  return {
    daemon,
    transport,
    connector,
    setOutcome: (value: unknown) => {
      outcome = value;
    },
    emit: (event: AgentEvent) => onEvent(event),
    closeStream: () => closeStream(),
    streamSignal: () => streamSignal,
    results: () => transport.sent.filter((sent): sent is Result => sent.type === 'result'),
  };
}

async function pendingCleanup(h: ReturnType<typeof setup>) {
  await h.connector.start();
  h.transport.inject(frame);
  await vi.advanceTimersByTimeAsync(1);
  h.emit(launchFailure);
  h.setOutcome({
    kind: 'intent_outcome',
    intent_id: 'intent-1',
    status: 'stopping',
    text: 'cleanup_pending: launch outcome unknown',
  });
  await vi.advanceTimersByTimeAsync(39);
  expect(h.daemon.stopIntent).toHaveBeenCalledWith('intent-1');
  expect(h.connector.inFlightIds).toEqual([frame.dispatch_id]);
  expect(h.results()).toEqual([]);
  expect(
    h.transport.sent.filter((sent) => sent.type === 'error' && sent.code === 'cleanup_pending'),
  ).toHaveLength(1);
}

afterEach(() => {
  vi.useRealTimers();
});

describe('Connector cleanup recovery and terminal delivery', () => {
  it.each([
    ['error', 'failed'],
    ['failed', 'failed'],
    ['cancelled', 'cancelled'],
  ] as const)(
    'automatically relays a reconciled %s result and receipt despite a hung SSE stream',
    async (daemonStatus, meshStatus) => {
      const h = setup();
      await pendingCleanup(h);
      // Replay while cleanup is uncertain cannot submit the original work twice or
      // admit a replacement that could buy a second GPU workspace.
      h.transport.inject(frame);
      h.transport.inject({ ...frame, dispatch_id: 'replacement' });
      await vi.advanceTimersByTimeAsync(0);
      expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
      expect(h.transport.sent).toContainEqual(
        expect.objectContaining({
          type: 'error',
          dispatch_id: 'replacement',
          code: 'not_admitted',
        }),
      );

      const outcome = terminalOutcome(daemonStatus);
      h.setOutcome(outcome);
      await vi.advanceTimersByTimeAsync(5);
      expect(h.results()).toEqual([
        expect.objectContaining({
          dispatch_id: frame.dispatch_id,
          status: meshStatus,
          result: outcome,
        }),
      ]);
      expect(h.results()[0]!.proof).toMatchObject({
        spec: 'covenant.connector-trace.v0',
        intent_id: 'intent-1',
        result: outcome,
        status: meshStatus,
        audit_root: audit.root_hash_hex,
        audit_verified: true,
        receipt_id: null,
        context: frame.intent.context,
        execution: { run_key: expect.stringMatching(/^[a-f0-9]{64}$/), max_cost_usd: 0.5, gpu },
        events: [launchFailure],
      });
      expect((h.results()[0]!.result as { text: string }).text).toContain(
        '[run accounting: {"total_usd":0.12}]',
      );
      expect(h.results()[0]!.proof).toMatchObject({
        result: { result_hash_hex: 'final-output-hash' },
      });
      expect(h.connector.inFlight).toBe(0);
      expect(h.streamSignal()?.aborted).toBe(true);
      expect(h.daemon.verify).toHaveBeenCalledTimes(1);
      h.closeStream();
      await vi.advanceTimersByTimeAsync(20);
      expect(h.results()).toHaveLength(1);
      expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
    },
  );

  it('emits one final receipt when cleanup polling, cancellation, and SSE completion race', async () => {
    const h = setup();
    await pendingCleanup(h);
    let releaseAudit!: (value: IntegrityReport) => void;
    h.daemon.verify.mockImplementation(
      () =>
        new Promise((resolve) => {
          releaseAudit = resolve;
        }),
    );
    const outcome = terminalOutcome('cancelled');
    h.setOutcome(outcome);
    await vi.advanceTimersByTimeAsync(5);
    expect(h.daemon.verify).toHaveBeenCalledTimes(1);
    const stopsBeforeRace = h.daemon.stopIntent.mock.calls.length;
    h.closeStream();
    h.transport.inject({ v: 1, type: 'cancel', dispatch_id: frame.dispatch_id });
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(15);
    expect(h.results()).toEqual([]);
    expect(h.connector.inFlight).toBe(1);
    expect(h.daemon.stopIntent).toHaveBeenCalledTimes(stopsBeforeRace);
    releaseAudit(audit);
    await vi.advanceTimersByTimeAsync(20);
    expect(h.results()).toEqual([
      expect.objectContaining({ status: 'cancelled', result: outcome }),
    ]);
    expect(h.daemon.verify).toHaveBeenCalledTimes(1);
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
    expect(h.connector.inFlight).toBe(0);
  });

  it('retains the same terminal receipt and slot until a rejected local send can be retried', async () => {
    const h = setup();
    await pendingCleanup(h);
    const send = h.transport.send.bind(h.transport);
    const attempts: Result[] = [];
    vi.spyOn(h.transport, 'send').mockImplementation(async (outbound) => {
      if (outbound.type === 'result') {
        attempts.push(outbound);
        if (attempts.length === 1) throw new Error('local transport rejected before queueing');
      }
      await send(outbound);
    });
    const outcome = terminalOutcome('error');
    h.setOutcome(outcome);
    await vi.advanceTimersByTimeAsync(5);
    expect(attempts).toHaveLength(1);
    expect(h.results()).toEqual([]);
    expect(h.connector.inFlightIds).toEqual([frame.dispatch_id]);
    expect(h.streamSignal()?.aborted).toBe(false);
    // Captured evidence survives daemon eviction/failure and late SSE events.
    // Retry delivery only, never execution or settlement.
    h.daemon.intentResult.mockRejectedValue(new Error('daemon result no longer available'));
    h.daemon.verify.mockRejectedValue(new Error('daemon unavailable'));
    h.emit({ type: 'reasoning', run_id: 'gateway-run-1', preview: 'late SSE event' });
    h.transport.inject(frame);
    const reads = h.daemon.intentResult.mock.calls.length;
    const stops = h.daemon.stopIntent.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5);
    expect(attempts).toHaveLength(2);
    expect(attempts[1]).toBe(attempts[0]);
    expect(h.results()).toEqual([attempts[0]]);
    expect(h.results()[0]!.proof).toMatchObject({ result: outcome, events: [launchFailure] });
    expect(h.daemon.intentResult).toHaveBeenCalledTimes(reads);
    expect(h.daemon.stopIntent).toHaveBeenCalledTimes(stops);
    expect(h.daemon.verify).toHaveBeenCalledTimes(1);
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
    expect(h.connector.inFlight).toBe(0);
    expect(h.streamSignal()?.aborted).toBe(true);
    h.closeStream();
    await vi.advanceTimersByTimeAsync(20);
    expect(h.results()).toHaveLength(1);
  });

  it('retries a pre-submission cancellation result without submitting or polling an empty intent id', async () => {
    const h = setup();
    let releaseGrant!: () => void;
    h.daemon.grant.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          releaseGrant = resolve;
        }),
    );
    const send = h.transport.send.bind(h.transport);
    let attempts = 0;
    vi.spyOn(h.transport, 'send').mockImplementation(async (outbound) => {
      if (outbound.type === 'result' && ++attempts === 1)
        throw new Error('local queue unavailable');
      await send(outbound);
    });
    await h.connector.start();
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(0);
    h.transport.inject({ v: 1, type: 'cancel', dispatch_id: frame.dispatch_id });
    releaseGrant();
    await vi.advanceTimersByTimeAsync(0);
    expect(h.connector.inFlight).toBe(1);
    expect(h.results()).toEqual([]);
    await vi.advanceTimersByTimeAsync(5);
    expect(h.results()).toEqual([
      expect.objectContaining({
        status: 'cancelled',
        result: { status: 'cancelled', text: 'cancelled before submission' },
      }),
    ]);
    expect(h.daemon.submitIntent).not.toHaveBeenCalled();
    expect(h.daemon.stopIntent).not.toHaveBeenCalled();
    expect(h.daemon.intentResult).not.toHaveBeenCalled();
    expect(h.connector.inFlight).toBe(0);
  });

  it('ignores a previous stream closing after the same dispatch id is replayed', async () => {
    const h = setup();
    await pendingCleanup(h);
    h.setOutcome(terminalOutcome('error'));
    await vi.advanceTimersByTimeAsync(5);
    expect(h.results()).toHaveLength(1);

    // Gateway deduplication may return the same execution for a later replay.
    // The old SSE callback must not finalize or remove its new connector entry.
    h.daemon.streamEvents.mockImplementationOnce(() => new Promise<void>(() => {}));
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.connector.inFlight).toBe(1);
    h.closeStream();
    await vi.advanceTimersByTimeAsync(0);
    expect(h.results()).toHaveLength(1);
    expect(h.connector.inFlight).toBe(1);
    await vi.advanceTimersByTimeAsync(5);
    expect(h.results()).toHaveLength(2);
    expect(h.connector.inFlight).toBe(0);
  });

  it('does not report stale cleanup_pending when an overlapping stop fails after final delivery', async () => {
    const h = setup();
    let rejectStop!: (reason: Error) => void;
    h.daemon.stopIntent.mockImplementationOnce(
      () =>
        new Promise((_resolve, reject) => {
          rejectStop = reject;
        }),
    );
    await h.connector.start();
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(1);
    h.transport.inject({ v: 1, type: 'cancel', dispatch_id: frame.dispatch_id });
    h.setOutcome(terminalOutcome('cancelled'));
    await vi.advanceTimersByTimeAsync(4);
    expect(h.results()).toHaveLength(1);
    expect(h.connector.inFlight).toBe(0);
    rejectStop(new Error('overlapping stop request failed'));
    h.closeStream();
    await vi.advanceTimersByTimeAsync(20);
    expect(h.results()).toHaveLength(1);
    expect(h.transport.sent.some((sent) => sent.type === 'error')).toBe(false);
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
  });

  it('keeps admitted work watched when accepted and trace frames are rejected', async () => {
    const h = setup();
    const send = h.transport.send.bind(h.transport);
    vi.spyOn(h.transport, 'send').mockImplementation(async (outbound) => {
      if (outbound.type === 'accepted' || outbound.type === 'trace') {
        throw new Error('progress frame rejected before queueing');
      }
      await send(outbound);
    });
    await h.connector.start();
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.daemon.streamEvents).toHaveBeenCalledTimes(1);
    h.emit(launchFailure);
    const outcome = terminalOutcome('error');
    h.setOutcome(outcome);
    await vi.advanceTimersByTimeAsync(4);
    expect(h.results()).toEqual([expect.objectContaining({ status: 'failed', result: outcome })]);
    expect(h.results()[0]!.proof).toMatchObject({ events: [launchFailure] });
    expect(h.connector.inFlight).toBe(0);
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
  });

  it('continues polling through rejected cleanup notifications and retries a rejected final send', async () => {
    const h = setup();
    const send = h.transport.send.bind(h.transport);
    let cleanupAttempts = 0;
    const resultAttempts: Result[] = [];
    vi.spyOn(h.transport, 'send').mockImplementation(async (outbound) => {
      if (outbound.type === 'error' && outbound.code === 'cleanup_pending') {
        cleanupAttempts++;
        throw new Error('cleanup notification rejected before queueing');
      }
      if (outbound.type === 'result') {
        resultAttempts.push(outbound);
        if (resultAttempts.length === 1) throw new Error('final result rejected before queueing');
      }
      await send(outbound);
    });
    await h.connector.start();
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(1);
    h.emit(launchFailure);
    h.setOutcome({ status: 'stopping', text: 'cleanup_pending' });
    await vi.advanceTimersByTimeAsync(39);
    expect(cleanupAttempts).toBeGreaterThan(1);
    expect(h.results()).toEqual([]);
    expect(h.connector.inFlight).toBe(1);
    const outcome = terminalOutcome('error');
    h.setOutcome(outcome);
    h.closeStream();
    await vi.advanceTimersByTimeAsync(0);
    expect(resultAttempts).toHaveLength(1);
    expect(h.connector.inFlight).toBe(1);
    await vi.advanceTimersByTimeAsync(10);
    expect(resultAttempts).toHaveLength(2);
    expect(resultAttempts[1]).toBe(resultAttempts[0]);
    expect(h.results()).toEqual([expect.objectContaining({ status: 'failed', result: outcome })]);
    expect(h.connector.inFlight).toBe(0);
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
  });

  it('watches an admitted intent while accepted delivery stalls and ignores its late completion', async () => {
    const h = setup();
    let releaseAccepted!: () => void;
    const accepted = new Promise<void>((resolve) => {
      releaseAccepted = resolve;
    });
    const send = h.transport.send.bind(h.transport);
    vi.spyOn(h.transport, 'send').mockImplementation(async (outbound) => {
      if (outbound.type === 'accepted') return accepted;
      await send(outbound);
    });
    await h.connector.start();
    h.transport.inject(frame);
    await vi.advanceTimersByTimeAsync(1);
    h.setOutcome(terminalOutcome('error'));
    await vi.advanceTimersByTimeAsync(4);
    expect(h.results()).toHaveLength(1);
    expect(h.connector.inFlight).toBe(0);
    releaseAccepted();
    await vi.advanceTimersByTimeAsync(10);
    expect(h.results()).toHaveLength(1);
    expect(h.daemon.streamEvents).not.toHaveBeenCalled();
    expect(h.daemon.submitIntent).toHaveBeenCalledTimes(1);
  });
});
