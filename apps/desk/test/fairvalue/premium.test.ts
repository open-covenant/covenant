import { afterEach, describe, expect, it } from 'vitest';
import { defaultConfig } from '../../src/core/config.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import type { Pool } from '../../src/core/types.js';
import { createSession } from '../../src/fairvalue/session.js';
import { createReference } from '../../src/fairvalue/reference.js';
import { createPremium } from '../../src/fairvalue/premium.js';
import {
  AAPL,
  NVDA,
  USDG,
  type ChainStub,
  lighterSource,
  makeChain,
  makePool,
  makeToken,
  poolsFor,
  rhjSource,
} from './helpers.js';

/** Saturday 2026-09-19, 08:00 New York time. Chainlink is frozen, the perpetual is not. */
const WEEKEND = Date.UTC(2026, 8, 19, 12);

const session = createSession();
const config = defaultConfig();
const stores: Store[] = [];

afterEach(() => {
  while (stores.length > 0) stores.pop()?.close();
});

function newStore(): Store {
  const store = openStore({ path: ':memory:' });
  stores.push(store);
  return store;
}

const tokens = {
  AAPL: makeToken({ address: AAPL, symbol: 'AAPL', uiMultiplier: 1 }),
  NVDA: makeToken({ address: NVDA, symbol: 'NVDA', uiMultiplier: 1 }),
};

function registryStub(): ChainStub['registry'] {
  const bySymbol = new Map(Object.entries(tokens));
  const byAddress = new Map([
    [AAPL.toLowerCase(), tokens.AAPL],
    [NVDA.toLowerCase(), tokens.NVDA],
  ]);
  return {
    token: (key) => bySymbol.get(key.toUpperCase()) ?? byAddress.get(key.toLowerCase()),
    stockTokens: () => [tokens.AAPL, tokens.NVDA],
    isStockToken: (address) => byAddress.has(address.toLowerCase()),
    feedFor: () => undefined,
  };
}

interface Setup {
  pools: readonly Pool[];
  store?: Store;
  usdgUsd?: number | (() => number);
  midPrice?: ChainStub['pools'] extends undefined ? never : (pool: Pool) => never;
  now?: () => number;
}

function build(setup: Setup) {
  const store = setup.store ?? newStore();
  const chain = makeChain({
    registry: registryStub(),
    feeds: {
      usdgUsd: async () => {
        const value = typeof setup.usdgUsd === 'function' ? setup.usdgUsd() : (setup.usdgUsd ?? 1);
        return {
          price: { value, unit: 'USD' as const, source: 'chainlink' as const, asOf: WEEKEND },
          updatedAt: WEEKEND,
          decimals: 8,
          roundId: 1n,
        };
      },
    },
    pools: {
      forToken: poolsFor(setup.pools),
      ...(setup.midPrice ? { midPrice: setup.midPrice } : {}),
    },
  });

  const reference = createReference({
    logger: silentLogger(),
    chain,
    session,
    rhj: rhjSource({}),
    lighter: lighterSource({
      AAPL: { symbol: 'AAPL', marketId: 10, markPrice: 320, asOf: WEEKEND - 5_000 },
      NVDA: { symbol: 'NVDA', marketId: 15, markPrice: 180, asOf: WEEKEND - 5_000 },
    }),
    now: () => WEEKEND,
  });

  const premium = createPremium({
    config,
    logger: silentLogger(),
    store,
    chain,
    session,
    reference,
    now: setup.now ?? (() => WEEKEND),
  });

  return { premium, store, chain };
}

describe('premium math', () => {
  const { premium } = build({ pools: [] });

  it('reports the gap in basis points', () => {
    expect(premium.computeBps(330, 300).value).toBeCloseTo(1000, 9);
    expect(premium.computeBps(300, 300).value).toBe(0);
    expect(premium.computeBps(285, 300).value).toBeCloseTo(-500, 9);
  });

  it('carries the unit and the origin', () => {
    const bps = premium.computeBps(330, 300, 1234);
    expect(bps).toMatchObject({ unit: 'bps', source: 'derived', asOf: 1234 });
  });

  it('refuses a reference that cannot divide', () => {
    expect(() => premium.computeBps(330, 0)).toThrow(/positive reference price/);
  });
});

describe('fair value for one stock token', () => {
  it('prices the token from the deepest USDG pool', async () => {
    const { premium } = build({
      pools: [makePool({ poolId: '0xaa', currency0: AAPL, currency1: USDG, mid: 336, liquidity: 5_000n })],
      usdgUsd: 0.999,
    });

    const value = await premium.forSymbol('AAPL');
    expect(value.onchainMid?.value).toBeCloseTo(336 * 0.999, 9);
    expect(value.reference?.value).toBe(320);
    expect(value.referenceSource).toBe('lighter-rh');
    expect(value.premiumBps?.value).toBeCloseTo(((336 * 0.999) / 320 - 1) * 10_000, 6);
    expect(value.pool).toBe('0xaa');
    expect(value.sessionState).toBe('closed');
  });

  it('reads the same price when the stock is the second currency', async () => {
    const flipped = build({
      pools: [makePool({ poolId: '0xbb', currency0: USDG, currency1: AAPL, mid: 1 / 336, liquidity: 5_000n })],
      usdgUsd: 0.999,
    });

    const value = await flipped.premium.forSymbol('AAPL');
    expect(value.onchainMid?.value).toBeCloseTo(336 * 0.999, 6);
  });

  it('leaves trap pools out of the price', async () => {
    const { premium } = build({
      pools: [
        makePool({ poolId: '0xtrap', currency0: AAPL, currency1: USDG, mid: 900, liquidity: 90_000n, trap: true }),
        makePool({ poolId: '0xgood', currency0: AAPL, currency1: USDG, mid: 336, liquidity: 5_000n }),
      ],
    });

    const value = await premium.forSymbol('AAPL');
    expect(value.pool).toBe('0xgood');
    expect(value.onchainMid?.value).toBeCloseTo(336, 9);
  });

  it('still names the reference when no pool quotes the token', async () => {
    const { premium } = build({ pools: [] });
    const value = await premium.forSymbol('AAPL');

    expect(value.onchainMid).toBeUndefined();
    expect(value.premiumBps).toBeUndefined();
    expect(value.reference?.value).toBe(320);
  });

  it('refuses a symbol the registry does not carry', async () => {
    const { premium } = build({ pools: [] });
    await expect(premium.forSymbol('ZZZZ')).rejects.toThrow(/was not found/);
  });
});

describe('the USDG rate', () => {
  it('reuses the last good reading when the feed stops answering', async () => {
    let calls = 0;
    let now = WEEKEND;
    const { premium } = build({
      pools: [makePool({ poolId: '0xaa', currency0: AAPL, currency1: USDG, mid: 336 })],
      usdgUsd: () => {
        calls += 1;
        if (calls > 1) throw new Error('feed unavailable');
        return 0.999;
      },
      now: () => now,
    });

    expect(await premium.usdgUsd()).toBe(0.999);
    now += 60_000;
    expect(await premium.usdgUsd()).toBe(0.999);
    now += 60 * 60 * 1000;
    expect(await premium.usdgUsd()).toBeUndefined();
  });

  it('leaves the on-chain price out when the rate cannot be read', async () => {
    const { premium } = build({
      pools: [makePool({ poolId: '0xaa', currency0: AAPL, currency1: USDG, mid: 336 })],
      usdgUsd: () => {
        throw new Error('feed unavailable');
      },
    });

    const value = await premium.forSymbol('AAPL');
    expect(value.onchainMid).toBeUndefined();
    expect(value.reference?.value).toBe(320);
  });
});

describe('the whole table', () => {
  it('ranks stored pools by depth and keeps a failing symbol in the table', async () => {
    const store = newStore();
    const shallow = makePool({ poolId: '0xaapl', currency0: AAPL, currency1: USDG, mid: 336, liquidity: 1_000n });
    const deep = makePool({ poolId: '0xnvda', currency0: NVDA, currency1: USDG, mid: 190, liquidity: 9_000n });
    store.upsertPool(shallow);
    store.upsertPool({ ...deep, midPrice: undefined });

    const { premium } = build({
      store,
      pools: [shallow, deep],
      midPrice: ((pool: Pool) => {
        throw new Error(`no state for ${pool.poolId}`);
      }) as never,
    });

    const rows = await premium.all();
    expect(rows.map((row) => row.symbol)).toEqual(['NVDA', 'AAPL']);

    const nvda = rows[0];
    expect(nvda?.onchainMid).toBeUndefined();
    expect(nvda?.premiumBps).toBeUndefined();
    expect(nvda?.reference?.value).toBe(180);
    expect(nvda?.candidates.map((entry) => entry.source)).toEqual(['lighter-rh']);

    const aapl = rows[1];
    expect(aapl?.onchainMid?.value).toBeCloseTo(336, 9);
    expect(aapl?.premiumBps?.value).toBeCloseTo((336 / 320 - 1) * 10_000, 6);
  });

  it('falls back to the registry when the store holds no pools', async () => {
    const { premium } = build({
      pools: [makePool({ poolId: '0xaapl', currency0: AAPL, currency1: USDG, mid: 336, liquidity: 1_000n })],
    });

    const rows = await premium.all({ limit: 5 });
    expect(rows.map((row) => row.symbol)).toEqual(['AAPL']);
    expect(rows[0]?.pool).toBe('0xaapl');
  });
});
