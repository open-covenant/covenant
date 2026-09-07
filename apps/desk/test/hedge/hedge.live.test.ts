/**
 * Reads against the live Lighter instance on Robinhood Chain.
 *
 * Runs only with `DESK_LIVE=1`. Every call here is a public read: no test in
 * this file holds a key, signs anything, or places an order.
 */

import { describe, expect, it } from 'vitest';
import { LighterRhClient } from '../../src/hedge/lighter-rh.js';
import { HedgeSizer } from '../../src/hedge/sizer.js';
import {
  AI_MARKET_ID,
  fixtureConfig,
  fixtureFairValue,
  fixtureKeystore,
  fixtureLogger,
  NVDA_MARKET_ID,
} from './fixtures.js';

const config = fixtureConfig();
const client = new LighterRhClient({
  config,
  logger: fixtureLogger(),
  keystore: fixtureKeystore(),
});
const sizer = new HedgeSizer({ config, logger: fixtureLogger(), fairvalue: fixtureFairValue({}), client });

describe('Lighter Robinhood Chain, live', () => {
  it('lists the markets the desk hedges on', async () => {
    const markets = await client.markets();
    expect(markets.length).toBeGreaterThan(50);

    const nvda = markets.find((market) => market.symbol === 'NVDA' && market.kind === 'perp');
    expect(nvda?.marketId).toBe(NVDA_MARKET_ID);
    expect(nvda?.sizeDecimals).toBe(4);
    expect(nvda?.priceDecimals).toBe(2);
    expect(nvda?.minBaseAmount).toBeGreaterThan(0);

    const ai = markets.find((market) => market.symbol === 'AI' && market.kind === 'perp');
    expect(ai?.marketId).toBe(AI_MARKET_ID);

    console.log(
      'markets: %d perpetuals, %d spot pairs',
      markets.filter((market) => market.kind === 'perp').length,
      markets.filter((market) => market.kind === 'spot').length,
    );
    console.log(
      'NVDA %o  AAPL %o  SPY %o',
      markets.find((market) => market.symbol === 'NVDA'),
      markets.find((market) => market.symbol === 'AAPL'),
      markets.find((market) => market.symbol === 'SPY'),
    );
  });

  it('reads the NVDA mark price and funding', async () => {
    const mark = await client.markPrice(NVDA_MARKET_ID);
    expect(mark.value).toBeGreaterThan(1);
    expect(mark.unit).toBe('USD');
    expect(mark.source).toBe('lighter-rh');

    const funding = await client.funding(NVDA_MARKET_ID);
    expect(funding.unit).toBe('bps');
    expect(Number.isFinite(funding.value)).toBe(true);

    console.log(
      'NVDA mark %s USD, funding %s bps over eight hours (%s)',
      mark.value.toFixed(2),
      funding.value.toFixed(4),
      funding.value >= 0 ? 'longs pay the short' : 'the short pays longs',
    );
  });

  it('sizes the NVDA short for a 1,000 USD AI position', async () => {
    const plan = await sizer.plan({ symbol: 'NVDA', stockLegUsd: 1000 });

    expect(plan.symbol).toBe('NVDA');
    expect(plan.marketId).toBe(NVDA_MARKET_ID);
    expect(plan.targetShortBase.value).toBeGreaterThan(0);
    expect(plan.action).toBe('open');

    console.log(
      'AI position 1,000 USD of NVDA exposure -> short %s NVDA at %s USD, action %s, executable %s',
      plan.targetShortBase.value.toFixed(4),
      plan.referencePrice.value.toFixed(2),
      plan.action,
      plan.executable,
    );
    if (plan.reason !== undefined) console.log('held back because: %s', plan.reason);
  });

  it('holds every write until an account exists on the instance', async () => {
    const ready = await client.canTrade();
    expect(ready.ok).toBe(false);
    expect(ready.reason).toContain('DESK_LIGHTER_RH_PRIVATE_KEY');

    const result = await client.placeOrder({
      marketId: NVDA_MARKET_ID,
      side: 'sell',
      sizeBase: 0.04,
      maxNotionalUsd: 10,
    });
    expect(result.sent).toBe(false);
    console.log('write refused: %s', result.reason);
  });

  it('reads no positions without a sub-account', async () => {
    expect(await client.positions()).toEqual([]);
  });
});
