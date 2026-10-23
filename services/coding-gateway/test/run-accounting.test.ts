import { describe, expect, it } from 'vitest';
import type { ComputeCleanupReport } from '../src/compute.js';
import {
  runAccounting,
  validAccountingBasis,
  type RunAccountingBasis,
} from '../src/run-accounting.js';

const prices = { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 };
const basis: RunAccountingBasis = {
  sandbox: {
    provider: 'e2b',
    attempted: true,
    maximumUsd: 0.12,
    usdPerSec: 0.0002,
    maximumSeconds: 600,
    listUsdPerSec: 0.00003025,
    tariffRef: 'https://evidence.example/e2b.json#sha256=abc',
    startedAtMs: 1_000_000,
    endedAtMs: 1_000_000 + 312_400,
  },
  model: { method: 'usage_estimate', estimatedUsd: 0.0322752, name: 'claude-sonnet-4-6', prices },
};
const usage = {
  inputTokens: 5,
  outputTokens: 625,
  cacheReadTokens: 3643,
  cacheCreationTokens: 2074,
};
// The receipt from the dispatch-04 smoke test.
const compute: ComputeCleanupReport = {
  runKey: 'k',
  complete: true,
  pendingJobIds: [],
  pendingLaunchKeys: [],
  settled: [
    {
      job_id: '2f5d013d-8c8e-4a6a-b255-269b748aa246',
      status: 'cancelled',
      maximum_usdc_micros: 33_951,
      receipt: {
        runtime_secs: 16,
        charged_usdc_micros: 1_811,
        refunded_usdc_micros: 32_140,
        provisioning_secs: 56,
        provisioning_usdc_micros: 6_225,
      },
    },
  ],
};

describe('run receipt', () => {
  it('reports charged amounts whose lines add up to the total', () => {
    const restored = JSON.parse(JSON.stringify(basis));
    expect(validAccountingBasis(restored)).toBe(true);
    const report = runAccounting({
      basis: restored,
      usage: { ...usage, extra: 'https://workspace.example/?token=secret' } as typeof usage,
      nonComputeUsd: 0.1522752,
      compute,
      conservativeTotalUsd: 0.1540862,
    });
    expect(report).toMatchObject({
      version: 2,
      final: true,
      gpu: { usd: 0.001811, basis: 'compute_receipt', runtimeSecs: 16 },
      model: {
        usd: 0.0322752,
        basis: 'usage_at_list_price',
        name: 'claude-sonnet-4-6',
        usdPerMillionTokens: prices,
        usage,
      },
      // 312.4 s rounds up to whole seconds at the list price, not the 600 s reservation.
      sandbox: {
        usd: 0.00946825,
        basis: 'metered_at_list_price',
        seconds: 313,
        usdPerSec: 0.00003025,
      },
      coveredByCovenant: { gpuProvisioningUsd: 0.006225, gpuProvisioningSecs: 56 },
    });
    expect(report.totalUsd).toBe(
      Number((report.gpu!.usd! + report.model.usd! + report.sandbox.usd!).toFixed(9)),
    );
    expect(report.totalUsd).toBe(0.04355445);
    expect(JSON.stringify(report)).not.toContain('secret');
  });

  it('falls back to the reservation when the sandbox end was never confirmed', () => {
    const unconfirmed = structuredClone(basis);
    delete unconfirmed.sandbox.endedAtMs;
    const report = runAccounting({
      basis: unconfirmed,
      usage,
      nonComputeUsd: 0.1522752,
      compute,
      conservativeTotalUsd: 0.1540862,
    });
    expect(report.final).toBe(false);
    expect(report.sandbox).toMatchObject({ usd: 0.12, basis: 'maximum_reservation', seconds: 600 });
    expect(report.totalUsd).toBe(0.1540862);
  });

  it('falls back to the reservation when no tariff was verified', () => {
    const unverified = structuredClone(basis);
    delete unverified.sandbox.listUsdPerSec;
    expect(
      runAccounting({
        basis: unverified,
        usage,
        nonComputeUsd: 0.15,
        compute,
        conservativeTotalUsd: 0.16,
      }).sandbox.basis,
    ).toBe('maximum_reservation');
  });

  it('holds the model reservation when usage is unknown', () => {
    const lost = structuredClone(basis);
    lost.model = { method: 'unknown' };
    const report = runAccounting({
      basis: lost,
      usage: undefined,
      nonComputeUsd: 0.3,
      compute,
      conservativeTotalUsd: 0.301811,
    });
    expect(report.final).toBe(false);
    expect(report.model).toEqual({ usd: 0.18, basis: 'conservative_hold', usage: null });
    expect(report.totalUsd).toBe(0.301811);
  });

  it('is not final while GPU cleanup is pending', () => {
    const report = runAccounting({
      basis,
      usage,
      nonComputeUsd: 0.15,
      compute: { ...compute, complete: false, pendingJobIds: ['j2'] },
      conservativeTotalUsd: 0.5,
    });
    expect(report.final).toBe(false);
    expect(report.totalUsd).toBe(0.5);
  });

  it('keeps legacy breakdown unknown instead of inferring a current sandbox tariff', () => {
    const report = runAccounting({
      basis: undefined,
      usage: undefined,
      nonComputeUsd: 0.1382604,
      compute,
      conservativeTotalUsd: 0.1722114,
    });
    expect(report).toMatchObject({
      final: false,
      totalUsd: 0.1722114,
      model: { usd: null, basis: 'unknown', usage: null },
      sandbox: { usd: null, basis: 'unknown', seconds: null, usdPerSec: null },
    });
  });

  it('does not charge a sandbox that was never started', () => {
    const beforeCreate = structuredClone(basis);
    beforeCreate.sandbox.attempted = false;
    delete beforeCreate.sandbox.startedAtMs;
    delete beforeCreate.sandbox.endedAtMs;
    beforeCreate.model = { method: 'no_requests' };
    const report = runAccounting({
      basis: beforeCreate,
      usage: undefined,
      nonComputeUsd: 0,
      compute: undefined,
      conservativeTotalUsd: 0,
    });
    expect(report).toMatchObject({
      final: true,
      totalUsd: 0,
      gpu: null,
      sandbox: { usd: 0, basis: 'not_started' },
    });
  });

  it.each([
    null,
    {},
    { ...basis, sandbox: { ...basis.sandbox, maximumUsd: -1 } },
    { ...basis, sandbox: { ...basis.sandbox, listUsdPerSec: -0.1 } },
    { ...basis, sandbox: { ...basis.sandbox, startedAtMs: 1.5 } },
    { ...basis, model: { method: 'invoice', estimatedUsd: 0 } },
    { ...basis, model: { method: 'usage_estimate', estimatedUsd: NaN } },
    { ...basis, model: { ...basis.model, prices: { ...prices, output: -15 } } },
  ])('rejects malformed persisted accounting basis: %j', (value) => {
    expect(validAccountingBasis(value)).toBe(false);
  });
});
