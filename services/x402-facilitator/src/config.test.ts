import { describe, expect, it } from 'vitest';
import { loadConfig } from './config.js';

const solana = {
  COVENANT_FACILITATOR_SOLANA_RPC_URL: 'https://solana.example',
  COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON: JSON.stringify(Array.from({ length: 64 }, (_, i) => i)),
  COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PUBLIC_KEY: '5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva',
};
const base = {
  COVENANT_FACILITATOR_BASE_RPC_URL: 'https://base.example',
  COVENANT_FACILITATOR_BASE_SIGNER_PRIVATE_KEY: `0x${'11'.repeat(32)}`,
  COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS: `0x${'22'.repeat(20)}`,
};
const common = { COVENANT_FACILITATOR_PUBLIC_URL: 'https://facilitator.example/' };

describe('config', () => {
  it('loads both networks with the published defaults', () => {
    const config = loadConfig({ ...common, ...solana, ...base, PORT: '10000' });
    expect(config).toMatchObject({
      port: 10_000,
      publicUrl: 'https://facilitator.example',
      minAmount: 1_000n,
      dailySettlements: 2_000,
      dailySettlementsPerPayTo: 500,
      unmeteredPayTo: [],
    });
    expect(config.solana?.feePayerSecretKey).toHaveLength(64);
    expect(config.base?.signerAddress).toBe(base.COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS);
  });

  it('runs with one network', () => {
    expect(loadConfig({ ...common, ...base }).solana).toBeUndefined();
  });

  it('refuses to start with no network', () => {
    expect(() => loadConfig(common)).toThrow('at least one network');
  });

  it('refuses a half-configured network', () => {
    expect(() =>
      loadConfig({ ...common, ...base, COVENANT_FACILITATOR_SOLANA_RPC_URL: 'https://solana.example' }),
    ).toThrow('Solana needs');
  });

  it('refuses plain http RPCs and malformed keys', () => {
    expect(() => loadConfig({ ...common, ...base, COVENANT_FACILITATOR_BASE_RPC_URL: 'http://base.example' })).toThrow();
    expect(() =>
      loadConfig({ ...common, ...solana, COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON: '[1,2,3]' }),
    ).toThrow();
  });

  it('reads the unmetered receivers as a list', () => {
    const config = loadConfig({ ...common, ...base, COVENANT_FACILITATOR_UNMETERED_PAY_TO: ' 0xaa , Bb11 ,' });
    expect(config.unmeteredPayTo).toEqual(['0xaa', 'Bb11']);
  });
});
