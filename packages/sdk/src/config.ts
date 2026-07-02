// Inlined from the workspace-internal `@covenant/config` package (private and
// unpublished, so it cannot be a dependency of a published SDK). Only the pieces
// the SDK needs, keeping the same env-var names and defaults as the rest of the
// Covenant stack.

export const SOLANA_ADDRESS_REGEX = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

export const DEFAULT_PROTOCOL_PROGRAM_ID = 'cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y';

interface ClusterConfig {
  key: string;
  name: string;
  cluster: string;
  explorerUrl: string;
  defaultRpcUrl: string;
  defaultWsUrl: string;
}

const NETWORKS: Record<string, ClusterConfig> = {
  devnet: {
    key: 'devnet',
    name: 'Solana Devnet',
    cluster: 'devnet',
    explorerUrl: 'https://explorer.solana.com',
    defaultRpcUrl: 'https://api.devnet.solana.com',
    defaultWsUrl: 'wss://api.devnet.solana.com',
  },
  localnet: {
    key: 'localnet',
    name: 'Solana Localnet',
    cluster: 'localnet',
    explorerUrl: 'http://localhost:8899',
    defaultRpcUrl: 'http://127.0.0.1:8899',
    defaultWsUrl: 'ws://127.0.0.1:8900',
  },
  mainnet: {
    key: 'mainnet',
    name: 'Solana Mainnet',
    cluster: 'mainnet-beta',
    explorerUrl: 'https://explorer.solana.com',
    defaultRpcUrl: 'https://api.mainnet-beta.solana.com',
    defaultWsUrl: 'wss://api.mainnet-beta.solana.com',
  },
};

export interface ResolvedCovenantSolanaNetwork extends ClusterConfig {
  rpcUrl: string;
  wsUrl: string;
  programId: string;
  covntMint: string | null;
}

export interface NetworkOverrides {
  cluster?: string;
  rpcUrl?: string;
  wsUrl?: string;
  programId?: string;
  covntMint?: string;
}

type Env = Record<string, string | undefined>;

function processEnv(): Env {
  return typeof process !== 'undefined' && process.env ? process.env : {};
}

// Per-cluster NEXT_PUBLIC_* URLs read as static `process.env.LITERAL` references
// so a bundler (Next.js) can inline them into a browser build. A dynamic
// `env[`...${cluster}...`]` key is never replaced. Guarded so a plain browser
// bundle without `process` is safe.
const NEXT_PUBLIC_RPC: Record<string, string | undefined> =
  typeof process === 'undefined'
    ? {}
    : {
        devnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_DEVNET_RPC_URL,
        localnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_LOCALNET_RPC_URL,
        mainnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL,
      };
const NEXT_PUBLIC_WS: Record<string, string | undefined> =
  typeof process === 'undefined'
    ? {}
    : {
        devnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_DEVNET_WS_URL,
        localnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_LOCALNET_WS_URL,
        mainnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_WS_URL,
      };

export function resolveCovenantNetwork(
  env: Env = processEnv(),
  overrides: NetworkOverrides = {},
): ResolvedCovenantSolanaNetwork {
  const selected =
    overrides.cluster ??
    env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER ??
    env.COVENANT_SOLANA_CLUSTER ??
    'devnet';
  const network: ClusterConfig = Object.prototype.hasOwnProperty.call(NETWORKS, selected)
    ? NETWORKS[selected]!
    : NETWORKS.devnet!;
  const upper = network.key.toUpperCase();
  return {
    ...network,
    rpcUrl:
      overrides.rpcUrl ??
      NEXT_PUBLIC_RPC[network.key] ??
      env[`COVENANT_SOLANA_${upper}_RPC_URL`] ??
      env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ??
      env.COVENANT_SOLANA_RPC_URL ??
      network.defaultRpcUrl,
    wsUrl:
      overrides.wsUrl ??
      NEXT_PUBLIC_WS[network.key] ??
      env[`COVENANT_SOLANA_${upper}_WS_URL`] ??
      env.NEXT_PUBLIC_COVENANT_SOLANA_WS_URL ??
      env.COVENANT_SOLANA_WS_URL ??
      network.defaultWsUrl,
    programId:
      overrides.programId ??
      env.NEXT_PUBLIC_COVENANT_PROTOCOL_PROGRAM_ID ??
      env.COVENANT_PROTOCOL_PROGRAM_ID ??
      DEFAULT_PROTOCOL_PROGRAM_ID,
    covntMint: overrides.covntMint ?? env.NEXT_PUBLIC_COVNT_MINT ?? env.COVNT_MINT ?? null,
  };
}

export function explorerHref(
  kind: 'address' | 'tx',
  value: string,
  network: ResolvedCovenantSolanaNetwork = resolveCovenantNetwork(),
): string {
  const clusterQuery = network.key === 'mainnet' ? '' : `?cluster=${network.cluster}`;
  return `${network.explorerUrl.replace(/\/$/, '')}/${kind}/${value}${clusterQuery}`;
}

export const COVENANT_TOKEN_SYMBOL: string =
  (typeof process !== 'undefined' ? process.env.NEXT_PUBLIC_COVENANT_TOKEN_SYMBOL : undefined) ??
  processEnv().COVENANT_TOKEN_SYMBOL ??
  'CVNT';
