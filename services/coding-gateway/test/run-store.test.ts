import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { IdempotencyConflictError, RunStore, type StoredRun } from '../src/run-store.js';

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
      run({
        status: 'completed',
        patch: 'diff',
        changedFiles: ['src/a.ts'],
        costUsd: 0.031,
        providerReceipts: [
          {
            model: 'deepseek-v3.2',
            route: 'marketplace',
            balanceRemaining: '5000000',
            providerReportedCostMicrounits: '31000',
            accounting: {
              accountedCostMicrounits: '31000',
              basis: 'max-of-configured-price-ceilings-and-provider-report',
              inputTokens: 100,
              outputTokens: 20,
              inputPriceMicrounitsPerMillion: 200000,
              outputPriceMicrounitsPerMillion: 400000,
            },
          },
        ],
      }),
    );

    expect(new RunStore(path).list()).toEqual([
      expect.objectContaining({
        id: 'run-1',
        status: 'completed',
        patch: 'diff',
        changedFiles: ['src/a.ts'],
        costUsd: 0.031,
        providerReceipts: [expect.objectContaining({ route: 'marketplace' })],
      }),
    ]);
    expect(() => JSON.parse(readFileSync(path, 'utf8'))).not.toThrow();
  });

  it('turns an interrupted run into a durable failed receipt', () => {
    const path = storePath();
    new RunStore(path).save(
      run({ status: 'running', reservationId: 'reservation-1', reservedMax: 2 }),
    );

    const [recovered] = new RunStore(path).list();
    expect(recovered).toMatchObject({
      status: 'failed',
      error: 'gateway restarted before the run completed',
      costUsd: 2,
    });
    expect(recovered?.events.at(-1)).toEqual({
      type: 'run.failed',
      error: 'gateway restarted before the run completed',
    });
    expect(new RunStore(path).list()[0]?.status).toBe('failed');
  });

  it('replays a lost creation response from the durable session binding after restart', () => {
    const path = storePath();
    const fingerprint = 'b'.repeat(64);
    new RunStore(path).save(
      run({
        id: 'run-replay',
        sessionId: 'job-1:implementation',
        requestFingerprint: fingerprint,
        status: 'running',
        reservationId: 'reservation-1',
        reservedMax: 2,
      }),
    );

    expect(new RunStore(path).replay('job-1:implementation', fingerprint)).toMatchObject({
      id: 'run-replay',
      status: 'failed',
    });
  });

  it('rejects a session key replay with a different request fingerprint', () => {
    const store = new RunStore(storePath());
    store.save(
      run({
        sessionId: 'job-1:implementation',
        requestFingerprint: 'b'.repeat(64),
      }),
    );

    expect(() => store.replay('job-1:implementation', 'c'.repeat(64))).toThrow(
      IdempotencyConflictError,
    );
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

  it('fails closed on an invalid provider receipt', () => {
    const path = storePath();
    writeFileSync(
      path,
      JSON.stringify([
        run({
          status: 'completed',
          providerReceipts: [
            {
              model: 'deepseek-v3.2',
              route: 'marketplace',
              balanceRemaining: '0',
            },
          ],
        }),
      ]),
    );
    expect(() => new RunStore(path)).toThrow('invalid records');
  });

  it('loads legacy provider receipts but requires complete accounting on new receipts', () => {
    const legacyPath = storePath();
    writeFileSync(
      legacyPath,
      JSON.stringify([
        run({
          status: 'completed',
          providerReceipts: [
            {
              model: 'deepseek-v3.2',
              route: 'marketplace',
              balanceRemaining: '5000000',
              costMicrounits: '31000',
            },
          ],
        }),
      ]),
    );
    expect(new RunStore(legacyPath).list()[0]?.providerReceipts).toHaveLength(1);

    const invalidPath = storePath();
    writeFileSync(
      invalidPath,
      JSON.stringify([
        run({
          status: 'completed',
          providerReceipts: [
            {
              model: 'deepseek-v3.2',
              route: 'marketplace',
              balanceRemaining: '5000000',
              providerReportedCostMicrounits: '10',
              accounting: {
                accountedCostMicrounits: '1',
                basis: 'max-of-configured-price-ceilings-and-provider-report',
                inputTokens: 10,
                outputTokens: 2,
                inputPriceMicrounitsPerMillion: 200000,
                outputPriceMicrounitsPerMillion: 400000,
              },
            },
          ],
        }),
      ]),
    );
    expect(() => new RunStore(invalidPath)).toThrow('invalid records');

    const wrongTypePath = storePath();
    writeFileSync(
      wrongTypePath,
      JSON.stringify([
        run({
          status: 'completed',
          providerReceipts: [
            {
              model: 'deepseek-v3.2',
              route: 'marketplace',
              balanceRemaining: '5000000',
              accounting: {
                accountedCostMicrounits: 3 as unknown as string,
                basis: 'configured-price-ceilings',
                inputTokens: 10,
                outputTokens: 2,
                inputPriceMicrounitsPerMillion: 200000,
                outputPriceMicrounitsPerMillion: 400000,
              },
            },
          ],
        }),
      ]),
    );
    expect(() => new RunStore(wrongTypePath)).toThrow('invalid records');
  });

  it('rejects a single receipt that could exhaust the persistent disk', () => {
    const store = new RunStore(storePath());
    expect(() => store.save(run({ output: 'x'.repeat(8 * 1024 * 1024) }))).toThrow(
      '8MB persistence limit',
    );
  });

  it('marks persistence unhealthy after a runtime write failure', () => {
    const directory = mkdtempSync(join(tmpdir(), 'mizuki-run-store-fail-'));
    directories.push(directory);
    const path = join(directory, 'runs.json');
    const store = new RunStore(path);
    rmSync(path, { force: true });
    rmSync(directory, { recursive: true, force: true });
    writeFileSync(directory, 'not a directory');
    expect(() => store.save(run({ status: 'completed' }))).toThrow(/persistence failed/);
    expect(store.persistenceReady).toBe(false);
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
