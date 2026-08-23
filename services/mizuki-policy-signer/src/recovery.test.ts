import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  PROCESS_SHUTDOWN_DEADLINE_MS,
  RecoveryRunner,
  shutdownResources,
  waitForRecovery,
  waitForShutdown,
} from './recovery.js';

describe('recovery runner', () => {
  afterEach(() => vi.useRealTimers());

  it('keeps a stalled initial recovery in the background without overlapping it', async () => {
    let finish: (() => void) | undefined;
    const recover = vi.fn(
      (limit?: number) =>
        new Promise<void>((resolve) => {
          expect(limit).toBe(100);
          finish = resolve;
        }),
    );
    const runner = new RecoveryRunner(recover, vi.fn());

    const initial = runner.run(100);
    const interval = runner.run();
    expect(initial).toBe(interval);
    await vi.waitFor(() => expect(recover).toHaveBeenCalledOnce());
    expect(runner.active()).toBe(initial);

    finish?.();
    await initial;
    expect(runner.active()).toBeNull();
  });

  it('reports a failed recovery and permits the next scheduled attempt', async () => {
    const recover = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error('rpc unavailable'))
      .mockResolvedValueOnce();
    const onFailure = vi.fn();
    const runner = new RecoveryRunner(recover, onFailure);

    await expect(runner.run(100)).resolves.toBeUndefined();
    expect(onFailure).toHaveBeenCalledOnce();
    await expect(runner.run()).resolves.toBeUndefined();
    expect(recover).toHaveBeenCalledTimes(2);
  });

  it('leaves an already-bound health server responsive while recovery is stalled', async () => {
    const server = createServer((_request, response) => {
      response.writeHead(503, { 'content-type': 'application/json' });
      response.end('{"ok":false}');
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address() as AddressInfo;
    let finish: (() => void) | undefined;
    const runner = new RecoveryRunner(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
      vi.fn(),
    );

    const recovery = runner.run(100);
    try {
      const health = await fetch(`http://127.0.0.1:${address.port}/health`);
      expect(health.status).toBe(503);
      expect(await health.json()).toEqual({ ok: false });
    } finally {
      finish?.();
      await recovery;
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });

  it('bounds shutdown waiting without starting a second recovery', async () => {
    vi.useFakeTimers();
    const recover = vi.fn(() => new Promise<void>(() => undefined));
    const runner = new RecoveryRunner(recover, vi.fn());
    const active = runner.run(100);
    await Promise.resolve();
    expect(recover).toHaveBeenCalledOnce();

    const closeHttp = vi.fn(async (_force: boolean) => undefined);
    const closeStore = vi.fn(async () => undefined);
    const settled = shutdownResources(active, closeHttp, closeStore, 30_000);
    await vi.advanceTimersByTimeAsync(30_000);

    await expect(settled).resolves.toBe(false);
    expect(recover).toHaveBeenCalledOnce();
    expect(closeHttp).toHaveBeenCalledWith(true);
    expect(closeStore).not.toHaveBeenCalled();
  });

  it('closes the store only after recovery and HTTP requests have settled', async () => {
    let finishRecovery: (() => void) | undefined;
    const recovery = new Promise<void>((resolve) => {
      finishRecovery = resolve;
    });
    const order: string[] = [];
    const shutdown = shutdownResources(
      recovery,
      async (force) => {
        expect(force).toBe(false);
        order.push('http');
      },
      async () => {
        order.push('store');
      },
      30_000,
    );

    await Promise.resolve();
    expect(order).toEqual([]);
    finishRecovery?.();

    await expect(shutdown).resolves.toBe(true);
    expect(order).toEqual(['http', 'store']);
  });

  it('finishes recovery waiting immediately when no work is active', async () => {
    await expect(waitForRecovery(Promise.resolve(), 30_000)).resolves.toBe(true);
    await expect(waitForRecovery(null, 30_000)).resolves.toBe(true);
  });

  it('caps the whole shutdown below the platform termination window', async () => {
    vi.useFakeTimers();
    expect(PROCESS_SHUTDOWN_DEADLINE_MS).toBeLessThan(120_000);
    const finished = waitForShutdown(new Promise<boolean>(() => undefined));

    await vi.advanceTimersByTimeAsync(PROCESS_SHUTDOWN_DEADLINE_MS);

    await expect(finished).resolves.toBe(false);
  });

  it('preserves an unclean shutdown result', async () => {
    await expect(waitForShutdown(Promise.resolve(false))).resolves.toBe(false);
    await expect(waitForShutdown(Promise.resolve(true))).resolves.toBe(true);
  });
});
