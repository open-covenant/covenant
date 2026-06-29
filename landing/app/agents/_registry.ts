// Shared constants for the agent passport pages. One module so the API
// route, the passport page, and the index never drift on program ids.

// MPL Agent identity registry ("014 Registry") program.
export const AGENT_IDENTITY_PROGRAM = "1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p";

// MPL Core program — owns every Core asset account.
export const MPL_CORE_PROGRAM = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";

// The Covenant minting key: the only address allowed to write Covenant
// attestation AppData. An attestation is Covenant-authored iff its AppData
// authority is exactly this address.
export const COVENANT_DATA_AUTHORITY = "DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK";

// The Covenant Agents MPL Core collection on mainnet.
export const COVENANT_COLLECTION = "Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC";

// The Covenant Oracle program: gates a gated agent's Core lifecycle events on a
// live audit verdict via the Core Oracle external plugin (one ["oracle", asset]
// PDA per agent). Source-verified on mainnet (see osecVerifyUrl).
export const COVENANT_ORACLE_PROGRAM = "2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD";

// The registered production agent + a known attestation, featured on /agents.
export const FEATURED_AGENT_ASSET = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";
export const FEATURED_ATTESTATION_ASSET = "4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG";

// Attestation payload schema written into AppData by covenantd's signer.
export const ATTESTATION_SCHEMA = "covenant.audit-root.appdata.v1";

// v2 record: an ERC-8004 validation attestation. `type` is the ERC-8004
// discriminator, `hashAlg` declares the commitment algorithm. The
// "Covenant Verified" check (/api/verify) keys on these.
export const ATTESTATION_TYPE = "https://eips.ethereum.org/EIPS/eip-8004#validation-v1";
export const ATTESTATION_SCHEMA_V2 = "covenant.audit-root.appdata.v2";
export const ATTESTATION_HASH_ALG = "sha256-merkle";

export function solscanAccountUrl(address: string): string {
  return `https://solscan.io/account/${address}`;
}

export function metaplexAgentUrl(asset: string): string {
  return `https://www.metaplex.com/agents/${asset}`;
}

// OtterSec verified-build status: anyone can confirm the on-chain program bytes
// were built from the public source at the recorded commit.
export function osecVerifyUrl(programId: string): string {
  return `https://verify.osec.io/status/${programId}`;
}
