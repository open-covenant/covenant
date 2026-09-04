/**
 * Stock-neutral positions.
 *
 * A memecoin quoted in a stock token carries that stock inside it. The hedge
 * module sizes a short on the Lighter Robinhood Chain instance that cancels the
 * stock leg, so the holder keeps the meme and drops the stock.
 *
 * Reads need no account. Writes need a funded sub-account on that instance,
 * which the desk does not have, so `apply` returns the plan with the reason it
 * was not sent. `NOTES.md` records what the live host answered, including the
 * signing chain id it publishes.
 */

export {
  LighterRhClient,
  LIGHTER_RH_CHAIN_ID,
  MARKETS_TTL_MS,
  toLighterMarket,
  type LighterRhOptions,
  type TradeReadiness,
  type TradingClientSpec,
  type TradingLike,
} from './lighter-rh.js';
export { HedgeSizer, chooseAction, truncateToStep, type SizerOptions } from './sizer.js';
export {
  HedgeRebalancer,
  type DeskRebalancer,
  type HedgeTarget,
  type RebalancerOptions,
} from './rebalancer.js';

import type { HedgePlan, HedgePosition, Quantity } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Keystore } from '../core/keystore.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { FairValueModule } from '../fairvalue/index.js';
import { LighterRhClient } from './lighter-rh.js';
import { HedgeSizer } from './sizer.js';
import { HedgeRebalancer, type DeskRebalancer } from './rebalancer.js';

export interface HedgeDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly keystore: Keystore;
  readonly fairvalue: FairValueModule;
}

/** One perpetual market on the Lighter Robinhood Chain instance. */
export interface LighterMarket {
  readonly marketId: number;
  readonly symbol: string;
  readonly kind: 'perp' | 'spot';
  /** Decimal places allowed on order size. */
  readonly sizeDecimals: number;
  /** Decimal places allowed on order price. */
  readonly priceDecimals: number;
  /** Smallest order size in base units. */
  readonly minBaseAmount: number;
  readonly initialMarginFraction?: number;
}

export interface LighterOrderRequest {
  readonly marketId: number;
  readonly side: 'buy' | 'sell';
  /** Base units, rounded to the market's `sizeDecimals`. */
  readonly sizeBase: number;
  /** Omit for a market order. */
  readonly price?: number;
  readonly reduceOnly?: boolean;
  /** Hard cap the client refuses to exceed, USD. */
  readonly maxNotionalUsd: number;
}

/** Reads and writes against `https://api.rh.lighter.xyz`. */
export interface LighterRh {
  markets(): Promise<LighterMarket[]>;
  /** Mark price for a market, USD. */
  markPrice(marketId: number): Promise<Quantity>;
  /** Funding over eight hours, basis points. Positive means longs pay. */
  funding(marketId: number): Promise<Quantity>;
  positions(): Promise<HedgePosition[]>;
  /** Account equity and margin, USD. */
  account(): Promise<{ equityUsd: Quantity; availableUsd: Quantity }>;
  /** Place an order. Refuses when credentials are absent or the chain id is unconfirmed. */
  placeOrder(request: LighterOrderRequest): Promise<{ orderId?: string; sent: boolean; reason?: string }>;
  cancelOrder(marketId: number, orderId: string): Promise<{ cancelled: boolean; reason?: string }>;
  /** True when a signed write could be sent right now, with the reason when it could not. */
  canTrade(): Promise<{ ok: boolean; reason?: string }>;
  /** The same answer without the promise, plus the sub-account it would use. */
  readiness(): { ok: boolean; reason?: string; accountIndex?: number; apiKeyIndex?: number };
  /** One market by id or ticker. */
  market(idOrSymbol: number | string): Promise<LighterMarket>;
}

/** Position sizing. */
export interface Sizer {
  /**
   * Stock exposure carried by a paired position.
   * `stockLegUsd = qtyX x ratio x reference(stockSymbol)`.
   */
  stockLegForPaired(input: {
    qtyX: number;
    ratio: number;
    stockSymbol: string;
  }): Promise<{ stockLegUsd: Quantity; referencePrice: Quantity }>;
  /** Stock exposure carried by a direct stock token holding, `qty x reference`. */
  stockLegForToken(input: { symbol: string; qty: number }): Promise<{ stockLegUsd: Quantity; referencePrice: Quantity }>;
  /** Round a base size down to the market's `sizeDecimals` and up to `minBaseAmount`. */
  roundSize(sizeBase: number, market: LighterMarket): number;
  /** Full plan for a stock leg, including the action that would close the gap. */
  plan(input: { symbol: string; stockLegUsd: number }): Promise<HedgePlan>;
}

/** Keeps the short within the configured drift of target. */
export interface Rebalancer {
  start(): void;
  stop(): void;
  running(): boolean;
  /** Compare target against current once. Returns the plans it acted on. */
  tick(): Promise<HedgePlan[]>;
  /** Send the plan, when sending is possible. Otherwise returns it with a reason. */
  apply(plan: HedgePlan): Promise<HedgePlan>;
  /** Close the short for a symbol, or every short when no symbol is given. */
  unwind(symbol?: string): Promise<HedgePlan[]>;
}

export interface HedgeModule {
  readonly client: LighterRh;
  readonly sizer: Sizer;
  readonly rebalancer: Rebalancer;
}

export interface HedgeModuleOverrides {
  /** Replace the venue client, for tests and for pointing at another host. */
  readonly client?: LighterRhClient;
  /** Clock, for tests. */
  readonly now?: () => number;
}

/** The hedge module, with the target book the rebalancer works from. */
export interface DeskHedgeModule extends HedgeModule {
  readonly client: LighterRhClient;
  readonly sizer: HedgeSizer;
  readonly rebalancer: DeskRebalancer;
}

/** Build the hedge module: venue client, sizing, and the maintenance loop. */
export function createHedgeModule(deps: HedgeDeps, overrides: HedgeModuleOverrides = {}): DeskHedgeModule {
  const client =
    overrides.client ??
    new LighterRhClient({
      config: deps.config,
      logger: deps.logger,
      keystore: deps.keystore,
      ...(overrides.now === undefined ? {} : { now: overrides.now }),
    });

  const sizer = new HedgeSizer({
    config: deps.config,
    logger: deps.logger,
    fairvalue: deps.fairvalue,
    client,
    ...(overrides.now === undefined ? {} : { now: overrides.now }),
  });

  const rebalancer = new HedgeRebalancer({
    config: deps.config,
    logger: deps.logger,
    store: deps.store,
    client,
    sizer,
    ...(overrides.now === undefined ? {} : { now: overrides.now }),
  });

  return { client, sizer, rebalancer };
}
