import { describe, expect, it, vi } from 'vitest';
import { runPredeploy } from './predeploy.js';
import type { OperatorControls } from './types.js';

describe('predeploy gate', () => {
  it('passes only when both durable controls are closed', async () => {
    const store = testStore(controls(false, false));

    await expect(runPredeploy({ connect: async () => store })).resolves.toBeUndefined();

    expect(store.close).toHaveBeenCalledOnce();
  });

  it('rejects deployment while paid intake is open', async () => {
    const store = testStore(controls(true, false));

    await expect(runPredeploy({ connect: async () => store })).rejects.toThrow(
      'admission must be closed before deployment',
    );

    expect(store.close).toHaveBeenCalledOnce();
  });

  it('rejects deployment while claims are open', async () => {
    const store = testStore(controls(false, true));

    await expect(runPredeploy({ connect: async () => store })).rejects.toThrow(
      'admission must be closed before deployment',
    );

    expect(store.close).toHaveBeenCalledOnce();
  });

  it('rejects deployment when durable controls cannot be read', async () => {
    const store = testStore(new Error('database unavailable'));

    await expect(runPredeploy({ connect: async () => store })).rejects.toThrow(
      'database unavailable',
    );

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
