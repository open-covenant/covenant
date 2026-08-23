import {
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { randomUUID } from 'node:crypto';
import { dirname } from 'node:path';
import { validCostMicrounits } from './usepod-http.js';
import type {
  GatewayEvent,
  ProviderReceipt,
  RunStatus,
  TokenUsage,
  ValidationResult,
} from './types.js';

export interface StoredRun {
  id: string;
  sessionId?: string;
  requestFingerprint?: string;
  reservationId?: string;
  reservedMax?: number;
  status: RunStatus;
  output?: string;
  error?: string;
  events: GatewayEvent[];
  files?: Array<{ path: string; content: string; truncated?: boolean }>;
  patch?: string;
  changedFiles?: string[];
  validations?: ValidationResult[];
  usage?: TokenUsage;
  costUsd?: number;
  providerRequestCount?: number;
  providerReceipts?: ProviderReceipt[];
  updatedAt: string;
}

const TERMINAL = new Set<RunStatus>(['completed', 'failed', 'cancelled']);
const MAX_RUNS = 500;
const MAX_RUN_BYTES = 8 * 1024 * 1024;
const MAX_STORE_BYTES = 256 * 1024 * 1024;

export class RunStore {
  private readonly runs = new Map<string, StoredRun>();
  private persistenceError?: string;

  constructor(private readonly path = process.env.RUN_STORE_PATH?.trim() || undefined) {
    this.load();
  }

  get persistent(): boolean {
    return Boolean(this.path);
  }

  get persistenceReady(): boolean {
    return this.persistenceError === undefined;
  }

  list(): StoredRun[] {
    return [...this.runs.values()].map(clone);
  }

  replay(sessionId: string, requestFingerprint: string): StoredRun | undefined {
    const run = [...this.runs.values()].find((candidate) => candidate.sessionId === sessionId);
    if (!run) return undefined;
    if (run.requestFingerprint !== requestFingerprint) {
      throw new IdempotencyConflictError();
    }
    return clone(run);
  }

  save(run: StoredRun): void {
    if (Buffer.byteLength(JSON.stringify(run)) > MAX_RUN_BYTES) {
      throw new Error('run receipt exceeds the 8MB persistence limit');
    }
    const previous = new Map(this.runs);
    try {
      if (
        run.sessionId &&
        [...this.runs.values()].some(
          (candidate) => candidate.id !== run.id && candidate.sessionId === run.sessionId,
        )
      ) {
        throw new IdempotencyConflictError();
      }
      this.runs.set(run.id, clone(run));
      this.prune();
      this.flush();
    } catch (cause) {
      this.runs.clear();
      for (const [id, stored] of previous) this.runs.set(id, stored);
      throw cause;
    }
  }

  private load(): void {
    if (!this.path) return;
    if (!existsSync(this.path)) {
      this.flush();
      return;
    }
    let parsed: unknown;
    try {
      const raw = readFileSync(this.path, 'utf8');
      if (Buffer.byteLength(raw) > MAX_STORE_BYTES) {
        throw new Error('run store exceeds the 256MB persistence limit');
      }
      parsed = JSON.parse(raw);
    } catch (cause) {
      throw new Error(
        `run store could not be loaded: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
    if (!Array.isArray(parsed) || !parsed.every(isStoredRun)) {
      throw new Error('run store contains invalid records');
    }
    let recovered = false;
    for (const item of parsed) {
      const run = clone(item);
      if (
        run.sessionId &&
        [...this.runs.values()].some((candidate) => candidate.sessionId === run.sessionId)
      ) {
        throw new Error('run store contains duplicate idempotency keys');
      }
      if (!TERMINAL.has(run.status)) {
        run.status = 'failed';
        run.error = 'gateway restarted before the run completed';
        run.costUsd = run.reservedMax;
        run.events.push({ type: 'run.failed', error: run.error });
        run.updatedAt = new Date().toISOString();
        recovered = true;
      }
      this.runs.set(run.id, run);
    }
    this.prune();
    if (recovered) this.flush();
  }

  private prune(): void {
    const ordered = [...this.runs.values()].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
    while (
      ordered.length > MAX_RUNS ||
      Buffer.byteLength(JSON.stringify(ordered)) > MAX_STORE_BYTES
    ) {
      const stale = ordered.pop();
      if (!stale) throw new Error('run store cannot satisfy its persistence limit');
      this.runs.delete(stale.id);
    }
  }

  private flush(): void {
    if (!this.path) return;
    const directoryPath = dirname(this.path);
    const temp = `${this.path}.${process.pid}.${randomUUID()}.tmp`;
    let fd: number | undefined;
    try {
      mkdirSync(directoryPath, { recursive: true });
      writeFileSync(temp, JSON.stringify([...this.runs.values()]), { mode: 0o600, flag: 'wx' });
      fd = openSync(temp, 'r');
      fsyncSync(fd);
      closeSync(fd);
      fd = undefined;
      renameSync(temp, this.path);
      const directory = openSync(directoryPath, 'r');
      try {
        fsyncSync(directory);
      } finally {
        closeSync(directory);
      }
      JSON.parse(readFileSync(this.path, 'utf8'));
    } catch (cause) {
      this.persistenceError = cause instanceof Error ? cause.message : String(cause);
      throw new Error(`run store persistence failed: ${this.persistenceError}`);
    } finally {
      if (fd !== undefined) closeSync(fd);
      try {
        unlinkSync(temp);
      } catch (cause) {
        const code = (cause as NodeJS.ErrnoException).code;
        if (code !== 'ENOENT' && code !== 'ENOTDIR') throw cause;
      }
    }
  }
}

function isStoredRun(value: unknown): value is StoredRun {
  if (!value || typeof value !== 'object') return false;
  const run = value as Partial<StoredRun>;
  const reservationValid =
    (run.reservationId === undefined && run.reservedMax === undefined) ||
    (typeof run.reservationId === 'string' &&
      run.reservationId.length > 0 &&
      typeof run.reservedMax === 'number' &&
      Number.isFinite(run.reservedMax) &&
      run.reservedMax > 0);
  const idempotencyValid =
    (run.sessionId === undefined && run.requestFingerprint === undefined) ||
    (typeof run.sessionId === 'string' &&
      /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(run.sessionId) &&
      typeof run.requestFingerprint === 'string' &&
      /^[a-f0-9]{64}$/.test(run.requestFingerprint));
  const receiptCount = Array.isArray(run.providerReceipts) ? run.providerReceipts.length : 0;
  const requestCount = run.providerRequestCount;
  return (
    typeof run.id === 'string' &&
    idempotencyValid &&
    reservationValid &&
    (TERMINAL.has(run.status as RunStatus) || run.reservationId !== undefined) &&
    typeof run.status === 'string' &&
    [
      'queued',
      'running',
      'waiting_for_approval',
      'stopping',
      'completed',
      'failed',
      'cancelled',
    ].includes(run.status) &&
    Array.isArray(run.events) &&
    (run.costUsd === undefined ||
      (typeof run.costUsd === 'number' && Number.isFinite(run.costUsd) && run.costUsd >= 0)) &&
    (run.providerReceipts === undefined ||
      (Array.isArray(run.providerReceipts) &&
        receiptCount <= 30 &&
        run.providerReceipts.every(isProviderReceipt))) &&
    (requestCount === undefined ||
      (Number.isSafeInteger(requestCount) &&
        requestCount >= 0 &&
        requestCount <= 30 &&
        receiptCount <= requestCount)) &&
    typeof run.updatedAt === 'string' &&
    Number.isFinite(Date.parse(run.updatedAt))
  );
}

export class IdempotencyConflictError extends Error {
  constructor() {
    super('session_id is already bound to a different request');
  }
}

function isProviderReceipt(value: unknown): value is ProviderReceipt {
  if (!value || typeof value !== 'object') return false;
  const receipt = value as Partial<ProviderReceipt>;
  return (
    typeof receipt.model === 'string' &&
    receipt.model.length > 0 &&
    receipt.route === 'marketplace' &&
    typeof receipt.balanceRemaining === 'string' &&
    /^\d{1,48}(?:\.\d{1,18})?$/.test(receipt.balanceRemaining) &&
    /[1-9]/.test(receipt.balanceRemaining) &&
    (receipt.providerId === undefined || typeof receipt.providerId === 'string') &&
    (receipt.requestId === undefined || typeof receipt.requestId === 'string') &&
    (receipt.costMicrounits === undefined || validCostMicrounits(receipt.costMicrounits))
  );
}

function clone<T>(value: T): T {
  return structuredClone(value);
}
