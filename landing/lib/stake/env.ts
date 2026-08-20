import { Connection, PublicKey } from "@solana/web3.js";

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

const TRANSIENT_STATUS = new Set([408, 425, 429, 500, 502, 503, 504]);
const REQUEST_TIMEOUT_MS = 20_000;
const MAX_RETRIES = 4;

// Public Solana RPC routinely answers heavy reads with a 504 / -32504
// "Request timed out". web3.js surfaces that as a hard throw, so wrap fetch
// with bounded exponential backoff + a per-attempt timeout. Resubmitting a
// signed transaction is idempotent (the network dedups by signature), so
// retrying send/confirm here is safe too.
const retryingFetch: typeof fetch = async (input, init) => {
  let lastErr: unknown;
  for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
    try {
      const res = await fetch(input, { ...init, signal: controller.signal });
      if (TRANSIENT_STATUS.has(res.status) && attempt < MAX_RETRIES) {
        await backoff(attempt);
        continue;
      }
      return res;
    } catch (e) {
      lastErr = e;
      if (attempt >= MAX_RETRIES) break;
      await backoff(attempt);
    } finally {
      clearTimeout(timer);
    }
  }
  throw lastErr ?? new Error("rpc fetch failed after retries");
};

function backoff(attempt: number): Promise<void> {
  const base = Math.min(250 * 2 ** attempt, 4_000);
  return new Promise((r) => setTimeout(r, base + Math.random() * 250));
}

let _readConnection: Connection | null = null;
export function getReadConnection(): Connection {
  if (_readConnection) return _readConnection;
  _readConnection = new Connection(getClusterConfig().rpcUrl, {
    commitment: "confirmed",
    fetch: retryingFetch,
    confirmTransactionInitialTimeout: 60_000,
  });
  return _readConnection;
}

export function explorerTxUrl(sig: string): string {
  const { explorerCluster } = getClusterConfig();
  return `https://explorer.solana.com/tx/${sig}?cluster=${explorerCluster}`;
}

export function explorerAddressUrl(addr: string): string {
  const { explorerCluster } = getClusterConfig();
  return `https://explorer.solana.com/address/${addr}?cluster=${explorerCluster}`;
}
