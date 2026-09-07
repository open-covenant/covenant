import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { migrate, openStore, type Store } from '../store.js';
import type { Address, Execution, Hex32, Order, Pool, Token } from '../types.js';

const NVDA = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
const USDG = '0x5fc5360d0400a0fd4f2af552add042d716f1d168' as Address;
const POOL = '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147' as Hex32;

const open: Store[] = [];

function store(): Store {
  const s = openStore({ path: ':memory:' });
  open.push(s);
  return s;
}

afterEach(() => {
  for (const s of open.splice(0)) s.close();
});

const token: Token = {
  address: NVDA,
  symbol: 'NVDA',
  name: 'NVIDIA',
  decimals: 18,
  isStockToken: true,
  uiMultiplier: 1.000566,
  tokenPaused: false,
  oraclePaused: false,
  feed: '0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15' as Address,
  lighterMarketId: 15,
  tradingCapabilities: { market: true, extended: true, overnight: true },
};

const pool: Pool = {
  poolId: POOL,
  currency0: USDG,
  currency1: NVDA,
  decimals0: 6,
  decimals1: 18,
  fee: 0x800000,
  tickSpacing: 10,
  hooks: '0x70a9a88402989226847ec122043ce5e7ff462080' as Address,
  initialBlock: 54_000_000n,
  lpFee: 25,
  liquidity: 123_456_789_000_000n,
  sqrtPriceX96: 79_228_162_514_264_337_593_543_950_336n,
  tick: 0,
  trap: false,
};

describe('migrations', () => {
  it('run once and are safe to repeat', () => {
    const s = store();
    expect(migrate(s.db)).toEqual([]);
    const tables = (s.db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as { name: string }[]).map(
      (row) => row.name,
    );
    for (const name of ['tokens', 'pools', 'observations', 'orders', 'executions', 'hedges', 'events']) {
      expect(tables).toContain(name);
    }
  });

  it('opens an existing file again without loss', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'desk-store-'));
    const file = path.join(dir, 'desk.sqlite');
    const first = openStore({ path: file });
    first.upsertToken(token);
    first.close();
    const second = openStore({ path: file });
    open.push(second);
    expect(second.getToken('NVDA')?.symbol).toBe('NVDA');
  });
});

describe('tokens', () => {
  it('round trips every field and updates in place', () => {
    const s = store();
    s.upsertToken(token);
    s.upsertToken({ ...token, uiMultiplier: 1.1 });
    const read = s.getToken(NVDA);
    expect(read?.uiMultiplier).toBe(1.1);
    expect(read?.lighterMarketId).toBe(15);
    expect(read?.tradingCapabilities?.overnight).toBe(true);
    expect(s.listTokens()).toHaveLength(1);
    expect(s.getToken('nvda')?.address).toBe(NVDA);
  });

  it('answers a symbol that several tokens share with the one in the deepest pool', () => {
    const s = store();
    const thin = '0x1deb38da68ea89f56b09fad1b134fffc1e6b1e18' as Address;
    const deep = '0x7851b2d9d38cce3471168f115e567afaf27c1e18' as Address;
    s.upsertToken({ address: thin, symbol: 'AI', name: 'Apple inu', decimals: 18, isStockToken: false });
    s.upsertToken({ address: deep, symbol: 'AI', name: 'Artificial Inu', decimals: 18, isStockToken: false });
    s.upsertPool({ ...pool, poolId: `0x${'1'.repeat(64)}` as Hex32, currency1: thin, liquidity: 10n });
    s.upsertPool({ ...pool, poolId: `0x${'2'.repeat(64)}` as Hex32, currency1: deep, liquidity: 1_000_000n });

    expect(s.getToken('AI')?.name).toBe('Artificial Inu');
  });

  it('prefers a stock token when a memecoin has taken its symbol', () => {
    const s = store();
    const impostor = '0x52522a30831cedefab1c12d7d6f501498afbca0a' as Address;
    s.upsertToken({ address: impostor, symbol: 'NVDA', name: 'NVDA Numeraire Test', decimals: 18, isStockToken: false });
    s.upsertPool({ ...pool, poolId: `0x${'3'.repeat(64)}` as Hex32, currency1: impostor, liquidity: 10n ** 30n });
    s.upsertToken(token);
    s.upsertPool(pool);

    expect(s.getToken('NVDA')?.address).toBe(NVDA);
  });
});

describe('pools', () => {
  it('keeps liquidity and sqrt price as exact integers', () => {
    const s = store();
    s.upsertPool(pool);
    const read = s.getPool(POOL);
    expect(read?.liquidity).toBe(123_456_789_000_000n);
    expect(read?.sqrtPriceX96).toBe(79_228_162_514_264_337_593_543_950_336n);
    expect(read?.initialBlock).toBe(54_000_000n);
  });

  it('leaves trap pools out unless they are asked for', () => {
    const s = store();
    s.upsertPool(pool);
    s.upsertPool({ ...pool, poolId: ('0x' + 'bb'.repeat(32)) as Hex32, lpFee: 6500, trap: true });
    expect(s.listPools()).toHaveLength(1);
    expect(s.listPools({ excludeTraps: false })).toHaveLength(2);
    expect(s.listPools({ currency: NVDA })).toHaveLength(1);
  });

  it('writes a token\'s real decimals into every pool holding it and clears the stale mid', () => {
    const s = store();
    s.upsertPool({ ...pool, decimals0: 18, midPrice: { value: 1, unit: 'token', source: 'pool', asOf: 1 } });
    const changed = s.applyTokenDecimals(USDG, 6);
    expect(changed).toBe(1);
    const read = s.getPool(POOL);
    expect(read?.decimals0).toBe(6);
    expect(read?.midPrice).toBeUndefined();
    expect(s.applyTokenDecimals(USDG, 6)).toBe(0);
  });

  it('counts rows without materialising them', () => {
    const s = store();
    s.upsertPool(pool);
    s.upsertToken(token);
    expect(s.countPools()).toBe(1);
    expect(s.countTokens()).toBe(1);
  });

  it('corrects the decimals a later read resolves', () => {
    const s = store();
    s.upsertPool({ ...pool, decimals0: 18, decimals1: 18 });
    s.upsertPool(pool);
    const read = s.getPool(POOL);
    expect(read?.decimals0).toBe(6);
    expect(read?.decimals1).toBe(18);
  });
});

describe('observations', () => {
  it('stores candidates and filters by time and symbol', () => {
    const s = store();
    s.insertObservation({
      ts: 1_000,
      symbol: 'NVDA',
      token: NVDA,
      pool: POOL,
      onchainMidUsd: 180.5,
      referenceUsd: 178,
      referenceSource: 'chainlink',
      premiumBps: 140.4,
      sessionState: 'open',
      blockNumber: 54_423_221n,
      candidates: [
        {
          symbol: 'NVDA',
          price: { value: 178, unit: 'USD', source: 'chainlink', asOf: 1_000 },
          source: 'chainlink',
          updatedAt: 900,
          ageSec: 0.1,
          stale: false,
        },
      ],
    });
    s.insertObservation({ ts: 2_000, symbol: 'AAPL', token: USDG, sessionState: 'closed' });

    expect(s.listObservations()).toHaveLength(2);
    expect(s.listObservations({ symbol: 'nvda' })).toHaveLength(1);
    expect(s.listObservations({ since: 1_500 })).toHaveLength(1);
    const first = s.listObservations({ symbol: 'NVDA' })[0];
    expect(first?.blockNumber).toBe(54_423_221n);
    expect(first?.candidates?.[0]?.source).toBe('chainlink');
    expect(s.countObservationsSince(0)).toBe(2);
  });
});

const order: Order = {
  id: 'ord_1',
  kind: 'limit',
  side: 'buy',
  tokenIn: USDG,
  tokenOut: NVDA,
  amountIn: 100_000_000n,
  trigger: { priceLte: 170, priceBasis: 'usdFair' },
  bounds: { maxSlippageBps: 100, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 },
  live: false,
  status: 'open',
  createdAt: 1_000,
  expiresAt: null,
  parentId: null,
};

describe('orders and executions', () => {
  it('round trips an order and moves it through its statuses', () => {
    const s = store();
    s.insertOrder(order);
    const read = s.getOrder('ord_1');
    expect(read?.amountIn).toBe(100_000_000n);
    expect(read?.trigger.priceLte).toBe(170);
    expect(read?.expiresAt).toBeNull();
    s.updateOrderStatus('ord_1', 'triggered');
    s.updateOrderStatus('ord_1', 'filled', 'quote accepted');
    expect(s.getOrder('ord_1')?.status).toBe('filled');
    expect(s.getOrder('ord_1')?.reason).toBe('quote accepted');
    expect(s.listOrders({ status: 'open' })).toHaveLength(0);
  });

  it('counts only sent and confirmed fills against the daily cap', () => {
    const s = store();
    s.insertOrder(order);
    const base: Omit<Execution, 'id' | 'status' | 'notionalUsd'> = {
      orderId: 'ord_1',
      live: true,
      amountIn: 100_000_000n,
      amountOut: 550_000_000_000_000_000n,
      quotedAmountOut: 551_000_000_000_000_000n,
      effectivePrice: { value: 181.5, unit: 'USD', source: 'pool', asOf: 2_000 },
      slippageBps: 18,
      createdAt: 2_000,
    };
    s.insertExecution({ ...base, id: 'ex_1', status: 'confirmed', notionalUsd: { value: 100, unit: 'USD', source: 'derived', asOf: 2_000 } });
    s.insertExecution({ ...base, id: 'ex_2', status: 'sent', notionalUsd: { value: 60, unit: 'USD', source: 'derived', asOf: 2_000 } });
    s.insertExecution({ ...base, id: 'ex_3', status: 'simulated', notionalUsd: { value: 500, unit: 'USD', source: 'derived', asOf: 2_000 } });

    expect(s.filledNotionalUsdSince(0)).toBe(160);
    expect(s.filledNotionalUsdSince(3_000)).toBe(0);
    expect(s.filledNotionalUsdSince(0, ['simulated'])).toBe(500);
    expect(s.filledNotionalUsdSince(0, ['simulated', 'sent', 'confirmed'])).toBe(660);
    expect(s.filledNotionalUsdSince(0, [])).toBe(0);
    expect(s.listExecutions({ orderId: 'ord_1' })).toHaveLength(3);
    expect(s.listExecutions()[0]?.amountOut).toBe(550_000_000_000_000_000n);
  });
});

describe('events', () => {
  it('appends an audit row and reads it back newest first', () => {
    const s = store();
    s.appendEvent({ ts: 1_000, kind: 'order.created', subject: 'ord_1', detail: { amountIn: 100n } });
    s.appendEvent({ ts: 2_000, kind: 'order.filled', subject: 'ord_1' });
    const events = s.listEvents({ subject: 'ord_1' });
    expect(events.map((event) => event.kind)).toEqual(['order.filled', 'order.created']);
    expect(events[1]?.detail).toEqual({ amountIn: '100' });
  });

  it('reads one kind of event without scanning the rest', () => {
    const s = store();
    s.appendEvent({ ts: 1_000, kind: 'hedge.sent', subject: 'NVDA' });
    s.appendEvent({ ts: 2_000, kind: 'order.filled', subject: 'ord_1' });
    s.appendEvent({ ts: 3_000, kind: 'hedge.blocked', subject: 'NVDA' });
    expect(s.listEvents({ kind: 'hedge.sent' }).map((event) => event.subject)).toEqual(['NVDA']);
    expect(s.listEvents({ kind: 'hedge.blocked', subject: 'ord_1' })).toEqual([]);
  });
});

describe('hedges', () => {
  it('keeps one row per symbol', () => {
    const s = store();
    const asOf = 5_000;
    s.upsertHedge({
      symbol: 'NVDA',
      marketId: 15,
      sizeBase: { value: -5.5, unit: 'token', source: 'lighter-rh', asOf },
      markPrice: { value: 180, unit: 'USD', source: 'lighter-rh', asOf },
      fundingBps8h: { value: 1.2, unit: 'bps', source: 'lighter-rh', asOf },
      asOf,
    });
    s.upsertHedge({
      symbol: 'NVDA',
      marketId: 15,
      sizeBase: { value: -6, unit: 'token', source: 'lighter-rh', asOf },
      markPrice: { value: 181, unit: 'USD', source: 'lighter-rh', asOf },
      asOf,
    });
    const hedges = s.listHedges();
    expect(hedges).toHaveLength(1);
    expect(hedges[0]?.sizeBase.value).toBe(-6);
    expect(hedges[0]?.markPrice.unit).toBe('USD');
  });
});
