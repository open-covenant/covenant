// Server-side Solana network resolution for the bot. Mirrors the cluster +
// mint + token-program selection in `landing/lib/stake/env.ts`, but reads
// plain server env (no `NEXT_PUBLIC_` build-time inlining) so the announcer
// can flip clusters via the Render dashboard without a code change.

import { PublicKey } from "@solana/web3.js";
import { SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "./constants.js";

export type ClusterKey = "mainnet" | "devnet" | "localnet";

export interface BotNetwork {
  /** Explorer/Solscan cluster identifier (`mainnet-beta` | `devnet` | `localnet`). */
  cluster: "mainnet-beta" | "devnet" | "localnet";
  clusterKey: ClusterKey;
  rpcUrl: string;
  /** `$CVNT` mint, or null when unconfigured (announcer stays disabled). */
  cvntMint: PublicKey | null;
  /** Token program owning the mint (Token-2022 on mainnet, SPL on devnet). */
  tokenProgramId: PublicKey;
}

const DEFAULT_RPC: Record<ClusterKey, string> = {
  mainnet: "https://api.mainnet-beta.solana.com",
  devnet: "https://api.devnet.solana.com",
  localnet: "http://127.0.0.1:8899",
};

// Per-cluster default mints, matching `landing/lib/stake/env.ts`. Overridable
// via COVNT_MINT so a new deployment never needs a code edit.
const DEFAULT_MINT: Record<ClusterKey, string | null> = {
  mainnet: "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump",
  devnet: "12zLnQiqHLosp4GpAG4b1ZyrcyHJK8863FiDcQZ5Drmd",
  localnet: null,
};

function pickClusterKey(): ClusterKey {
  const raw = (
    process.env.COVENANT_SOLANA_CLUSTER ??
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_CLUSTER ??
    "mainnet"
  )
    .trim()
    .toLowerCase();
  if (raw === "mainnet" || raw === "mainnet-beta") return "mainnet";
  if (raw === "localnet") return "localnet";
  return "devnet";
}

function resolveTokenProgram(clusterKey: ClusterKey): PublicKey {
  const override = process.env.COVNT_TOKEN_PROGRAM_ID?.trim();
  if (override) return new PublicKey(override);
  return clusterKey === "mainnet" ? TOKEN_2022_PROGRAM_ID : SPL_TOKEN_PROGRAM_ID;
}

export function resolveBotNetwork(): BotNetwork {
  const clusterKey = pickClusterKey();
  const cluster = clusterKey === "mainnet" ? "mainnet-beta" : clusterKey;
  const rpcUrl =
    process.env.COVENANT_SOLANA_RPC_URL?.trim() ||
    process.env.NEXT_PUBLIC_COVENANT_SOLANA_RPC_URL?.trim() ||
    DEFAULT_RPC[clusterKey];
  const mintRaw =
    process.env.COVNT_MINT?.trim() ||
    process.env.NEXT_PUBLIC_COVNT_MINT?.trim() ||
    DEFAULT_MINT[clusterKey];
  const cvntMint = mintRaw ? new PublicKey(mintRaw) : null;
  return {
    cluster,
    clusterKey,
    rpcUrl,
    cvntMint,
    tokenProgramId: resolveTokenProgram(clusterKey),
  };
}
