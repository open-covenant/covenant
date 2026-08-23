import { describe, expect, it } from 'vitest';
import { IdempotencyConflictError, RunStore } from '../src/run-store.js';
import {
  failedRunCost,
  maximumSandboxCostUsd,
  parseRunRequest,
  providerBudgetUsd,
  runRequestFingerprint,
  sandboxAccountingChargeUsd,
} from '../src/server.js';
import type { RunRequest } from '../src/types.js';
import { SpendLedger } from '../src/budget.js';
import { config } from '../src/config.js';

const request: RunRequest = {
  input: 'fix docs',
  session_id: 'job-1:implementation',
  max_cost_usd: 0.5,
};

describe('run spend cap contract', () => {
  it('requires a finite positive cap within the gateway ceiling', () => {
    expect(parseRunRequest(request)).toMatchObject({ ok: true });
    for (const max_cost_usd of [undefined, Number.NaN, Number.POSITIVE_INFINITY, 0, -1, 2.01]) {
      expect(parseRunRequest({ ...request, max_cost_usd })).toMatchObject({ ok: false });
    }
    expect(parseRunRequest({ ...request, max_cost_usd: 0.01 })).toMatchObject({
      ok: false,
      error: expect.stringMatching(/sandbox charge/),
    });
  });

  it('binds the cap into the idempotency fingerprint', () => {
    const first = runRequestFingerprint(request);
    const changed = runRequestFingerprint({ ...request, max_cost_usd: 0.6 });
    expect(changed).not.toBe(first);

    const store = new RunStore();
    store.save({
      id: 'run-1',
      sessionId: request.session_id,
      requestFingerprint: first,
      status: 'completed',
      events: [],
      updatedAt: new Date().toISOString(),
    });
    expect(store.replay(request.session_id, first)?.id).toBe('run-1');
    expect(() => store.replay(request.session_id, changed)).toThrow(IdempotencyConflictError);
  });

  it('reserves the full pinned sandbox ceiling before any model spend', () => {
    const sandboxMax = maximumSandboxCostUsd();
    const providerMax = providerBudgetUsd(request.max_cost_usd);

    expect(sandboxMax).toBeGreaterThan(0);
    expect(providerMax).toBeGreaterThan(0);
    expect(providerMax + sandboxMax).toBeCloseTo(request.max_cost_usd);
  });

  it('charges the sandbox maximum after any attempted create', () => {
    const sandboxMax = maximumSandboxCostUsd();

    expect(sandboxAccountingChargeUsd(false)).toBe(0);
    expect(sandboxAccountingChargeUsd(true)).toBe(sandboxMax);
  });

  it('uses durable receipt totals on failure and kills on a visible overrun', () => {
    const receipt = (accountedCostMicrounits?: string) => ({
      model: config.model,
      route: 'marketplace' as const,
      balanceRemaining: '1000000',
      ...(accountedCostMicrounits === undefined
        ? {}
        : {
            providerReportedCostMicrounits: accountedCostMicrounits,
            accounting: {
              accountedCostMicrounits,
              basis: 'max-of-configured-price-ceilings-and-provider-report' as const,
              inputTokens: 0,
              outputTokens: 0,
              inputPriceMicrounitsPerMillion: config.usePodMaxInputPriceMicrounits,
              outputPriceMicrounitsPerMillion: config.usePodMaxOutputPriceMicrounits,
            },
          }),
    });
    expect(failedRunCost(2, [receipt('100'), receipt('200')], 0.01, 0.5)).toBe(0.0103);
    expect(failedRunCost(2, [receipt('100'), receipt()], 0.01, 0.5)).toBe(0.5);
    expect(failedRunCost(1, [receipt('100'), receipt('200')], 0.01, 0.5)).toBe(0.5);
    expect(failedRunCost(0, [receipt('100')], 0.01, 0.5)).toBe(0.5);

    const actual = failedRunCost(1, [receipt('600000')], 0.01, 0.5);
    const ledger = new SpendLedger({
      dailyUsd: 10,
      monthlyUsd: 100,
      perRunUsdMax: 2,
      maxConcurrent: 1,
      wallMs: 60_000,
    });
    const reservation = ledger.reserve(0.5);
    expect(reservation.ok).toBe(true);
    if (!reservation.ok) return;
    ledger.commit(reservation.id, reservation.max, actual, 'failed');
    expect(ledger.snapshot()).toMatchObject({ dailyUsd: 0.61, killed: true });
  });
});
