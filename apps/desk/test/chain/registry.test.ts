import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { defaultConfig } from '../../src/core/config.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore } from '../../src/core/store.js';
import { createRegistry, dataFile, loadFeedFile, parseFeeds } from '../../src/chain/registry.js';
import { decodeInitializeLog, INITIALIZE_TOPIC0 } from '../../src/chain/pools.js';

const config = defaultConfig();
const logger = silentLogger();

/** The two endpoints the registry reads, answered from the files in `data/`. */
function stubFetch(overrides: Record<string, unknown> = {}): typeof fetch {
  const assets = JSON.parse(readFileSync(dataFile('rhj-assets-snapshot.json'), 'utf8')) as unknown;
  const books = {
    order_books: [
      {
        symbol: 'AAPL',
        market_id: 10,
        market_type: 'perp',
        supported_size_decimals: 4,
        supported_price_decimals: 2,
      },
      {
        symbol: 'NVDA',
        market_id: 15,
        market_type: 'perp',
        supported_size_decimals: 4,
        supported_price_decimals: 2,
      },
      { symbol: 'AAPL/USDG', market_id: 2049, market_type: 'spot', supported_size_decimals: 4 },
    ],
  };
  const details = {
    order_book_details: [
      {
        symbol: 'NVDA',
        market_id: 15,
        market_type: 'perp',
        size_decimals: 4,
        price_decimals: 2,
        mark_price: '180.25',
      },
    ],
  };
  const bodies: Record<string, unknown> = { assets, books, details, ...overrides };
  return (async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/rhj/assets')) return answer(bodies.assets);
    if (url.includes('orderBookDetails')) return answer(bodies.details);
    if (url.includes('orderBooks')) return answer(bodies.books);
    throw new Error(`unexpected request to ${url}`);
  }) as typeof fetch;
}

function answer(body: unknown): Response {
  if (body instanceof Error) throw body;
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function registryFor(fetchImpl: typeof fetch) {
  const store = openStore({ path: ':memory:' });
  return { store, registry: createRegistry({ config, logger, store, fetchImpl }) };
}

describe('parseFeeds', () => {
  const feeds = loadFeedFile();

  it('reads every dollar-quoted feed published on 4663', () => {
    // 57 feeds are published; five of them price a token against another token
    // rather than against the dollar and are not references.
    expect(feeds).toHaveLength(52);
    expect(feeds.filter((feed) => feed.equity)).toHaveLength(35);
    expect(feeds.map((feed) => feed.symbol)).not.toContain('SYRUPUSDG');
    expect(new Set(feeds.map((feed) => feed.symbol)).size).toBe(52);
  });

  it('takes the symbol from the feed name for both naming styles', () => {
    const bySymbol = new Map(feeds.map((feed) => [feed.symbol, feed]));
    // "Robinhood NVDA / USD"
    expect(bySymbol.get('NVDA')?.address).toBe('0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15');
    // "Robinhood DELL-USD", whose docs call the asset RHDELL
    expect(bySymbol.get('DELL')?.equity).toBe(true);
    expect(bySymbol.get('AAPL')?.address).toBe('0x6b22a786baa607d76728168703a39ea9c99f2cd0');
    expect(bySymbol.get('ETH')?.address).toBe('0x78f3556b67e17df817d51ef5a990cdaf09e8d3a9');
    expect(bySymbol.get('USDG')?.address).toBe('0x61b7e5650328764b076a108eff5fa7282a1b9ad2');
  });

  it('records eight decimals and the daily heartbeat on equity feeds', () => {
    const nvda = feeds.find((feed) => feed.symbol === 'NVDA');
    expect(nvda?.decimals).toBe(8);
    expect(nvda?.heartbeatSec).toBe(86_400);
  });

  it('refuses a feed list that is not a list', () => {
    expect(() => parseFeeds({ feeds: [] })).toThrow(/could not be read/);
  });
});

describe('registry', () => {
  it('answers from the shipped snapshot before anything is refreshed', () => {
    const { registry } = registryFor(stubFetch());
    expect(registry.stockTokens()).toHaveLength(194);
    expect(registry.lastRefreshedAt()).toBeUndefined();
  });

  it('merges assets, feeds, and Lighter markets on refresh', async () => {
    const { registry, store } = registryFor(stubFetch());
    const counts = await registry.refresh();
    expect(counts.tokens).toBe(194);
    expect(counts.feeds).toBe(52);
    expect(counts.markets).toBe(3);

    const aapl = registry.token('AAPL');
    expect(aapl?.address).toBe('0xaf3d76f1834a1d425780943c99ea8a608f8a93f9');
    expect(aapl?.decimals).toBe(18);
    expect(aapl?.isStockToken).toBe(true);
    expect(aapl?.feed).toBe('0x6b22a786baa607d76728168703a39ea9c99f2cd0');
    expect(aapl?.lighterMarketId).toBe(10);
    expect(aapl?.uiMultiplier).toBeCloseTo(1.000566, 6);
    expect(aapl?.tradingCapabilities?.overnight).toBe(true);

    expect(registry.token('0xAF3D76f1834A1d425780943C99Ea8A608f8a93f9')?.symbol).toBe('AAPL');
    expect(registry.lighterPerpFor('NVDA')).toBe(15);
    // The spot book is listed but is not a perpetual, so hedging does not see it.
    expect(registry.lighterPerpFor('AAPL/USDG')).toBeUndefined();
    expect(registry.isStockToken('0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC')).toBe(true);
    expect(registry.isStockToken('0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168')).toBe(false);
    expect(registry.lastRefreshedAt()).toBeGreaterThan(0);

    expect(store.listTokens({ stockOnly: true })).toHaveLength(194);
    expect(store.getToken('NVDA')?.address).toBe('0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec');
  });

  it('names the mark price and the size decimals of every Lighter market', async () => {
    const { registry } = registryFor(stubFetch());
    await registry.refresh();
    const markets = registry.lighterMarkets();
    expect(markets.map((market) => market.marketId)).toEqual([10, 15, 2049]);
    const nvda = markets.find((market) => market.marketId === 15);
    expect(nvda?.markPrice).toBe(180.25);
    expect(nvda?.sizeDecimals).toBe(4);
    expect(markets.find((market) => market.marketId === 2049)?.kind).toBe('spot');
  });

  it('falls back to the shipped snapshot when the assets API refuses', async () => {
    const failing = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/rhj/assets')) return new Response('nope', { status: 503 });
      return stubFetch()(input);
    }) as typeof fetch;
    const { registry } = registryFor(failing);
    const counts = await registry.refresh();
    expect(counts.tokens).toBe(194);
    expect(registry.token('AAPL')?.symbol).toBe('AAPL');
  });

  it('keeps working when the Lighter market list is unreachable', async () => {
    const noLighter = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('orderBook')) throw new Error('connection refused');
      return stubFetch()(input);
    }) as typeof fetch;
    const { registry } = registryFor(noLighter);
    const counts = await registry.refresh();
    expect(counts.tokens).toBe(194);
    expect(counts.markets).toBe(0);
    expect(registry.lighterPerpFor('NVDA')).toBeUndefined();
    expect(registry.token('NVDA')?.feed).toBe('0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15');
  });
});

describe('decodeInitializeLog', () => {
  /**
   * The `Initialize` log for the AAPL/USDG reference pool, read from
   * transaction 0x2a18c109 in block 41,258,956 on 2026-09-04.
   */
  const log = {
    topics: [
      INITIALIZE_TOPIC0,
      '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147',
      '0x0000000000000000000000005fc5360d0400a0fd4f2af552add042d716f1d168',
      '0x000000000000000000000000af3d76f1834a1d425780943c99ea8a608f8a93f9',
    ],
    data:
      '0x0000000000000000000000000000000000000000000000000000000000800000' +
      '000000000000000000000000000000000000000000000000000000000000000a' +
      '00000000000000000000000070a9a88402989226847ec122043ce5e7ff462080' +
      '000000000000000000000000000000000000db8dee980f4a5904a6e90f4fd364' +
      '000000000000000000000000000000000000000000000000000000000003567a',
    blockNumber: '0x2758fcc',
  };

  it('reads the pool key and the opening price out of the log', () => {
    const decoded = decodeInitializeLog(log);
    expect(decoded?.poolId).toBe(
      '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147',
    );
    expect(decoded?.currency0).toBe('0x5fc5360d0400a0fd4f2af552add042d716f1d168');
    expect(decoded?.currency1).toBe('0xaf3d76f1834a1d425780943c99ea8a608f8a93f9');
    expect(decoded?.fee).toBe(0x800000);
    expect(decoded?.tickSpacing).toBe(10);
    expect(decoded?.hooks).toBe('0x70a9a88402989226847ec122043ce5e7ff462080');
    expect(decoded?.tick).toBe(218_746);
    expect(decoded?.blockNumber).toBe(41_258_956n);
    expect(decoded?.sqrtPriceX96).toBe(0xdb8dee980f4a5904a6e90f4fd364n);
  });

  it('ignores a log that is not an Initialize', () => {
    expect(
      decodeInitializeLog({ ...log, topics: ['0xdeadbeef', ...log.topics.slice(1)] }),
    ).toBeUndefined();
  });
});
