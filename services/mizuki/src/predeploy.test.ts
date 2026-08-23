import { describe, expect, it, vi } from 'vitest';
import { runPredeploy } from './predeploy.js';
import type { OperatorControls } from './types.js';

describe('predeploy gate', () => {
  it('bypasses static and dynamic checks when both durable controls are closed', async () => {
    const store = testStore(controls(false, false));
    const assertStaticConfig = vi.fn();
    const checkReadiness = vi.fn(async () => ({ ready: false }));

    await expect(
      runPredeploy({ connect: async () => store, assertStaticConfig, checkReadiness }),
    ).resolves.toBeUndefined();

    expect(assertStaticConfig).not.toHaveBeenCalled();
    expect(checkReadiness).not.toHaveBeenCalled();
    expect(store.close).toHaveBeenCalledOnce();
  });

  it('passes an open deployment only after static and dynamic readiness succeed', async () => {
    const store = testStore(controls(true, false));
    const assertStaticConfig = vi.fn();
    const checkReadiness = vi.fn(async () => ({ ready: true }));

    await expect(
      runPredeploy({ connect: async () => store, assertStaticConfig, checkReadiness }),
    ).resolves.toBeUndefined();

    expect(assertStaticConfig).toHaveBeenCalledOnce();
    expect(checkReadiness).toHaveBeenCalledWith(store);
    expect(store.close).toHaveBeenCalledOnce();
  });

  it('rejects an open deployment when dynamic readiness is incomplete', async () => {
    const store = testStore(controls(false, true));
    const checkReadiness = vi.fn(async () => ({ ready: false }));

    await expect(
      runPredeploy({ connect: async () => store, assertStaticConfig: vi.fn(), checkReadiness }),
    ).rejects.toThrow('dependencies are not ready');

    expect(store.close).toHaveBeenCalledOnce();
  });

  it('rejects deployment when durable controls cannot be read', async () => {
    const store = testStore(new Error('database unavailable'));
    const assertStaticConfig = vi.fn();
    const checkReadiness = vi.fn(async () => ({ ready: true }));

    await expect(
      runPredeploy({ connect: async () => store, assertStaticConfig, checkReadiness }),
    ).rejects.toThrow('database unavailable');

    expect(assertStaticConfig).not.toHaveBeenCalled();
    expect(checkReadiness).not.toHaveBeenCalled();
    expect(store.close).toHaveBeenCalledOnce();
  });
});

function testStore(result: OperatorControls | Error) {
  return {
    operatorControls: vi.fn(async () => {
      if (result instanceof Error) throw result;
      return result;
    }),
    close: vi.fn(async () => {}),
  };
}

function controls(intakeEnabled: boolean, claimsEnabled: boolean): OperatorControls {
  return {
    intakeEnabled,
    claimsEnabled,
    revision: 1,
    reason: 'predeploy test state',
    updatedBy: 'test',
    updatedAt: '2026-08-23T00:00:00.000Z',
  };
}
