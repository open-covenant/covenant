import { describe, expect, it } from 'vitest';
import { encodeGroth16ProofHex, encodePublicInputWords } from '../proof-format.js';

describe('proof formatting', () => {
  it('encodes groth16 proof bytes in canonical verifier order', () => {
    const proof = {
      pi_a: ['1', '2', '1'] as [string, string, string],
      pi_b: [['3', '4'], ['5', '6'], ['1', '0']] as [[string, string], [string, string], [string, string]],
      pi_c: ['7', '8', '1'] as [string, string, string],
      protocol: 'groth16',
      curve: 'bn128',
    };

    expect(encodeGroth16ProofHex(proof)).toBe(
      [
        '1',
        '2',
        '4',
        '3',
        '6',
        '5',
        '7',
        '8',
      ].map((value) => BigInt(value).toString(16).padStart(64, '0')).join(''),
    );
  });

  it('encodes public signals as field words', () => {
    expect(encodePublicInputWords(['9', '10'])).toEqual([
      BigInt(9).toString(16).padStart(64, '0'),
      BigInt(10).toString(16).padStart(64, '0'),
    ]);
  });

  it('rejects field elements outside the uint256 range and accepts the inclusive endpoints', () => {
    const UINT256_MAX = (1n << 256n) - 1n;
    expect(() => encodePublicInputWords([-1n])).toThrow(/out of uint256 range/);
    expect(() => encodePublicInputWords([UINT256_MAX + 1n])).toThrow(/out of uint256 range/);
    // 0 and the maximal uint256 are valid field elements: the bounds are
    // `< 0` / `> MAX`, not `<= 0` / `>= MAX`. An over-range value would
    // otherwise survive into padWord and emit a word longer than 64 hex chars.
    expect(() => encodePublicInputWords([0n, UINT256_MAX])).not.toThrow();
  });

  it('rejects malformed groth16 proof component shapes', () => {
    const valid = {
      pi_a: ['1', '2', '1'],
      pi_b: [['3', '4'], ['5', '6'], ['1', '0']],
      pi_c: ['7', '8', '1'],
      protocol: 'groth16',
      curve: 'bn128',
    };
    expect(() => encodeGroth16ProofHex({ ...valid, pi_a: ['1', '2'] } as any)).toThrow(/pi_a length mismatch/);
    expect(() => encodeGroth16ProofHex({ ...valid, pi_b: [['3'], ['5', '6'], ['1', '0']] } as any)).toThrow(/pi_b\[0\] length mismatch/);
  });
});
