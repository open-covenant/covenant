import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import type { GpuCommandOutcome } from '../src/gpu-run.js';
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

  it('keeps interrupted GPU work stopping until its journal is reconciled', () => {
    const path = storePath();
    const gpu = { max_usdc_micros: 200000, duration_secs: 300, max_launches: 1 } as const;
    new RunStore(path).save(
      run({
        status: 'running',
        sessionId: 'gpu-session',
        requestFingerprint: 'b'.repeat(64),
        reservationId: 'r',
        reservedMax: 0.5,
        gpu,
      }),
    );
    const [restored] = new RunStore(path).list();
    expect(restored).toMatchObject({ status: 'stopping', desiredStatus: 'failed', costUsd: 0.5 });
    expect(restored?.events.some((e) => e.type === 'run.failed')).toBe(false);
    expect(new RunStore(path).findSession('gpu-session')?.status).toBe('stopping');
  });

  it.each(['pending', 'failed'] as const)(
    'preserves %s command evidence and existing financial fields across restart',
    (status) => {
      const path = storePath();
      const command = commandOutcome({
        status,
        ...(status === 'failed' ? { failure: 'execution_error' } : {}),
      });
      const stored = run({
        status: 'stopping',
        desiredStatus: 'completed',
        sessionId: 'gpu-session',
        requestFingerprint: 'b'.repeat(64),
        reservationId: 'r',
        reservedMax: 0.5,
        nonComputeUsd: 0.12045,
        gpu: { max_usdc_micros: 200000, duration_secs: 300, max_launches: 1 },
        gpuCommands: [command],
      });
      new RunStore(path).save(stored);
      expect(new RunStore(path).list()[0]).toMatchObject({
        status: 'stopping',
        desiredStatus: 'failed',
        reservedMax: 0.5,
        nonComputeUsd: 0.12045,
        gpuCommands: [command],
      });
    },
  );

  it.each([
    {
      status: 'succeeded',
      phase: 'complete',
      exit_code: null,
      timed_out: false,
      collected_outputs: ['report.json'],
    },
    { status: 'succeeded', phase: 'complete', exit_code: 0, timed_out: false },
    {
      status: 'succeeded',
      phase: 'complete',
      exit_code: 0,
      timed_out: true,
      collected_outputs: ['report.json'],
    },
    { status: 'failed', failure: 'invented_failure' },
    { stdout_tail: 'x'.repeat(2001) },
    { phase: 'invented_phase' },
    { timings: { readinessWaitMs: -1 } },
  ])('rejects invalid or fabricated workload evidence %j', (patch) => {
    const path = storePath();
    writeFileSync(
      path,
      JSON.stringify([
        run({
          sessionId: 'gpu-session',
          requestFingerprint: 'b'.repeat(64),
          gpu: { max_usdc_micros: 200000, duration_secs: 300, max_launches: 1 },
          gpuCommands: [{ ...commandOutcome(), ...patch } as GpuCommandOutcome],
        }),
      ]),
    );
    expect(() => new RunStore(path)).toThrow(/invalid records/);
  });

  it('rejects a forged receipt that does not balance the booked maximum', () => {
    const path = storePath();
    const gpu = { max_usdc_micros: 200000, duration_secs: 300, max_launches: 1 } as const;
    new RunStore(path).save(
      run({
        sessionId: 'gpu-session',
        requestFingerprint: 'b'.repeat(64),
        gpu,
        compute: {
          runKey: 'gpu-session',
          complete: true,
          pendingJobIds: [],
          pendingLaunchKeys: [],
          settled: [
            {
              job_id: 'job',
              status: 'cancelled',
              maximum_usdc_micros: 100000,
              receipt: {
                runtime_secs: 30,
                charged_usdc_micros: 10000,
                refunded_usdc_micros: 90001,
              },
            },
          ],
        },
      }),
    );
    expect(() => new RunStore(path)).toThrow('invalid records');
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

  it('retains only the latest 100 receipts', () => {
    const store = new RunStore();
    for (let index = 0; index < 101; index += 1) {
      store.save(
        run({
          id: `run-${index}`,
          updatedAt: new Date(Date.UTC(2026, 7, 22, 0, 0, index)).toISOString(),
        }),
      );
    }

    const receipts = store.list();
    expect(receipts).toHaveLength(100);
    expect(receipts.some((receipt) => receipt.id === 'run-0')).toBe(false);
    expect(receipts.some((receipt) => receipt.id === 'run-100')).toBe(true);
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

  it('retains a pinned non-GPU session through pruning and restart', () => {
    const path = storePath();
    const key = 'a'.repeat(64);
    const fingerprint = 'b'.repeat(64);
    const store = new RunStore(path, key);
    store.save(run({ id: 'pinned', sessionId: key, requestFingerprint: fingerprint }));
    for (let index = 0; index < 101; index += 1) {
      store.save(
        run({
          id: `other-${index}`,
          updatedAt: new Date(Date.UTC(2026, 7, 23, 0, 0, index)).toISOString(),
        }),
      );
    }
    expect(store.list()).toHaveLength(100);
    const restored = new RunStore(path, key);
    expect(restored.replay(key, fingerprint)).toMatchObject({ id: 'pinned', status: 'failed' });
    expect(() => restored.replay(key, 'c'.repeat(64))).toThrow(IdempotencyConflictError);
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

function commandOutcome(patch: Partial<GpuCommandOutcome> = {}): GpuCommandOutcome {
  return {
    id: '00000000-0000-0000-0000-000000000001',
    job_id: 'job-1',
    status: 'pending',
    phase: 'executing',
    exit_code: null,
    timed_out: null,
    requested_outputs: ['report.json'],
    collected_outputs: [],
    missing_outputs: [],
    oversized_outputs: [],
    failure: null,
    timings: {
      readinessWaitMs: 10,
      workspaceReadyWaitMs: 20,
      stagingMs: 30,
      commandBudgetMs: 250000,
      commandElapsedMs: null,
      collectionMs: null,
    },
    stdout_tail: '',
    stderr_tail: '',
    ...patch,
  };
}
