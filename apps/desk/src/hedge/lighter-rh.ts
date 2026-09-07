/**
 * Client for the Lighter instance on Robinhood Chain.
 *
 * Reads are public and need no credentials: markets, mark price, funding,
 * positions, and account balances all come from `https://api.rh.lighter.xyz`.
 * Writes are signed, so they need an API key, a sub-account index, and the
 * layer-2 chain id the sequencer expects. When any of those is missing the
 * client says so and sends nothing.
 *
 * Prices on this instance settle in USDG. They are reported here in USD
 * because USDG tracks the dollar and the desk holds a USDG/USD feed on chain
 * 4663 to check that. See `NOTES.md` for what was measured against the live
 * host.
 */

import {
  LighterRestClient,
  TradingClient,
  type HttpTransport,
  type MarketDetail,
  type MarketSpec,
} from 'lighter-agent-sdk';
import type { Config } from '../core/config.js';
import { liveBlockedReason } from '../core/config.js';
import { KeystoreError, NotFoundError, UpstreamError } from '../core/errors.js';
import type { Keystore } from '../core/keystore.js';
import type { Logger } from '../core/logger.js';
import { quantity, type HedgePosition, type Quantity } from '../core/types.js';
import { truncateToStep } from './sizer.js';
import type { LighterMarket, LighterOrderRequest, LighterRh } from './index.js';

/**
 * Layer-2 chain id bound into every signature on the Robinhood instance.
 *
 * Published by the venue at `apidocs.rh.lighter.xyz/docs/get-started`, which
 * gives `466324` where the Lighter mainnet copy of the same page gives `304`.
 * No signature has been settled against it here, because the desk holds no
 * account on this instance. A wrong id is rejected by the sequencer, so it
 * costs a refusal rather than a fill.
 */
export const LIGHTER_RH_CHAIN_ID = 466324;

/** How long a market list is reused before it is read again. */
export const MARKETS_TTL_MS = 15 * 60 * 1000;

/** Everything the client needs, with test seams for the transport and the clock. */
export interface LighterRhOptions {
  readonly config: Config;
  readonly logger: Logger;
  readonly keystore: Keystore;
  /** Supply a read client to point at another host or to serve fixtures. */
  readonly rest?: LighterRestClient;
  /** Transport for the read client the constructor builds. */
  readonly transport?: HttpTransport;
  /** Override the signing chain id. Defaults to {@link LIGHTER_RH_CHAIN_ID}. */
  readonly signingChainId?: number;
  /** Clock, for tests. */
  readonly now?: () => number;
  /** Market cache lifetime. Defaults to {@link MARKETS_TTL_MS}. */
  readonly marketsTtlMs?: number;
  /** Build the signing client. Replaced in tests so nothing ever signs. */
  readonly createTradingClient?: (options: TradingClientSpec) => TradingLike;
  /** Read one JSON document. Replaced in tests so no unit test touches the network. */
  readonly fetchJson?: (url: string) => Promise<unknown>;
}

/** What the client hands the signing layer once every credential is present. */
export interface TradingClientSpec {
  readonly rest: LighterRestClient;
  readonly accountIndex: number;
  readonly apiKeyIndex: number;
  readonly privateKey: string;
  readonly chainId: number;
  readonly maxNotionalUsd: number;
}

/** The slice of the signing client this module uses. */
export interface TradingLike {
  placeOrder(request: {
    market: number;
    side: 'buy' | 'sell';
    size: string;
    type: 'limit' | 'market';
    price?: string;
    reduceOnly?: boolean;
  }): Promise<{ txHash: string | undefined; clientOrderIndex: bigint }>;
  cancelOrder(request: { market: number; orderIndex: string }): Promise<unknown>;
}

/** Credentials and the reason they are not usable, when they are not. */
export interface TradeReadiness {
  readonly ok: boolean;
  readonly reason?: string;
  readonly accountIndex?: number;
  readonly apiKeyIndex?: number;
}

/** Reads and writes against the Lighter instance on Robinhood Chain. */
export class LighterRhClient implements LighterRh {
  readonly #config: Config;
  readonly #logger: Logger;
  readonly #keystore: Keystore;
  readonly #rest: LighterRestClient;
  readonly #now: () => number;
  readonly #ttlMs: number;
  readonly #chainId: number;
  readonly #createTrading: (options: TradingClientSpec) => TradingLike;
  readonly #fetchJson: (url: string) => Promise<unknown>;

  #markets: LighterMarket[] = [];
  #marketsAt = 0;
  #trading: TradingLike | undefined;

  constructor(options: LighterRhOptions) {
    this.#config = options.config;
    this.#logger = options.logger.child({ component: 'hedge.lighter-rh' });
    this.#keystore = options.keystore;
    this.#now = options.now ?? Date.now;
    this.#ttlMs = options.marketsTtlMs ?? MARKETS_TTL_MS;
    this.#chainId = options.signingChainId ?? LIGHTER_RH_CHAIN_ID;
    this.#createTrading = options.createTradingClient ?? defaultTradingClient;
    this.#fetchJson = options.fetchJson ?? fetchJson;
    this.#rest =
      options.rest ??
      new LighterRestClient({
        network: options.config.hedge.lighterBaseUrl,
        ...(options.transport === undefined ? {} : { transport: options.transport }),
      });
  }

  /** The read client, for callers that need a route this class does not wrap. */
  get rest(): LighterRestClient {
    return this.#rest;
  }

  /** Every market the instance lists, perpetuals and spot pairs. */
  async markets(): Promise<LighterMarket[]> {
    const age = this.#now() - this.#marketsAt;
    if (this.#markets.length > 0 && age < this.#ttlMs) return this.#markets;

    const registry = await this.#call('orderBooks', () => this.#rest.loadMarkets(true));
    const margins = await this.#marginFractions();
    this.#markets = registry.all().map((spec) => toLighterMarket(spec, margins.get(spec.marketId)));
    this.#marketsAt = this.#now();
    return this.#markets;
  }

  /** One market by id or ticker. Throws when the instance does not list it. */
  async market(idOrSymbol: number | string): Promise<LighterMarket> {
    const markets = await this.markets();
    const found =
      typeof idOrSymbol === 'number'
        ? markets.find((market) => market.marketId === idOrSymbol)
        : markets.find((market) => market.symbol.toUpperCase() === idOrSymbol.trim().toUpperCase());
    if (found === undefined) {
      throw new NotFoundError(`Market ${String(idOrSymbol)} on the Lighter Robinhood Chain instance`, {
        market: String(idOrSymbol),
      });
    }
    return found;
  }

  /** The perpetual for a stock ticker, or undefined when none is listed. */
  async perpFor(symbol: string): Promise<LighterMarket | undefined> {
    const wanted = symbol.trim().toUpperCase();
    const markets = await this.markets();
    return markets.find((market) => market.kind === 'perp' && market.symbol.toUpperCase() === wanted);
  }

  /** Mark price for one market, USD per unit of base. */
  async markPrice(marketId: number): Promise<Quantity> {
    const detail = await this.#call('orderBookDetails', () => this.#rest.marketDetail(marketId));
    return quantity(detail.stats.markPrice, 'USD', 'lighter-rh', this.#now());
  }

  /**
   * Funding over eight hours, in basis points.
   *
   * Positive means longs pay shorts, so a short hedge is paid to hold the
   * position. The venue quotes this as a fraction of notional over eight hours.
   */
  async funding(marketId: number): Promise<Quantity> {
    const rate = await this.#call('funding-rates', () => this.#rest.fundingRate(marketId, 'lighter'));
    if (rate === undefined) {
      throw new UpstreamError(
        'Lighter Robinhood Chain',
        `no funding quote for market ${marketId}`,
        { marketId },
      );
    }
    return quantity(rate.eightHourFraction * 10_000, 'bps', 'lighter-rh', this.#now());
  }

  /** Open positions on the configured sub-account. Empty when no account is set. */
  async positions(): Promise<HedgePosition[]> {
    const readiness = this.readiness();
    if (readiness.accountIndex === undefined) return [];

    const summary = await this.#call('account', () => this.#rest.account(readiness.accountIndex as number));
    const open = summary.positions.filter((position) => position.size !== 0);
    if (open.length === 0) return [];

    const marks = await this.#markPrices();
    const asOf = this.#now();
    return open.map((position) => {
      const mark = marks.get(position.marketId) ?? position.positionValue / Math.abs(position.size);
      return {
        symbol: position.symbol,
        marketId: position.marketId,
        sizeBase: quantity(position.size, 'token', 'lighter-rh', asOf),
        entryPrice: quantity(position.avgEntryPrice, 'USD', 'lighter-rh', asOf),
        markPrice: quantity(mark, 'USD', 'lighter-rh', asOf),
        unrealizedPnlUsd: quantity(position.unrealizedPnl, 'USD', 'lighter-rh', asOf),
        asOf,
      } satisfies HedgePosition;
    });
  }

  /** Equity and free margin on the configured sub-account. */
  async account(): Promise<{ equityUsd: Quantity; availableUsd: Quantity }> {
    const readiness = this.readiness();
    if (readiness.accountIndex === undefined) {
      throw new KeystoreError(
        'DESK_LIGHTER_RH_ACCOUNT_INDEX is not set, so the desk does not know which sub-account to read. Add it to keys.env.',
        { key: 'DESK_LIGHTER_RH_ACCOUNT_INDEX' },
      );
    }
    const summary = await this.#call('account', () => this.#rest.account(readiness.accountIndex as number));
    const asOf = this.#now();
    return {
      equityUsd: quantity(summary.totalAssetValue, 'USD', 'lighter-rh', asOf),
      availableUsd: quantity(summary.availableBalance, 'USD', 'lighter-rh', asOf),
    };
  }

  /**
   * Place an order.
   *
   * Refuses before anything is signed when the desk is in dry run, when a
   * credential is missing, or when the order is worth more than the caller's
   * own cap. The refusal names what to change.
   */
  async placeOrder(request: LighterOrderRequest): Promise<{ orderId?: string; sent: boolean; reason?: string }> {
    const cap = Math.min(request.maxNotionalUsd, this.#config.hedge.maxNotionalUsd);
    if (!(cap > 0)) {
      return { sent: false, reason: 'The hedge notional cap is zero, so no order can be sent.' };
    }

    const ready = await this.canTrade();
    if (!ready.ok) return { sent: false, reason: ready.reason ?? 'Signed orders are not available.' };

    const readiness = this.readiness();
    const trading = this.#tradingClient(readiness, cap);
    const market = await this.market(request.marketId);
    // Truncate rather than round: an order one step larger than the exposure
    // would leave the position the wrong side of flat.
    const size = truncateToStep(request.sizeBase, market.sizeDecimals).toFixed(market.sizeDecimals);
    if (Number(size) <= 0) {
      return { sent: false, reason: `A size of ${request.sizeBase} is below one step on ${market.symbol}.` };
    }
    const price = request.price === undefined ? undefined : request.price.toFixed(market.priceDecimals);

    const placement = await this.#call('sendTx', () =>
      trading.placeOrder({
        market: request.marketId,
        side: request.side,
        size,
        type: request.price === undefined ? 'market' : 'limit',
        ...(price === undefined ? {} : { price }),
        ...(request.reduceOnly === undefined ? {} : { reduceOnly: request.reduceOnly }),
      }),
    );

    this.#logger.info('hedge order sent', {
      marketId: request.marketId,
      symbol: market.symbol,
      side: request.side,
      size,
      maxNotionalUsd: cap,
    });
    return { sent: true, orderId: placement.txHash ?? placement.clientOrderIndex.toString() };
  }

  /** Cancel a resting order. Refuses for the same reasons as {@link placeOrder}. */
  async cancelOrder(marketId: number, orderId: string): Promise<{ cancelled: boolean; reason?: string }> {
    const ready = await this.canTrade();
    if (!ready.ok) return { cancelled: false, reason: ready.reason ?? 'Signed orders are not available.' };

    const readiness = this.readiness();
    const trading = this.#tradingClient(readiness, this.#config.hedge.maxNotionalUsd);
    await this.#call('sendTx', () => trading.cancelOrder({ market: marketId, orderIndex: orderId }));
    return { cancelled: true };
  }

  /** Whether a signed write could go out right now, and the reason when it could not. */
  async canTrade(): Promise<{ ok: boolean; reason?: string }> {
    const readiness = this.readiness();
    return readiness.ok ? { ok: true } : { ok: false, reason: readiness.reason ?? 'Signed orders are not available.' };
  }

  /**
   * The same check as {@link canTrade}, without the promise, plus the indexes
   * it parsed. Used by the sizer so a plan can name its own blocker.
   */
  readiness(): TradeReadiness {
    const accountIndex = readIndex(this.#keystore.get('DESK_LIGHTER_RH_ACCOUNT_INDEX'));
    const apiKeyIndex = readIndex(this.#keystore.get('DESK_LIGHTER_RH_API_KEY_INDEX'));
    const base = {
      ...(accountIndex === undefined ? {} : { accountIndex }),
      ...(apiKeyIndex === undefined ? {} : { apiKeyIndex }),
    };

    const blocked = liveBlockedReason(this.#config);
    if (blocked !== null) return { ok: false, reason: blocked, ...base };
    if (!this.#keystore.has('DESK_LIGHTER_RH_PRIVATE_KEY')) {
      return {
        ok: false,
        reason:
          'No Lighter Robinhood Chain key. Add DESK_LIGHTER_RH_PRIVATE_KEY to keys.env, then fund the sub-account it belongs to.',
        ...base,
      };
    }
    if (accountIndex === undefined) {
      return {
        ok: false,
        reason: 'DESK_LIGHTER_RH_ACCOUNT_INDEX is missing or is not a whole number. Add it to keys.env.',
        ...base,
      };
    }
    if (apiKeyIndex === undefined) {
      return {
        ok: false,
        reason: 'DESK_LIGHTER_RH_API_KEY_INDEX is missing or is not a whole number. Add it to keys.env.',
        ...base,
      };
    }
    if (!Number.isInteger(this.#chainId) || this.#chainId <= 0) {
      return {
        ok: false,
        reason:
          'The signing chain id for the Lighter Robinhood Chain instance is unknown, so a signature would be rejected.',
        ...base,
      };
    }
    return { ok: true, ...base };
  }

  /** The chain id signatures would bind to. */
  get signingChainId(): number {
    return this.#chainId;
  }

  #tradingClient(readiness: TradeReadiness, maxNotionalUsd: number): TradingLike {
    if (this.#trading !== undefined) return this.#trading;
    this.#trading = this.#createTrading({
      rest: this.#rest,
      accountIndex: readiness.accountIndex as number,
      apiKeyIndex: readiness.apiKeyIndex as number,
      privateKey: this.#keystore.require('DESK_LIGHTER_RH_PRIVATE_KEY'),
      chainId: this.#chainId,
      maxNotionalUsd,
    });
    return this.#trading;
  }

  /** Mark price for every listed perpetual, keyed by market id. */
  async #markPrices(): Promise<Map<number, number>> {
    const details = await this.#call('orderBookDetails', () => this.#rest.orderBookDetails());
    return new Map(details.map((detail: MarketDetail) => [detail.marketId, detail.stats.markPrice]));
  }

  /**
   * Initial margin fraction per market.
   *
   * The typed read client drops this field, and it is the number that decides
   * how much collateral a short of a given size needs, so it is read straight
   * from the details route. A failure here leaves the field unset rather than
   * failing the whole market list.
   */
  async #marginFractions(): Promise<Map<number, number>> {
    const url = new URL('/api/v1/orderBookDetails', this.#config.hedge.lighterBaseUrl).toString();
    try {
      const body = (await this.#fetchJson(url)) as {
        order_book_details?: { market_id: number; default_initial_margin_fraction?: number }[];
      };
      const rows = body.order_book_details ?? [];
      const out = new Map<number, number>();
      for (const row of rows) {
        if (typeof row.default_initial_margin_fraction === 'number') {
          out.set(row.market_id, row.default_initial_margin_fraction);
        }
      }
      return out;
    } catch (error) {
      this.#logger.debug('margin fractions unavailable', { reason: (error as Error).message });
      return new Map();
    }
  }

  async #call<T>(route: string, run: () => Promise<T>): Promise<T> {
    try {
      return await run();
    } catch (error) {
      throw new UpstreamError('Lighter Robinhood Chain', `${route} failed: ${(error as Error).message}`, { route });
    }
  }
}

/** Map a read-client market spec onto the desk's shape. */
export function toLighterMarket(spec: MarketSpec, initialMarginFraction?: number): LighterMarket {
  return {
    marketId: spec.marketId,
    symbol: spec.symbol,
    kind: spec.marketType === 'spot' ? 'spot' : 'perp',
    sizeDecimals: spec.sizeDecimals,
    priceDecimals: spec.priceDecimals,
    minBaseAmount: Number.parseFloat(spec.minBaseAmount),
    ...(initialMarginFraction === undefined ? {} : { initialMarginFraction }),
  };
}

/** Parse a sub-account or API key index. Anything else reads as absent. */
function readIndex(value: string | undefined): number | undefined {
  if (value === undefined) return undefined;
  const parsed = Number(value.trim());
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : undefined;
}

async function fetchJson(url: string): Promise<unknown> {
  const response = await fetch(url, { headers: { accept: 'application/json' } });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

function defaultTradingClient(spec: TradingClientSpec): TradingLike {
  const client = new TradingClient({
    rest: spec.rest,
    accountIndex: spec.accountIndex,
    apiKeyIndex: spec.apiKeyIndex,
    privateKey: spec.privateKey,
    chainId: spec.chainId,
    maxNotionalUsd: spec.maxNotionalUsd,
  });
  return {
    async placeOrder(request) {
      const placement = await client.placeOrder(request);
      return { txHash: placement.txHash, clientOrderIndex: placement.clientOrderIndex };
    },
    async cancelOrder(request) {
      return client.cancelOrder(request);
    },
  };
}
