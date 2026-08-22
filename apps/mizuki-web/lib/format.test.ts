import { describe, expect, it } from 'vitest';
import {
  formatPercent,
  formatSolLamports,
  formatUsdcAtomic,
  relativeTime,
  truncateAddress,
} from './format';

describe('format helpers', () => {
  it('formats USDC atomic units without floating point loss', () => {
    expect(formatUsdcAtomic('2000000')).toBe('$2');
    expect(formatUsdcAtomic('10500000')).toBe('$10.5');
  });

  it('formats native SOL fees without floating point conversion', () => {
    expect(formatSolLamports('1250000000')).toBe('1.25 SOL');
    expect(formatSolLamports('1')).toBe('0.000000001 SOL');
  });

  it('formats ratios as whole percentages', () => {
    expect(formatPercent(0.997)).toBe('100%');
  });

  it('truncates long addresses', () => {
    expect(truncateAddress('1234567890abcdefghij')).toBe('12345…fghij');
  });

  it('uses relative times for stable activity copy', () => {
    const now = Date.parse('2026-08-22T12:00:00Z');
    expect(relativeTime('2026-08-22T11:30:00Z', now)).toBe('30 minutes ago');
  });
});
