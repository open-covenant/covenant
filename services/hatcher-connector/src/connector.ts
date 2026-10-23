// The connector core: translates Hatcher dispatch frames into local covenantd
// intents, relays the live SSE trace back, and folds the result + audit root
// into a covenant.connector-trace.v0 proof envelope. The daemon-facing calls
// are all verified (daemon.ts); the Hatcher side is behind HatcherTransport so
// this logic is testable today with StubTransport.

import { DaemonUnreachableError, type DaemonClient, type AgentEvent, type GrantRequest, type IntentExecution } from './daemon.js';
import { mapManifest, mapMeshGrants, baselineGrants, type ConsentPolicy } from './capabilities.js';
import { executionFor } from './execution.js';
import type { ManifestCapability } from './manifest.js';
import type { HatcherTransport, InboundFrame, OutboundFrame } from './transport.js';

export interface ConnectorOptions {
  maxConcurrentDispatch: number;
  defaultDeadlineMs: number;
  // Capabilities the paired manifest authorizes; minted per-dispatch.
  manifestCapabilities: ManifestCapability[];
  agentId?: string;
  gpuEnabled?: boolean;
  cancelTimeoutMs?: number;
  now?: () => number;
  // How often the terminal-status watchdog polls /intents/:id/result as an
  // SSE-hang fallback. Default 5s.
  terminalPollMs?: number;
  log?: (msg: string, extra?: Record<string, unknown>) => void;
}

interface ActiveDispatch {
  intentId: string;
  seq: number;
  events: AgentEvent[];
  abort: AbortController;
  policy?: ConsentPolicy;
  // Covenant grants minted for this dispatch — echoed into the proof's capability_grants.
  grants?: GrantRequest[];
  // Hatcher's intent.context, carried into the proof for the auditable task record.
  context?: Record<string, unknown>;
  // The IntentResult from the submit response — used as the proof result for the
  // synchronous fast path, where GET /intents/:id/result 404s after the fact.
  submitResult?: unknown;
  // Guards against the SSE-end path and the poll watchdog both finalizing.
  finalizing?: boolean;
  // Preserve terminal evidence until the transport accepts it locally. The mesh
  // protocol has no result acknowledgement, so this is not remote delivery proof.
  finalFrame?: Extract<OutboundFrame, { type: 'result' }>;
  watchdogStarted?: boolean;
  execution: IntentExecution;
  deadline: number;
  cancelRequestedAt?: number;
  lastStopAttempt?: number;
  cleanupPendingReported?: boolean;
  submissionStarted?: boolean;
}

export class Connector {
  private readonly active = new Map<string, ActiveDispatch>();
  private readonly now: () => number;
  private readonly terminalPollMs: number;
  private readonly log: (msg: string, extra?: Record<string, unknown>) => void;

  constructor(
    private readonly daemon: DaemonClient,
    private readonly transport: HatcherTransport,
    private readonly opts: ConnectorOptions,
  ) {
    this.now = opts.now ?? (() => Date.now());
    this.terminalPollMs = opts.terminalPollMs ?? 5_000;
    this.log = opts.log ?? (() => {});
  }

  async start(): Promise<void> {
    this.transport.onFrame((frame) => {
      void this.handleFrame(frame).catch((err) => this.log('frame handler error', { err: String(err) }));
    });
    await this.transport.connect();
  }

  private async handleFrame(frame: InboundFrame): Promise<void> {
    switch (frame.type) {
      case 'ping':
        return this.transport.send({ v: 1, type: 'pong', ts: this.now() });
      case 'cancel':
        return this.cancel(frame.dispatch_id);
      case 'dispatch':
        return this.dispatch(frame);
    }
  }

  // Admission -> grant -> POST /intent -> SSE relay -> result+proof.
  private async dispatch(frame: Extract<InboundFrame, { type: 'dispatch' }>): Promise<void> {
    const { dispatch_id } = frame;
    if (this.active.has(dispatch_id)) return; // idempotent on reconnect/replay
    if (this.active.size >= this.opts.maxConcurrentDispatch) {
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code: 'not_admitted', message: 'max concurrent dispatch reached' });
    }
    const text = frame.intent?.text?.trim();
    if (!text) {
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code: 'not_admitted', message: 'empty intent text' });
    }

    let execution: IntentExecution;
    const deadline = frame.deadline_ms ?? this.now() + this.opts.defaultDeadlineMs;
    let mapped: ReturnType<typeof mapMeshGrants>;
    try {
      if (!Number.isSafeInteger(deadline) || deadline <= this.now()) throw new Error('dispatch deadline must be in the future');
      execution = executionFor(frame, this.opts.agentId, this.opts.gpuEnabled ?? false);
      mapped = frame.grants?.length
        ? mapMeshGrants(frame.grants, deadline)
        : mapManifest(this.opts.manifestCapabilities, deadline);
    } catch (err) {
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code: 'not_admitted', message: err instanceof Error ? err.message : 'invalid execution policy' });
    }

    // Reserve the slot SYNCHRONOUSLY before the first await so a duplicate
    // dispatch_id (reconnect/replay) cannot race past the has() guard above.
    const entry: ActiveDispatch = { intentId: '', seq: 0, events: [], abort: new AbortController(), execution, deadline };
    this.active.set(dispatch_id, entry);

    // Mint least-privilege capabilities for this dispatch, expiring at the deadline.
    // baselineGrants covers the Stage-1 intent-submission gate (memory.write); the
    // manifest contributes the enforced tool.call.<name>/a2a grants. filesystem/
    // terminal/browser are sandbox+audit governed (policy), not token grants.
    // Hatcher sends per-dispatch grants in the frame (its consent UI); fall back to the
    // paired manifest if none. fs/terminal/browser land in the consent policy
    // (sandbox-gated), github/mcp/a2a become enforced covenant grants.
    const { grants, policy } = mapped;
    entry.policy = policy;
    entry.context = frame.intent.context;
    // Authorize the native coder only for this explicitly consented GPU run.
    // The daemon matches this scope against execution.run_key, so these grants
    // cannot authorize another dispatch or an ordinary/legacy intent.
    const codingGrants: GrantRequest[] = execution.gpu
      ? ['intent.subscribe', 'tool.code'].map((action) => ({
          action,
          scope: { version: 1, run_key: execution.run_key },
          expires_at: deadline,
        }))
      : [];
    const minted = [...baselineGrants(deadline), ...grants, ...codingGrants];
    entry.grants = minted;
    try {
      for (const g of minted) {
        await this.daemon.grant(g);
      }
    } catch (err) {
      this.active.delete(dispatch_id);
      const code = err instanceof DaemonUnreachableError ? 'daemon_unreachable' : 'capability_denied';
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code, message: String(err) });
    }

    // Dispatch the intent.
    if (this.now() >= deadline) entry.cancelRequestedAt ??= this.now();
    if (entry.cancelRequestedAt !== undefined) {
      // Cancellation during capability minting: no workload has been submitted.
      entry.submitResult = { status: 'cancelled', text: 'cancelled before submission' };
      await this.finalize(dispatch_id, entry, entry.submitResult);
      return;
    }
    let intentStatus: string;
    try {
      entry.submissionStarted = true;
      const res = await this.daemon.submitIntent(text, execution);
      entry.intentId = res.intent_id;
      entry.submitResult = res;
      intentStatus = res.status;
    } catch (err) {
      if (err instanceof DaemonUnreachableError) {
        // A timed-out POST can already be running. Keep its slot and never claim
        // cancellation; the stable run_key makes any operator retry idempotent.
        await this.reportCleanupPending(dispatch_id, entry);
        return;
      }
      this.active.delete(dispatch_id);
      const code = err instanceof DaemonUnreachableError ? 'daemon_unreachable' : 'dispatch_failed';
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code, message: String(err) });
    }

    // Once the daemon accepts work, transport failures must not leave it unwatched.
    this.startWatchdog(dispatch_id, entry);
    try {
      await this.transport.send({ v: 1, type: 'accepted', dispatch_id, intent_id: entry.intentId });
    } catch {
      this.log('accepted frame not accepted by transport; terminal tracking retained', { dispatch_id });
    }
    if (this.active.get(dispatch_id) !== entry) return;

    // Async Hermes runs report status "running" and stream their trace over SSE; a
    // synchronously-terminal status (e.g. the daemon's phase-0 fast path) has no live
    // trace to relay, so finalize directly rather than opening a stream that would
    // just block on keepalives.
    if (!terminalStatus(intentStatus)) {
      void this.runTrace(dispatch_id, entry).catch((err) => this.log('trace error', { dispatch_id, err: String(err) }));
      if (entry.cancelRequestedAt !== undefined) await this.requestStop(dispatch_id, entry);
    } else {
      void this.finalize(dispatch_id, entry).catch((err) => this.log('finalize error', { dispatch_id, err: String(err) }));
    }
  }

  private async runTrace(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    // The daemon's /intents/:id/events SSE is best-effort: it can hang half-open
    // over a proxied network and never deliver the terminal close, leaving the
    // dispatch wedged. Poll the result endpoint in parallel so we finalize on a
    // terminal status regardless of whether the stream ever ends.
    try {
      await this.daemon.streamEvents(
        entry.intentId,
        (event) => {
          if (entry.finalFrame || this.active.get(dispatchId) !== entry) return;
          entry.events.push(event);
          void this.sendTrace(dispatchId, entry.seq++, event);
        },
        entry.abort.signal,
      );
    } catch (err) {
      if (entry.abort.signal.aborted) return; // cancelled or finalized by the watchdog
      this.log('stream ended with error', { dispatchId, err: String(err) });
    }
    await this.finalize(dispatchId, entry);
  }

  private async sendTrace(dispatchId: string, seq: number, event: AgentEvent): Promise<void> {
    try {
      await this.transport.send({ v: 1, type: 'trace', dispatch_id: dispatchId, seq, event });
    } catch {
      // Trace delivery is best-effort; the event remains in the final proof.
      this.log('trace frame not accepted by transport', { dispatchId, seq });
    }
  }

  private startWatchdog(dispatchId: string, entry: ActiveDispatch): void {
    if (entry.watchdogStarted) return;
    entry.watchdogStarted = true;
    void this.watchTerminal(dispatchId, entry)
      .catch(() => this.log('terminal watchdog error; tracking retained', { dispatchId }))
      .finally(() => {
        entry.watchdogStarted = false;
        if (this.active.get(dispatchId) === entry && !entry.abort.signal.aborted) {
          this.startWatchdog(dispatchId, entry);
        }
      });
  }

  // Poll /intents/:id/result until the run reaches a terminal status, then
  // finalize — the fallback for a hung SSE stream. finalize() is guarded, so a
  // race with the stream-end path resolves to a single finalize.
  private async watchTerminal(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    while (this.active.get(dispatchId) === entry) {
      await sleep(this.terminalPollMs, entry.abort.signal);
      if (this.active.get(dispatchId) !== entry || entry.abort.signal.aborted) return;
      if (entry.finalizing) continue;
      if (entry.finalFrame) {
        await this.finalize(dispatchId, entry);
        continue;
      }
      if (this.now() >= entry.deadline && entry.cancelRequestedAt === undefined) entry.cancelRequestedAt = this.now();
      if (entry.cancelRequestedAt !== undefined) {
        if (entry.lastStopAttempt === undefined || this.now() - entry.lastStopAttempt >= this.terminalPollMs) {
          await this.requestStop(dispatchId, entry);
        }
        if (this.now() - entry.cancelRequestedAt >= (this.opts.cancelTimeoutMs ?? 30_000)) await this.reportCleanupPending(dispatchId, entry);
      }
      let result: unknown;
      try {
        result = await this.daemon.intentResult(entry.intentId);
      } catch {
        continue; // result not ready (404) or transient — keep polling
      }
      if (terminalStatus(resultStatus(result))) {
        await this.finalize(dispatchId, entry, result);
      }
    }
  }

  private async finalize(dispatchId: string, entry: ActiveDispatch, confirmedResult?: unknown): Promise<void> {
    if (entry.finalizing || this.active.get(dispatchId) !== entry) return;
    entry.finalizing = true;
    try {
      if (!entry.finalFrame) {
        let result: unknown = confirmedResult ?? entry.submitResult ?? null;
        let auditRoot: string | null = null;
        let auditVerified = false;
        try {
          // Async runs expose the final result here; the synchronous fast path 404s on
          // re-fetch (its result already arrived in the submit response), so keep that.
          if (confirmedResult === undefined) result = await this.daemon.intentResult(entry.intentId);
        } catch (err) {
          this.log('result re-fetch failed; using submit-time result', { dispatchId, err: String(err) });
        }
        if (!terminalStatus(resultStatus(result))) {
          return; // A closed SSE stream, missing status, or failed read is not completion.
        }
        try {
          const report = await this.daemon.verify();
          auditRoot = report.root_hash_hex;
          auditVerified = report.valid;
        } catch (err) {
          this.log('audit verify failed', { dispatchId, err: String(err) });
        }
        const status = meshStatus(resultStatus(result)!);
        const receiptId = (result as { settlement?: { receipt_id?: string } | null })?.settlement?.receipt_id ?? null;
        const proof = {
          spec: 'covenant.connector-trace.v0',
          intent_id: entry.intentId,
          result,
          status,
          audit_root: auditRoot,
          audit_verified: auditVerified,
          receipt_id: receiptId,
          memory_ids: [] as string[], // best-effort v1: populated once memory↔intent linkage is wired
          capability_grants: entry.grants ?? [],
          declared_policy: entry.policy ?? null,
          context: entry.context ?? null,
          execution: entry.execution,
          events: [...entry.events],
        };
        entry.finalFrame = { v: 1, type: 'result', dispatch_id: dispatchId, status, result, proof };
      }
      try {
        await this.transport.send(entry.finalFrame);
      } catch {
        // Keep the same receipt/proof even if a later daemon read is unavailable.
        // Also covers cancellation before submission, which has no watchdog yet.
        this.log('terminal result not accepted by transport; retry pending', { dispatchId });
        this.startWatchdog(dispatchId, entry);
        return;
      }
      // send() confirms only local acceptance/queueing, not a remote mesh ack.
      this.active.delete(dispatchId);
      entry.abort.abort();
    } finally {
      entry.finalizing = false;
    }
  }

  private async cancel(dispatchId: string): Promise<void> {
    const entry = this.active.get(dispatchId);
    if (!entry) return;
    entry.cancelRequestedAt ??= this.now();
    if (entry.intentId) {
      await this.requestStop(dispatchId, entry);
      await this.finalize(dispatchId, entry);
    } else if (entry.submissionStarted) {
      await this.reportCleanupPending(dispatchId, entry);
    }
  }

  private async requestStop(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    if (!entry.intentId || entry.finalizing || entry.finalFrame) return;
    entry.lastStopAttempt = this.now();
    try {
      await this.daemon.stopIntent(entry.intentId);
    } catch {
      await this.reportCleanupPending(dispatchId, entry);
    }
  }

  private async reportCleanupPending(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    if (entry.cleanupPendingReported || entry.finalFrame || this.active.get(dispatchId) !== entry) return;
    entry.cleanupPendingReported = true;
    try {
      await this.transport.send({ v: 1, type: 'error', dispatch_id: dispatchId, code: 'cleanup_pending', message: 'Workload cleanup is not confirmed; the dispatch remains tracked. Do not start a replacement run.' });
    } catch {
      entry.cleanupPendingReported = false;
      this.log('cleanup_pending frame not accepted by transport; tracking retained', { dispatchId });
    }
  }

  get inFlight(): number {
    return this.active.size;
  }

  get inFlightIds(): string[] { return [...this.active.keys()]; }
}

function resultStatus(result: unknown): string | undefined {
  return typeof (result as { status?: unknown } | null)?.status === 'string' ? (result as { status: string }).status : undefined;
}

function terminalStatus(status: string | undefined): boolean {
  return status !== undefined && ['ok', 'success', 'failed', 'error', 'cancelled', 'ignored'].includes(status);
}

function meshStatus(s: string): 'success' | 'failed' | 'cancelled' {
  if (s === 'ok' || s === 'success') return 'success';
  if (s === 'cancelled') return 'cancelled';
  return 'failed';
}

// Resolves after `ms`, or early if the signal aborts — so finalize doesn't wait
// out a full poll interval after a cancel.
function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener('abort', done, { once: true });
  });
}
