/**
 * Quoting through the V4Quoter.
 *
 * The quoter reverts with the answer, so both entry points are declared
 * non-payable and read through `eth_call`. Nothing is signed and no state
 * changes. A quote is what the pool would pay at the head of the current
 * block; the swap path applies slippage to it before anything is sent.
 *
 * Route selection prefers a direct pool. When none exists, the desk tries one
 * hop through the currencies that actually carry liquidity on 4663: USDG,
 * wrapped ether, and the stock token a paired launch is quoted in.
 */

import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import { NotFoundError, TrapPoolError, UpstreamError } from '../core/errors.js';
import { quantity, type Address, type Hex32, type Pool, type Quantity } from '../core/types.js';
import { v4QuoterAbi } from './abis.js';
import type { DeskChainClient } from './client.js';
import { effectivePriceOf } from './price.js';
import { NATIVE_CURRENCY, poolKeyOf, type DeskPools } from './pools.js';

/** Address used when the desk has no key loaded and only needs to read. */
const READ_ONLY_CALLER: Address = '0x0000000000000000000000000000000000000001';

export interface QuoteRequest {
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  /** Amount to sell, smallest units of `tokenIn`. */
  readonly amountIn: bigint;
  /** Force one pool. Otherwise the deepest non-trap route is chosen. */
  readonly poolId?: Hex32;
  /**
   * Force a whole route, one pool id per hop, in the order the swap takes
   * them. A quote and the swap that follows it use the same pools this way.
   */
  readonly route?: readonly Hex32[];
}

export interface QuoteResult {
  readonly amountIn: bigint;
  readonly amountOut: bigint;
  readonly gasEstimate: bigint;
  readonly route: readonly Hex32[];
  /** `tokenOut` per `tokenIn`, decimals applied on both sides. */
  readonly effectivePrice: Quantity;
  readonly blockNumber: bigint;
}

/** One hop of a route, with the pool it goes through. */
export interface RouteHop {
  readonly pool: Pool;
  readonly tokenIn: Address;
  readonly tokenOut: Address;
}

export interface DeskQuoter {
  quoteExactInputSingle(request: QuoteRequest & { poolId: Hex32 }): Promise<QuoteResult>;
  quoteExactInput(request: QuoteRequest): Promise<QuoteResult>;
  /**
   * The hops a swap would take. Used by the swap encoder and by dry runs.
   * Pass `path` to walk a route that was already chosen, or `poolId` to pin a
   * single pool. With neither, the deepest non-trap route is found.
   */
  route(
    tokenIn: Address,
    tokenOut: Address,
    poolId?: Hex32,
    path?: readonly Hex32[],
  ): Promise<RouteHop[]>;
}

export interface QuoterDeps {
  readonly client: DeskChainClient;
  readonly logger: Logger;
  readonly store: Store;
  readonly pools: DeskPools;
  readonly quoterAddress: Address;
  /** Currencies tried as the middle of a two-hop route, in order. */
  readonly intermediates: readonly Address[];
}

export function createQuoter(deps: QuoterDeps): DeskQuoter {
  const logger = deps.logger.child({ component: 'chain.quoter' });

  const decimalsOf = (pool: Pool, token: Address): number =>
    sameAddress(pool.currency0, token) ? pool.decimals0 : pool.decimals1;

  const poolOrThrow = (poolId: Hex32): Pool => {
    const pool = deps.store.getPool(poolId);
    if (!pool) throw new NotFoundError(`pool ${poolId}`, { poolId });
    return pool;
  };

  /** Pools holding both tokens, deepest first, traps left out. */
  const directPools = async (tokenIn: Address, tokenOut: Address): Promise<Pool[]> => {
    const pools = await deps.pools.forToken(tokenIn, { limit: 50 });
    return pools.filter((pool) => holds(pool, tokenOut));
  };

  /** Walk a route that has already been chosen, checking every hop. */
  const hopsAlong = (
    path: readonly Hex32[],
    tokenIn: Address,
    tokenOut: Address,
  ): RouteHop[] => {
    const hops: RouteHop[] = [];
    let current = tokenIn;
    for (const poolId of path) {
      const pool = poolOrThrow(poolId);
      if (!holds(pool, current)) {
        throw new NotFoundError(`a route through pool ${poolId}`, { poolId, tokenIn: current });
      }
      if (deps.pools.isTrap(pool)) throw new TrapPoolError(pool.poolId, feeBpsOf(pool));
      const next = other(pool, current);
      hops.push({ pool, tokenIn: current, tokenOut: next });
      current = next;
    }
    if (!sameAddress(current, tokenOut)) {
      throw new NotFoundError(`a route from ${tokenIn} to ${tokenOut}`, { tokenIn, tokenOut });
    }
    return hops;
  };

  const route = async (
    tokenIn: Address,
    tokenOut: Address,
    poolId?: Hex32,
    path?: readonly Hex32[],
  ): Promise<RouteHop[]> => {
    if (path && path.length > 0) return hopsAlong(path, tokenIn, tokenOut);
    if (poolId) {
      const pool = poolOrThrow(poolId);
      if (!holds(pool, tokenIn) || !holds(pool, tokenOut)) {
        throw new NotFoundError(`a route through pool ${poolId}`, { poolId, tokenIn, tokenOut });
      }
      if (deps.pools.isTrap(pool)) throw new TrapPoolError(pool.poolId, feeBpsOf(pool));
      return [{ pool, tokenIn, tokenOut }];
    }

    const direct = await directPools(tokenIn, tokenOut);
    const best = direct[0];
    if (best) return [{ pool: best, tokenIn, tokenOut }];

    const first = await deps.pools.forToken(tokenIn, { limit: 50 });
    const second = await deps.pools.forToken(tokenOut, { limit: 50 });
    for (const middle of deps.intermediates) {
      const legIn = first.find((pool) => holds(pool, middle));
      const legOut = second.find((pool) => holds(pool, middle));
      if (!legIn || !legOut) continue;
      return [
        { pool: legIn, tokenIn, tokenOut: middle },
        { pool: legOut, tokenIn: middle, tokenOut },
      ];
    }

    // Any currency both sides share, when none of the named ones do.
    for (const legIn of first) {
      const middle = other(legIn, tokenIn);
      const legOut = second.find((pool) => holds(pool, middle) && pool.poolId !== legIn.poolId);
      if (!legOut) continue;
      return [
        { pool: legIn, tokenIn, tokenOut: middle },
        { pool: legOut, tokenIn: middle, tokenOut },
      ];
    }

    throw new NotFoundError(`a pool route from ${tokenIn} to ${tokenOut}`, { tokenIn, tokenOut });
  };

  const callQuoter = async (
    functionName: 'quoteExactInputSingle' | 'quoteExactInput',
    args: unknown[],
  ) => {
    try {
      const { result } = await deps.client.publicClient.simulateContract({
        address: deps.quoterAddress,
        abi: v4QuoterAbi,
        functionName,
        args: args as never,
        account: deps.client.account()?.address ?? READ_ONLY_CALLER,
      });
      return result as readonly [bigint, bigint];
    } catch (error) {
      throw new UpstreamError('v4-quoter', `${functionName} reverted: ${messageOf(error)}`);
    }
  };

  const finish = async (
    request: QuoteRequest,
    hops: readonly RouteHop[],
    amountOut: bigint,
    gasEstimate: bigint,
  ): Promise<QuoteResult> => {
    const firstHop = hops[0];
    const lastHop = hops[hops.length - 1];
    if (!firstHop || !lastHop) throw new NotFoundError('a route', { tokenIn: request.tokenIn });
    const blockNumber = await deps.client.blockNumber();
    const price = effectivePriceOf(
      request.amountIn,
      decimalsOf(firstHop.pool, request.tokenIn),
      amountOut,
      decimalsOf(lastHop.pool, request.tokenOut),
    );
    return {
      amountIn: request.amountIn,
      amountOut,
      gasEstimate,
      route: hops.map((hop) => hop.pool.poolId),
      effectivePrice: { ...quantity(price, 'token', 'pool', Date.now()), blockNumber },
      blockNumber,
    };
  };

  const quoteSingle = async (request: QuoteRequest & { poolId: Hex32 }): Promise<QuoteResult> => {
    const pool = poolOrThrow(request.poolId);
    if (deps.pools.isTrap(pool)) throw new TrapPoolError(pool.poolId, feeBpsOf(pool));
    if (!holds(pool, request.tokenIn) || !holds(pool, request.tokenOut)) {
      throw new NotFoundError(`a route through pool ${request.poolId}`, { poolId: request.poolId });
    }
    const zeroForOne = sameAddress(pool.currency0, request.tokenIn);
    const [amountOut, gasEstimate] = await callQuoter('quoteExactInputSingle', [
      { poolKey: poolKeyOf(pool), zeroForOne, exactAmount: request.amountIn, hookData: '0x' },
    ]);
    logger.debug('single-pool quote', {
      poolId: pool.poolId,
      amountIn: request.amountIn,
      amountOut,
    });
    return finish(
      request,
      [{ pool, tokenIn: request.tokenIn, tokenOut: request.tokenOut }],
      amountOut,
      gasEstimate,
    );
  };

  const quoteRoute = async (request: QuoteRequest): Promise<QuoteResult> => {
    const hops = await route(request.tokenIn, request.tokenOut, request.poolId, request.route);
    if (hops.length === 1) {
      const only = hops[0];
      if (!only) throw new NotFoundError('a route', { tokenIn: request.tokenIn });
      return quoteSingle({ ...request, poolId: only.pool.poolId });
    }
    const [amountOut, gasEstimate] = await callQuoter('quoteExactInput', [
      {
        exactCurrency: request.tokenIn,
        path: hops.map((hop) => pathKeyFor(hop)),
        exactAmount: request.amountIn,
      },
    ]);
    logger.debug('multi-hop quote', { hops: hops.length, amountIn: request.amountIn, amountOut });
    return finish(request, hops, amountOut, gasEstimate);
  };

  return { route, quoteExactInputSingle: quoteSingle, quoteExactInput: quoteRoute };
}

/** One `PathKey`: the currency this hop lands in, plus the pool it goes through. */
export function pathKeyFor(hop: RouteHop): {
  intermediateCurrency: Address;
  fee: number;
  tickSpacing: number;
  hooks: Address;
  hookData: `0x${string}`;
} {
  return {
    intermediateCurrency: hop.tokenOut,
    fee: hop.pool.fee,
    tickSpacing: hop.pool.tickSpacing,
    hooks: hop.pool.hooks,
    hookData: '0x',
  };
}

/** True when the pool holds this currency on either side. */
export function holds(pool: Pool, token: Address): boolean {
  return sameAddress(pool.currency0, token) || sameAddress(pool.currency1, token);
}

/** The currency on the other side of the pool from `token`. */
export function other(pool: Pool, token: Address): Address {
  return sameAddress(pool.currency0, token) ? pool.currency1 : pool.currency0;
}

/** Native ether and the zero address are the same currency in a v4 pool key. */
export function sameAddress(a: Address, b: Address): boolean {
  return a.toLowerCase() === b.toLowerCase();
}

/** True when the currency is native ether. */
export function isNative(token: Address): boolean {
  return sameAddress(token, NATIVE_CURRENCY);
}

function feeBpsOf(pool: Pool): number {
  return (pool.lpFee ?? pool.fee) / 100;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
