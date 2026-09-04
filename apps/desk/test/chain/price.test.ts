import { describe, expect, it } from 'vitest';
import {
  currentFeeBps,
  effectivePriceOf,
  feePipsToBps,
  isDynamicFee,
  Q96,
  ratioToNumber,
  sqrtPriceX96ToInversePrice,
  sqrtPriceX96ToPrice,
} from '../../src/chain/price.js';

/**
 * Fixed vector, read from the AAPL/USDG reference pool on chain 4663 at block
 * 54,444,943 on 2026-09-04. USDG sorts below AAPL, so currency0 is USDG with
 * six decimals and currency1 is AAPL with eighteen.
 */
const AAPL_USDG_SQRT_PRICE = 4417497546894993845498601652811337n;

describe('sqrtPriceX96ToPrice', () => {
  it('prices a six-decimal currency against an eighteen-decimal one', () => {
    const aaplPerUsdg = sqrtPriceX96ToPrice(AAPL_USDG_SQRT_PRICE, 6, 18);
    // (sqrtPriceX96 / 2^96)^2 is 3,108,804,891.087 raw AAPL per raw USDG, which
    // after 10^(6-18) is 0.0031088048910873950 AAPL per USDG.
    expect(aaplPerUsdg).toBeCloseTo(0.003108804891087395, 18);

    const usdgPerAapl = sqrtPriceX96ToInversePrice(AAPL_USDG_SQRT_PRICE, 6, 18);
    expect(usdgPerAapl).toBeCloseTo(321.6670183667335, 10);
  });

  it('prices two eighteen-decimal currencies with no shift', () => {
    expect(sqrtPriceX96ToPrice(Q96, 18, 18)).toBe(1);
    // Four times the square root is sixteen times the price.
    expect(sqrtPriceX96ToPrice(Q96 * 4n, 18, 18)).toBeCloseTo(16, 12);
    expect(sqrtPriceX96ToPrice(Q96 / 2n, 18, 18)).toBeCloseTo(0.25, 12);
  });

  it('keeps the decimal shift and the price independent', () => {
    // The same raw ratio read as 18/6 instead of 6/18 moves twelve decimal
    // places the other way.
    const asSixEighteen = sqrtPriceX96ToPrice(AAPL_USDG_SQRT_PRICE, 6, 18);
    const asEighteenSix = sqrtPriceX96ToPrice(AAPL_USDG_SQRT_PRICE, 18, 6);
    expect(asEighteenSix / asSixEighteen).toBeCloseTo(1e24, -10);
  });

  it('refuses a price of zero', () => {
    expect(() => sqrtPriceX96ToPrice(0n, 18, 18)).toThrow(RangeError);
  });
});

describe('ratioToNumber', () => {
  it('keeps very small and very large ratios', () => {
    expect(ratioToNumber(1n, 10n ** 24n)).toBeCloseTo(1e-24, 30);
    expect(ratioToNumber(10n ** 30n, 1n)).toBeCloseTo(1e30, -20);
    expect(ratioToNumber(-3n, 4n)).toBe(-0.75);
  });

  it('refuses a zero denominator', () => {
    expect(() => ratioToNumber(1n, 0n)).toThrow(RangeError);
  });
});

describe('effectivePriceOf', () => {
  it('reports tokenOut per tokenIn with both decimal counts applied', () => {
    // The live quote on 2026-09-04: 10 USDG bought 0.031071830425342773 AAPL.
    const price = effectivePriceOf(10_000_000n, 6, 31_071_830_425_342_773n, 18);
    expect(price).toBeCloseTo(0.0031071830425342773, 18);
    expect(1 / price).toBeCloseTo(321.8349180949382, 6);
  });

  it('refuses a zero input', () => {
    expect(() => effectivePriceOf(0n, 18, 1n, 18)).toThrow(RangeError);
  });
});

describe('fees', () => {
  it('counts a v4 fee in hundredths of a basis point', () => {
    expect(feePipsToBps(3000)).toBe(30);
    expect(feePipsToBps(500_000)).toBe(5000);
  });

  it('reads the dynamic-fee flag', () => {
    expect(isDynamicFee(0x800000)).toBe(true);
    expect(isDynamicFee(3000)).toBe(false);
  });

  it('prefers the fee slot0 reports over the fee in the key', () => {
    expect(currentFeeBps({ fee: 3000 })).toBe(30);
    expect(currentFeeBps({ fee: 3000, lpFee: 10_000 })).toBe(100);
    // A dynamic pool that has not been read yet has no known fee.
    expect(currentFeeBps({ fee: 0x800000 })).toBeUndefined();
    expect(currentFeeBps({ fee: 0x800000, lpFee: 0 })).toBe(0);
    // The trap pools on 4663 charge between 65% and 90%.
    expect(currentFeeBps({ fee: 0x800000, lpFee: 900_000 })).toBe(9000);
  });
});
