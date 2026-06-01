// Solscan link helpers. Surfaced from any page that displays an
// on-chain artifact (tx signature, account/program address) so a
// visitor can verify the activity in a familiar explorer.
//
// Cluster handling: solscan's mainnet URLs take no query; devnet and
// testnet attach `?cluster=devnet` / `?cluster=testnet`. We accept the
// permissive set of aliases the rest of the daemon uses
// (`mainnet`, `mainnet-beta`, `devnet`, `testnet`, `localnet`) and
// fall back to mainnet when given anything else.

export type ClusterAlias =
  | "mainnet"
  | "mainnet-beta"
  | "devnet"
  | "testnet"
  | "localnet"
  | string;

function clusterSuffix(cluster: ClusterAlias): string {
  const c = (cluster ?? "").toLowerCase();
  if (c === "devnet") return "?cluster=devnet";
  if (c === "testnet") return "?cluster=testnet";
  // localnet has no public explorer; the link is still useful for
  // copy/paste of the address so leave the suffix empty (mainnet UI).
  return "";
}

export function solscanTxUrl(signature: string, cluster: ClusterAlias): string {
  return `https://solscan.io/tx/${encodeURIComponent(signature)}${clusterSuffix(cluster)}`;
}

export function solscanAddressUrl(address: string, cluster: ClusterAlias): string {
  return `https://solscan.io/account/${encodeURIComponent(address)}${clusterSuffix(cluster)}`;
}

export function solscanTokenUrl(mint: string, cluster: ClusterAlias): string {
  return `https://solscan.io/token/${encodeURIComponent(mint)}${clusterSuffix(cluster)}`;
}
