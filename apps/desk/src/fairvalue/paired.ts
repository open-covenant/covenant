/**
 * Tokens quoted in a stock token.
 *
 * A memecoin paired with NVDA has no USD price of its own. It has a ratio to
 * NVDA, and NVDA has two prices: the one the chain is charging and the one the
 * stock is worth. Multiply the ratio by each and the holder can see how much
 * of what they are paying is the coin and how much is the stock leg riding
 * above fair value.
 */

import type { Address, FairValue, PairedQuote, Pool, Quantity, RouteSummary } from '../core/types.js';
import { quantity, toRawAmount } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';
import { NotFoundError, NoReferenceError, UpstreamError } from '../core/errors.js';
import type { Paired } from './contracts.js';
import type { DeskPremium } from './premium.js';
import {
  USDG_ADDRESS,
  WETH_ADDRESS,
  byLiquidity,
  decimalsOf,
  liquidityOf,
  orientMid,
  otherCurrency,
  sameAddress,
} from './pools.js';

/** Pools examined per token. Multipool launches use up to five stock pools. */
const POOLS_PER_TOKEN = 25;

export interface PairedDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
  readonly premium: DeskPremium;
  readonly now?: () => number;
}

interface Leg {
  readonly quote: PairedQuote;
  readonly liquidity: bigint;
  /** True when the ratio came from a sized quote instead of the pool mid. */
  readonly sized: boolean;
  /** USD per whole token from the pool mid, with no fee and no price impact. */
  readonly usdMid: number;
}

/** How a quote was asked for: the size in USD and the direction. */
export interface QuoteOptions {
  readonly stockSymbol?: string;
  readonly amountUsd?: number;
  readonly side?: 'buy' | 'sell';
}

export function createPaired(deps: PairedDeps): Paired {
  const clock = deps.now ?? Date.now;

  /** Every stock token the registry knows, so a pool lookup can ask for those pairs only. */
  const stockCurrencies = (): Address[] =>
    (safely(() => deps.chain.registry.stockTokens()) ?? []).map(
      (stock) => stock.address.toLowerCase() as Address,
    );

  const stockPools = async (token: Address): Promise<Pool[]> => {
    const counterparties = stockCurrencies();
    const pools = await deps.chain.pools.forToken(token, {
      limit: POOLS_PER_TOKEN,
      ...(counterparties.length > 0 ? { counterparties } : {}),
    });
    const quoted = pools.filter((pool) => {
      if (pool.trap === true) return false;
      const other = otherCurrency(pool, token);
      return other !== undefined && stockSymbolOf(deps, other) !== undefined;
    });
    if (quoted.length === 0) {
      throw new NotFoundError(`A stock pool quoting ${token}`, { token });
    }
    return byLiquidity(quoted);
  };

  /** Fair value per stock symbol, read once per call. */
  const fairValues = () => {
    const cache = new Map<string, Promise<FairValue>>();
    return (symbol: string): Promise<FairValue> => {
      const held = cache.get(symbol);
      if (held) return held;
      const pending = deps.premium.forSymbol(symbol);
      cache.set(symbol, pending);
      return pending;
    };
  };

  const buildLeg = async (
    token: Address,
    pool: Pool,
    fairValue: (symbol: string) => Promise<FairValue>,
    now: number,
    options: QuoteOptions = {},
  ): Promise<Leg> => {
    const stockToken = otherCurrency(pool, token);
    if (!stockToken) throw new NotFoundError(`Token ${token} in pool ${pool.poolId}`, { token, pool: pool.poolId });
    const stockSymbol = stockSymbolOf(deps, stockToken);
    if (!stockSymbol) throw new NotFoundError(`A stock token at ${stockToken}`, { token: stockToken });

    const stock = await fairValue(stockSymbol);
    if (!stock.reference) {
      throw new NoReferenceError(
        stockSymbol,
        stock.candidates.map((candidate) => candidate.source),
      );
    }
    if (!stock.onchainMid) {
      throw new UpstreamError('pool', `No on-chain price for ${stockSymbol}, so the pair cannot be priced in USD.`, {
        symbol: stockSymbol,
      });
    }

    const mid = pool.midPrice ?? deps.chain.pools.midPrice(pool);
    const midRatio = orientMid(pool, token, stockToken, mid.value);
    const usdMid = midRatio * stock.onchainMid.value;
    let ratioValue = midRatio;
    let ratioSource: Quantity['source'] = 'pool';
    let sized = false;

    const amountUsd = options.amountUsd;
    if (amountUsd !== undefined && amountUsd > 0) {
      const quoted = await quotedRatio(deps, {
        token,
        stockToken,
        pool,
        stockUsd: stock.onchainMid.value,
        usdPerToken: usdMid,
        amountUsd,
        side: options.side ?? 'buy',
      });
      if (quoted !== undefined) {
        ratioValue = quoted;
        ratioSource = 'derived';
        sized = true;
      }
    }

    const usdOnchain = ratioValue * stock.onchainMid.value;
    const usdFair = ratioValue * stock.reference.value;
    const premiumBps =
      stock.premiumBps ?? deps.premium.computeBps(stock.onchainMid.value, stock.reference.value, now);

    const quote: PairedQuote = {
      token,
      symbol: symbolOf(deps, token),
      stockSymbol,
      stockToken,
      pool: pool.poolId,
      ratio: quantity(ratioValue, 'token', ratioSource, now),
      usdOnchain: quantity(usdOnchain, 'USD', 'derived', now),
      usdFair: quantity(usdFair, 'USD', 'derived', now),
      stockLegPremiumBps: premiumBps,
      asOf: now,
    };

    return { quote, liquidity: liquidityOf(pool), sized, usdMid };
  };

  const buildLegs = async (
    token: Address,
    pools: readonly Pool[],
    fairValue: (symbol: string) => Promise<FairValue>,
    now: number,
  ): Promise<Leg[]> => {
    const legs: Leg[] = [];
    const failures: string[] = [];
    for (const pool of pools) {
      try {
        legs.push(await buildLeg(token, pool, fairValue, now));
      } catch (error) {
        failures.push(`${pool.poolId}: ${describe(error)}`);
        deps.logger.debug('paired leg failed', { pool: pool.poolId, reason: describe(error) });
      }
    }
    if (legs.length === 0) {
      throw new UpstreamError('pool', `No stock pool for ${token} could be priced. ${failures.join('; ')}`, { token });
    }
    return legs;
  };

  /**
   * Put the routes side by side.
   *
   * Two prices only compare when they were measured the same way. A sized
   * quote carries the pool fee and the price impact of that size; a mid does
   * not. When one route can be sized and the other cannot, both are compared
   * at their mids and the note says the impact is missing from both.
   */
  const assemble = async (
    token: Address,
    legs: readonly Leg[],
    chosen: Leg,
    now: number,
    options: QuoteOptions = {},
  ): Promise<PairedQuote> => {
    const weighted = weightedFair(legs, now);
    const viaWeth = await wethRoute(deps, token, now, options);
    const amountUsd = options.amountUsd;
    const bothSized = chosen.sized && (viaWeth?.sized ?? false);
    const compareMids = viaWeth !== undefined && !bothSized;
    const impactNote = compareMids ? ' Price impact is not included for either route.' : '';

    const stockRoute: RouteSummary = {
      pools: [chosen.quote.pool],
      path: [token, chosen.quote.stockToken],
      usdPrice: compareMids
        ? quantity(chosen.usdMid, 'USD', 'derived', now)
        : chosen.quote.usdOnchain,
      ...(chosen.liquidity > 0n ? { liquidity: chosen.liquidity } : {}),
      note:
        chosen.sized && !compareMids
          ? `Quoted in ${chosen.quote.stockSymbol} for ${amountUsd} USD, price impact included.`
          : `Quoted in ${chosen.quote.stockSymbol} at the pool mid.${impactNote}`,
    };

    let ethRoute: RouteSummary | undefined;
    if (viaWeth) {
      ethRoute = compareMids
        ? { ...viaWeth.route, usdPrice: viaWeth.usdMid, note: `Quoted in ETH at the pool mid.${impactNote}` }
        : viaWeth.route;
    }

    const routes = ethRoute ? [stockRoute, ethRoute] : [stockRoute];
    const cheapest = routes.reduce((low, route) => (route.usdPrice.value < low.usdPrice.value ? route : low));
    const richest = routes.reduce((high, route) => (route.usdPrice.value > high.usdPrice.value ? route : high));
    const alternatives = legs.filter((leg) => leg !== chosen).map((leg) => leg.quote);

    return {
      ...chosen.quote,
      ...(viaWeth ? { usdViaWeth: viaWeth.usd } : {}),
      bestEntryRoute: cheapest,
      bestExitRoute: richest,
      ...(alternatives.length > 0 ? { alternatives } : {}),
      ...(weighted ? { weightedUsdFair: weighted } : {}),
    };
  };

  return {
    async quote(token, options: QuoteOptions = {}) {
      const now = clock();
      const fairValue = fairValues();
      const pools = await stockPools(token);
      const wanted = options.stockSymbol?.toUpperCase();

      const target = wanted
        ? pools.find((pool) => {
            const other = otherCurrency(pool, token);
            return other !== undefined && stockSymbolOf(deps, other) === wanted;
          })
        : pools[0];
      if (!target) {
        throw new NotFoundError(`A ${wanted} pool quoting ${token}`, { token, ...(wanted ? { stock: wanted } : {}) });
      }

      const chosen = await buildLeg(token, target, fairValue, now, options);
      const rest = pools.filter((pool) => pool.poolId !== target.poolId);
      const others = rest.length > 0 ? await buildLegs(token, rest, fairValue, now).catch(() => [] as Leg[]) : [];

      return assemble(token, [chosen, ...others], chosen, now, options);
    },

    async quoteAll(token) {
      const now = clock();
      const fairValue = fairValues();
      const pools = await stockPools(token);
      const legs = await buildLegs(token, pools, fairValue, now);
      const weighted = weightedFair(legs, now);
      return legs.map((leg) => ({
        ...leg.quote,
        ...(weighted ? { weightedUsdFair: weighted } : {}),
      }));
    },
  };
}

/** Liquidity-weighted fair value across every stock pool quoting the token. */
function weightedFair(legs: readonly Leg[], now: number): Quantity | undefined {
  if (legs.length === 0) return undefined;
  const total = legs.reduce((sum, leg) => sum + leg.liquidity, 0n);
  if (total === 0n) {
    const mean = legs.reduce((sum, leg) => sum + leg.quote.usdFair.value, 0) / legs.length;
    return quantity(mean, 'USD', 'derived', now);
  }
  const scale = Number(total);
  const value = legs.reduce((sum, leg) => sum + leg.quote.usdFair.value * (Number(leg.liquidity) / scale), 0);
  return quantity(value, 'USD', 'derived', now);
}

/** Price through the deepest WETH pool, when the token has one. */
async function wethRoute(
  deps: PairedDeps,
  token: Address,
  now: number,
  options: QuoteOptions = {},
): Promise<{ usd: Quantity; usdMid: Quantity; sized: boolean; route: RouteSummary } | undefined> {
  try {
    const pools = await deps.chain.pools.forToken(token, {
      limit: POOLS_PER_TOKEN,
      counterparties: [WETH_ADDRESS],
    });
    const weth = byLiquidity(
      pools.filter((pool) => pool.trap !== true && sameAddress(otherCurrency(pool, token), WETH_ADDRESS)),
    )[0];
    if (!weth) return undefined;

    const ethUsd = await deps.chain.feeds.ethUsd();
    const mid = weth.midPrice ?? deps.chain.pools.midPrice(weth);
    const ethPerTokenMid = orientMid(weth, token, WETH_ADDRESS, mid.value);
    const usdMid = quantity(ethPerTokenMid * ethUsd.price.value, 'USD', 'derived', now);

    let ethPerToken = ethPerTokenMid;
    let sized = false;
    if (options.amountUsd !== undefined && options.amountUsd > 0) {
      const quoted = await quotedRatio(deps, {
        token,
        stockToken: WETH_ADDRESS,
        pool: weth,
        stockUsd: ethUsd.price.value,
        usdPerToken: usdMid.value,
        amountUsd: options.amountUsd,
        side: options.side ?? 'buy',
      });
      if (quoted !== undefined) {
        ethPerToken = quoted;
        sized = true;
      }
    }
    const usd = quantity(ethPerToken * ethUsd.price.value, 'USD', 'derived', now);

    return {
      usd,
      usdMid,
      sized,
      route: {
        pools: [weth.poolId],
        path: [token, WETH_ADDRESS],
        usdPrice: usd,
        ...(liquidityOf(weth) > 0n ? { liquidity: liquidityOf(weth) } : {}),
        note: sized
          ? `Quoted in ETH for ${options.amountUsd} USD, price impact included.`
          : 'Quoted in ETH at the pool mid.',
      },
    };
  } catch (error) {
    deps.logger.debug('ETH route unavailable', { token, reason: describe(error) });
    return undefined;
  }
}

/**
 * Ratio from a real quote for the requested size, so a thin pool shows its
 * price impact. A buy prices the quote currency into the token and inverts the
 * result; a sell prices the token into the quote currency and reads it
 * directly. Returns undefined when the quoter cannot answer, and the caller
 * falls back to the pool mid.
 */
async function quotedRatio(
  deps: PairedDeps,
  input: {
    token: Address;
    /** The currency the pool quotes in: a stock token, or wrapped ether. */
    stockToken: Address;
    pool: Pool;
    /** USD per whole unit of the quote currency. */
    stockUsd: number;
    /** USD per whole token at the pool mid, used to size a sell. */
    usdPerToken: number;
    amountUsd: number;
    side: 'buy' | 'sell';
  },
): Promise<number | undefined> {
  const { token, stockToken, pool, stockUsd, usdPerToken, amountUsd, side } = input;
  try {
    if (side === 'sell') {
      if (!(usdPerToken > 0)) return undefined;
      const decimals = decimalsOf(pool, token);
      if (decimals === undefined) return undefined;
      const amountIn = toRawAmount((amountUsd / usdPerToken).toFixed(Math.min(decimals, 18)), decimals);
      if (amountIn <= 0n) return undefined;

      const result = await deps.chain.quoter.quoteExactInputSingle({
        tokenIn: token,
        tokenOut: stockToken,
        amountIn,
        poolId: pool.poolId,
      });
      const stockPerToken = result.effectivePrice.value;
      if (!Number.isFinite(stockPerToken) || stockPerToken <= 0) return undefined;
      return stockPerToken;
    }

    if (!(stockUsd > 0)) return undefined;
    const decimals = decimalsOf(pool, stockToken);
    if (decimals === undefined) return undefined;
    const amountIn = toRawAmount((amountUsd / stockUsd).toFixed(Math.min(decimals, 18)), decimals);
    if (amountIn <= 0n) return undefined;

    const result = await deps.chain.quoter.quoteExactInputSingle({
      tokenIn: stockToken,
      tokenOut: token,
      amountIn,
      poolId: pool.poolId,
    });
    const tokensPerStock = result.effectivePrice.value;
    if (!Number.isFinite(tokensPerStock) || tokensPerStock <= 0) return undefined;
    return 1 / tokensPerStock;
  } catch (error) {
    deps.logger.debug('sized quote unavailable, using the pool mid', {
      pool: pool.poolId,
      side,
      reason: describe(error),
    });
    return undefined;
  }
}

/** Symbol of a stock token, or undefined when the address is not one. */
function stockSymbolOf(deps: PairedDeps, token: Address): string | undefined {
  if (sameAddress(token, USDG_ADDRESS) || sameAddress(token, WETH_ADDRESS)) return undefined;
  const known = safely(() => deps.chain.registry.token(token));
  if (known?.isStockToken) return known.symbol.toUpperCase();
  const stored = safely(() => deps.store.getToken(token));
  if (stored?.isStockToken) return stored.symbol.toUpperCase();
  return undefined;
}

/** Best known symbol for any token, falling back to a short address. */
function symbolOf(deps: PairedDeps, token: Address): string {
  const known = safely(() => deps.chain.registry.token(token))?.symbol;
  if (known) return known;
  const stored = safely(() => deps.store.getToken(token))?.symbol;
  if (stored) return stored;
  return `${token.slice(0, 6)}...${token.slice(-4)}`;
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function safely<T>(read: () => T): T | undefined {
  try {
    return read();
  } catch {
    return undefined;
  }
}
