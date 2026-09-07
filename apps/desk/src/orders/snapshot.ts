/**
 * Prices the engine evaluates triggers against.
 *
 * A snapshot answers one question: what is the order's subject token worth
 * right now, on chain and at reference, and what premium is the stock leg
 * carrying. Stock tokens come from the premium reader, paired tokens from the
 * paired reader. A missing price is reported, never guessed.
 */

import type { Order } from '../core/types.js';
import type { Store } from '../core/store.js';
import type { Logger } from '../core/logger.js';
import type { FairValueModule } from '../fairvalue/index.js';
import type { ChainModule } from '../chain/index.js';
import type { OrderSnapshot, SnapshotProvider } from './types.js';
import { subjectToken } from './support.js';

export interface SnapshotDeps {
  readonly store: Store;
  readonly logger: Logger;
  readonly fairvalue: FairValueModule;
  readonly chain: ChainModule;
}

/**
 * Read prices from the fair value module.
 *
 * The subject of a buy is `tokenOut` and the subject of a sell is `tokenIn`.
 * When the subject is a Robinhood stock token the premium reader answers
 * directly; otherwise the token is priced through its stock leg.
 */
export function createFairValueSnapshotProvider(deps: SnapshotDeps): SnapshotProvider {
  return {
    async snapshot(order: Order, now: number): Promise<OrderSnapshot> {
      const token = subjectToken(order);
      const known = deps.store.getToken(token);
      const isStock = known?.isStockToken ?? deps.chain.registry.isStockToken(token);

      if (isStock) {
        const symbol = known?.symbol ?? deps.chain.registry.token(token)?.symbol;
        if (!symbol) {
          return { token, asOf: now, note: `No symbol is known for ${token}, so it cannot be priced.` };
        }
        const fair = await deps.fairvalue.premium.forSymbol(symbol);
        return {
          token,
          symbol,
          usdOnchain: fair.onchainMid?.value,
          usdFair: fair.reference?.value,
          premiumBps: fair.premiumBps?.value,
          source: fair.referenceSource,
          asOf: fair.asOf,
        };
      }

      const paired = await deps.fairvalue.paired.quote(token);
      return {
        token,
        symbol: paired.symbol,
        usdOnchain: paired.usdOnchain.value,
        usdFair: paired.usdFair.value,
        premiumBps: paired.stockLegPremiumBps.value,
        source: paired.usdFair.source,
        asOf: paired.asOf,
      };
    },
  };
}

/**
 * Reuse a reading for a short window.
 *
 * The engine wakes every five seconds and every open order asks what its
 * subject token is worth. Orders on the same token ask the same question, and
 * each answer costs a pool read and a feed read, so one reading per token per
 * pass keeps the price current without repeating the work.
 */
export function createCachedSnapshotProvider(inner: SnapshotProvider, ttlMs = 4_000): SnapshotProvider {
  const cache = new Map<string, { readonly at: number; readonly value: Promise<OrderSnapshot> }>();

  return {
    snapshot(order: Order, now: number): Promise<OrderSnapshot> {
      const key = `${subjectToken(order)}:${order.side}`;
      const hit = cache.get(key);
      if (hit && now - hit.at < ttlMs) return hit.value;

      const value = inner.snapshot(order, now);
      cache.set(key, { at: now, value });
      // A failed read is not worth keeping: the next order should try again.
      void value.catch(() => cache.delete(key));
      for (const [existing, entry] of cache) {
        if (now - entry.at >= ttlMs) cache.delete(existing);
      }
      return value;
    },
  };
}
