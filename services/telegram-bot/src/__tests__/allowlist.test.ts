import { describe, expect, it } from 'vitest';
import { parseAllowlist } from '../index.js';

describe('parseAllowlist', () => {
  it('denies everyone when the allowlist is unset or blank', () => {
    expect(parseAllowlist(undefined).size).toBe(0);
    expect(parseAllowlist('').size).toBe(0);
    expect(parseAllowlist('   ').size).toBe(0);
  });

  it('splits, trims, and coerces comma-separated numeric ids', () => {
    expect(parseAllowlist('111, 222 ,333')).toEqual(new Set([111, 222, 333]));
  });

  it('drops empty entries from trailing or repeated commas', () => {
    expect(parseAllowlist('111,,222,')).toEqual(new Set([111, 222]));
  });

  it('rejects non-positive, non-integer, and non-numeric entries', () => {
    expect(parseAllowlist('0,-5,abc,1.5,222')).toEqual(new Set([222]));
  });
});
