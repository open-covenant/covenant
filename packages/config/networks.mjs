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

export function resolveCovenantNetwork(env = process.env, overrides = {}) {
  const selected =
    overrides.cluster ??
    env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER ??
    env.COVENANT_SOLANA_CLUSTER ??
    'devnet';
  const network = covenantSolanaNetworks[selected] ?? covenantSolanaNetworks.devnet;
  return {
    ...network,
    rpcUrl:
      overrides.rpcUrl ??
      env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ??
      env.COVENANT_SOLANA_RPC_URL ??
      network.defaultRpcUrl,
    wsUrl:
      overrides.wsUrl ??
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

export function explorerHref(kind, value, network = resolveCovenantNetwork()) {
  const clusterQuery = network.key === 'mainnet' ? '' : `?cluster=${network.cluster}`;
  return `${network.explorerUrl.replace(/\/$/, '')}/${kind}/${value}${clusterQuery}`;
}
