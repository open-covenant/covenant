# FairScale integration: reputation, agent trust, and credit as soft signals

Status: live (read-only oracle with per-read provenance; x402 pay-per-read built and settled on mainnet). Credit read built, gated off by default.
Crate: `agent-os/crates/covenant-fairscale`

## Summary

[FairScale](https://fairscale.xyz) scores Solana wallets and agents over three REST products: reputation (`fairscore`), agent trust (a 0-100 score plus an allow/deny trust-gate), and credit (an underwriting opinion). This crate consumes those as a score provider plugged into Covenant, the same posture as `covenant-krexa`: a labeled soft signal that sits next to Covenant's own audit-derived reputation, never blended into it and never a gate on its own.

- **Read-only oracle (on by config).** A FairScore read surfaced as the `fairscale.score` MCP tool, optionally with the agent trust-gate decision and the credit opinion. Every read carries a `ReadProvenance` record so the read itself is verifiable after the fact. No funds move on the reputation read.
- **Pay-per-read (x402, opt-in).** With a payer wired, keyless reads settle FairScale's live 402 challenge in USDC on Solana mainnet through the Dexter facilitator, bounded by per-read caps. Proven end to end with real settlement 2026-07-20.

FairScale is a score provider. It is never an identity source: identity stays on the Covenant side.

## Their API surface

Two hosts, three products. Docs: [docs.fairscale.xyz](https://docs.fairscale.xyz) (shipped 2026-07-21).

| Product | Host | Endpoints | Auth |
| --- | --- | --- | --- |
| Reputation | `api.fairscale.xyz` | `GET /score`, `/fairScore`, `/walletScore`, `/socialScore` | `fairkey` (docs also claim x402 on `/score`; live it 401s keyless, see [Docs check](#docs-check-2026-07-21)) |
| Agent trust | `agent-api.fairscale.xyz` | `GET /v1/score`, `/v1/score/ai`, `/v1/trust-gate`, `/v1/agent`, `/v1/score-history`, `POST /v1/score/batch`, `/v1/directory`, `/v1/leaderboard` | `fairkey` or x402 ($0.005/read) |
| Credit | `agent-api.fairscale.xyz` | `GET /v1/credit`, public `GET /v1/verify-hash` | `fairkey` or x402 ($0.50/read); verify-hash needs neither |

Facts that matter for consumers:

- **Reputation** (`/score`): `fairscore` 0-100 (fractional), `final_score = core_blend + identity_pillar + program_pillar + ownership_pillar`. `fairscore_base` is a 15-feature on-chain neural-net score; social and peer signals blend in; program membership (Superteam +20) and 01Resolved ownership (capped at 10) are additive. Unverified wallets are capped at 88 during the VeryAI beta (palm-scan humanity proof, a human product, not applicable to agent wallets). Tiers: bronze 0-24, silver 25-49, gold 50-74, platinum 75-89, diamond 90-100.
- **Agent trust** (`/v1/score`): 0-100 across five pillars: `verification` (SAID, ERC-8004, SATI registry presence), `wallet_history`, `work_history` (verified job completions from Kamiyo, Dexter, 8004scan), `network_quality`, `peer_reputation`. Recommendation tiers: unverified 0-20, high_risk 21-40, caution 41-60, trusted 61-100. `/v1/trust-gate` returns `decision: "allow" | "deny"` with `reasons[]` and per-registry `verification` flags; their default `min_score` is 40. The directory indexed ~3,443 agents as of Jul 2026.
- **Credit** (`/v1/credit`): `credit_score` 0-100, amount-agnostic (`score_basis: "profile"`), risk bands prime 75-100 / near_prime 60-74 / subprime 45-59 / deep_subprime 25-44 / decline 0-24. Six pillars, live multi-venue obligations (Kamino, Jupiter Lend, Save, Marginfi) with LTV and liquidation headroom, and suggested terms (APR range, collateral ratio, max line, 7-90 day terms). Advisory only; FairScale moves no funds. Every response carries an HMAC-signed attestation (`payload_hash` over `wallet|credit_score|risk_band|scored_at`) checkable at the public `GET /v1/verify-hash`.
- **Caching**: 15 min on score/agent/credit reads (`?nocache=1` bypasses), 5 min on trust-gate, 2 min on directory. 429 above plan rate; FairScale prescribes backoff-and-retry. The client surfaces 429s instead of retrying them; a sub-second retry cannot clear a per-minute window.
- **Keys**: `fairkey: zpka_...` from sales.fairscale.xyz. Plans: Free 1,000 req/mo at 10/min, Builder 20k/100, Scale 50k/300, Pro 100k/600.

## Score scales

FairScale publishes the same numbers at two scales. This tripped us up pre-docs; it is now pinned in their reference:

| Field | Scale | Where |
| --- | --- | --- |
| `fairscore`, `credit_score`, agent `score`, all pillars | 0-100 (canonical, may be fractional) | `/score`, `/v1/score`, `/v1/credit` |
| `fair_score`, `wallet_score`, `social_score` | 0-1000 integers, exactly the 0-100 number x 10 | shortcut endpoints `/fairScore`, `/walletScore`, `/socialScore` |

The crate consumes the canonical 0-100 fields only. Anything comparing FairScale numbers against Krexa's 200-850 Krexit band must map ranges explicitly; the two providers share neither scale nor meaning.

## Enable it

The daemon registers the tool when `COVENANT_FAIRSCALE_ENABLED` is truthy. Everything is off by default; with neither a fairkey nor a wired payer, keyless reads fail 401/402, loudly, at startup-warning level.

| Variable | Default | Effect |
| --- | --- | --- |
| `COVENANT_FAIRSCALE_ENABLED` | off | Truthy (`1`, `true`, `yes`) registers the `fairscale.score` tool. |
| `COVENANT_FAIRSCALE_KEY` | unset | The `fairkey`, sent on every read. Free-tier friendly. |
| `COVENANT_FAIRSCALE_X402` | off | Settle keyless reads per call in USDC on Solana mainnet through `COVENANT_X402_SIGNER_BINARY` (the same signer sidecar as outbound x402 dispatch, fed `COVENANT_X402_FUNDING_KEYPAIR` and `COVENANT_X402_RPC_URL`). Pinned to the x402 v2 + verbatim-network envelope Dexter validates. |
| `COVENANT_FAIRSCALE_X402_READ_CAP` | `10000` | Per-read cap, atomic USDC (6 decimals), for score and trust-gate reads. Live quote: 5000 ($0.005). |
| `COVENANT_FAIRSCALE_X402_CREDIT_CAP` | `600000` | Per-read cap for the credit read. Live quote: 500000 ($0.50). |
| `COVENANT_FAIRSCALE_REPUTATION_URL` | `https://api.fairscale.xyz` | Override the reputation host. |
| `COVENANT_FAIRSCALE_AGENT_URL` | `https://agent-api.fairscale.xyz` | Override the agent + credit host. |
| `COVENANT_FAIRSCALE_TRUST_GATE` | off | Include the agent trust-gate decision in the result. |
| `COVENANT_FAIRSCALE_CREDIT_ENABLED` | off | Include the credit underwriting read. Heavier and pricier; see below. |

```
COVENANT_FAIRSCALE_ENABLED=1 COVENANT_FAIRSCALE_KEY=zpka_... covenantd
```

## The `fairscale.score` tool

Input: `{ "pubkey": "<base58 Solana address>" }`. Validated as base58 (32-44 chars) at the tool boundary, so an agent-supplied string cannot smuggle `/`, `?`, `#`, or `..` into the request URL.

Output: a labeled projection, a provenance record, and the raw upstream blob, in that order.

```json
{
  "provider": "fairscale",
  "trust": "fairscale-attested (third-party REST), soft signal",
  "pubkey": "EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg",
  "fairscore": 65.3,
  "tier": "gold",
  "trustGate": {
    "decision": "deny",
    "recommendation": "unverified",
    "reasons": ["score_below_threshold: 24 < 60"],
    "verification": { "said": false, "erc8004": false, "sati": true }
  },
  "credit": {
    "amountUsd": 1000,
    "creditScore": 78.0,
    "riskBand": "prime",
    "lendingTerms": { "max_credit_line": 2500, "suggested_apr_range": "8-12%" }
  }
}
```

Semantics worth knowing:

- **Host selection.** A keyed read targets `GET {reputation}/score`. A paying read targets `GET {agent}/v1/score` instead: FairScale serves the score behind x402 on the agent host, and the reputation host rejects keyless reads (observed live).
- **Side reads degrade, never fail.** `trustGate` (forwarded `min_score=60`, deliberately stricter than FairScale's default 40) and `credit` (underwritten against a fixed `amount=1000` probe, echoed as `amountUsd` so the terms are interpretable) are best-effort. If either read fails, the tool logs at debug and returns the reputation signal alone; only a failed `/score` read is a tool error. The example above pairs a reputation `fairscore` of 65.3 with an agent-trust deny computed on 24: different products, different scores; consistent, not a typo.
- **Retry posture.** 429 and 502-504 (serverless cold starts) retry up to 3 attempts with exponential backoff (200ms doubling). Timeouts: 5s connect, 20s total.

### Provenance

FairScale's score is off-chain and a plain read is unsigned, so on its own a read cannot be proven after the fact. Every result therefore carries:

```json
{
  "provenance": {
    "provider": "fairscale",
    "trust": "fairscale-attested (third-party REST), soft signal",
    "wallet": "<pubkey>",
    "endpoint": "/score",
    "responseSha256": "<SHA-256 over the JCS-canonical response>"
  }
}
```

The hash is over the RFC 8785 (JCS) canonical form, so an identical response hashes identically regardless of key order or whitespace. `ReadProvenance::digest()` gives one stable hash over the whole record; the daemon's audit chain records the call today, and anchoring the digest there is the next increment. The record binds the `/score` response only; `trustGate` and `credit` are side reads and carry no anchor. This does not make FairScale's number trustworthy. It makes the read verifiable: this response, for this wallet, from this endpoint.

## Paying per read (x402)

With `COVENANT_FAIRSCALE_X402` wired, a keyless read settles FairScale's live 402 challenge:

- **Challenge** (shape captured live and pinned in tests): `{"error": "Payment required", "accepts": [{ "scheme": "exact", "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp", "amount": "5000", "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "payTo": "fairAUEuR1SCcHL254Vb3F3XpUWLruJ2a11f6QfANEN", "maxTimeoutSeconds": 60, "extra": { "feePayer": "DeXterR2kQm8AvRHnNPatWkE46TfAcMeBDjb6FySoAb8", "decimals": 6 } }]}`. Credit quotes `500000` ($0.50) against the same wallet. Abbreviated: the live envelope also mirrors `amount` as `maxAmountRequired` (the x402-canonical field the client prefers) and carries a `resource` object.
- **Accounting**: spend is operator-bounded by the per-read caps alone; worst case for one tool call with trust-gate and credit on is 10000 + 10000 + 600000 atomic ($0.62), $0.51 at live quotes. Each settlement logs at info. The daemon's `ExternalPaymentSettled` feed and digest anchoring are one deferred increment (see `provenance.rs`).
- **Guard rails.** The client only accepts an `exact` / Solana-mainnet / USDC option; refuses zero-amount quotes as malformed rather than treating them as free; refuses any quote above the per-read cap before anything is signed; pays once and retries once; a second 401/402 after payment is surfaced, never re-settled. An "insufficient funds" simulation failure is translated into a plain top-up-the-funder message.
- **Header quirk.** FairScale reads the payment envelope from `payment-signature` (their documented name) and ignores the x402-standard `x-payment`; the client sends both, so it works against FairScale today and any standard-compliant 402 wall later.
- **Facilitation** is Dexter (`x402.dexter.cash`), the same facilitator Covenant already integrates elsewhere; Dexter fee-pays the settlement transaction.

Receipts: first paid FairScore reads settled end to end on mainnet 2026-07-20 at $0.005 per read ($0.50 credit path exercised under its own cap). Reproducible via `examples/live_paid_read.rs` (plus `probe_verify.rs` and `probe_header_variant.rs` for the discovery runs).

## Trust boundary

Covenant's reputation is audit-derived: computed from an agent's signed work history, meant to be trustless. FairScale's numbers are third-party REST values: the reputation read is unsigned, and the credit read is HMAC-attested by FairScale itself, which authenticates the response but does not make the scoring model verifiable. These are different kinds of trust, so the crate keeps them separate. Every result is stamped `"fairscale-attested (third-party REST), soft signal"` so nothing downstream mistakes it for a Covenant-verified fact. A consumer can weigh both signals; neither is laundered into the other.

One honest caveat, and the reason the label matters doubly here: FairScale's agent `work_history` pillar is partly derived from Covenant's own exported conduct events (alongside Kamiyo, Dexter, and 8004scan feedback). Consuming the composite back is a routing prior, not an independent correctness check; treating it as confirmation would be feedback, not evidence.

## Credit (gated off)

`COVENANT_FAIRSCALE_CREDIT_ENABLED` adds the `/v1/credit` underwriting opinion to the tool output. It stays off by default for three reasons: it is 100x the price of a score read over x402 ($0.50 vs $0.005), it is a heavier read (live obligations scan across four venues), and it is advisory by FairScale's own framing ("suggestions, not decisions"). Unlike Krexa's credit module there is no draw seam here at all: FairScale extends no credit line to us and the read is wired to move nothing. The attestation (`payload_hash` + public `verify-hash`) makes this the one FairScale product where the provider signs what it said, which pairs naturally with our per-read provenance: store both and the whole exchange is re-checkable.

## Docs check (2026-07-21)

We tracked two points through FairScale's docs launch, both raised from live behavior before the docs shipped. Verdicts against the published docs plus fresh live probes (2026-07-21):

1. **Score scale: resolved.** The reference pins 0-100 as canonical with the x10 shortcut representation documented explicitly ("`fair_score` is `fairscore` x 10"), plus tier and band tables and verified live examples. Matches what the client already consumes; no code change needed.
2. **Keyless x402: resolved, and fixed live.** The authentication page now documents Option B (x402, no account required) with the exact challenge flow, and the agent host honors it keyless: `/v1/score` and `/v1/trust-gate` return HTTP 402 with the challenge (amount 5000), `/v1/credit` likewise (amount 500000). Earlier the agent host returned 401 `No Authorization Header` keyless, which is what forced the original probe work.

Residual nit, worth a one-line note to FairScale: the `/score` reference on the reputation host says "Auth: fairkey header (or x402)", but a live keyless `GET api.fairscale.xyz/score` still returns 401 `No Authorization Header` (receipt: requestId `abab56d4-3e7b-42bd-ba28-7db93466aec6`, 2026-07-21T19:47:34Z, buildId `dda3037d-3b6d-4f82-a026-6a9f0869575b`). Either the claim or the middleware is ahead of the other. Our client sidesteps it (paying reads target the agent host), so this blocks nothing.

Also new in their 2026-07-21 changelog and verified live: the public `GET /v1/verify-hash` (no key, no payment) for credit attestations.

## Partnership

The concrete fits, each a specific seam rather than a category:

- **We are their first documented x402 consumer pattern in the wild.** The pay path they document (Dexter facilitation, `payment-signature` envelope) is exactly what this crate settles against, receipts on mainnet. That makes Covenant a reference integration for their keyless tier, and the docs-PR offer stands (the header-casing and host-split quirks in this doc are the material).
- **Their verification pillar already reads registries Covenant populates.** Agent trust `verification` scores SAID, ERC-8004, and SATI presence, and Covenant registers agents on SAID. A Covenant agent tends to score better on FairScale for free, the same shape as the Krexa `.sol` boost.
- **Their `work_history` pillar consumes Covenant conduct exports.** That is the deep seam: Covenant-signed work receipts feeding their pillar makes their agent score partly Covenant-derived, which is good for them (verifiable input) and demands the circularity discipline on our side (see Trust boundary).
- **Attestation symmetry is the ask.** Credit responses are HMAC-signed with a public verify endpoint; reputation responses are not. If `/score` grew the same `payload_hash` + `verify-hash` treatment, our `ReadProvenance` could anchor a FairScale-signed hash instead of an unsigned body, upgrading the soft signal to provider-attested end to end. This is the single highest-value item on their side.
- **Shared facilitator.** Both sides settle through Dexter, so payment-side integration work amortizes across FairScale, PayAI, and the rest of the x402 estate.

## Scope

The crate consumes FairScale as a score and credit-opinion provider only. Out of scope: FairScale's dashboard products, VeryAI humanity verification (a palm-scan product for humans, meaningless for agent wallets), vouching on FairScale's side, and anything that would blend their number into Covenant's audit-derived reputation or gate an action on it alone. The defunct $FAIR token is a non-topic: the API bills in USDC and the integration carries zero token exposure. Identity remains Covenant's; FairScale remains a signal.
