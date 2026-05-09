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
});
