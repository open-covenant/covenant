import { DomainRuleError, assertUsdCents } from './state-machine.js';

export const MIN_REFUND_RESERVE_CENTS = 5_000;
export const MIN_OPERATING_RESERVE_CENTS = 2_500;
export const CAPABILITY_ALLOCATION_PERCENT = 70;

export type TreasuryWaterfallInput = {
  liquidFundsCents: number;
  settledUnfinishedLiabilitiesCents: number;
  trailingThirtyDayOperatingCostsCents: number;
};

export type TreasuryWaterfall = {
  liquidFundsCents: number;
  refundReserveTargetCents: number;
  operatingReserveTargetCents: number;
  refundReserveCents: number;
  operatingReserveCents: number;
  capabilityFundCents: number;
  researchFundCents: number;
  refundReserveShortfallCents: number;
  operatingReserveShortfallCents: number;
  reservesSatisfied: boolean;
};

export function calculateTreasuryWaterfall(input: TreasuryWaterfallInput): TreasuryWaterfall {
  const liquidFundsCents = assertUsdCents(input.liquidFundsCents, 'liquid funds');
  const liabilities = assertUsdCents(
    input.settledUnfinishedLiabilitiesCents,
    'settled unfinished liabilities',
  );
  const operatingCosts = assertUsdCents(
    input.trailingThirtyDayOperatingCostsCents,
    'trailing operating costs',
  );
  const refundReserveTargetCents = Math.max(MIN_REFUND_RESERVE_CENTS, liabilities);
  const operatingReserveTargetCents = Math.max(MIN_OPERATING_RESERVE_CENTS, operatingCosts);

  const refundReserveCents = Math.min(liquidFundsCents, refundReserveTargetCents);
  const afterRefund = liquidFundsCents - refundReserveCents;
  const operatingReserveCents = Math.min(afterRefund, operatingReserveTargetCents);
  const surplus = afterRefund - operatingReserveCents;
  const capabilityFundCents =
    Math.floor(surplus / 100) * CAPABILITY_ALLOCATION_PERCENT +
    Math.floor(((surplus % 100) * CAPABILITY_ALLOCATION_PERCENT) / 100);
  const researchFundCents = surplus - capabilityFundCents;
  const refundReserveShortfallCents = refundReserveTargetCents - refundReserveCents;
  const operatingReserveShortfallCents = operatingReserveTargetCents - operatingReserveCents;

  return {
    liquidFundsCents,
    refundReserveTargetCents,
    operatingReserveTargetCents,
    refundReserveCents,
    operatingReserveCents,
    capabilityFundCents,
    researchFundCents,
    refundReserveShortfallCents,
    operatingReserveShortfallCents,
    reservesSatisfied: refundReserveShortfallCents === 0 && operatingReserveShortfallCents === 0,
  };
}

export function assertCanFundImprovement(
  waterfall: TreasuryWaterfall,
  improvementCents: number,
): void {
  const amount = assertUsdCents(improvementCents, 'improvement amount');
  if (amount === 0) {
    throw new DomainRuleError(
      'INVALID_IMPROVEMENT_AMOUNT',
      'Improvement amount must be greater than zero',
    );
  }
  if (!waterfall.reservesSatisfied) {
    throw new DomainRuleError('RESERVES_UNDERFUNDED', 'Required reserves are not fully funded');
  }
  if (waterfall.capabilityFundCents < amount) {
    throw new DomainRuleError(
      'IMPROVEMENT_BUDGET_UNAVAILABLE',
      'Retained improvement budget cannot cover the requested work',
    );
  }
}

export function spendImprovementBudget(
  waterfall: TreasuryWaterfall,
  improvementCents: number,
): TreasuryWaterfall {
  assertCanFundImprovement(waterfall, improvementCents);
  return {
    ...waterfall,
    liquidFundsCents: waterfall.liquidFundsCents - improvementCents,
    capabilityFundCents: waterfall.capabilityFundCents - improvementCents,
  };
}
