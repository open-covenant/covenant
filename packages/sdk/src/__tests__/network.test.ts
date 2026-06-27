import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { resolveSolanaNetwork } from '../solana/network.js';

// resolveSolanaNetwork reads process.env per call (programId from
// COVENANT_PROTOCOL_PROGRAM_ID, covntMint from COVNT_MINT), so the resolved
// config is injectable without mocking. Clear the keys that steer it so each
// case starts from a known baseline.
const ENV_KEYS = [
  'COVENANT_PROTOCOL_PROGRAM_ID',
  'NEXT_PUBLIC_COVENANT_PROTOCOL_PROGRAM_ID',
  'COVNT_MINT',
  'NEXT_PUBLIC_COVNT_MINT',
  'COVENANT_SOLANA_CLUSTER',
  'NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER',
];

describe('resolveSolanaNetwork', () => {
  const saved: Record<string, string | undefined> = {};
  beforeEach(() => {
    for (const k of ENV_KEYS) {
      saved[k] = process.env[k];
      delete process.env[k];
    }
  });
  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (saved[k] === undefined) delete process.env[k];
      else process.env[k] = saved[k];
    }
  });

  it('returns the resolved network and skips mint validation when no mint is set', () => {
    const net = resolveSolanaNetwork();
    expect(net.programId).toBeTruthy();
    expect(net.covntMint).toBeNull();
  });

  it('fails closed when the resolved program id is not a Solana address', () => {
    process.env.COVENANT_PROTOCOL_PROGRAM_ID = 'not-an-address';
    expect(() => resolveSolanaNetwork()).toThrow('program id must be a Solana base58 address');
  });

  it('fails closed when a present COVNT mint is not a Solana address', () => {
    process.env.COVNT_MINT = 'definitely-not-base58!!';
    expect(() => resolveSolanaNetwork()).toThrow('COVNT mint must be a Solana base58 address');
  });
});
