// Resolves the SAID bridge config from environment variables. Mirrors
// `Config::from_env` in the Rust `covenant-said-bridge` crate — the
// daemon and worker must read the same variables to stay consistent.

export type Cluster = 'devnet' | 'localnet' | 'mainnet';

export interface SaidConfig {
  enabled: boolean;
  cluster: Cluster;
  programId: string;
  rpcUrl: string;
  apiBaseUrl: string;
  paid: {
    register: boolean;
    verify: boolean;
    anchor: boolean;
    validateWork: boolean;
  };
}

const DEFAULT_MAINNET_PROGRAM_ID = '5dpw6KEQPn248pnkkaYyWfHwu2nfb3LUMbTucb6LaA8G';
const DEFAULT_DEVNET_PROGRAM_ID = 'ESPreFucjVwtDmZbhtL3JLJ9VxCethNEYtosMQhkcurv';
const DEFAULT_API_BASE_URL = 'https://api.saidprotocol.com';

function parseBool(value: string | undefined): boolean {
  if (!value) return false;
  const v = value.trim().toLowerCase();
  return v === '1' || v === 'true' || v === 'yes';
}

function parseCluster(value: string | undefined): Cluster {
  switch ((value ?? '').toLowerCase()) {
    case 'mainnet':
    case 'mainnet-beta':
      return 'mainnet';
    case 'localnet':
      return 'localnet';
    case 'devnet':
    default:
      return 'devnet';
  }
}

function defaultProgramId(cluster: Cluster): string {
  return cluster === 'mainnet' ? DEFAULT_MAINNET_PROGRAM_ID : DEFAULT_DEVNET_PROGRAM_ID;
}

function defaultRpcUrl(cluster: Cluster): string {
  switch (cluster) {
    case 'mainnet':
      return 'https://api.mainnet-beta.solana.com';
    case 'localnet':
      return 'http://127.0.0.1:8899';
    case 'devnet':
    default:
      return 'https://api.devnet.solana.com';
  }
}

export function resolveSaidConfig(env: NodeJS.ProcessEnv): SaidConfig {
  const cluster = parseCluster(env.COVENANT_SOLANA_CLUSTER);
  const upper = cluster.toUpperCase();

  const enabled = parseBool(
    env[`COVENANT_SAID_${upper}_ENABLED`] ?? env.COVENANT_SAID_ENABLED,
  );
  const programId =
    env[`COVENANT_SAID_${upper}_PROGRAM_ID`] ??
    env.COVENANT_SAID_PROGRAM_ID ??
    defaultProgramId(cluster);
  const rpcUrl =
    env[`COVENANT_SAID_${upper}_RPC_URL`] ??
    env.COVENANT_SAID_RPC_URL ??
    defaultRpcUrl(cluster);
  const apiBaseUrl = env.COVENANT_SAID_API_BASE_URL ?? DEFAULT_API_BASE_URL;

  return {
    enabled,
    cluster,
    programId,
    rpcUrl,
    apiBaseUrl,
    paid: {
      register: parseBool(env.COVENANT_SAID_ALLOW_PAID_REGISTER),
      verify: parseBool(env.COVENANT_SAID_ALLOW_PAID_VERIFY),
      anchor: parseBool(env.COVENANT_SAID_ALLOW_PAID_ANCHOR),
      validateWork: parseBool(env.COVENANT_SAID_ALLOW_PAID_VALIDATE),
    },
  };
}
