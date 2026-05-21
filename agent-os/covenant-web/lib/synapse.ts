// Inlined synapse config + explorer link helper. This file mirrors
// `resolveSynapseConfig` and `synapseExplorerHref` in
// `packages/config/networks.mjs`. We duplicate the logic because
// `covenant-web` installs with `--ignore-workspace` and cannot import
// from the rest of the monorepo.

export const DEFAULT_SYNAPSE_PROGRAM_ID =
  "SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ";

const DEFAULT_EXPLORER_URL = "https://explorer.oobeprotocol.ai";

export type SynapseCluster = "devnet" | "localnet" | "mainnet";

export interface SynapseStatus {
  enabled: boolean;
  cluster: SynapseCluster;
  clusterAlias: string;
  programId: string;
  rpcUrl: string;
  explorerUrl: string;
  // This daemon's published SAP agent account, if it has registered
  // one. Set by the operator (or daemon) via COVENANT_SAP_AGENT_PDA
  // once `covenant-sap-worker publish-agent` returns an agentPda.
  agentPda: string | null;
}

const DEFAULT_RPC_URLS: Record<SynapseCluster, string> = {
  mainnet: "https://api.mainnet-beta.solana.com",
  devnet: "https://api.devnet.solana.com",
  localnet: "http://127.0.0.1:8899",
};

const CLUSTER_ALIASES: Record<SynapseCluster, string> = {
  mainnet: "mainnet-beta",
  devnet: "devnet",
  localnet: "localnet",
};

function parseCluster(value: string | undefined): SynapseCluster {
  switch ((value ?? "").trim().toLowerCase()) {
    case "mainnet":
    case "mainnet-beta":
      return "mainnet";
    case "localnet":
      return "localnet";
    default:
      return "devnet";
  }
}

function parseBool(value: string | undefined): boolean {
  const normalised = (value ?? "").trim().toLowerCase();
  return normalised === "1" || normalised === "true" || normalised === "yes";
}

export function resolveSynapseStatus(
  env: NodeJS.ProcessEnv = process.env,
): SynapseStatus {
  const cluster = parseCluster(env.COVENANT_SOLANA_CLUSTER);
  const upper = cluster.toUpperCase();
  const enabled = parseBool(
    env[`COVENANT_SAP_${upper}_ENABLED`] ?? env.COVENANT_SAP_ENABLED,
  );
  const programId =
    env[`COVENANT_SAP_${upper}_PROGRAM_ID`] ??
    env.COVENANT_SAP_PROGRAM_ID ??
    DEFAULT_SYNAPSE_PROGRAM_ID;
  const rpcUrl =
    env[`COVENANT_SAP_${upper}_RPC_URL`] ??
    env.COVENANT_SAP_RPC_URL ??
    DEFAULT_RPC_URLS[cluster];
  const explorerUrl = env.COVENANT_SAP_EXPLORER_URL ?? DEFAULT_EXPLORER_URL;
  const agentPda =
    env[`COVENANT_SAP_${upper}_AGENT_PDA`] ??
    env.COVENANT_SAP_AGENT_PDA ??
    env.NEXT_PUBLIC_COVENANT_SAP_AGENT_PDA ??
    null;
  return {
    enabled,
    cluster,
    clusterAlias: CLUSTER_ALIASES[cluster],
    programId,
    rpcUrl,
    explorerUrl,
    agentPda,
  };
}

export type SynapseExplorerKind = "address" | "tx" | "agent";

export function synapseExplorerHref(
  kind: SynapseExplorerKind,
  value: string,
  status: SynapseStatus,
): string {
  const base = status.explorerUrl.replace(/\/$/, "");
  const query =
    status.clusterAlias === "mainnet-beta" ? "" : `?cluster=${status.clusterAlias}`;
  return `${base}/${kind}/${value}${query}`;
}
