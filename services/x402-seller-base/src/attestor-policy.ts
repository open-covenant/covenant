export function mayUseEphemeralAttestor(
  network: string,
  nodeEnv: string | undefined,
  explicitOptIn: string | undefined,
): boolean {
  const normalizedNetwork = network.trim().toLowerCase();
  const mainnet = ["base", "eip155:8453", "8453"].includes(normalizedNetwork);
  const production = nodeEnv?.trim().toLowerCase() === "production";
  return !mainnet && !production && explicitOptIn === "true";
}
