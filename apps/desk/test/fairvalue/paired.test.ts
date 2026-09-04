import { afterEach, describe, expect, it } from 'vitest';
import { defaultConfig } from '../../src/core/config.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import type { Address, Pool } from '../../src/core/types.js';
import { quantity } from '../../src/core/types.js';
import type { QuoteRequest } from '../../src/chain/index.js';
import { createSession } from '../../src/fairvalue/session.js';
import { createReference } from '../../src/fairvalue/reference.js';
import { createPremium } from '../../src/fairvalue/premium.js';
import { createPaired } from '../../src/fairvalue/paired.js';
import {
  AAPL,
  AI,
  NVDA,
  USDG,
  WETH,
  lighterSource,
  makeChain,
  makePool,
  makeToken,
  poolsFor,
  rhjSource,
} from './helpers.js';

/** Saturday 2026-09-19, 08:00 New York time. */
const WEEKEND = Date.UTC(2026, 8, 19, 12);
const ETH_USD = 3_000;

const session = createSession();
const config = defaultConfig();
const stores: Store[] = [];

afterEach(() => {
  while (stores.length > 0) stores.pop()?.close();
});

const stock = {
  NVDA: makeToken({ address: NVDA, symbol: 'NVDA', uiMultiplier: 1 }),
  AAPL: makeToken({ address: AAPL, symbol: 'AAPL', uiMultiplier: 1 }),
};

const nvdaUsdg = makePool({ poolId: '0xnvdausdg', currency0: NVDA, currency1: USDG, mid: 190, liquidity: 50_000n });
const aaplUsdg = makePool({ poolId: '0xaaplusdg', currency0: AAPL, currency1: USDG, mid: 336, liquidity: 50_000n });
const aiNvda = makePool({ poolId: '0xainvda', currency0: AI, currency1: NVDA, mid: 0.001, liquidity: 1_000n });
const aiAapl = makePool({ poolId: '0xaiaapl', currency0: AI, currency1: AAPL, mid: 0.0006, liquidity: 3_000n });
const aiWeth = makePool({ poolId: '0xaiweth', currency0: AI, currency1: WETH, mid: 0.00005, liquidity: 2_000n });

interface Setup {
  pools: readonly Pool[];
  marks?: Record<string, number>;
  quoted?: { tokensPerStock: number; seen: QuoteRequest[] };
}

function build(setup: Setup) {
  const store = openStore({ path: ':memory:' });
  stores.push(store);
  store.upsertToken(makeToken({ address: AI, symbol: 'AI', isStockToken: false }));

  const byAddress = new Map<string, ReturnType<typeof makeToken>>([
    [NVDA.toLowerCase(), stock.NVDA],
    [AAPL.toLowerCase(), stock.AAPL],
  ]);
  const marks = setup.marks ?? { NVDA: 180, AAPL: 320 };

  const chain = makeChain({
    registry: {
      token: (key) => byAddress.get(key.toLowerCase()) ?? (key.toUpperCase() === 'AI' ? undefined : stockBySymbol(key)),
      stockTokens: () => [stock.NVDA, stock.AAPL],
      isStockToken: (address) => byAddress.has(address.toLowerCase()),
      feedFor: () => undefined,
    },
    feeds: {
      ethUsd: async () => ({
        price: quantity(ETH_USD, 'USD', 'chainlink', WEEKEND),
        updatedAt: WEEKEND,
        decimals: 8,
        roundId: 1n,
      }),
    },
    pools: { forToken: poolsFor(setup.pools) },
    ...(setup.quoted
      ? {
          quoter: {
            quoteExactInputSingle: async (request: QuoteRequest & { poolId: `0x${string}` }) => {
              setup.quoted?.seen.push(request);
              return {
                amountIn: request.amountIn,
                amountOut: 1n,
                gasEstimate: 100_000n,
                route: [request.poolId],
                effectivePrice: quantity(setup.quoted?.tokensPerStock ?? 1, 'token', 'pool', WEEKEND),
                blockNumber: 1n,
              };
            },
          },
        }
      : {}),
  });

  function stockBySymbol(key: string) {
    return key.toUpperCase() === 'NVDA' ? stock.NVDA : key.toUpperCase() === 'AAPL' ? stock.AAPL : undefined;
  }

  const reference = createReference({
    logger: silentLogger(),
    chain,
    session,
    rhj: rhjSource({}),
    lighter: lighterSource(
      Object.fromEntries(
        Object.entries(marks).map(([symbol, markPrice]) => [
          symbol,
          { symbol, marketId: 1, markPrice, asOf: WEEKEND - 5_000 },
        ]),
      ),
    ),
    now: () => WEEKEND,
  });

  const premium = createPremium({
    config,
    logger: silentLogger(),
    store,
    chain,
    session,
    reference,
    now: () => WEEKEND,
  });

  const paired = createPaired({
    config,
    logger: silentLogger(),
    store,
    chain,
    premium,
    now: () => WEEKEND,
  });

  return { paired, store };
}

describe('paired token pricing', () => {
  it('prices the token through its stock leg', async () => {
    const { paired } = build({ pools: [aiNvda, nvdaUsdg] });
    const quote = await paired.quote(AI as Address);

    expect(quote.symbol).toBe('AI');
    expect(quote.stockSymbol).toBe('NVDA');
    expect(quote.pool).toBe('0xainvda');
    expect(quote.ratio.value).toBeCloseTo(0.001, 12);
    expect(quote.usdOnchain.value).toBeCloseTo(0.19, 12);
    expect(quote.usdFair.value).toBeCloseTo(0.18, 12);
    expect(quote.stockLegPremiumBps.value).toBeCloseTo((190 / 180 - 1) * 10_000, 6);
  });

  it('compares the stock route against the ETH route', async () => {
    const { paired } = build({ pools: [aiNvda, aiWeth, nvdaUsdg] });
    const quote = await paired.quote(AI as Address);

    expect(quote.usdViaWeth?.value).toBeCloseTo(0.00005 * ETH_USD, 12);
    expect(quote.bestEntryRoute?.pools).toEqual(['0xaiweth']);
    expect(quote.bestEntryRoute?.usdPrice.value).toBeCloseTo(0.15, 12);
    expect(quote.bestExitRoute?.pools).toEqual(['0xainvda']);
    expect(quote.bestExitRoute?.usdPrice.value).toBeCloseTo(0.19, 12);
  });

  it('weights fair value across every stock pool and names the deepest', async () => {
    const { paired } = build({ pools: [aiNvda, aiAapl, nvdaUsdg, aaplUsdg] });
    const quote = await paired.quote(AI as Address);

    expect(quote.stockSymbol).toBe('AAPL');
    expect(quote.usdFair.value).toBeCloseTo(0.0006 * 320, 12);
    expect(quote.alternatives?.map((entry) => entry.stockSymbol)).toEqual(['NVDA']);
    // (3000 x 0.192 + 1000 x 0.18) / 4000
    expect(quote.weightedUsdFair?.value).toBeCloseTo(0.189, 12);
  });

  it('honours a requested stock leg', async () => {
    const { paired } = build({ pools: [aiNvda, aiAapl, nvdaUsdg, aaplUsdg] });
    const quote = await paired.quote(AI as Address, { stockSymbol: 'nvda' });

    expect(quote.stockSymbol).toBe('NVDA');
    expect(quote.usdOnchain.value).toBeCloseTo(0.19, 12);
    expect(quote.alternatives?.map((entry) => entry.stockSymbol)).toEqual(['AAPL']);
  });

  it('lists every stock pool with the same weighted fair value', async () => {
    const { paired } = build({ pools: [aiNvda, aiAapl, nvdaUsdg, aaplUsdg] });
    const quotes = await paired.quoteAll(AI as Address);

    expect(quotes.map((entry) => entry.stockSymbol)).toEqual(['AAPL', 'NVDA']);
    expect(quotes.every((entry) => Math.abs((entry.weightedUsdFair?.value ?? 0) - 0.189) < 1e-12)).toBe(true);
  });

  it('uses a sized quote when an amount is given', async () => {
    const seen: QuoteRequest[] = [];
    const { paired } = build({
      pools: [aiNvda, nvdaUsdg],
      quoted: { tokensPerStock: 900, seen },
    });

    const quote = await paired.quote(AI as Address, { amountUsd: 100 });
    expect(quote.ratio.value).toBeCloseTo(1 / 900, 12);
    expect(quote.ratio.source).toBe('derived');
    expect(quote.usdOnchain.value).toBeCloseTo(190 / 900, 12);
    expect(quote.bestEntryRoute?.note).toContain('price impact');

    expect(seen).toHaveLength(1);
    expect(seen[0]?.tokenIn).toBe(NVDA);
    expect(seen[0]?.tokenOut).toBe(AI);
    // 100 USD at 190 USD per NVDA, in 18 decimals.
    expect(Number(seen[0]?.amountIn ?? 0n) / 1e18).toBeCloseTo(100 / 190, 12);
  });

  it('sizes a sell in the token rather than in the stock leg', async () => {
    const seen: QuoteRequest[] = [];
    const { paired } = build({
      pools: [aiNvda, nvdaUsdg],
      quoted: { tokensPerStock: 0.0009, seen },
    });

    const quote = await paired.quote(AI as Address, { amountUsd: 100, side: 'sell' });
    // The quoter answers NVDA per AI directly on a sell, so the ratio is the
    // effective price rather than its inverse.
    expect(quote.ratio.value).toBeCloseTo(0.0009, 12);
    expect(seen[0]?.tokenIn).toBe(AI);
    expect(seen[0]?.tokenOut).toBe(NVDA);
    // 100 USD at 0.19 USD per AI, in 18 decimals.
    expect(Number(seen[0]?.amountIn ?? 0n) / 1e18).toBeCloseTo(100 / 0.19, 6);
  });

  it('compares two routes at the same size, or at their mids', async () => {
    const seen: QuoteRequest[] = [];
    const { paired } = build({
      pools: [aiNvda, aiWeth, nvdaUsdg],
      quoted: { tokensPerStock: 900, seen },
    });

    const quote = await paired.quote(AI as Address, { amountUsd: 100 });
    // Both legs were quoted for the same size, so the comparison stands.
    expect(seen.map((request) => request.poolId)).toEqual(['0xainvda', '0xaiweth']);
    expect(quote.bestEntryRoute?.note).toContain('price impact included');
    expect(quote.bestExitRoute?.note).toContain('price impact included');
  });

  it('falls back to the pool mid when the quoter cannot answer', async () => {
    const { paired } = build({ pools: [aiNvda, nvdaUsdg] });
    const quote = await paired.quote(AI as Address, { amountUsd: 100 });

    expect(quote.ratio.value).toBeCloseTo(0.001, 12);
    expect(quote.ratio.source).toBe('pool');
    expect(quote.bestEntryRoute?.note).toContain('pool mid');
  });

  it('refuses a token with no stock pool', async () => {
    const { paired } = build({ pools: [aiWeth] });
    await expect(paired.quote(AI as Address)).rejects.toThrow(/was not found/);
  });

  it('refuses to price a stock leg with no reference', async () => {
    const { paired } = build({ pools: [aiNvda, nvdaUsdg], marks: {} });
    await expect(paired.quote(AI as Address)).rejects.toThrow(/No reference price for NVDA/);
  });
});
