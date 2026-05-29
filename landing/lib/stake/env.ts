import { PublicKey } from "@solana/web3.js";

export const STAKE_PROGRAM_ID = new PublicKey(
  "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED",
);

export const TOKEN_2022_PROGRAM_ID = new PublicKey(
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
);

export const SPL_TOKEN_PROGRAM_ID = new PublicKey(
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
);

export const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey(
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
);

export interface ClusterConfig {
  cluster: "mainnet-beta" | "devnet";
  rpcUrl: string;
  cvntMint: PublicKey;
  tokenProgramId: PublicKey;
  explorerCluster: string;
}

const MAINNET: ClusterConfig = {
  cluster: "mainnet-beta",
  rpcUrl:
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_MAINNET_RPC_URL ??
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ??
    "https://api.mainnet-beta.solana.com",
  cvntMint: new PublicKey("2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump"),
  tokenProgramId: TOKEN_2022_PROGRAM_ID,
  explorerCluster: "mainnet-beta",
};

const DEVNET: ClusterConfig = {
  cluster: "devnet",
  rpcUrl:
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_DEVNET_RPC_URL ??
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL ??
    "https://api.devnet.solana.com",
  cvntMint: new PublicKey("12zLnQiqHLosp4GpAG4b1ZyrcyHJK8863FiDcQZ5Drmd"),
  tokenProgramId: SPL_TOKEN_PROGRAM_ID,
  explorerCluster: "devnet",
};

export function getClusterConfig(): ClusterConfig {
  const env = (
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER ?? "mainnet"
  ).toLowerCase();
  return env === "mainnet" || env === "mainnet-beta" ? MAINNET : DEVNET;
}

export function explorerTxUrl(sig: string): string {
  const { explorerCluster } = getClusterConfig();
  return `https://explorer.solana.com/tx/${sig}?cluster=${explorerCluster}`;
}

export function explorerAddressUrl(addr: string): string {
  const { explorerCluster } = getClusterConfig();
  return `https://explorer.solana.com/address/${addr}?cluster=${explorerCluster}`;
}
