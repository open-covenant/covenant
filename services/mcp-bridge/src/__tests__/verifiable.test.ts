import { describe, expect, it } from 'vitest';
import { z } from 'zod';
import { buildSolanaProposal } from '../verifiable.js';
import { callDaemonTool } from '../daemon.js';
import { tools } from '../server.js';

const PROGRAM = '11111111111111111111111111111111';
const PAYER = 'So11111111111111111111111111111111111111112';

function validArgs(overrides: Record<string, unknown> = {}) {
  return {
    programId: PROGRAM,
    instruction: 'transfer',
    accounts: [{ name: 'payer', address: PAYER, signer: true, writable: true }],
    data: { amount: 1000 },
    ...overrides,
  };
}

describe('solana_propose_tx', () => {
  it('is listed alongside fetch_witness_proof', () => {
    const names = tools.map((tool) => tool.name);
    expect(names).toContain('solana_propose_tx');
    expect(names).toContain('fetch_witness_proof');
  });

  it('builds an unsigned devnet bundle by default', () => {
    const bundle = buildSolanaProposal(validArgs());
    expect(bundle.chain).toBe('solana');
    expect(bundle.cluster).toBe('devnet');
    expect(bundle.rpcUrl).toBe('https://api.devnet.solana.com');
    expect(bundle.instructions).toHaveLength(1);
    expect(bundle.instructions[0]).toMatchObject({ programId: PROGRAM, instruction: 'transfer' });
  });

  it('resolves localnet and mainnet cluster keys', () => {
    expect(buildSolanaProposal(validArgs({ cluster: 'localnet' })).rpcUrl).toBe('http://127.0.0.1:8899');
    expect(buildSolanaProposal(validArgs({ cluster: 'mainnet' })).cluster).toBe('mainnet-beta');
  });

  it('honors an explicit rpcUrl', () => {
    expect(buildSolanaProposal(validArgs({ rpcUrl: 'https://example.devnet' })).rpcUrl).toBe(
      'https://example.devnet',
    );
  });

  it('resolves the cluster from COVENANT_SOLANA_CLUSTER when no arg is given', () => {
    const bundle = buildSolanaProposal(validArgs(), { COVENANT_SOLANA_CLUSTER: 'localnet' });
    expect(bundle.cluster).toBe('localnet');
    expect(bundle.rpcUrl).toBe('http://127.0.0.1:8899');
  });

  it('lets an explicit cluster arg override the env pin', () => {
    const bundle = buildSolanaProposal(validArgs({ cluster: 'devnet' }), {
      COVENANT_SOLANA_CLUSTER: 'mainnet',
    });
    expect(bundle.cluster).toBe('devnet');
  });

  it('rejects a non-base58 programId', () => {
    expect(() => buildSolanaProposal(validArgs({ programId: 'not-an-address' }))).toThrow(z.ZodError);
  });

  it('rejects an empty accounts list', () => {
    expect(() => buildSolanaProposal(validArgs({ accounts: [] }))).toThrow(z.ZodError);
  });

  it('rejects unknown top-level arguments', () => {
    expect(() => buildSolanaProposal(validArgs({ extra: true }))).toThrow(z.ZodError);
  });
});

describe('fetch_witness_proof', () => {
  it('queries the daemon /verify endpoint with the window', async () => {
    let seen: URL | undefined;
    const fetchImpl = ((input: RequestInfo | URL) => {
      seen = input instanceof URL ? input : new URL(String(input));
      return Promise.resolve(
        new Response(JSON.stringify({ verdict: 'verifiable' }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      );
    }) as typeof fetch;

    const result = await callDaemonTool(
      'fetch_witness_proof',
      { window: 25 },
      { env: { COVENANT_AUTH_TOKEN: 'test-token' }, fetchImpl },
    );

    expect(result?.isError).toBeUndefined();
    expect(seen?.pathname).toBe('/verify');
    expect(seen?.searchParams.get('window')).toBe('25');
    expect(result?.content[0]?.text).toContain('verifiable');
  });

  it('rejects a non-positive window before calling the daemon', async () => {
    let called = false;
    const fetchImpl = (() => {
      called = true;
      return Promise.resolve(new Response('{}'));
    }) as typeof fetch;

    const result = await callDaemonTool(
      'fetch_witness_proof',
      { window: 0 },
      { env: { COVENANT_AUTH_TOKEN: 'test-token' }, fetchImpl },
    );

    expect(called).toBe(false);
    expect(result?.isError).toBe(true);
  });
});
