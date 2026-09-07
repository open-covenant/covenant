/**
 * Pool orientation helpers.
 *
 * A Uniswap v4 pool prices currency0 in currency1, and which side a token
 * lands on is decided by address order. Everything below turns that into the
 * direction the desk actually wants: how much of one named token buys one of
 * another.
 */

import type { Address, Pool } from '../core/types.js';
import { CONTRACTS } from '../chain/index.js';

/** USDG on chain 4663, lowercase. */
export const USDG_ADDRESS = CONTRACTS.usdg.toLowerCase() as Address;
/** WETH9 on chain 4663, lowercase. */
export const WETH_ADDRESS = CONTRACTS.weth9.toLowerCase() as Address;

/** Case-insensitive address comparison. */
export function sameAddress(left: string | undefined, right: string | undefined): boolean {
  if (!left || !right) return false;
  return left.toLowerCase() === right.toLowerCase();
}

/** The currency on the far side of a pool from `token`, or undefined. */
export function otherCurrency(pool: Pool, token: Address): Address | undefined {
  if (sameAddress(pool.currency0, token)) return pool.currency1.toLowerCase() as Address;
  if (sameAddress(pool.currency1, token)) return pool.currency0.toLowerCase() as Address;
  return undefined;
}

/** In-range liquidity, with a missing reading treated as zero. */
export function liquidityOf(pool: Pool): bigint {
  return pool.liquidity ?? 0n;
}

/** Sort a copy of the pools by in-range liquidity, deepest first. */
export function byLiquidity(pools: readonly Pool[]): Pool[] {
  return [...pools].sort((left, right) => {
    const difference = liquidityOf(right) - liquidityOf(left);
    return difference > 0n ? 1 : difference < 0n ? -1 : 0;
  });
}

/**
 * Units of `quote` that one whole `base` buys, from the pool mid.
 *
 * `mid` is the price of currency0 in currency1, decimals applied on both
 * sides, which is what `Pools.midPrice` returns.
 */
export function orientMid(pool: Pool, base: Address, quote: Address, mid: number): number {
  if (!Number.isFinite(mid) || mid <= 0) {
    throw new RangeError(`Pool ${pool.poolId} reported a mid price of ${mid}.`);
  }
  if (sameAddress(pool.currency0, base) && sameAddress(pool.currency1, quote)) return mid;
  if (sameAddress(pool.currency1, base) && sameAddress(pool.currency0, quote)) return 1 / mid;
  throw new RangeError(`Pool ${pool.poolId} does not hold both ${base} and ${quote}.`);
}

/** Decimals of one side of a pool. */
export function decimalsOf(pool: Pool, token: Address): number | undefined {
  if (sameAddress(pool.currency0, token)) return pool.decimals0;
  if (sameAddress(pool.currency1, token)) return pool.decimals1;
  return undefined;
}
