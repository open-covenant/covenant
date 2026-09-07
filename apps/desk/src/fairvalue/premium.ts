/**
 * On-chain price against reference price.
 *
 * The premium is the whole product: what the chain charges for a stock token
 * divided by what the stock is worth, in basis points. A positive number means
 * the token trades above the stock, which is the weekend pattern the desk was
 * built for.
 */

import type { Address, FairValue, Hex32, Pool, Quantity } from '../core/types.js';
import { quantity, toRawAmount } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';
import { NotFoundError } from '../core/errors.js';
import type { Premium, ResolvedReference, Session, SizedQuote } from './contracts.js';
import type { DeskReference } from './reference.js';
import { USDG_ADDRESS, byLiquidity, decimalsOf, orientMid, otherCurrency, sameAddress } from './pools.js';

/** How long a USDG/USD reading is reused, ms. */
const USDG_TTL_MS = 30_000;
/** How long the last good USDG/USD reading stands in after a failed read, ms. */
const USDG_GRACE_MS = 10 * 60 * 1000;
/** Pools examined per token when the deepest USDG pool is picked. */
const POOLS_PER_TOKEN = 25;
/** Ceiling on per-token pool lookups when the store holds no pools yet. */
const MAX_FALLBACK_LOOKUPS = 200;
/** USDG pools one ranking pass reads state for. Two chain calls each. */
const STATE_BUDGET = 1500;
/** USDG pools one ranking pass considers, read from the store. */
const MAX_USDG_POOLS = 20_000;
/**
 * How long one symbol's fair value is reused. A recorder pass asks for the
 * same stock leg once per paired pool, and every ask costs a feed read, an
 * issuer read, a venue read, and a pool state read.
 */
const FAIR_VALUE_TTL_MS = 15_000;

export interface PremiumDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
  readonly session: Session;
  readonly reference: DeskReference;
  readonly now?: () => number;
}

/** One stock token and the pool its USD price is read from. */
export interface StockPick {
  readonly symbol: string;
  readonly token: Address;
  readonly pool?: Pool;
}

export interface DeskPremium extends Premium {
  /** USD per USDG from the Chainlink feed, or undefined when it cannot be read. */
  usdgUsd(): Promise<number | undefined>;
  /** Deepest non-trap USDG pool quoting this token. */
  usdgPool(token: Address): Promise<Pool | undefined>;
  /** Stock tokens ranked by the depth of their USDG pool, deepest first. */
  ranked(limit: number): Promise<StockPick[]>;
}

export function createPremium(deps: PremiumDeps): DeskPremium {
  const clock = deps.now ?? Date.now;
  let usdgReading: { value: number; at: number } | undefined;
  const recent = new Map<string, { at: number; value: Promise<FairValue> }>();

  const usdgUsd = async (): Promise<number | undefined> => {
    const now = clock();
    if (usdgReading && now - usdgReading.at < USDG_TTL_MS) return usdgReading.value;
    try {
      const round = await deps.chain.feeds.usdgUsd();
      if (round.price.value > 0) {
        usdgReading = { value: round.price.value, at: now };
        return round.price.value;
      }
      deps.logger.warn('USDG/USD feed returned a non-positive price', { price: round.price.value });
    } catch (error) {
      deps.logger.warn('USDG/USD feed read failed', { reason: describe(error) });
    }
    if (usdgReading && now - usdgReading.at < USDG_GRACE_MS) return usdgReading.value;
    return undefined;
  };

  const usdgPool = async (token: Address): Promise<Pool | undefined> => {
    const pools = await deps.chain.pools.forToken(token, {
      limit: POOLS_PER_TOKEN,
      counterparties: [USDG_ADDRESS],
    });
    const usdg = pools.filter(
      (pool) => pool.trap !== true && sameAddress(otherCurrency(pool, token), USDG_ADDRESS),
    );
    return byLiquidity(usdg)[0];
  };

  const midUsd = async (pool: Pool, token: Address): Promise<Quantity | undefined> => {
    const usdg = await usdgUsd();
    if (usdg === undefined) return undefined;
    try {
      const mid = pool.midPrice ?? deps.chain.pools.midPrice(pool);
      const usdgPerToken = orientMid(pool, token, USDG_ADDRESS, mid.value);
      return quantity(usdgPerToken * usdg, 'USD', 'pool', mid.asOf, {
        ...(mid.blockNumber === undefined ? {} : { blockNumber: mid.blockNumber }),
      });
    } catch (error) {
      deps.logger.warn('pool mid unavailable', { pool: pool.poolId, reason: describe(error) });
      return undefined;
    }
  };

  const computeBps = (onchainMid: number, reference: number, asOf: number = clock()): Quantity => {
    if (!Number.isFinite(reference) || reference <= 0) {
      throw new RangeError(`A premium needs a positive reference price, got ${reference}.`);
    }
    return quantity((onchainMid / reference - 1) * 10_000, 'bps', 'derived', asOf);
  };

  const build = async (
    symbol: string,
    token: Address,
    known: Pool | undefined,
    resolved: ResolvedReference,
    now: number,
  ): Promise<FairValue> => {
    const sessionState = deps.session.state(now);
    const pool = known ?? (await usdgPool(token));
    const onchainMid = pool ? await midUsd(pool, token) : undefined;
    const chosen = resolved.chosen;
    const premiumBps =
      onchainMid && chosen ? computeBps(onchainMid.value, chosen.price.value, now) : undefined;

    return {
      symbol,
      token,
      ...(onchainMid ? { onchainMid } : {}),
      ...(chosen ? { reference: chosen.price, referenceSource: chosen.source } : {}),
      candidates: resolved.candidates,
      ...(premiumBps ? { premiumBps } : {}),
      sessionState,
      ...(pool ? { pool: pool.poolId } : {}),
      ...(onchainMid?.blockNumber === undefined ? {} : { blockNumber: onchainMid.blockNumber }),
      asOf: now,
    };
  };

  const tokenAddress = (symbol: string): Address | undefined => {
    const fromRegistry = safely(() => deps.chain.registry.token(symbol));
    if (fromRegistry?.address) return fromRegistry.address;
    return safely(() => deps.store.getToken(symbol))?.address;
  };

  const ranked = async (limit: number): Promise<StockPick[]> => {
    const pools = await usdgPools();
    const picks = new Map<string, { pick: StockPick; liquidity: bigint }>();

    for (const pool of pools) {
      const token = otherCurrency(pool, USDG_ADDRESS);
      if (!token) continue;
      const symbol = symbolFor(deps, token);
      if (!symbol) continue;
      const liquidity = pool.liquidity ?? 0n;
      const held = picks.get(token);
      if (held && held.liquidity >= liquidity) continue;
      picks.set(token, { pick: { symbol, token, pool }, liquidity });
    }

    if (picks.size === 0) {
      const tokens = safely(() => deps.chain.registry.stockTokens()) ?? [];
      for (const token of tokens.slice(0, MAX_FALLBACK_LOOKUPS)) {
        try {
          const pool = await usdgPool(token.address);
          if (!pool) continue;
          picks.set(token.address.toLowerCase(), {
            pick: { symbol: token.symbol, token: token.address, pool },
            liquidity: pool.liquidity ?? 0n,
          });
        } catch (error) {
          deps.logger.debug('pool lookup failed', { symbol: token.symbol, reason: describe(error) });
        }
      }
      return refreshState(
        deps,
        [...picks.values()]
          .sort((left, right) => compare(right.liquidity, left.liquidity))
          .slice(0, limit)
          .map((entry) => entry.pick),
      );
    }

    return [...picks.values()]
      .sort((left, right) => compare(right.liquidity, left.liquidity))
      .slice(0, limit)
      .map((entry) => entry.pick);
  };

  /**
   * Every USDG pool the desk knows, with fresh state for as many as one pass
   * can afford. Depth is what decides which pool prices a stock token, and a
   * pool whose liquidity has never been read looks empty, so each pass spends
   * half its budget refreshing the pools already known to be deep and half
   * reading pools it has never priced, newest first.
   */
  const usdgPools = async (): Promise<Pool[]> => {
    const stored =
      safely(() =>
        deps.store.listPools({ currency: USDG_ADDRESS, excludeTraps: true, limit: MAX_USDG_POOLS }),
      ) ?? [];
    if (stored.length === 0) return [];

    const priced = stored.filter((pool) => pool.liquidity !== undefined);
    const unread = stored.filter((pool) => pool.liquidity === undefined);
    const half = Math.ceil(STATE_BUDGET / 2);
    const wanted = [
      ...[...priced].sort((left, right) => compare(right.liquidity ?? 0n, left.liquidity ?? 0n)).slice(0, half),
      ...[...unread]
        .sort((left, right) => compare(right.initialBlock, left.initialBlock))
        .slice(0, STATE_BUDGET - Math.min(priced.length, half)),
    ];

    let fresh = new Map<string, Pool>();
    try {
      fresh = new Map(
        (await deps.chain.pools.state(wanted.map((pool) => pool.poolId))).map((pool) => [
          pool.poolId.toLowerCase(),
          pool,
        ]),
      );
    } catch (error) {
      deps.logger.warn('USDG pool state could not be refreshed', { reason: describe(error) });
    }

    return stored
      .map((pool) => fresh.get(pool.poolId.toLowerCase()) ?? pool)
      .filter((pool) => pool.trap !== true && (pool.liquidity ?? 0n) > 0n);
  };

  /**
   * What a given size would actually pay or receive.
   *
   * The mid is the price of an infinitely small trade. A real order pays the
   * pool fee and moves the price, so the size is quoted through the same USDG
   * pool the fair value was read from and the answer is what that size costs.
   */
  const sized = async (
    fair: FairValue,
    options: { amountUsd: number; side?: 'buy' | 'sell' },
  ): Promise<SizedQuote | undefined> => {
    const side = options.side ?? 'buy';
    const amountUsd = options.amountUsd;
    if (!(amountUsd > 0)) return undefined;

    const known = fair.pool ? safely(() => deps.store.getPool(fair.pool as Hex32)) : undefined;
    const pool = known ?? (await usdgPool(fair.token));
    if (!pool) return undefined;
    const usdg = await usdgUsd();
    if (usdg === undefined || !(usdg > 0)) return undefined;

    try {
      if (side === 'buy') {
        const decimals = decimalsOf(pool, USDG_ADDRESS);
        if (decimals === undefined) return undefined;
        const amountIn = toRawAmount((amountUsd / usdg).toFixed(Math.min(decimals, 18)), decimals);
        if (amountIn <= 0n) return undefined;
        const result = await deps.chain.quoter.quoteExactInputSingle({
          tokenIn: USDG_ADDRESS,
          tokenOut: fair.token,
          amountIn,
          poolId: pool.poolId,
        });
        const tokensPerUsdg = result.effectivePrice.value;
        if (!Number.isFinite(tokensPerUsdg) || tokensPerUsdg <= 0) return undefined;
        return {
          price: quantity(usdg / tokensPerUsdg, 'USD', 'derived', clock()),
          amountUsd,
          side,
          pool: pool.poolId,
          note: `What buying ${amountUsd} USD of ${fair.symbol} costs per token in this pool, price impact included.`,
        };
      }

      const reference = fair.onchainMid?.value ?? fair.reference?.value;
      if (reference === undefined || !(reference > 0)) return undefined;
      const decimals = decimalsOf(pool, fair.token);
      if (decimals === undefined) return undefined;
      const amountIn = toRawAmount((amountUsd / reference).toFixed(Math.min(decimals, 18)), decimals);
      if (amountIn <= 0n) return undefined;
      const result = await deps.chain.quoter.quoteExactInputSingle({
        tokenIn: fair.token,
        tokenOut: USDG_ADDRESS,
        amountIn,
        poolId: pool.poolId,
      });
      const usdgPerToken = result.effectivePrice.value;
      if (!Number.isFinite(usdgPerToken) || usdgPerToken <= 0) return undefined;
      return {
        price: quantity(usdgPerToken * usdg, 'USD', 'derived', clock()),
        amountUsd,
        side,
        pool: pool.poolId,
        note: `What selling ${amountUsd} USD of ${fair.symbol} pays per token in this pool, price impact included.`,
      };
    } catch (error) {
      deps.logger.debug('sized stock quote unavailable', {
        symbol: fair.symbol,
        side,
        reason: describe(error),
      });
      return undefined;
    }
  };

  return {
    usdgUsd,
    usdgPool,
    ranked,
    computeBps,
    sized,

    async forSymbol(symbol) {
      const now = clock();
      const upper = symbol.toUpperCase();
      const held = recent.get(upper);
      if (held && now - held.at < FAIR_VALUE_TTL_MS) return held.value;

      const pending = (async () => {
        const resolved = await deps.reference.resolve(upper, now);
        const token = resolved.token ?? tokenAddress(upper);
        if (!token) throw new NotFoundError(`Stock token ${upper}`, { symbol: upper });
        return build(upper, token, undefined, resolved, now);
      })();
      recent.set(upper, { at: now, value: pending });
      pending.catch(() => recent.delete(upper));
      return pending;
    },

    async all(options) {
      const limit = options?.limit ?? deps.config.recorderTopStocks;
      const picks = await ranked(limit);
      const rows: FairValue[] = [];

      for (const pick of picks) {
        const now = clock();
        let resolved: ResolvedReference | undefined;
        try {
          resolved = await deps.reference.resolve(pick.symbol, now);
          rows.push(await build(pick.symbol, pick.token, pick.pool, resolved, now));
        } catch (error) {
          deps.logger.warn('fair value failed for one symbol', {
            symbol: pick.symbol,
            reason: describe(error),
          });
          const chosen = resolved?.chosen;
          rows.push({
            symbol: pick.symbol,
            token: pick.token,
            ...(chosen ? { reference: chosen.price, referenceSource: chosen.source } : {}),
            candidates: resolved?.candidates ?? [],
            sessionState: deps.session.state(now),
            ...(pick.pool ? { pool: pick.pool.poolId } : {}),
            asOf: now,
          });
        }
      }

      return rows;
    },
  };
}

/** Replace the picked pools with a fresh state read, keeping the stored copy on failure. */
async function refreshState(deps: PremiumDeps, picks: readonly StockPick[]): Promise<StockPick[]> {
  const ids = picks.flatMap((pick) => (pick.pool ? [pick.pool.poolId] : []));
  if (ids.length === 0) return [...picks];

  try {
    const fresh = new Map((await deps.chain.pools.state(ids)).map((pool) => [pool.poolId.toLowerCase(), pool]));
    return picks.map((pick) => {
      const updated = pick.pool ? fresh.get(pick.pool.poolId.toLowerCase()) : undefined;
      return updated ? { ...pick, pool: updated } : pick;
    });
  } catch (error) {
    deps.logger.debug('pool state refresh failed, using the stored reading', { reason: describe(error) });
    return [...picks];
  }
}

function symbolFor(deps: PremiumDeps, token: Address): string | undefined {
  const known = safely(() => deps.chain.registry.token(token));
  if (known?.isStockToken) return known.symbol;
  const stored = safely(() => deps.store.getToken(token));
  if (stored?.isStockToken) return stored.symbol;
  return undefined;
}

function compare(left: bigint, right: bigint): number {
  return left > right ? 1 : left < right ? -1 : 0;
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
