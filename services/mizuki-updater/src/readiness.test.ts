import { describe, expect, it, vi } from 'vitest';
import { createStableReadinessProbe } from './readiness.js';

describe('stable updater readiness', () => {
  it('retries one transient failure and caches the successful deep probe', async () => {
    let now = 1_000;
    const check = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error('transient failure'))
      .mockResolvedValue(undefined);
    const wait = vi.fn(async () => undefined);
    const readiness = createStableReadinessProbe(check, {
      successTtlMs: 60_000,
      retryDelayMs: 250,
      now: () => now,
      wait,
    });

    await readiness();
    await readiness();

    expect(check).toHaveBeenCalledTimes(2);
    expect(wait).toHaveBeenCalledWith(250);

    now += 60_001;
    await readiness();
    expect(check).toHaveBeenCalledTimes(3);
  });

  it('coalesces concurrent deep probes', async () => {
    let release: (() => void) | undefined;
    const check = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    const readiness = createStableReadinessProbe(check);

    const first = readiness();
    const second = readiness();
    expect(check).toHaveBeenCalledOnce();

    release?.();
    await Promise.all([first, second]);
  });

  it('expires the default cache at 30 seconds', async () => {
    let now = 10_000;
    const check = vi.fn(async () => undefined);
    const readiness = createStableReadinessProbe(check, { now: () => now });

    await readiness();
    now += 29_999;
    await readiness();
    expect(check).toHaveBeenCalledOnce();

    now += 1;
    await readiness();
    expect(check).toHaveBeenCalledTimes(2);
  });

  it('expires cached evidence when the clock moves backward', async () => {
    let now = 10_000;
    const check = vi.fn(async () => undefined);
    const readiness = createStableReadinessProbe(check, { now: () => now });

    await readiness();
    now -= 1;
    await readiness();
    expect(check).toHaveBeenCalledTimes(2);
  });

  it('fails closed after the retry and does not cache a failure', async () => {
    const check = vi.fn(async () => {
      throw new Error('dependency unavailable');
    });
    const readiness = createStableReadinessProbe(check, {
      wait: async () => undefined,
    });

    await expect(readiness()).rejects.toThrow('dependency unavailable');
    await expect(readiness()).rejects.toThrow('dependency unavailable');
    expect(check).toHaveBeenCalledTimes(4);
  });
});
