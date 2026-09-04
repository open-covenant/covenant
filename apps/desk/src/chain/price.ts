/**
 * Price arithmetic for Uniswap v4 pools.
 *
 * A v4 pool stores `sqrtPriceX96`, the square root of the price of currency0 in
 * currency1 expressed in the smallest units of each currency, multiplied by
 * 2^96. Two conversions are needed before the number means anything to a
 * person: square it, then move the decimal point by the difference between the
 * two token decimal counts.
 *
 *     price(currency1 per currency0) = (sqrtPriceX96 / 2^96)^2 * 10^(d0 - d1)
 *
 * Every step below stays in `bigint` until the last division, so a pool whose
 * currencies differ by twelve decimals keeps full precision.
 */

/** 2^96, the fixed-point scale v4 stores prices in. */
export const Q96 = 2n ** 96n;
/** 2^192, the scale of `sqrtPriceX96` squared. */
export const Q192 = Q96 * Q96;

/** Bits needed to write a non-negative bigint. */
function bitLength(value: bigint): number {
  return value === 0n ? 0 : value.toString(2).length;
}

/**
 * Divide two bigints into a double without losing the exponent.
 *
 * The whole part converts directly. The remainder is shifted left until it
 * carries the 54 bits a double can hold, divided there, and scaled back by a
 * power of two, which is exact. That keeps 3/4 at exactly 0.75 and still
 * reaches a ratio as small as one part in 10^24, where a fixed-point scale
 * would round to zero. `den` must not be zero.
 */
export function ratioToNumber(num: bigint, den: bigint): number {
  if (den === 0n) throw new RangeError('division by zero');
  const negative = num < 0n !== den < 0n;
  const absNum = num < 0n ? -num : num;
  const absDen = den < 0n ? -den : den;
  const whole = absNum / absDen;
  const remainder = absNum % absDen;
  let value = Number(whole);
  if (remainder !== 0n) {
    const shift = BigInt(bitLength(absDen) - bitLength(remainder) + 54);
    value += Number((remainder << shift) / absDen) * 2 ** -Number(shift);
  }
  return negative ? -value : value;
}

/**
 * Whole units of currency1 per whole unit of currency0.
 *
 * Worked example, the AAPL/USDG reference pool on 4663. USDG sorts below AAPL,
 * so currency0 is USDG with 6 decimals and currency1 is AAPL with 18.
 * `sqrtPriceX96 = 4417497546894993845498601652811337` squares to a raw ratio of
 * about 3.1088e9 AAPL wei per USDG unit, which after `10^(6-18)` is
 * 0.0031088049 AAPL per USDG, or 321.67 USDG per AAPL.
 */
export function sqrtPriceX96ToPrice(
  sqrtPriceX96: bigint,
  decimals0: number,
  decimals1: number,
): number {
  if (sqrtPriceX96 <= 0n)
    throw new RangeError(`sqrtPriceX96 must be positive, got ${sqrtPriceX96}`);
  if (decimals0 < 0 || decimals1 < 0) throw new RangeError('decimals must be >= 0');
  const shift = decimals0 - decimals1;
  const numerator = sqrtPriceX96 * sqrtPriceX96 * (shift > 0 ? 10n ** BigInt(shift) : 1n);
  const denominator = Q192 * (shift < 0 ? 10n ** BigInt(-shift) : 1n);
  return ratioToNumber(numerator, denominator);
}

/** Whole units of currency0 per whole unit of currency1: the inverse mid. */
export function sqrtPriceX96ToInversePrice(
  sqrtPriceX96: bigint,
  decimals0: number,
  decimals1: number,
): number {
  const price = sqrtPriceX96ToPrice(sqrtPriceX96, decimals0, decimals1);
  if (price === 0) throw new RangeError('pool price is zero');
  return 1 / price;
}

/**
 * Price of `tokenIn` denominated in `tokenOut` for a swap of `amountIn` into
 * `amountOut`, with the decimals of each side applied.
 */
export function effectivePriceOf(
  amountIn: bigint,
  decimalsIn: number,
  amountOut: bigint,
  decimalsOut: number,
): number {
  if (amountIn === 0n) throw new RangeError('amountIn must be non-zero');
  const shift = decimalsIn - decimalsOut;
  const numerator = amountOut * (shift > 0 ? 10n ** BigInt(shift) : 1n);
  const denominator = amountIn * (shift < 0 ? 10n ** BigInt(-shift) : 1n);
  return ratioToNumber(numerator, denominator);
}

/** The v4 dynamic-fee flag. A pool carrying it sets its fee through its hook. */
export const DYNAMIC_FEE_FLAG = 0x800000;

/** True when the pool's static fee field is the dynamic-fee flag. */
export function isDynamicFee(fee: number): boolean {
  return (fee & DYNAMIC_FEE_FLAG) !== 0;
}

/**
 * Convert a v4 fee to basis points. Uniswap counts fees in hundredths of a
 * basis point, so 3000 in a pool key is 30 bps and 500000 is 5000 bps.
 */
export function feePipsToBps(pips: number): number {
  return pips / 100;
}

/**
 * The fee a pool is charging right now, in basis points.
 *
 * A static pool charges its key fee. A dynamic pool charges whatever its hook
 * last wrote into slot0, so the slot0 reading wins whenever it is available and
 * the key fee is ignored, since the flag itself is not a fee.
 */
export function currentFeeBps(pool: { fee: number; lpFee?: number }): number | undefined {
  if (pool.lpFee !== undefined) return feePipsToBps(pool.lpFee);
  if (isDynamicFee(pool.fee)) return undefined;
  return feePipsToBps(pool.fee);
}
