import { describe, expect, it } from 'vitest';
import { LighterRhClient, LIGHTER_RH_CHAIN_ID } from '../../src/hedge/lighter-rh.js';
import {
  AAPL_MARKET_ID,
  FULL_CREDENTIALS,
  fixtureConfig,
  fixtureFetchJson,
  fixtureKeystore,
  fixtureLogger,
  fixtureTransport,
  NVDA_MARKET_ID,
  shortPosition,
  type FixtureState,
} from './fixtures.js';

function client(options: { credentials?: boolean; state?: FixtureState; config?: ReturnType<typeof fixtureConfig> } = {}) {
  const state: FixtureState = options.state ?? { positions: [], collateral: 5000 };
  return new LighterRhClient({
    config: options.config ?? fixtureConfig(),
    logger: fixtureLogger(),
    keystore: fixtureKeystore(options.credentials === true ? FULL_CREDENTIALS : {}),
    transport: fixtureTransport(state),
    fetchJson: fixtureFetchJson,
  });
}

describe('LighterRhClient reads', () => {
  it('lists perpetuals and spot pairs with their step sizes', async () => {
    const markets = await client().markets();
    const nvda = markets.find((market) => market.symbol === 'NVDA');
    expect(nvda).toEqual({
      marketId: NVDA_MARKET_ID,
      symbol: 'NVDA',
      kind: 'perp',
      sizeDecimals: 4,
      priceDecimals: 2,
      minBaseAmount: 0.04,
      initialMarginFraction: 5000,
    });
    expect(markets.find((market) => market.symbol === 'AAPL/USDG')?.kind).toBe('spot');
  });

  it('finds a perpetual by ticker and ignores the spot pair of the same name', async () => {
    const perp = await client().perpFor('aapl');
    expect(perp?.marketId).toBe(AAPL_MARKET_ID);
    expect(perp?.kind).toBe('perp');
  });

  it('reads a mark price in USD', async () => {
    const mark = await client().markPrice(NVDA_MARKET_ID);
    expect(mark.value).toBeCloseTo(230.72, 2);
    expect(mark.unit).toBe('USD');
    expect(mark.source).toBe('lighter-rh');
  });

  it('reports funding in basis points over eight hours, positive when longs pay', async () => {
    const nvda = await client().funding(NVDA_MARKET_ID);
    expect(nvda.value).toBeCloseTo(0.32, 6);
    expect(nvda.unit).toBe('bps');

    const aapl = await client().funding(AAPL_MARKET_ID);
    expect(aapl.value).toBeCloseTo(-1, 6);
  });

  it('takes the venue rate rather than another exchange quoted on the same route', async () => {
    const funding = await client().funding(NVDA_MARKET_ID);
    expect(funding.value).not.toBeCloseTo(0.64, 6);
  });

  it('returns no positions when no sub-account is configured', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    expect(await client({ state }).positions()).toEqual([]);
  });

  it('signs positions negative for a short and prices them at the mark', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    const positions = await client({ credentials: true, state }).positions();
    expect(positions).toHaveLength(1);
    expect(positions[0]?.sizeBase.value).toBeCloseTo(-4.33, 4);
    expect(positions[0]?.markPrice.value).toBeCloseTo(230.72, 2);
    expect(positions[0]?.marketId).toBe(NVDA_MARKET_ID);
  });

  it('reads account equity and free margin', async () => {
    const account = await client({ credentials: true }).account();
    expect(account.equityUsd.value).toBe(5000);
    expect(account.availableUsd.value).toBe(4000);
  });

  it('names the missing key when the account is not configured', async () => {
    await expect(client().account()).rejects.toThrow(/DESK_LIGHTER_RH_ACCOUNT_INDEX/);
  });
});

describe('LighterRhClient write readiness', () => {
  it('refuses while the desk is in dry run', async () => {
    const config = fixtureConfig({ live: false });
    const ready = await client({ credentials: true, config }).canTrade();
    expect(ready.ok).toBe(false);
    expect(ready.reason).toContain('dry run');
  });

  it('refuses until the jurisdiction notice is acknowledged', async () => {
    const config = fixtureConfig({ live: true, acknowledgedRestrictions: false });
    const ready = await client({ credentials: true, config }).canTrade();
    expect(ready.ok).toBe(false);
    expect(ready.reason).toContain('acknowledge-restrictions');
  });

  it('names the missing key and the file it belongs in', async () => {
    const ready = await client().canTrade();
    expect(ready.ok).toBe(false);
    expect(ready.reason).toContain('DESK_LIGHTER_RH_PRIVATE_KEY');
    expect(ready.reason).toContain('keys.env');
  });

  it('names the sub-account index when only the key is present', async () => {
    const partial = new LighterRhClient({
      config: fixtureConfig(),
      logger: fixtureLogger(),
      keystore: fixtureKeystore({ DESK_LIGHTER_RH_PRIVATE_KEY: FULL_CREDENTIALS.DESK_LIGHTER_RH_PRIVATE_KEY }),
      transport: fixtureTransport(),
      fetchJson: fixtureFetchJson,
    });
    const ready = await partial.canTrade();
    expect(ready.reason).toContain('DESK_LIGHTER_RH_ACCOUNT_INDEX');
  });

  it('refuses when the signing chain id is unknown', async () => {
    const unknownChain = new LighterRhClient({
      config: fixtureConfig(),
      logger: fixtureLogger(),
      keystore: fixtureKeystore(FULL_CREDENTIALS),
      transport: fixtureTransport(),
      fetchJson: fixtureFetchJson,
      signingChainId: 0,
    });
    const ready = await unknownChain.canTrade();
    expect(ready.ok).toBe(false);
    expect(ready.reason).toContain('signing chain id');
  });

  it('is ready once credentials and the published chain id are in place', async () => {
    const ready = await client({ credentials: true }).canTrade();
    expect(ready).toEqual({ ok: true });
    expect(LIGHTER_RH_CHAIN_ID).toBe(466324);
  });

  it('sends nothing while a credential is missing', async () => {
    const result = await client().placeOrder({
      marketId: NVDA_MARKET_ID,
      side: 'sell',
      sizeBase: 4.33,
      maxNotionalUsd: 1000,
    });
    expect(result.sent).toBe(false);
    expect(result.reason).toContain('DESK_LIGHTER_RH_PRIVATE_KEY');
  });

  it('rounds the order to the market step and holds the caller cap', async () => {
    const seen: { size?: string; type?: string; side?: string }[] = [];
    const trading = new LighterRhClient({
      config: fixtureConfig({ hedge: { ...fixtureConfig().hedge, maxNotionalUsd: 250 } }),
      logger: fixtureLogger(),
      keystore: fixtureKeystore(FULL_CREDENTIALS),
      transport: fixtureTransport(),
      fetchJson: fixtureFetchJson,
      createTradingClient: (spec) => {
        expect(spec.chainId).toBe(LIGHTER_RH_CHAIN_ID);
        expect(spec.maxNotionalUsd).toBe(250);
        return {
          async placeOrder(request) {
            seen.push(request);
            return { txHash: '0xabc', clientOrderIndex: 7n };
          },
          async cancelOrder() {
            return undefined;
          },
        };
      },
    });

    const result = await trading.placeOrder({
      marketId: NVDA_MARKET_ID,
      side: 'sell',
      sizeBase: 4.3345,
      maxNotionalUsd: 1000,
    });
    expect(result).toEqual({ sent: true, orderId: '0xabc' });
    expect(seen).toEqual([{ market: NVDA_MARKET_ID, side: 'sell', size: '4.3345', type: 'market' }]);
  });
});
