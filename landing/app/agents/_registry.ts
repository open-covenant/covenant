// Shared constants for the agent passport pages. One module so the API
// route, the passport page, and the index never drift on program ids.

// MPL Agent identity registry ("014 Registry") program.
export const AGENT_IDENTITY_PROGRAM = "1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p";

// MPL Core program — owns every Core asset account.
export const MPL_CORE_PROGRAM = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";

// The Covenant minting key: the only address allowed to write Covenant
// attestation AppData. An attestation is Covenant-authored iff its AppData
// authority is exactly this address.
export const COVENANT_DATA_AUTHORITY = "96GsGo69kVfPZffudCexfnsSi5EuhAyd278MuJPwzGdu";

// The Covenant Agents MPL Core collection on mainnet.
export const COVENANT_COLLECTION = "Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC";

// The registered production agent + a known attestation, featured on /agents.
export const FEATURED_AGENT_ASSET = "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH";
export const FEATURED_ATTESTATION_ASSET = "AHZE6uSSnQ2Y1rLLCi7Pv86m6JgTzpcD8s2DEhzfrm3U";

// Attestation payload schema written into AppData by covenantd's signer.
export const ATTESTATION_SCHEMA = "covenant.audit-root.appdata.v1";

export function solscanAccountUrl(address: string): string {
  return `https://solscan.io/account/${address}`;
}

export function metaplexAgentUrl(asset: string): string {
  return `https://www.metaplex.com/agents/${asset}`;
}
