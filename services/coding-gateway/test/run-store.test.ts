import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { RunStore, type StoredRun } from '../src/run-store.js';

const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe('RunStore', () => {
  it('restores terminal artifacts after a process restart', () => {
    const path = storePath();
    const store = new RunStore(path);
    store.save(
      run({ status: 'completed', patch: 'diff', changedFiles: ['src/a.ts'], costUsd: 0.031 }),
    );

    expect(new RunStore(path).list()).toEqual([
      expect.objectContaining({
        id: 'run-1',
        status: 'completed',
        patch: 'diff',
        changedFiles: ['src/a.ts'],
        costUsd: 0.031,
      }),
    ]);
    expect(() => JSON.parse(readFileSync(path, 'utf8'))).not.toThrow();
  });

  it('turns an interrupted run into a durable failed receipt', () => {
    const path = storePath();
    new RunStore(path).save(run({ status: 'running' }));

    const [recovered] = new RunStore(path).list();
    expect(recovered).toMatchObject({
      status: 'failed',
      error: 'gateway restarted before the run completed',
    });
    expect(recovered?.events.at(-1)).toEqual({
      type: 'run.failed',
      error: 'gateway restarted before the run completed',
    });
    expect(new RunStore(path).list()[0]?.status).toBe('failed');
  });

  it('fails closed on a corrupt persistent store', () => {
    const path = storePath();
    writeFileSync(path, '{broken');
    expect(() => new RunStore(path)).toThrow('run store could not be loaded');
  });

  it('fails closed on an invalid persisted cost receipt', () => {
    const path = storePath();
    writeFileSync(path, JSON.stringify([run({ status: 'completed', costUsd: -1 })]));
    expect(() => new RunStore(path)).toThrow('invalid records');
  });

  it('rejects a single receipt that could exhaust the persistent disk', () => {
    const store = new RunStore(storePath());
    expect(() => store.save(run({ output: 'x'.repeat(8 * 1024 * 1024) }))).toThrow(
      '8MB persistence limit',
    );
  });
});

function storePath(): string {
  const directory = mkdtempSync(join(tmpdir(), 'mizuki-run-store-'));
  directories.push(directory);
  return join(directory, 'runs.json');
}

function run(patch: Partial<StoredRun>): StoredRun {
  return {
    id: 'run-1',
    status: 'failed',
    events: [],
    updatedAt: '2026-08-22T00:00:00.000Z',
    ...patch,
  };
}
