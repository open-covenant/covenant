import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { describe, expect, it, vi } from 'vitest';
import { RecoveryRunner } from './recovery.js';

describe('recovery runner', () => {
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
});
