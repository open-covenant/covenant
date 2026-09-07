import { describe, expect, it } from 'vitest';
import type { Config } from '../../src/core/config.js';
import type { LighterMarket } from '../../src/hedge/index.js';
import { LighterRhClient } from '../../src/hedge/lighter-rh.js';
import { chooseAction, HedgeSizer, truncateToStep } from '../../src/hedge/sizer.js';
import {
  AAPL_MARKET_ID,
  FULL_CREDENTIALS,
  fixtureConfig,
  fixtureFairValue,
  fixtureFetchJson,
  fixtureKeystore,
  fixtureLogger,
  fixtureTransport,
  NVDA_MARKET_ID,
  shortPosition,
  SPY_MARKET_ID,
  type FixtureState,
} from './fixtures.js';

const NVDA: LighterMarket = {
  marketId: NVDA_MARKET_ID,
  symbol: 'NVDA',
  kind: 'perp',
  sizeDecimals: 4,
  priceDecimals: 2,
  minBaseAmount: 0.04,
};
const AAPL: LighterMarket = { ...NVDA, marketId: AAPL_MARKET_ID, symbol: 'AAPL', minBaseAmount: 0.02 };
const SPY: LighterMarket = { ...NVDA, marketId: SPY_MARKET_ID, symbol: 'SPY', minBaseAmount: 0.01 };

function build(options: { credentials?: boolean; state?: FixtureState; config?: Config } = {}) {
  const config = options.config ?? fixtureConfig();
  const state: FixtureState = options.state ?? { positions: [], collateral: 5000 };
  const client = new LighterRhClient({
    config,
    logger: fixtureLogger(),
    keystore: fixtureKeystore(options.credentials === true ? FULL_CREDENTIALS : {}),
    transport: fixtureTransport(state),
    fetchJson: fixtureFetchJson,
  });
  const sizer = new HedgeSizer({
    config,
    logger: fixtureLogger(),
    fairvalue: fixtureFairValue({ NVDA: 231.5, AAPL: 258.4 }),
    client,
  });
  return { client, sizer, config };
}

describe('size rounding', () => {
  const { sizer } = build();

  it('truncates a NVDA size to four decimals', () => {
    expect(sizer.roundSize(4.334547, NVDA)).toBeCloseTo(4.3345, 6);
    expect(sizer.roundSize(4.33459999, NVDA)).toBeCloseTo(4.3345, 6);
  });

  it('never rounds a hedge up past the exposure it cancels', () => {
    expect(sizer.roundSize(1.99999, AAPL)).toBeCloseTo(1.9999, 6);
    expect(sizer.roundSize(0.5, SPY)).toBeCloseTo(0.5, 6);
  });

  it('keeps a size that already sits on a step', () => {
    expect(sizer.roundSize(0.04, NVDA)).toBeCloseTo(0.04, 6);
    expect(sizer.roundSize(1.4661, SPY)).toBeCloseTo(1.4661, 6);
  });

  it('lifts a size under the market floor to the floor', () => {
    expect(sizer.roundSize(0.031, NVDA)).toBeCloseTo(0.04, 6);
    expect(sizer.roundSize(0.015, AAPL)).toBeCloseTo(0.02, 6);
    expect(sizer.roundSize(0.004, SPY)).toBeCloseTo(0.01, 6);
  });

  it('leaves a zero size at zero', () => {
    expect(sizer.roundSize(0, NVDA)).toBe(0);
    expect(sizer.roundSize(0.00001, NVDA)).toBe(0);
  });

  it('takes the magnitude of a signed size', () => {
    expect(sizer.roundSize(-4.3345, NVDA)).toBeCloseTo(4.3345, 6);
  });

  it('rounds towards zero at every step size', () => {
    expect(truncateToStep(1234.56789, 4)).toBeCloseTo(1234.5678, 6);
    expect(truncateToStep(0.10422, 1)).toBeCloseTo(0.1, 6);
    expect(truncateToStep(-2.5, 0)).toBe(-2);
  });
});

describe('drift decisions', () => {
  const limit = 500;

  it('opens when nothing is held', () => {
    expect(chooseAction({ targetShort: 4.33, currentShort: 0, driftBps: 10_000, driftLimitBps: limit })).toBe('open');
  });

  it('holds inside the band', () => {
    expect(chooseAction({ targetShort: 4.33, currentShort: 4.2, driftBps: 300, driftLimitBps: limit })).toBe('none');
    expect(chooseAction({ targetShort: 4.33, currentShort: 4.5, driftBps: -392, driftLimitBps: limit })).toBe('none');
  });

  it('adds to the short when the target has grown past the band', () => {
    expect(chooseAction({ targetShort: 5, currentShort: 4, driftBps: 2000, driftLimitBps: limit })).toBe(
      'increaseShort',
    );
  });

  it('cuts the short when the target has shrunk past the band', () => {
    expect(chooseAction({ targetShort: 4, currentShort: 5, driftBps: -2500, driftLimitBps: limit })).toBe(
      'decreaseShort',
    );
  });

  it('unwinds when the stock leg is gone', () => {
    expect(chooseAction({ targetShort: 0, currentShort: 4.33, driftBps: -10_000, driftLimitBps: limit })).toBe(
      'unwind',
    );
  });

  it('does nothing when there is no leg and no short', () => {
    expect(chooseAction({ targetShort: 0, currentShort: 0, driftBps: 0, driftLimitBps: limit })).toBe('none');
  });

  it('sits exactly on the band without acting', () => {
    expect(chooseAction({ targetShort: 4, currentShort: 3.8, driftBps: 500, driftLimitBps: limit })).toBe('none');
    expect(chooseAction({ targetShort: 4, currentShort: 3.79, driftBps: 501, driftLimitBps: limit })).toBe(
      'increaseShort',
    );
  });
});

describe('stock leg sizing', () => {
  it('prices a paired position through its stock leg', async () => {
    const { sizer } = build();
    const leg = await sizer.stockLegForPaired({ qtyX: 10_000, ratio: 0.00043, stockSymbol: 'NVDA' });
    expect(leg.stockLegUsd.value).toBeCloseTo(995.45, 2);
    expect(leg.stockLegUsd.unit).toBe('USD');
    expect(leg.referencePrice.value).toBe(231.5);
  });

  it('prices a direct stock token holding', async () => {
    const { sizer } = build();
    const leg = await sizer.stockLegForToken({ symbol: 'aapl', qty: 3.5 });
    expect(leg.stockLegUsd.value).toBeCloseTo(904.4, 2);
  });

  it('names the sources it tried when no reference is available', async () => {
    const { sizer } = build();
    await expect(sizer.stockLegForToken({ symbol: 'HIMS', qty: 1 })).rejects.toThrow(/No reference price for HIMS/);
  });
});

describe('hedge plan', () => {
  it('sizes a short for a 1,000 USD NVDA leg', async () => {
    const { sizer } = build({ credentials: true });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });
    expect(plan.marketId).toBe(NVDA_MARKET_ID);
    expect(plan.referencePrice.value).toBeCloseTo(230.72, 2);
    expect(plan.targetShortBase.value).toBeCloseTo(4.334, 3);
    expect(plan.action).toBe('open');
    expect(plan.actionSizeBase?.value).toBeCloseTo(4.334, 3);
    expect(plan.fundingBps8h?.value).toBeCloseTo(0.32, 6);
    expect(plan.executable).toBe(true);
    expect(plan.reason).toBeUndefined();
  });

  it('holds when the short already sits inside the band', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.3, 229)], collateral: 5000 };
    const { sizer } = build({ credentials: true, state });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });
    expect(plan.action).toBe('none');
    expect(plan.executable).toBe(false);
    expect(plan.reason).toMatch(/within \d+ bps of target/);
  });

  it('adds to the short when the leg has grown', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 2, 229)], collateral: 5000 };
    const { sizer } = build({ credentials: true, state });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });
    expect(plan.action).toBe('increaseShort');
    expect(plan.actionSizeBase?.value).toBeCloseTo(2.334, 3);
    expect(plan.driftBps?.value).toBeGreaterThan(500);
  });

  it('unwinds when the stock leg has gone to zero', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    const { sizer } = build({ credentials: true, state });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 0 });
    expect(plan.action).toBe('unwind');
    expect(plan.actionSizeBase?.value).toBeCloseTo(4.33, 4);
  });

  it('refuses a leg smaller than the smallest order the venue takes', async () => {
    const { sizer } = build({ credentials: true });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 5 });
    expect(plan.executable).toBe(false);
    expect(plan.reason).toContain('0.0400');
    expect(plan.reason).toContain('0.0216');
  });

  it('says the desk is in dry run rather than that the plan is wrong', async () => {
    const { sizer } = build({ credentials: true, config: fixtureConfig({ live: false }) });
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });
    expect(plan.action).toBe('open');
    expect(plan.actionSizeBase?.value).toBeCloseTo(4.334, 3);
    expect(plan.executable).toBe(false);
    expect(plan.reason).toContain('dry run');
  });

  it('names the missing credential when the desk is live but has no key', async () => {
    const { sizer } = build();
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });
    expect(plan.executable).toBe(false);
    expect(plan.reason).toContain('DESK_LIGHTER_RH_PRIVATE_KEY');
  });

  it('refuses a symbol the venue does not list', async () => {
    const { sizer } = build({ credentials: true });
    await expect(sizer.plan({ symbol: 'HIMS', stockLegUsd: 1000 })).rejects.toThrow(/HIMS perpetual/);
  });
});
