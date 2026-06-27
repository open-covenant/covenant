import { describe, it, expect } from 'vitest';
import {
  resolveCovenantNetwork,
  resolveNetworkFromRequestHeaders,
  explorerHref,
} from '@covenant/config/networks';

const DEFAULT_PROGRAM_ID = 'cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y';

describe('resolveCovenantNetwork', () => {
  it('defaults to the devnet network when no cluster is configured', () => {
    const net = resolveCovenantNetwork({});
    expect(net.key).toBe('devnet');
    expect(net.cluster).toBe('devnet');
    expect(net.rpcUrl).toBe('https://api.devnet.solana.com');
    expect(net.wsUrl).toBe('wss://api.devnet.solana.com');
  });

  it('resolves the requested cluster to its on-chain defaults', () => {
    const net = resolveCovenantNetwork({ COVENANT_SOLANA_CLUSTER: 'mainnet' });
    expect(net.key).toBe('mainnet');
    expect(net.cluster).toBe('mainnet-beta');
    expect(net.rpcUrl).toBe('https://api.mainnet-beta.solana.com');
    expect(net.wsUrl).toBe('wss://api.mainnet-beta.solana.com');
  });

  it('falls back to devnet for an unrecognised cluster instead of throwing', () => {
    const net = resolveCovenantNetwork({ COVENANT_SOLANA_CLUSTER: 'staging-typo' });
    expect(net.key).toBe('devnet');
    expect(net.rpcUrl).toBe('https://api.devnet.solana.com');
  });

  it('prefers the public cluster var over the server cluster var', () => {
    const net = resolveCovenantNetwork({
      NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER: 'mainnet',
      COVENANT_SOLANA_CLUSTER: 'localnet',
    });
    expect(net.key).toBe('mainnet');
  });

  it('lets the overrides.cluster argument win over every cluster env var', () => {
    const net = resolveCovenantNetwork(
      { NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER: 'mainnet', COVENANT_SOLANA_CLUSTER: 'mainnet' },
      { cluster: 'localnet' },
    );
    expect(net.key).toBe('localnet');
  });

  it('uses the cluster-specific rpc/ws env vars over the network defaults', () => {
    const net = resolveCovenantNetwork({
      COVENANT_SOLANA_CLUSTER: 'mainnet',
      COVENANT_SOLANA_MAINNET_RPC_URL: 'https://rpc.mainnet.internal',
      COVENANT_SOLANA_MAINNET_WS_URL: 'wss://ws.mainnet.internal',
    });
    expect(net.rpcUrl).toBe('https://rpc.mainnet.internal');
    expect(net.wsUrl).toBe('wss://ws.mainnet.internal');
  });

  it('applies the generic rpc/ws env when no cluster-specific override exists', () => {
    const net = resolveCovenantNetwork({
      COVENANT_SOLANA_CLUSTER: 'mainnet',
      COVENANT_SOLANA_RPC_URL: 'https://rpc.generic.internal',
      COVENANT_SOLANA_WS_URL: 'wss://ws.generic.internal',
    });
    expect(net.rpcUrl).toBe('https://rpc.generic.internal');
    expect(net.wsUrl).toBe('wss://ws.generic.internal');
  });

  it('defaults the protocol program id and leaves the covnt mint unset', () => {
    const net = resolveCovenantNetwork({});
    expect(net.programId).toBe(DEFAULT_PROGRAM_ID);
    expect(net.covntMint).toBeNull();
  });

  it('prefers env-provided program id and covnt mint over the defaults', () => {
    const net = resolveCovenantNetwork({
      COVENANT_PROTOCOL_PROGRAM_ID: 'EnvProgramId',
      COVNT_MINT: 'EnvCovntMint',
    });
    expect(net.programId).toBe('EnvProgramId');
    expect(net.covntMint).toBe('EnvCovntMint');
  });
});

describe('resolveNetworkFromRequestHeaders', () => {
  it('routes by the cluster from a Headers instance', () => {
    const net = resolveNetworkFromRequestHeaders(
      new Headers({ 'x-covenant-cluster': 'mainnet' }),
      {},
    );
    expect(net.key).toBe('mainnet');
  });

  it('routes by the lowercase plain-object header', () => {
    const net = resolveNetworkFromRequestHeaders({ 'x-covenant-cluster': 'localnet' }, {});
    expect(net.key).toBe('localnet');
  });

  it('routes by the capitalised plain-object header', () => {
    const net = resolveNetworkFromRequestHeaders({ 'X-Covenant-Cluster': 'mainnet' }, {});
    expect(net.key).toBe('mainnet');
  });

  it('defaults to devnet when the request carries no cluster header', () => {
    expect(resolveNetworkFromRequestHeaders(new Headers(), {}).key).toBe('devnet');
    expect(resolveNetworkFromRequestHeaders({}, {}).key).toBe('devnet');
    expect(resolveNetworkFromRequestHeaders(null, {}).key).toBe('devnet');
  });

  it('falls through to the env cluster when no header overrides it', () => {
    const net = resolveNetworkFromRequestHeaders({}, { COVENANT_SOLANA_CLUSTER: 'mainnet' });
    expect(net.key).toBe('mainnet');
  });
});

describe('explorerHref', () => {
  it('omits the cluster query on mainnet', () => {
    const mainnet = resolveCovenantNetwork({ COVENANT_SOLANA_CLUSTER: 'mainnet' });
    expect(explorerHref('tx', 'Sig111', mainnet)).toBe('https://explorer.solana.com/tx/Sig111');
  });

  it('appends the cluster query for non-mainnet clusters', () => {
    const devnet = resolveCovenantNetwork({});
    expect(explorerHref('address', 'Wallet11', devnet)).toBe(
      'https://explorer.solana.com/address/Wallet11?cluster=devnet',
    );
  });

  it('strips a trailing slash from the explorer base url', () => {
    const net = { ...resolveCovenantNetwork({}), explorerUrl: 'https://explorer.test/' };
    expect(explorerHref('tx', 'Sig222', net)).toBe('https://explorer.test/tx/Sig222?cluster=devnet');
  });
});
