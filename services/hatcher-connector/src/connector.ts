// The connector core: translates Hatcher dispatch frames into local covenantd
// intents, relays the live SSE trace back, and folds the result + audit root
// into a covenant.connector-trace.v0 proof envelope. The daemon-facing calls
// are all verified (daemon.ts); the Hatcher side is behind HatcherTransport so
// this logic is testable today with StubTransport.

import { DaemonUnreachableError, type DaemonClient, type AgentEvent, type GrantRequest } from './daemon.js';
import { mapManifest, mapMeshGrants, baselineGrants, type ConsentPolicy } from './capabilities.js';
import type { ManifestCapability } from './manifest.js';
import type { HatcherTransport, InboundFrame } from './transport.js';

export interface ConnectorOptions {
  maxConcurrentDispatch: number;
  defaultDeadlineMs: number;
  // Capabilities the paired manifest authorizes; minted per-dispatch.
  manifestCapabilities: ManifestCapability[];
  now?: () => number;
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
}

export class Connector {
  private readonly active = new Map<string, ActiveDispatch>();
  private readonly now: () => number;
  private readonly log: (msg: string, extra?: Record<string, unknown>) => void;

  constructor(
    private readonly daemon: DaemonClient,
    private readonly transport: HatcherTransport,
    private readonly opts: ConnectorOptions,
  ) {
    this.now = opts.now ?? (() => Date.now());
    this.log = opts.log ?? (() => {});
  }

  async start(): Promise<void> {
    await this.transport.connect();
    this.transport.onFrame((frame) => {
      void this.handleFrame(frame).catch((err) => this.log('frame handler error', { err: String(err) }));
    });
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

    // Reserve the slot SYNCHRONOUSLY before the first await so a duplicate
    // dispatch_id (reconnect/replay) cannot race past the has() guard above.
    const entry: ActiveDispatch = { intentId: '', seq: 0, events: [], abort: new AbortController() };
    this.active.set(dispatch_id, entry);

    // Mint least-privilege capabilities for this dispatch, expiring at the deadline.
    // baselineGrants covers the Stage-1 intent-submission gate (memory.write); the
    // manifest contributes the enforced tool.call.<name>/a2a grants. filesystem/
    // terminal/browser are sandbox+audit governed (policy), not token grants.
    const deadline = frame.deadline_ms ?? this.now() + this.opts.defaultDeadlineMs;
    // Hatcher sends per-dispatch grants in the frame (its consent UI); fall back to the
    // paired manifest if none. fs/terminal/browser land in the consent policy
    // (sandbox-gated), github/mcp/a2a become enforced covenant grants.
    const { grants, policy } = frame.grants?.length
      ? mapMeshGrants(frame.grants, deadline)
      : mapManifest(this.opts.manifestCapabilities, deadline);
    entry.policy = policy;
    entry.context = frame.intent.context;
    const minted = [...baselineGrants(deadline), ...grants];
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
    let intentStatus: string;
    try {
      const res = await this.daemon.submitIntent(text);
      entry.intentId = res.intent_id;
      entry.submitResult = res;
      intentStatus = res.status;
    } catch (err) {
      this.active.delete(dispatch_id);
      const code = err instanceof DaemonUnreachableError ? 'daemon_unreachable' : 'dispatch_failed';
      return this.transport.send({ v: 1, type: 'error', dispatch_id, code, message: String(err) });
    }

    await this.transport.send({ v: 1, type: 'accepted', dispatch_id, intent_id: entry.intentId });

    // Async Hermes runs report status "running" and stream their trace over SSE; a
    // synchronously-terminal status (e.g. the daemon's phase-0 fast path) has no live
    // trace to relay, so finalize directly rather than opening a stream that would
    // just block on keepalives.
    if (intentStatus === 'running') {
      void this.runTrace(dispatch_id, entry).catch((err) => this.log('trace error', { dispatch_id, err: String(err) }));
    } else {
      void this.finalize(dispatch_id, entry).catch((err) => this.log('finalize error', { dispatch_id, err: String(err) }));
    }
  }

  private async runTrace(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    try {
      await this.daemon.streamEvents(
        entry.intentId,
        (event) => {
          entry.events.push(event);
          void this.transport.send({ v: 1, type: 'trace', dispatch_id: dispatchId, seq: entry.seq++, event });
        },
        entry.abort.signal,
      );
    } catch (err) {
      if (entry.abort.signal.aborted) return; // cancelled — finalized elsewhere
      this.log('stream ended with error', { dispatchId, err: String(err) });
    }
    await this.finalize(dispatchId, entry);
  }

  private async finalize(dispatchId: string, entry: ActiveDispatch): Promise<void> {
    if (!this.active.has(dispatchId)) return;
    let result: unknown = entry.submitResult ?? null;
    let auditRoot: string | null = null;
    let auditVerified = false;
    try {
      // Async runs expose the final result here; the synchronous fast path 404s on
      // re-fetch (its result already arrived in the submit response), so keep that.
      result = await this.daemon.intentResult(entry.intentId);
    } catch (err) {
      this.log('result re-fetch failed; using submit-time result', { dispatchId, err: String(err) });
    }
    try {
      const report = await this.daemon.verify();
      auditRoot = report.root_hash_hex;
      auditVerified = report.valid;
    } catch (err) {
      this.log('audit verify failed', { dispatchId, err: String(err) });
    }
    const daemonStatus = typeof (result as { status?: string })?.status === 'string' ? (result as { status: string }).status : 'ok';
    const status = meshStatus(daemonStatus);
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
      events: entry.events,
    };
    this.active.delete(dispatchId);
    await this.transport.send({ v: 1, type: 'result', dispatch_id: dispatchId, status, result, proof });
  }

  private async cancel(dispatchId: string): Promise<void> {
    const entry = this.active.get(dispatchId);
    if (!entry) return;
    entry.abort.abort();
    this.active.delete(dispatchId);
    // Stop relaying and report a terminal cancelled result (the mesh contract's
    // result.status enum is success|failed|cancelled). Daemon-side run stop is wired
    // through the runner cancel.
    const proof = {
      spec: 'covenant.connector-trace.v0',
      intent_id: entry.intentId,
      status: 'cancelled',
      audit_root: null,
      capability_grants: entry.grants ?? [],
      declared_policy: entry.policy ?? null,
      context: entry.context ?? null,
      events: entry.events,
    };
    await this.transport.send({ v: 1, type: 'result', dispatch_id: dispatchId, status: 'cancelled', result: null, proof });
  }

  get inFlight(): number {
    return this.active.size;
  }
}

function meshStatus(s: string): 'success' | 'failed' | 'cancelled' {
  if (s === 'ok' || s === 'success') return 'success';
  if (s === 'cancelled') return 'cancelled';
  return 'failed';
}
