# Krexa integration: credit/risk oracle as a soft signal

Status: phase 0 built (read-only oracle: REST reads + direct on-chain account decode). Phase 1 (credit-backed x402) built but gated off.
Crate: `agent-os/crates/covenant-krexa`

## Summary

[Krexa](https://krexa.io) scores agent creditworthiness (the on-chain "Krexit Score", 200-850) and lends uncollateralized USDC against it. This crate consumes that as a credit/risk provider plugged into Covenant identity, in two layers with very different risk.

- **Read-only oracle (on by config).** REST reads of an agent's Krexit score, eligibility, and active credit line, surfaced as the `krexa.score` MCP tool. It is a labeled soft signal that sits next to Covenant's signer-authenticated audit heuristic, never blended into it. No funds, no counterparty risk.
- **Credit-backed x402 (built, gated off).** The seam for an agent to cover an x402 payment shortfall from a Krexa credit line and repay from earnings. It refuses unless `credit_enabled` is set, and is not wired into the live payment path regardless. See [Credit](#credit-gated-off) below.

Krexa is a score and credit provider, not an identity source. Keep its score separate from Covenant registration and provenance records.

## Enable it

The daemon registers the tool when `COVENANT_KREXA_ENABLED` is truthy. Everything is off by default.

| Variable | Default | Effect |
| --- | --- | --- |
| `COVENANT_KREXA_ENABLED` | off | Truthy (`1`, `true`, `yes`) registers the `krexa.score` tool. |
| `COVENANT_KREXA_BASE_URL` | prod backend | Override the Krexa REST host. Empty falls back to the default. |
| `COVENANT_KREXA_CREDIT_ENABLED` | off | Un-gates the built-but-inert credit module. Not wired into live settlement; leave off. |

```
COVENANT_KREXA_ENABLED=1 covenantd
```

## The `krexa.score` tool

Input: `{ "pubkey": "<base58 Solana address>" }`. The pubkey is validated as base58 at the boundary, so a malformed or injection-shaped argument is refused before any request leaves the host.

Output: a labeled JSON projection plus the raw upstream blob.

```json
{
  "provider": "krexa",
  "trust": "krexa-attested (third-party REST), soft signal",
  "pubkey": "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg",
  "score": 342,
  "creditLevel": 1,
  "riskBand": "deep_subprime",
  "registered": false,
  "snsBoostApplied": false,
  "recommendation": "Approve with strict limits",
  "attestationHash": "610f0de3..."
}
```

The crate's public API (`KrexaClient`, `KrexitScore`, `BASE_URL`) is usable on its own if you want the reads without the daemon.

## Trust boundary

Covenant's audit-derived score is a signer-authenticated heuristic over selected records; it is not trustless and does not prove work quality. Krexa's score is a third-party REST value carrying a self-attested SHA-256 hash, not a signature. These are different third-party signals, so the crate keeps them separate. Every result is stamped `"krexa-attested (third-party REST), soft signal"` so nothing downstream mistakes it for an independently established fact. A consumer can weigh both signals; neither is laundered into the other.

The score response also carries `scorePda`, the on-chain account holding the score. Covenant can decode that account directly instead of relying on the REST representation (`onchain.rs`), against the `KrexitScore` layout and discriminator Krexa provided 2026-07-08. That makes the stored value independently readable, but consumers still trust the score program, its authorities and inputs, and their RPC view. The remaining deployment boundary is Krexa's: the score program is devnet-only today, so independently readable mainnet values wait on their mainnet deploy. REST stays the default until then.

One concrete fit: Krexa reports a score boost for agents that hold a `.sol` name, and Covenant can provision `.sol` names when that integration is configured. The boost remains a Krexa policy signal, not Covenant evidence of identity or quality.

## Credit (gated off)

The `credit` module covers a 402 shortfall from a Krexa credit line and repays from earnings. It is built and tested, but every entry point refuses with `KrexaError::CreditDisabled` unless `credit_enabled` is set, and it is deliberately not plugged into `covenant-x402`'s live settlement path. Turning it on needs three answers from Krexa: an audit, real vault TVL with the default and insurance ratio, and the custody model (the agent's own signer versus a Krexa PDA wallet routed through the Revenue Router). The last one decides how the draw seam plugs into the x402 path, so it is built once, after the answer.

## Scope

The crate consumes Krexa as a credit and score provider only. KYA and Krexa's wider trade, swap, and perps SDK are out of scope. KYA in particular would collide with Covenant identity.
