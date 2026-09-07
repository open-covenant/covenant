import { describe, expect, it } from 'vitest';
import { quantity, rawQuantity, toDecimalNumber, toRawAmount } from '../types.js';

describe('toDecimalNumber', () => {
  it('applies decimals', () => {
    expect(toDecimalNumber(1_000_000_000_000_000_000n, 18)).toBe(1);
    expect(toDecimalNumber(1_500_000n, 6)).toBe(1.5);
    expect(toDecimalNumber(0n, 18)).toBe(0);
  });

  it('keeps precision on values above 2^53 in the whole part', () => {
    // 12,345,678 tokens at 18 decimals.
    expect(toDecimalNumber(12_345_678n * 10n ** 18n, 18)).toBe(12_345_678);
  });

  it('handles negative amounts', () => {
    expect(toDecimalNumber(-2_500_000n, 6)).toBe(-2.5);
  });

  it('refuses negative decimals', () => {
    expect(() => toDecimalNumber(1n, -1)).toThrow(RangeError);
  });
});

describe('toRawAmount', () => {
  it('converts decimal strings exactly', () => {
    expect(toRawAmount('1', 18)).toBe(10n ** 18n);
    expect(toRawAmount('0.000001', 6)).toBe(1n);
    expect(toRawAmount('123.456', 6)).toBe(123_456_000n);
  });

  it('truncates digits below the token precision', () => {
    expect(toRawAmount('1.9999999', 2)).toBe(199n);
  });

  it('round trips through toDecimalNumber', () => {
    const raw = toRawAmount('42.5', 18);
    expect(toDecimalNumber(raw, 18)).toBe(42.5);
  });

  it('handles negatives and refuses junk', () => {
    expect(toRawAmount('-1.5', 6)).toBe(-1_500_000n);
    expect(() => toRawAmount('abc', 6)).toThrow(RangeError);
    expect(() => toRawAmount('', 6)).toThrow(RangeError);
  });
});

describe('quantity envelopes', () => {
  it('carries unit, source, and time', () => {
    const q = quantity(101.25, 'USD', 'chainlink', 1_700_000_000_000);
    expect(q).toEqual({ value: 101.25, unit: 'USD', source: 'chainlink', asOf: 1_700_000_000_000 });
  });

  it('keeps the exact raw amount alongside the display value', () => {
    const q = rawQuantity(2_500_000n, 6, 'pool', 1_700_000_000_000);
    expect(q.raw).toBe(2_500_000n);
    expect(q.decimals).toBe(6);
    expect(q.value).toBe(2.5);
    expect(q.unit).toBe('raw');
  });
});
