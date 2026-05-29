import { describe, it, expect } from 'vitest';
import {
  resolveSynapseConfig,
  synapseExplorerHref,
} from '@covenant/config/networks';

describe('resolveSynapseConfig', () => {
  it('is disabled by default', () => {
    const resolved = resolveSynapseConfig({});
    expect(resolved.enabled).toBe(false);
  });

  it('parses truthy enabled values', () => {
    for (const value of ['1', 'true', 'TRUE', 'yes']) {
      const resolved = resolveSynapseConfig({ COVENANT_SAP_ENABLED: value });
      expect(resolved.enabled).toBe(true);
    }
  });

  it('parses falsy enabled values', () => {
    for (const value of ['0', 'false', 'no', '']) {
      const resolved = resolveSynapseConfig({ COVENANT_SAP_ENABLED: value });
      expect(resolved.enabled).toBe(false);
    }
  });

  it('defaults to the known mainnet program id', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'mainnet',
    });
    expect(resolved.programId).toBe('SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ');
    expect(resolved.network.cluster).toBe('mainnet-beta');
  });

  it('prefers cluster-specific env over the global program id override', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'devnet',
      COVENANT_SAP_PROGRAM_ID: 'GlobalDefaultProgramId',
      COVENANT_SAP_DEVNET_PROGRAM_ID: 'DevnetOnlyProgramId',
    });
    expect(resolved.programId).toBe('DevnetOnlyProgramId');
  });

  it('prefers the overrides argument above every env layer', () => {
    const resolved = resolveSynapseConfig(
      {
        COVENANT_SAP_ENABLED: 'true',
        COVENANT_SAP_PROGRAM_ID: 'env-program',
        COVENANT_SAP_RPC_URL: 'https://env.example/rpc',
      },
      {
        programId: 'override-program',
        rpcUrl: 'https://override.example/rpc',
        enabled: true,
      },
    );
    expect(resolved.programId).toBe('override-program');
    expect(resolved.rpcUrl).toBe('https://override.example/rpc');
  });

  it('falls back to the resolved cluster rpc url when no synapse-specific url is set', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'devnet',
    });
    expect(resolved.rpcUrl).toBe('https://api.devnet.solana.com');
  });
});

describe('synapseExplorerHref', () => {
  it('omits the cluster query on mainnet', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'mainnet',
    });
    const href = synapseExplorerHref('agent', 'AgentPda111', resolved);
    expect(href).toBe('https://explorer.oobeprotocol.ai/agent/AgentPda111');
  });

  it('appends the cluster query on devnet', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'devnet',
    });
    const href = synapseExplorerHref('tx', 'SignatureXYZ', resolved);
    expect(href).toBe(
      'https://explorer.oobeprotocol.ai/tx/SignatureXYZ?cluster=devnet',
    );
  });

  it('honours a custom explorer url', () => {
    const resolved = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SAP_EXPLORER_URL: 'https://staging.synapse.test/',
    });
    const href = synapseExplorerHref('address', 'Wallet11', resolved);
    expect(href.startsWith('https://staging.synapse.test/address/Wallet11')).toBe(true);
  });
});
