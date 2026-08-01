// Shared constants for the agent passport pages. One module so the API
// route, the passport page, and the index never drift on program ids.

// MPL Agent identity registry ("014 Registry") program.
export const AGENT_IDENTITY_PROGRAM = "1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p";

// Expected AppData authority for the Covenant records shown by this site.
// A match attributes the observed bytes to this configured key; it does not
// establish that the payload's claim is true.
export const COVENANT_DATA_AUTHORITY =
  "DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK";

// The Covenant Agents MPL Core collection on mainnet.
export const COVENANT_COLLECTION = "Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC";

// Prototype Core Oracle program. Its configured plugin can veto selected Core
// asset lifecycle events. It does not mediate agent execution, signing, or
// payments. The deployed program has a source-build record (see osecVerifyUrl).
export const COVENANT_ORACLE_PROGRAM =
  "2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD";

// Featured mainnet identity and AppData record shown on /agents.
export const FEATURED_AGENT_ASSET =
  "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";
export const FEATURED_ATTESTATION_ASSET =
  "4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG";

// Historical experimental record identifiers. The ERC-8004 URI is an opaque
// application type tag here; matching it does not provide ERC interoperability.
// The parser uses these constants only for structural field checks.
export const ATTESTATION_TYPE =
  "https://eips.ethereum.org/EIPS/eip-8004#validation-v1";
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
