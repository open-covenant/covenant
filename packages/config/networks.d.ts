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

export declare const SOLANA_ADDRESS_REGEX: RegExp;
export declare const DEFAULT_PROTOCOL_PROGRAM_ID: string;
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
