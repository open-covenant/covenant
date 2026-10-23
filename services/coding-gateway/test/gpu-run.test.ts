import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ArtifactStore } from '../src/artifacts.js';
import type { ComputeSession } from '../src/compute.js';
import {
  GpuRunner,
  GpuWorkloadError,
  gpuWorkloadSummary,
  isGpuCommandOutcome,
  isGpuOutput,
  MAX_GPU_COMMANDS,
  type GpuCommandOutcome,
  type GpuOutput,
} from '../src/gpu-run.js';
import type { Sandbox } from '../src/types.js';

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

const NOW = 1_000_000;
const ACCESS_URL = `https://203.0.113.7:41234/lab?token=${'t'.repeat(32)}`;

function setup(
  options: {
    readySinceMs?: number;
    runDeadlineMs?: number;
    recorded?: number;
    files?: Record<string, string>;
    commands?: number;
    failRecord?: boolean;
    failOutputRecord?: boolean;
  } = {},
) {
  const dir = mkdtempSync(join(tmpdir(), 'gpu-run-'));
  directories.push(dir);
  const store = new ArtifactStore({
    dir,
    publicBaseUrl: 'https://connector.example/artifacts',
    ttlMs: 3_600_000,
    maxTotalBytes: 1_000_000,
  });
  const awaitRunning = vi.fn(async () => ({
    accessUrl: ACCESS_URL,
    readySinceMs: options.readySinceMs ?? NOW,
    durationSecs: 300,
  }));
  const recorded: GpuOutput[] = [];
  const commands: GpuCommandOutcome[] = [];
  const recordCommand = vi.fn((outcome: GpuCommandOutcome) => {
    expect(isGpuCommandOutcome(outcome)).toBe(true);
    const index = commands.findIndex((previous) => previous.id === outcome.id);
    if (index < 0) commands.push(structuredClone(outcome));
    else commands[index] = structuredClone(outcome);
    if (options.failRecord) throw new Error('cannot persist command');
  });
  const execute = vi.fn(async (code: string) => ({
    ok: true,
    stdout: code.includes('covenant-run.py')
      ? '__covenant__{"pid":1}'
      : `__covenant__${JSON.stringify({ stdout: 'ok', stderr: '', sizes: { 'report.json': 13 } })}`,
  }));
  const client = {
    waitUntilServing: vi.fn(async () => {}),
    makeDirectory: vi.fn(async () => {}),
    upload: vi.fn(async () => {}),
    download: vi.fn(async (path: string) =>
      path.endsWith('covenant-exit.json')
        ? Buffer.from('{"exit_code":0,"duration_ms":900}')
        : Buffer.from('{"rows":1000}'),
    ),
    execute,
    close: vi.fn(),
  };
  const connect = vi.fn((url: string) => {
    expect(url).toBe(ACCESS_URL);
    return client;
  });
  const runner = new GpuRunner({
    compute: { awaitRunning } as unknown as ComputeSession,
    store,
    signal: new AbortController().signal,
    runDeadlineMs: options.runDeadlineMs ?? NOW + 600_000,
    recorded: () => (options.recorded ?? 0) + recorded.length,
    record: (outputs) => {
      recorded.push(...outputs);
      if (options.failOutputRecord) throw new Error('cannot persist outputs');
    },
    commands: () =>
      options.commands ? Array.from({ length: options.commands }, () => commands[0]!) : commands,
    recordCommand,
    connect,
    now: () => NOW,
    pollMs: 1,
  });
  const sandbox = {
    readFile: vi.fn(async (path: string) => options.files?.[path] ?? ''),
    readBytes: vi.fn(async (path: string) => Buffer.from(options.files?.[path] ?? '')),
  } as unknown as Sandbox;
  return {
    runner,
    sandbox,
    client,
    connect,
    awaitRunning,
    recorded,
    commands,
    recordCommand,
  };
}

const run = (overrides: Record<string, unknown> = {}) => ({
  job_id: 'job-1',
  command: 'python3 embed.py',
  files: ['embed.py'],
  outputs: ['report.json'],
  ...overrides,
});

describe('GpuRunner', () => {
  it('runs on the launched GPU, stores outputs and records them for the result', async () => {
    const t = setup({ files: { 'embed.py': 'print(1)' } });
    const result = JSON.parse(await t.runner.run(run(), t.sandbox));
    expect(result).toMatchObject({
      job_id: 'job-1',
      exit_code: 0,
      timed_out: false,
      duration_secs: 0.9,
      outputs: [{ path: 'report.json', bytes: 13, text: '{"rows":1000}' }],
      missing: [],
    });
    expect(t.commands).toHaveLength(1);
    expect(t.commands[0]).toMatchObject({
      status: 'succeeded',
      phase: 'complete',
      exit_code: 0,
      timed_out: false,
      failure: null,
      collected_outputs: ['report.json'],
      timings: { readinessWaitMs: 0, commandBudgetMs: 255_000 },
    });
    expect(t.recordCommand.mock.calls[0]![0].status).toBe('pending');
    expect(t.recorded).toHaveLength(1);
    expect(isGpuOutput(t.recorded[0])).toBe(true);
    expect(t.recorded[0]!.url).toMatch(
      /^https:\/\/connector\.example\/artifacts\/[A-Za-z0-9_-]{32}\/report\.json$/,
    );
    // The workspace address never reaches the model.
    expect(JSON.stringify(result)).not.toContain('token');
    expect(t.client.close).toHaveBeenCalledOnce();
  });

  it('fits the command inside the booking, ahead of the margin Compute needs', async () => {
    // Ready 250 s ago on a 300 s booking: 50 s left, minus the 45 s margin.
    const t = setup({ readySinceMs: NOW - 250_000 });
    await expect(t.runner.run(run({ files: [] }), t.sandbox)).rejects.toThrow(
      /GPU workload did not succeed/,
    );
    expect(t.connect).not.toHaveBeenCalled();
  });

  it('never lets a command outlive the run itself', async () => {
    const t = setup({ runDeadlineMs: NOW + 60_000 });
    await expect(t.runner.run(run({ files: [] }), t.sandbox)).rejects.toThrow(
      /GPU workload did not succeed/,
    );
  });

  it.each([
    [{ job_id: '' }, /job_id is required/],
    [{ command: ' ' }, /command must be/],
    [{ command: 'x'.repeat(16 * 1024 + 1) }, /command must be/],
    [{ outputs: ['../secret'] }, /relative path/],
    [{ outputs: ['a/r.json', 'b/r.json'] }, /distinct file names/],
    [{ outputs: Array.from({ length: 9 }, (_, i) => `o${i}`) }, /at most 8 paths/],
    [{ files: ['.env'] }, /relative path/],
  ])('rejects %j before touching the GPU', async (overrides, error) => {
    const t = setup();
    await expect(t.runner.run(run(overrides), t.sandbox)).rejects.toThrow(error);
    expect(t.awaitRunning).not.toHaveBeenCalled();
  });

  it('caps outputs across every command in the run', async () => {
    const t = setup({ recorded: 8 });
    await expect(t.runner.run(run({ files: [] }), t.sandbox)).rejects.toThrow(/at most 8 output/);
  });

  it('persists pending evidence before remote execution and retains the last phase on failure', async () => {
    const t = setup();
    t.client.execute.mockImplementation(async () => {
      expect(t.commands[0]).toMatchObject({ status: 'pending', phase: 'executing' });
      throw new Error(`transport rejected ${ACCESS_URL}`);
    });
    const error = await t.runner.run(run(), t.sandbox).catch((error: unknown) => error);
    expect(error).toBeInstanceOf(GpuWorkloadError);
    expect((error as GpuWorkloadError).toolResult).not.toContain(ACCESS_URL);
    expect(t.commands[0]).toMatchObject({
      status: 'failed',
      phase: 'executing',
      failure: 'execution_error',
      timings: { readinessWaitMs: 0, workspaceReadyWaitMs: 0, stagingMs: 0 },
    });
    expect(t.client.close).toHaveBeenCalledOnce();
  });

  it('fails closed if artifact persistence fails after a successful remote command', async () => {
    const t = setup({ failOutputRecord: true });
    await expect(t.runner.run(run(), t.sandbox)).rejects.toBeInstanceOf(GpuWorkloadError);
    expect(t.commands[0]).toMatchObject({
      status: 'failed',
      phase: 'complete',
      failure: 'execution_error',
      exit_code: 0,
      timed_out: false,
      stdout_tail: 'ok',
      timings: { commandElapsedMs: 0 },
    });
    expect(gpuWorkloadSummary(t.commands).status).toBe('failed');
  });

  it('does not execute if pending evidence could not be persisted', async () => {
    const t = setup({ failRecord: true });
    await expect(t.runner.run(run(), t.sandbox)).rejects.toThrow('cannot persist command');
    expect(t.awaitRunning).not.toHaveBeenCalled();
    expect(t.commands[0]?.status).toBe('pending');
    expect(gpuWorkloadSummary(t.commands).status).toBe('failed');
  });

  it('redacts workspace credentials from bounded durable and tool log tails', async () => {
    const t = setup();
    t.client.execute.mockImplementation(async (code) => ({
      ok: true,
      stdout: code.includes('covenant-run.py')
        ? '__covenant__{"pid":1}'
        : `__covenant__${JSON.stringify({ stdout: 'x'.repeat(3_000) + ACCESS_URL, stderr: 't'.repeat(32), sizes: { 'report.json': 13 } })}`,
    }));
    const result = JSON.parse(await t.runner.run(run(), t.sandbox));
    expect(result.stdout_tail).toHaveLength(2_000);
    expect(result.stdout_tail).not.toContain(ACCESS_URL);
    expect(result.stderr_tail).not.toContain('t'.repeat(32));
    expect(t.commands[0]?.stdout_tail).toBe(result.stdout_tail);
    expect(t.commands[0]?.stderr_tail).toBe(result.stderr_tail);
  });

  it('bounds durable command history before another provider command', async () => {
    const t = setup({ commands: MAX_GPU_COMMANDS });
    await expect(t.runner.run(run(), t.sandbox)).rejects.toThrow(/at most 32 GPU commands/);
    expect(t.awaitRunning).not.toHaveBeenCalled();
  });

  it('keeps an earlier failed attempt visible after a later successful command', async () => {
    const t = setup();
    t.client.download.mockImplementationOnce(async () =>
      Buffer.from('{"exit_code":2,"duration_ms":900}'),
    );
    await expect(t.runner.run(run(), t.sandbox)).rejects.toBeInstanceOf(GpuWorkloadError);
    await t.runner.run(run(), t.sandbox);
    expect(t.commands.map((command) => command.status)).toEqual(['failed', 'succeeded']);
    expect(gpuWorkloadSummary(t.commands)).toMatchObject({
      status: 'failed',
      commands: 2,
      failed: 1,
      failures: ['nonzero_exit'],
    });
  });

  it('limits the size of files copied to the GPU', async () => {
    const t = setup({ files: { 'big.bin': 'x'.repeat(4 * 1024 * 1024 + 1) } });
    await expect(t.runner.run(run({ files: ['big.bin'] }), t.sandbox)).rejects.toThrow(
      /files may total/,
    );
  });
});
