import { describe, expect, it } from 'vitest';
import { assertServerMode, loadConfig } from './config.js';

const TOKEN = 'test-token-with-at-least-thirty-two-characters';

describe('signer configuration', () => {
  it('allows mock mode only on a non-production loopback listener', () => {
    const config = loadConfig({
      NODE_ENV: 'test',
      MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
      MIZUKI_SIGNER_MOCK_MODE: 'true',
      MIZUKI_SIGNER_HOST: '127.0.0.1',
    });
    expect(config.mockMode).toBe(true);
    expect(() => assertServerMode(config)).toThrow(
      'Mock adapters are test-only and cannot start the signer HTTP service',
    );

    expect(() =>
      loadConfig({
        NODE_ENV: 'production',
        MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
        MIZUKI_SIGNER_MOCK_MODE: 'true',
      }),
    ).toThrow('Mock mode is disabled in production');

    expect(() =>
      loadConfig({
        NODE_ENV: 'test',
        MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
        MIZUKI_SIGNER_MOCK_MODE: 'true',
        MIZUKI_SIGNER_HOST: '0.0.0.0',
      }),
    ).toThrow('Mock mode must bind to a loopback address');
  });

  it('rejects an operation limit above the rolling limit', () => {
    expect(() =>
      loadConfig({
        NODE_ENV: 'test',
        MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
        MIZUKI_SIGNER_MOCK_MODE: 'true',
        MIZUKI_OPERATION_LIMIT_USD_CENTS: '2501',
        MIZUKI_REFUND_DAILY_LIMIT_USD_CENTS: '2500',
        MIZUKI_ESCROW_DAILY_LIMIT_USD_CENTS: '2500',
      }),
    ).toThrow('Per-operation limit cannot exceed either rolling daily limit');
  });

  it('fails closed when production dependencies are absent', () => {
    expect(() =>
      loadConfig({
        NODE_ENV: 'production',
        MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
        MIZUKI_SIGNER_MOCK_MODE: 'false',
      }),
    ).toThrow('Missing production signer settings');
  });

  it('requires HTTPS RPC and price feeds in production', () => {
    const base = {
      NODE_ENV: 'production',
      MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
      MIZUKI_SIGNER_MOCK_MODE: 'false',
      MIZUKI_SIGNER_DATABASE_URL: 'postgresql://db.internal/signer?sslmode=require',
      MIZUKI_SIGNER_RPC_URL: 'http://rpc.internal',
      MIZUKI_SIGNER_SECONDARY_RPC_URL: 'https://rpc-secondary.internal',
      MIZUKI_REFUND_PRIVATE_KEY_JSON: `[${Array(64).fill(0).join(',')}]`,
      MIZUKI_ESCROW_PRIVATE_KEY_JSON: `[${Array(64).fill(1).join(',')}]`,
      MIZUKI_SIGNER_GITHUB_TOKEN: 'github-read-only-test-token',
      MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: '5'.repeat(32),
      MIZUKI_REFUND_TREASURY: '2'.repeat(32),
      MIZUKI_ESCROW_AUTHORITY: '6'.repeat(32),
      MIZUKI_REFUND_MINT: '3'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_ID: '4'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_DATA_SHA256: 'a'.repeat(64),
      MIZUKI_SOL_USD_PRICE_URL: 'https://price.internal',
      MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'https://price-secondary.internal',
    };
    expect(() => loadConfig(base)).toThrow(
      'MIZUKI_SIGNER_RPC_URL must use HTTPS unless it targets loopback',
    );
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
        MIZUKI_SOL_USD_PRICE_URL: 'http://price.internal',
      }),
    ).toThrow('MIZUKI_SOL_USD_PRICE_URL must use HTTPS unless it targets loopback');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
        MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'http://price-secondary.internal',
      }),
    ).toThrow('MIZUKI_SOL_USD_SECONDARY_PRICE_URL must use HTTPS unless it targets loopback');
  });

  it('requires database TLS away from loopback and Render private networking', () => {
    expect(() =>
      loadConfig({
        NODE_ENV: 'development',
        MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
        MIZUKI_SIGNER_MOCK_MODE: 'false',
        MIZUKI_SIGNER_DATABASE_URL: 'postgresql://db.internal/signer',
        MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
        MIZUKI_SIGNER_SECONDARY_RPC_URL: 'https://rpc-secondary.internal',
        MIZUKI_REFUND_PRIVATE_KEY_JSON: `[${Array(64).fill(0).join(',')}]`,
        MIZUKI_ESCROW_PRIVATE_KEY_JSON: `[${Array(64).fill(1).join(',')}]`,
        MIZUKI_SIGNER_GITHUB_TOKEN: 'github-read-only-test-token',
        MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: '5'.repeat(32),
        MIZUKI_REFUND_TREASURY: '2'.repeat(32),
        MIZUKI_ESCROW_AUTHORITY: '6'.repeat(32),
        MIZUKI_REFUND_MINT: '3'.repeat(32),
        MIZUKI_ESCROW_PROGRAM_ID: '4'.repeat(32),
        MIZUKI_ESCROW_PROGRAM_DATA_SHA256: 'a'.repeat(64),
        MIZUKI_SOL_USD_PRICE_URL: 'https://price.internal',
        MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'https://price-secondary.internal',
      }),
    ).toThrow('MIZUKI_SIGNER_DATABASE_URL must require TLS');
  });

  it('accepts a Render private database URL without sslmode', () => {
    const config = loadConfig({
      NODE_ENV: 'production',
      MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
      MIZUKI_SIGNER_MOCK_MODE: 'false',
      MIZUKI_SIGNER_DATABASE_URL: 'postgresql://user:password@dpg-cv1234-a:5432/mizuki',
      MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
      MIZUKI_SIGNER_SECONDARY_RPC_URL: 'https://rpc-secondary.internal',
      MIZUKI_REFUND_PRIVATE_KEY_JSON: `[${Array(64).fill(0).join(',')}]`,
      MIZUKI_ESCROW_PRIVATE_KEY_JSON: `[${Array(64).fill(1).join(',')}]`,
      MIZUKI_SIGNER_GITHUB_TOKEN: 'github-read-only-test-token',
      MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: '5'.repeat(32),
      MIZUKI_REFUND_TREASURY: '2'.repeat(32),
      MIZUKI_ESCROW_AUTHORITY: '6'.repeat(32),
      MIZUKI_REFUND_MINT: '3'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_ID: '4'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_DATA_SHA256: 'a'.repeat(64),
      MIZUKI_SOL_USD_PRICE_URL: 'https://price.internal',
      MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'https://price-secondary.internal',
    });

    expect(config.databaseUrl).toBe('postgresql://user:password@dpg-cv1234-a:5432/mizuki');

    expect(() =>
      loadConfig({
        ...{
          NODE_ENV: 'production',
          MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
          MIZUKI_SIGNER_MOCK_MODE: 'false',
          MIZUKI_SIGNER_DATABASE_URL:
            'postgresql://user:password@dpg-cv1234-a.example.com:5432/mizuki',
          MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
          MIZUKI_SIGNER_SECONDARY_RPC_URL: 'https://rpc-secondary.internal',
          MIZUKI_REFUND_PRIVATE_KEY_JSON: `[${Array(64).fill(0).join(',')}]`,
          MIZUKI_ESCROW_PRIVATE_KEY_JSON: `[${Array(64).fill(1).join(',')}]`,
          MIZUKI_SIGNER_GITHUB_TOKEN: 'github-read-only-test-token',
          MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: '5'.repeat(32),
          MIZUKI_REFUND_TREASURY: '2'.repeat(32),
          MIZUKI_ESCROW_AUTHORITY: '6'.repeat(32),
          MIZUKI_REFUND_MINT: '3'.repeat(32),
          MIZUKI_ESCROW_PROGRAM_ID: '4'.repeat(32),
          MIZUKI_ESCROW_PROGRAM_DATA_SHA256: 'a'.repeat(64),
          MIZUKI_SOL_USD_PRICE_URL: 'https://price.internal',
          MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'https://price-secondary.internal',
        },
      }),
    ).toThrow('MIZUKI_SIGNER_DATABASE_URL must require TLS');
  });

  it('requires distinct RPC and price providers', () => {
    const base = {
      NODE_ENV: 'development',
      MIZUKI_SIGNER_AUTH_TOKEN: TOKEN,
      MIZUKI_SIGNER_MOCK_MODE: 'false',
      MIZUKI_SIGNER_DATABASE_URL: 'postgresql://127.0.0.1/signer',
      MIZUKI_SIGNER_RPC_URL: 'https://rpc.internal',
      MIZUKI_SIGNER_SECONDARY_RPC_URL: 'https://rpc-secondary.internal',
      MIZUKI_REFUND_PRIVATE_KEY_JSON: `[${Array(64).fill(0).join(',')}]`,
      MIZUKI_ESCROW_PRIVATE_KEY_JSON: `[${Array(64).fill(1).join(',')}]`,
      MIZUKI_SIGNER_GITHUB_TOKEN: 'github-read-only-test-token',
      MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: '5'.repeat(32),
      MIZUKI_REFUND_TREASURY: '2'.repeat(32),
      MIZUKI_ESCROW_AUTHORITY: '6'.repeat(32),
      MIZUKI_REFUND_MINT: '3'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_ID: '4'.repeat(32),
      MIZUKI_ESCROW_PROGRAM_DATA_SHA256: 'a'.repeat(64),
      MIZUKI_SOL_USD_PRICE_URL: 'https://price.internal',
      MIZUKI_SOL_USD_SECONDARY_PRICE_URL: 'https://price-secondary.internal',
    };
    expect(() =>
      loadConfig({ ...base, MIZUKI_SIGNER_SECONDARY_RPC_URL: base.MIZUKI_SIGNER_RPC_URL }),
    ).toThrow('Primary and secondary RPC URLs must be different');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_SOL_USD_SECONDARY_PRICE_URL: base.MIZUKI_SOL_USD_PRICE_URL,
      }),
    ).toThrow('Primary and secondary price URLs must be different');
  });
});
