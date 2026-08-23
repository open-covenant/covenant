import { afterEach, describe, expect, it, vi } from 'vitest';
import { startupReadinessPasses } from './startup.js';

describe('startup readiness bound', () => {
  afterEach(() => vi.useRealTimers());

  it('passes only a healthy completed probe', async () => {
    await expect(startupReadinessPasses(async () => ({ healthy: true }), 1_000)).resolves.toBe(
      true,
    );
    await expect(startupReadinessPasses(async () => ({ healthy: false }), 1_000)).resolves.toBe(
      false,
    );
  });

  it('degrades instead of rejecting when a dependency probe fails', async () => {
    await expect(
      startupReadinessPasses(async () => {
        throw new Error('dependency unavailable');
      }, 1_000),
    ).resolves.toBe(false);
  });

  it('degrades after the bound when a dependency never settles', async () => {
    vi.useFakeTimers();
    const readiness = startupReadinessPasses(
      () => new Promise<{ healthy: boolean }>(() => undefined),
      1_000,
    );

    await vi.advanceTimersByTimeAsync(999);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(1);

    await expect(readiness).resolves.toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });
});
