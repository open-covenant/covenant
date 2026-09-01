import { describe, expect, it } from 'vitest';
import { loadConfig, SOLANA_MAINNET } from './config.js';

const KEY = JSON.stringify(Array.from({ length: 64 }, (_, index) => index % 256));

function env(overrides: Record<string, string | undefined> = {}): NodeJS.ProcessEnv {
  return {
    MIZUKI_FACILITATOR_RPC_URL: 'https://rpc.example/mainnet',
    MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON: KEY,
    MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY: '5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva',
    MIZUKI_FACILITATOR_TOKEN: 't'.repeat(48),
    ...overrides,
  } as NodeJS.ProcessEnv;
}

describe('facilitator configuration', () => {
  it('defaults the port and network', () => {
    const config = loadConfig(env());

    expect(config.port).toBe(8402);
    expect(config.network).toBe(SOLANA_MAINNET);
    expect(config.feePayerSecretKey).toHaveLength(64);
  });

  it.each([
    ['a key that is not JSON', { MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON: 'not-json' }],
    ['a key of the wrong length', { MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON: '[1,2,3]' }],
    ['a base58 key', { MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON: '5Xmc9QDRLHepaFAq' }],
    ['a missing token', { MIZUKI_FACILITATOR_TOKEN: undefined }],
    ['a short token', { MIZUKI_FACILITATOR_TOKEN: 'too-short' }],
    ['a non-https rpc', { MIZUKI_FACILITATOR_RPC_URL: 'http://rpc.example' }],
    ['a missing public key', { MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY: undefined }],
  ])('refuses to start with %s', (_name, overrides) => {
    expect(() => loadConfig(env(overrides))).toThrow(/facilitator configuration/i);
  });

  it('names the offending variable', () => {
    expect(() => loadConfig(env({ MIZUKI_FACILITATOR_TOKEN: 'short' }))).toThrow(
      /MIZUKI_FACILITATOR_TOKEN/,
    );
  });
});
