export type SolanaAddress = string;

export interface CovenantSolanaNetwork {
  key: string;
  name: string;
  cluster: string;
  explorerUrl: string;
  defaultRpcUrl: string;
  defaultWsUrl: string;
}

export interface ResolvedCovenantSolanaNetwork extends CovenantSolanaNetwork {
  rpcUrl: string;
  wsUrl: string;
  programId: string;
  covntMint: string | null;
}

export interface ResolvedSynapseConfig {
  enabled: boolean;
  network: ResolvedCovenantSolanaNetwork;
  programId: string;
  rpcUrl: string;
  explorerUrl: string;
}

export declare const SOLANA_ADDRESS_REGEX: RegExp;
export declare const DEFAULT_PROTOCOL_PROGRAM_ID: string;
export declare const DEFAULT_SYNAPSE_PROGRAM_ID: string;
export declare const COVENANT_CLUSTER_HEADER: 'x-covenant-cluster';
export declare const covenantSolanaNetworks: Record<string, Readonly<CovenantSolanaNetwork>>;
export function resolveCovenantNetwork(
  env?: Record<string, string | undefined>,
  overrides?: {
    cluster?: string;
    rpcUrl?: string;
    wsUrl?: string;
    programId?: string;
    covntMint?: string;
  },
): ResolvedCovenantSolanaNetwork;
export function resolveNetworkFromRequestHeaders(
  headers: Headers | Record<string, string | undefined> | null | undefined,
  env?: Record<string, string | undefined>,
): ResolvedCovenantSolanaNetwork;
export function explorerHref(
  kind: 'address' | 'tx',
  value: string,
  network?: ResolvedCovenantSolanaNetwork,
): string;
export function resolveSynapseConfig(
  env?: Record<string, string | undefined>,
  overrides?: {
    enabled?: boolean;
    programId?: string;
    rpcUrl?: string;
    explorerUrl?: string;
    network?: {
      cluster?: string;
      rpcUrl?: string;
      wsUrl?: string;
      programId?: string;
      covntMint?: string;
    };
  },
): ResolvedSynapseConfig;
export function synapseExplorerHref(
  kind: 'address' | 'tx' | 'agent',
  value: string,
  synapse?: ResolvedSynapseConfig,
): string;
