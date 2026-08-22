import { describe, expect, it } from 'vitest';
import {
  assertCanFundImprovement,
  calculateTreasuryWaterfall,
  spendImprovementBudget,
} from './treasury.js';
import { DomainRuleError } from './state-machine.js';

describe('treasury waterfall', () => {
  it('funds the refund reserve before the operating reserve', () => {
    expect(
      calculateTreasuryWaterfall({
        liquidFundsCents: 4_000,
        settledUnfinishedLiabilitiesCents: 2_000,
        trailingThirtyDayOperatingCostsCents: 1_000,
      }),
    ).toMatchObject({
      refundReserveTargetCents: 5_000,
      operatingReserveTargetCents: 2_500,
      refundReserveCents: 4_000,
      operatingReserveCents: 0,
      capabilityFundCents: 0,
      researchFundCents: 0,
      refundReserveShortfallCents: 1_000,
      operatingReserveShortfallCents: 2_500,
      reservesSatisfied: false,
    });
  });

  it('uses actual liabilities and trailing costs when they exceed minimums', () => {
    expect(
      calculateTreasuryWaterfall({
        liquidFundsCents: 12_000,
        settledUnfinishedLiabilitiesCents: 7_000,
        trailingThirtyDayOperatingCostsCents: 4_000,
      }),
    ).toMatchObject({
      refundReserveTargetCents: 7_000,
      operatingReserveTargetCents: 4_000,
      refundReserveCents: 7_000,
      operatingReserveCents: 4_000,
      capabilityFundCents: 700,
      researchFundCents: 300,
      reservesSatisfied: true,
    });
  });

  it('allocates every surplus cent with a deterministic 70/30 split', () => {
    const allocation = calculateTreasuryWaterfall({
      liquidFundsCents: 7_511,
      settledUnfinishedLiabilitiesCents: 0,
      trailingThirtyDayOperatingCostsCents: 0,
    });
    expect(allocation).toMatchObject({
      refundReserveCents: 5_000,
      operatingReserveCents: 2_500,
      capabilityFundCents: 7,
      researchFundCents: 4,
    });
    expect(
      allocation.refundReserveCents +
        allocation.operatingReserveCents +
        allocation.capabilityFundCents +
        allocation.researchFundCents,
    ).toBe(allocation.liquidFundsCents);
  });

  it('blocks improvement spending until reserves and the retained budget cover it', () => {
    const underfunded = calculateTreasuryWaterfall({
      liquidFundsCents: 7_000,
      settledUnfinishedLiabilitiesCents: 0,
      trailingThirtyDayOperatingCostsCents: 0,
    });
    expect(() => assertCanFundImprovement(underfunded, 100)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'RESERVES_UNDERFUNDED' }),
    );

    const reserved = calculateTreasuryWaterfall({
      liquidFundsCents: 9_000,
      settledUnfinishedLiabilitiesCents: 0,
      trailingThirtyDayOperatingCostsCents: 0,
    });
    expect(() => assertCanFundImprovement(reserved, 1_051)).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'IMPROVEMENT_BUDGET_UNAVAILABLE',
      }),
    );
    expect(() => assertCanFundImprovement(reserved, 1_050)).not.toThrow();
  });

  it('spends only from liquid and capability balances', () => {
    const reserved = calculateTreasuryWaterfall({
      liquidFundsCents: 10_000,
      settledUnfinishedLiabilitiesCents: 0,
      trailingThirtyDayOperatingCostsCents: 0,
    });
    const after = spendImprovementBudget(reserved, 1_000);
    expect(after).toMatchObject({
      liquidFundsCents: 9_000,
      refundReserveCents: 5_000,
      operatingReserveCents: 2_500,
      capabilityFundCents: 750,
      researchFundCents: 750,
    });
  });

  it('rejects negative and fractional money values', () => {
    expect(() =>
      calculateTreasuryWaterfall({
        liquidFundsCents: -1,
        settledUnfinishedLiabilitiesCents: 0,
        trailingThirtyDayOperatingCostsCents: 0,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_MONEY' }));
    expect(() =>
      calculateTreasuryWaterfall({
        liquidFundsCents: 1.5,
        settledUnfinishedLiabilitiesCents: 0,
        trailingThirtyDayOperatingCostsCents: 0,
      }),
    ).toThrow();
  });

  it('keeps allocations exact near the largest supported balance', () => {
    const allocation = calculateTreasuryWaterfall({
      liquidFundsCents: Number.MAX_SAFE_INTEGER,
      settledUnfinishedLiabilitiesCents: 0,
      trailingThirtyDayOperatingCostsCents: 0,
    });
    expect(
      allocation.refundReserveCents +
        allocation.operatingReserveCents +
        allocation.capabilityFundCents +
        allocation.researchFundCents,
    ).toBe(Number.MAX_SAFE_INTEGER);
    expect(Number.isSafeInteger(allocation.capabilityFundCents)).toBe(true);
    expect(Number.isSafeInteger(allocation.researchFundCents)).toBe(true);
  });
});
