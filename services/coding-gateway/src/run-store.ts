import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import type { GatewayEvent, RunStatus, TokenUsage, ValidationResult } from './types.js';

export interface StoredRun {
  id: string;
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
  updatedAt: string;
}

const TERMINAL = new Set<RunStatus>(['completed', 'failed', 'cancelled']);
const MAX_RUNS = 500;
const MAX_RUN_BYTES = 8 * 1024 * 1024;
const MAX_STORE_BYTES = 256 * 1024 * 1024;

export class RunStore {
  private readonly runs = new Map<string, StoredRun>();

  constructor(private readonly path = process.env.RUN_STORE_PATH?.trim() || undefined) {
    this.load();
  }

  get persistent(): boolean {
    return Boolean(this.path);
  }

  list(): StoredRun[] {
    return [...this.runs.values()].map(clone);
  }

  save(run: StoredRun): void {
    if (Buffer.byteLength(JSON.stringify(run)) > MAX_RUN_BYTES) {
      throw new Error('run receipt exceeds the 8MB persistence limit');
    }
    const previous = new Map(this.runs);
    try {
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
    if (!this.path || !existsSync(this.path)) return;
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
      if (!TERMINAL.has(run.status)) {
        run.status = 'failed';
        run.error = 'gateway restarted before the run completed';
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
    mkdirSync(dirname(this.path), { recursive: true });
    const temp = `${this.path}.${process.pid}.tmp`;
    writeFileSync(temp, JSON.stringify([...this.runs.values()]), { mode: 0o600 });
    renameSync(temp, this.path);
  }
}

function isStoredRun(value: unknown): value is StoredRun {
  if (!value || typeof value !== 'object') return false;
  const run = value as Partial<StoredRun>;
  return (
    typeof run.id === 'string' &&
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
    typeof run.updatedAt === 'string' &&
    Number.isFinite(Date.parse(run.updatedAt))
  );
}

function clone<T>(value: T): T {
  return structuredClone(value);
}
