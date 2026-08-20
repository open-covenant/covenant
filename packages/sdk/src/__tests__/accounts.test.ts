import { describe, expect, it } from 'vitest';
import { assertHash32, assertSolanaAddress, isSolanaAddress } from '../solana/accounts.js';

const VALID_ADDRESS = '11111111111111111111111111111111';

describe('isSolanaAddress', () => {
  it('accepts a base58 address within the 32..44 length window', () => {
    expect(isSolanaAddress(VALID_ADDRESS)).toBe(true);
  });

  it('rejects out-of-window lengths and non-base58 characters', () => {
    expect(isSolanaAddress('1'.repeat(31))).toBe(false);
    expect(isSolanaAddress('1'.repeat(45))).toBe(false);
    expect(isSolanaAddress('0'.repeat(32))).toBe(false); // 0 is not in the base58 alphabet
  });
});

describe('assertSolanaAddress', () => {
  it('returns the value unchanged for a valid address', () => {
    expect(assertSolanaAddress(VALID_ADDRESS)).toBe(VALID_ADDRESS);
  });

  it('throws for an invalid address and includes the label', () => {
    expect(() => assertSolanaAddress('not-an-address')).toThrow('address must be a Solana base58 address');
    expect(() => assertSolanaAddress('nope', 'agent_pda')).toThrow('agent_pda must be a Solana base58 address');
  });
});

describe('assertHash32', () => {
  it('returns the lowercased hash for valid 64-char hex', () => {
    expect(assertHash32('ab'.repeat(32))).toBe('ab'.repeat(32));
    expect(assertHash32('AB'.repeat(32))).toBe('ab'.repeat(32)); // uppercase normalized to lowercase
  });

  it('throws for the wrong length or non-hex characters', () => {
    expect(() => assertHash32('ab'.repeat(31))).toThrow('hash must be a 32-byte');
    expect(() => assertHash32('zz'.repeat(32))).toThrow('hash must be a 32-byte');
    expect(() => assertHash32('ab'.repeat(32), 'task_id')).not.toThrow();
    expect(() => assertHash32('xx', 'task_id')).toThrow('task_id must be a 32-byte');
  });
});
