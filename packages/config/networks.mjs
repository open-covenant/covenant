export const SOLANA_ADDRESS_REGEX = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

export const covenantSolanaNetworks = Object.freeze({
  devnet: Object.freeze({
    key: 'devnet',
    name: 'Solana Devnet',
    cluster: 'devnet',
    explorerUrl: 'https://explorer.solana.com',
    defaultRpcUrl: 'https://api.devnet.solana.com',
    defaultWsUrl: 'wss://api.devnet.solana.com',
  }),
  localnet: Object.freeze({
    key: 'localnet',
    name: 'Solana Localnet',
    cluster: 'localnet',
    explorerUrl: 'http://localhost:8899',
    defaultRpcUrl: 'http://127.0.0.1:8899',
    defaultWsUrl: 'ws://127.0.0.1:8900',
  }),
  mainnet: Object.freeze({
    key: 'mainnet',
    name: 'Solana Mainnet',
    cluster: 'mainnet-beta',
    explorerUrl: 'https://explorer.solana.com',
    defaultRpcUrl: 'https://api.mainnet-beta.solana.com',
    defaultWsUrl: 'wss://api.mainnet-beta.solana.com',
  }),
});

export const DEFAULT_PROTOCOL_PROGRAM_ID = 'CovntSettLement1111111111111111111111111111';

// Static NEXT_PUBLIC_* references so Next.js inlines every cluster's URL
// into the client bundle at build time. The browser can then flip clusters
// at runtime (via overrides.cluster) without a redeploy. Dynamic property
// access via `env[`NEXT_PUBLIC_..._${cluster}_...`]` would not be inlined.
const NEXT_PUBLIC_CLUSTER_RPC_URLS = Object.freeze({
  devnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_DEVNET_RPC_URL,
  localnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_LOCALNET_RPC_URL,
  mainnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL,
});
const NEXT_PUBLIC_CLUSTER_WS_URLS = Object.freeze({
  devnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_DEVNET_WS_URL,
  localnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_LOCALNET_WS_URL,
  mainnet: process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_WS_URL,
});

export function resolveCovenantNetwork(env = process.env, overrides = {}) {
  const selected =
    overrides.cluster ??
    env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER ??
    env.COVENANT_SOLANA_CLUSTER ??
    'devnet';
  const network = covenantSolanaNetworks[selected] ?? covenantSolanaNetworks.devnet;
  const upper = network.key.toUpperCase();
  return {
    ...network,
    rpcUrl:
      overrides.rpcUrl ??
      NEXT_PUBLIC_CLUSTER_RPC_URLS[network.key] ??
      env[`COVENANT_SOLANA_${upper}_RPC_URL`] ??
      env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ??
      env.COVENANT_SOLANA_RPC_URL ??
      network.defaultRpcUrl,
    wsUrl:
      overrides.wsUrl ??
      NEXT_PUBLIC_CLUSTER_WS_URLS[network.key] ??
      env[`COVENANT_SOLANA_${upper}_WS_URL`] ??
      env.NEXT_PUBLIC_COVENANT_SOLANA_WS_URL ??
      env.COVENANT_SOLANA_WS_URL ??
      network.defaultWsUrl,
    programId:
      overrides.programId ??
      env.NEXT_PUBLIC_COVENANT_PROTOCOL_PROGRAM_ID ??
      env.COVENANT_PROTOCOL_PROGRAM_ID ??
      DEFAULT_PROTOCOL_PROGRAM_ID,
    covntMint:
      overrides.covntMint ?? env.NEXT_PUBLIC_COVNT_MINT ?? env.COVNT_MINT ?? null,
  };
}

export const COVENANT_CLUSTER_HEADER = 'x-covenant-cluster';

// Server-side helper: pull the cluster override from a request's headers
// (accepts a Fetch `Headers` instance or a plain object) and resolve the
// matching network. Lets an API route flip clusters per-request without
// a Render redeploy — the operator just sets `X-Covenant-Cluster: devnet`
// (or mainnet, localnet) on the inbound call.
export function resolveNetworkFromRequestHeaders(headers, env = process.env) {
  let cluster;
  if (headers && typeof headers.get === 'function') {
    cluster = headers.get(COVENANT_CLUSTER_HEADER) ?? undefined;
  } else if (headers && typeof headers === 'object') {
    cluster =
      headers[COVENANT_CLUSTER_HEADER] ??
      headers['X-Covenant-Cluster'] ??
      undefined;
  }
  return resolveCovenantNetwork(env, cluster ? { cluster } : {});
}

export function explorerHref(kind, value, network = resolveCovenantNetwork()) {
  const clusterQuery = network.key === 'mainnet' ? '' : `?cluster=${network.cluster}`;
  return `${network.explorerUrl.replace(/\/$/, '')}/${kind}/${value}${clusterQuery}`;
}
