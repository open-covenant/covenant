import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { solanaExplorerHref } from '../solana/network.js';

// solanaExplorerHref is the SDK's public block-explorer link builder. It wraps
// the config explorerHref with the per-call resolved network, so the kind/value
// routing and the resolved-cluster composition (a non-mainnet cluster appends
// ?cluster=, mainnet omits it) are its own contract — pinned by nothing.
const CLUSTER_KEYS = ['NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER', 'COVENANT_SOLANA_CLUSTER'] as const;
const ADDRESS = '11111111111111111111111111111111';
const SIGNATURE = 'sig123abc';

describe('solanaExplorerHref', () => {
  const saved: Record<string, string | undefined> = {};

  beforeEach(() => {
    for (const key of CLUSTER_KEYS) {
      saved[key] = process.env[key];
      delete process.env[key];
    }
  });

  afterEach(() => {
    for (const key of CLUSTER_KEYS) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  });

  it('routes address vs tx into the explorer path and stamps the resolved cluster', () => {
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER = 'devnet';
    expect(solanaExplorerHref('address', ADDRESS)).toContain(`/address/${ADDRESS}?cluster=devnet`);
    expect(solanaExplorerHref('tx', SIGNATURE)).toContain(`/tx/${SIGNATURE}?cluster=devnet`);
  });

  it('omits the cluster query for mainnet, proving it uses the resolved network', () => {
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER = 'mainnet';
    const href = solanaExplorerHref('address', ADDRESS);
    expect(href).toContain(`/address/${ADDRESS}`);
    expect(href).not.toContain('?cluster=');
  });
});
