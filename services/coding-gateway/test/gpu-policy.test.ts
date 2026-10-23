import { describe, expect, it } from 'vitest';
import {
  gpuAvailable,
  parseGpuPolicy,
  smokeComputeConfig,
  SMOKE_GPU_POLICY,
} from '../src/gpu-policy.js';
import {
  parseRunRequest,
  providerBudgetUsd,
  runRequestFingerprint,
  computeChargedUsd,
} from '../src/server.js';

const cfg = {
  apiUrl: 'https://compute.test',
  apiToken: 'test',
  journalPath: '/data/compute.json',
  maxUsdcMicros: 500000,
  maxDurationSecs: 900,
  maxLaunches: 2,
};
const allowed = {
  backend: 'anthropic' as const,
  authToken: 'a'.repeat(32),
  compute: cfg,
  persistentRuns: true,
  recovered: true,
  env: { COMPUTE_GPU_ENABLED: '1', COMPUTE_DURABLE_STORAGE: '1', LEDGER_PATH: '/data/ledger.json' },
};
const request = {
  input: 'smoke',
  session_id: 'same-dispatch',
  max_cost_usd: 0.5,
  gpu: SMOKE_GPU_POLICY,
};

describe('GPU dispatch authorization', () => {
  it('requires exact one-booking policy and rejects unknown/malformed fields', () => {
    expect(parseGpuPolicy(undefined)).toBeUndefined();
    expect(parseGpuPolicy(SMOKE_GPU_POLICY)).toEqual(SMOKE_GPU_POLICY);
    for (const policy of [
      null,
      [],
      {},
      { ...SMOKE_GPU_POLICY, duration_secs: 301 },
      { ...SMOKE_GPU_POLICY, max_launches: 2 },
      { ...SMOKE_GPU_POLICY, max_usdc_micros: 200001 },
      { ...SMOKE_GPU_POLICY, another: true },
    ]) {
      expect(() => parseGpuPolicy(policy)).toThrow();
    }
  });
  it('fails closed without operator enablement, auth, journal, durable storage, supported backend and recovery', () => {
    expect(gpuAvailable(allowed)).toBe(true);
    expect(gpuAvailable({ ...allowed, env: {} })).toBe(false);
    expect(gpuAvailable({ ...allowed, authToken: undefined })).toBe(false);
    expect(gpuAvailable({ ...allowed, compute: { ...cfg, journalPath: undefined } })).toBe(false);
    expect(gpuAvailable({ ...allowed, persistentRuns: false })).toBe(false);
    expect(gpuAvailable({ ...allowed, recovered: false })).toBe(false);
    expect(gpuAvailable({ ...allowed, backend: 'usepod' })).toBe(false);
  });
  it('clamps broad operator settings to the smoke profile', () => {
    expect(smokeComputeConfig(cfg)).toMatchObject({
      maxUsdcMicros: 200000,
      maxDurationSecs: 300,
      maxLaunches: 1,
    });
  });
  it('reserves GPU budget before model/sandbox and binds policy to replay identity', () => {
    expect(parseRunRequest(request).ok).toBe(true);
    expect(parseRunRequest({ ...request, max_cost_usd: 0.2 }).ok).toBe(false);
    expect(providerBudgetUsd(0.5, 200000)).toBeCloseTo(0.18);
    expect(runRequestFingerprint(request)).not.toEqual(
      runRequestFingerprint({ ...request, gpu: undefined }),
    );
  });
  it('never treats an unsettled reservation as a final GPU charge', () => {
    expect(() =>
      computeChargedUsd({
        runKey: 'x',
        complete: false,
        settled: [],
        pendingJobIds: ['job'],
        pendingLaunchKeys: [],
      }),
    ).toThrow('pending');
    expect(
      computeChargedUsd({
        runKey: 'x',
        complete: true,
        pendingJobIds: [],
        pendingLaunchKeys: [],
        settled: [
          {
            job_id: 'job',
            status: 'cancelled',
            maximum_usdc_micros: 100000,
            receipt: { runtime_secs: 30, charged_usdc_micros: 10000, refunded_usdc_micros: 90000 },
          },
        ],
      }),
    ).toBe(0.01);
  });
});
