/**
 * Test fixtures for the orders module: an in-memory store, a controllable
 * clock, a quoter that answers from a script, and a swap that records what it
 * was asked to send without touching a network.
 */

import { defaultConfig, type Config } from '../../src/core/config.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import type { Address, Hex32, Quantity, Token } from '../../src/core/types.js';
import type { ChainModule, QuoteResult, SwapRequest, SwapResult } from '../../src/chain/index.js';
import type { FairValueModule } from '../../src/fairvalue/index.js';
import { createOrdersModule } from '../../src/orders/index.js';
import type { OrderSnapshot, OrdersModule, SnapshotProvider } from '../../src/orders/index.js';

export const USDG = '0x5fc5360d0400a0fd4f2af552add042d716f1d168' as Address;
export const NVDA = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
export const AI = '0x00000000000000000000000000000000000000a1' as Address;
export const POOL = '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147' as Hex32;

/** 2026-09-04T12:00:00Z. Every test that cares about time starts here. */
export const T0 = Date.UTC(2026, 8, 4, 12, 0, 0);

const TOKENS: Token[] = [
  { address: USDG, symbol: 'USDG', name: 'Global Dollar', decimals: 6, isStockToken: false },
  { address: NVDA, symbol: 'NVDA', name: 'NVIDIA', decimals: 18, isStockToken: true, uiMultiplier: 1 },
  { address: AI, symbol: 'AI', name: 'Artificial Inu', decimals: 18, isStockToken: false },
];

export interface Harness {
  readonly config: Config;
  readonly store: Store;
  readonly orders: OrdersModule;
  /** Current time in tests. Move it with {@link Harness.setNow}. */
  now(): number;
  setNow(ms: number): void;
  /** What the quoter answers next. */
  quote: { amountOut: bigint; gasEstimate: bigint; route: Hex32[]; fail?: string };
  /** What the snapshot provider answers next. */
  snapshot: Partial<OrderSnapshot>;
  /** Next regular open the session reports, ms since epoch. */
  nextOpen: number | Error;
  /** Decimals the chain reader answers for tokens the store does not hold. */
  readonly chainDecimals: Map<string, number>;
  /** Every swap the engine asked for. Empty in a dry run. */
  readonly sent: SwapRequest[];
  close(): void;
}

export function harness(overrides: Partial<Config> = {}): Harness {
  const store = openStore({ path: ':memory:' });
  for (const token of TOKENS) store.upsertToken(token);

  const config = defaultConfig({
    live: false,
    acknowledgedRestrictions: false,
    engineIntervalSec: 5,
    ...overrides,
  });

  let clock = T0;
  const sent: SwapRequest[] = [];
  const state = {
    quote: { amountOut: 50n * 10n ** 18n, gasEstimate: 210_000n, route: [POOL] as Hex32[] } as Harness['quote'],
    snapshot: { usdOnchain: 2, usdFair: 2, premiumBps: 0 } as Partial<OrderSnapshot>,
    nextOpen: T0 + 3_600_000 as number | Error,
    /** Decimals the chain reader knows for tokens the store has never seen. */
    chainDecimals: new Map<string, number>(),
  };

  const price = (value: number, unit: Quantity['unit'] = 'token'): Quantity => ({
    value,
    unit,
    source: 'pool',
    asOf: clock,
  });

  const chain = {
    registry: {
      isStockToken: (address: Address) => store.getToken(address)?.isStockToken ?? false,
      token: (key: string) => store.getToken(key),
    },
    tokens: {
      // The chain reader answers only for tokens it has read. Anything else is
      // left out of the map, the same as the real one.
      async decimalsOf(addresses: readonly Address[]): Promise<Map<string, number>> {
        const out = new Map<string, number>();
        for (const address of addresses) {
          const known = store.getToken(address) ?? state.chainDecimals.get(address.toLowerCase());
          const decimals = typeof known === 'number' ? known : known?.decimals;
          if (decimals !== undefined) out.set(address.toLowerCase(), decimals);
        }
        return out;
      },
    },
    quoter: {
      async quoteExactInput(request: { amountIn: bigint }): Promise<QuoteResult> {
        if (state.quote.fail) throw new Error(state.quote.fail);
        return {
          amountIn: request.amountIn,
          amountOut: state.quote.amountOut,
          gasEstimate: state.quote.gasEstimate,
          route: state.quote.route,
          effectivePrice: price(1),
          blockNumber: 54_423_221n,
        };
      },
      async quoteExactInputSingle(request: { amountIn: bigint }): Promise<QuoteResult> {
        return this.quoteExactInput(request);
      },
    },
    swap: {
      async execute(request: SwapRequest): Promise<SwapResult> {
        sent.push(request);
        return {
          txHash: '0x'.padEnd(66, 'a') as Hex32,
          amountIn: request.amountIn,
          amountOut: state.quote.amountOut,
          effectivePrice: price(1),
          gasUsed: 180_000n,
          blockNumber: 54_423_222n,
          simulated: false,
        };
      },
    },
  } as unknown as ChainModule;

  const fairvalue = {
    session: {
      nextOpen: () => {
        if (state.nextOpen instanceof Error) throw state.nextOpen;
        return state.nextOpen;
      },
    },
  } as unknown as FairValueModule;

  const snapshots: SnapshotProvider = {
    async snapshot(order, at): Promise<OrderSnapshot> {
      const subject = order.side === 'buy' ? order.tokenOut : order.tokenIn;
      return { token: subject, symbol: store.getToken(subject)?.symbol, asOf: at, ...state.snapshot };
    },
  };

  const orders = createOrdersModule({
    config,
    logger: silentLogger(),
    store,
    chain,
    fairvalue,
    snapshots,
    now: () => clock,
  });

  return {
    config,
    store,
    orders,
    sent,
    now: () => clock,
    setNow: (ms: number) => {
      clock = ms;
    },
    get quote() {
      return state.quote;
    },
    set quote(value: Harness['quote']) {
      state.quote = value;
    },
    get snapshot() {
      return state.snapshot;
    },
    set snapshot(value: Partial<OrderSnapshot>) {
      state.snapshot = value;
    },
    chainDecimals: state.chainDecimals,
    get nextOpen() {
      return state.nextOpen;
    },
    set nextOpen(value: number | Error) {
      state.nextOpen = value;
    },
    close: () => store.close(),
  };
}

/** 100 USDG, smallest units. */
export const HUNDRED_USDG = 100n * 10n ** 6n;

/** A buy of AI paid for in USDG, triggering when the on-chain price is at or below 2.50. */
export function buyAi(overrides: Record<string, unknown> = {}) {
  return {
    kind: 'limit' as const,
    side: 'buy' as const,
    tokenIn: USDG,
    tokenOut: AI,
    amountIn: HUNDRED_USDG,
    trigger: { priceLte: 2.5 },
    ...overrides,
  };
}
