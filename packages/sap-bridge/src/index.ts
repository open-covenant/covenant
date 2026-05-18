// @covenant/sap-bridge
//
// Thin wrapper over @oobe-protocol-labs/synapse-sap-sdk. The wrapper
// keeps the Synapse SDK as a peer dependency so consumers that opt
// out of the on-chain path do not have to install it. Every method
// gates on `config.enabled` first — callers must be prepared for
// `BridgeDisabledError` and treat it as a soft no-op.

import { resolveSynapseConfig, type ResolvedSynapseConfig } from '@covenant/config/networks';

export { resolveSynapseConfig };
export type { ResolvedSynapseConfig };

export class BridgeDisabledError extends Error {
  constructor() {
    super('synapse bridge is disabled');
    this.name = 'BridgeDisabledError';
  }
}

export interface AgentManifest {
  name: string;
  capabilities: CapabilityDescriptor[];
  pricing: PricingTier[];
  protocols: string[];
}

export interface CapabilityDescriptor {
  id: string;
  protocolId: string;
  version: string;
}

export interface PricingTier {
  id: string;
  priceUsdMicros: number;
  unit: string;
}

export interface PublishedAgent {
  agentPda: string;
  signature: string;
}

export interface PeerRecord {
  agentPda: string;
  display: string;
  protocols: string[];
  reputationScore: number | null;
}

export interface SapBridgeOptions {
  config?: ResolvedSynapseConfig;
}

export interface BridgeStatus {
  enabled: boolean;
  cluster: string;
  programId: string;
  rpcUrl: string;
  explorerUrl: string;
}

export class SapBridge {
  readonly config: ResolvedSynapseConfig;

  constructor(options: SapBridgeOptions = {}) {
    this.config = options.config ?? resolveSynapseConfig();
  }

  requireEnabled(): void {
    if (!this.config.enabled) {
      throw new BridgeDisabledError();
    }
  }

  // Snapshot of the resolved bridge config. Safe to expose over an
  // operator surface — never returns secrets and does not touch the
  // network.
  status(): BridgeStatus {
    return {
      enabled: this.config.enabled,
      cluster: this.config.network.key,
      programId: this.config.programId,
      rpcUrl: this.config.rpcUrl,
      explorerUrl: this.config.explorerUrl,
    };
  }

  async publishAgent(_manifest: AgentManifest): Promise<PublishedAgent> {
    this.requireEnabled();
    throw new Error('not implemented — wiring to synapse-sap-sdk lands in a follow-up');
  }

  async findAgentsByProtocol(_protocol: string): Promise<PeerRecord[]> {
    this.requireEnabled();
    throw new Error('not implemented — wiring to synapse-sap-sdk lands in a follow-up');
  }

  async findAgentByPda(_pda: string): Promise<PeerRecord | null> {
    this.requireEnabled();
    throw new Error('not implemented — wiring to synapse-sap-sdk lands in a follow-up');
  }
}
