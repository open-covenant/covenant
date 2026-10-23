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
import { dirname, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

/**
 * Single-writer journal for the gateway mounted disk. Transactions are fully
 * synchronous (no event-loop interleaving), atomic, and fsynced before a paid
 * request. The service must not mount this file in multiple writer processes.
 */
export class ComputeJournal<T> {
  private memory: Record<string, T> = {};
  readonly path?: string;

  constructor(
    path: string | undefined,
    private readonly validate: (value: unknown) => value is T,
  ) {
    this.path = path ? resolve(path) : undefined;
  }

  entries(): Record<string, T> {
    if (!this.path || !existsSync(this.path)) return structuredClone(this.memory);
    const data = JSON.parse(readFileSync(this.path, 'utf8')) as {
      version?: unknown;
      runs?: unknown;
    };
    if (
      data?.version !== 1 ||
      !data.runs ||
      typeof data.runs !== 'object' ||
      Array.isArray(data.runs)
    ) {
      throw new Error('invalid GPU journal; refusing new bookings');
    }
    const runs = data.runs as Record<string, T>;
    if (Object.values(runs).some((value) => !this.validate(value))) {
      throw new Error('invalid GPU journal entry; refusing new bookings');
    }
    return runs;
  }

  update(key: string, change: (existing: T | undefined) => T): T {
    if (!this.path) {
      const value = change(structuredClone(this.memory[key]));
      this.memory[key] = structuredClone(value);
      return structuredClone(value);
    }
    mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const temp = `${this.path}.${randomUUID()}.tmp`;
    try {
      const runs = this.entries();
      const value = change(runs[key]);
      runs[key] = value;
      const fd = openSync(temp, 'wx', 0o600);
      try {
        writeFileSync(fd, JSON.stringify({ version: 1, runs }));
        fsyncSync(fd);
      } finally {
        closeSync(fd);
      }
      renameSync(temp, this.path);
      const dir = openSync(dirname(this.path), 'r');
      try {
        fsyncSync(dir);
      } finally {
        closeSync(dir);
      }
      return structuredClone(value);
    } finally {
      if (existsSync(temp)) unlinkSync(temp);
    }
  }
}
