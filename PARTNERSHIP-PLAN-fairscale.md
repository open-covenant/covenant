# Covenant × FairScale — Partnership & Integration Plan

*Status: PLAN ONLY (local). Nothing built or pushed. Follows the partner-integration convention: nested worktree `covenant-fairscale`, branch `feat/fairscale`, new crate `agent-os/crates/covenant-fairscale`. Mirrors the merged `covenant-krexa` oracle pattern, consumed inbound as a labeled soft signal.*

---

## 1. Thesis (one paragraph)

FairScale (fairscale.xyz) scores Solana wallets and agents across three live REST products: **Reputation** (`fairscore`, sybil/behavioral), **Agent Trust** (a 0-100 five-pillar agent score plus an allow/deny `trust-gate`), and **Credit** (institutional-style underwriting: a 0-100 `credit_score`, risk band, suggested APR, and max credit line). It is the reputation/credit signal we want to consume, and we already run the other half of the relationship: Covenant's live `services/fairscale-bridge` pushes our audit-attested conduct events into FairScale's `work_history` pillar, which their docs credit alongside Kamiyo and Dexter. This plan adds the inbound direction: Covenant consumes a FairScore as a **labeled soft signal** via a `fairscale.score` MCP tool, exactly as `covenant-krexa` consumes a Krexit score, and pays for the read over **x402** (FairScale already ships x402 at $0.005/read on Solana mainnet via the Dexter facilitator, so we dogfood our own x402 payer). The differentiator is provenance: a FairScore is **off-chain and not trustlessly verifiable** (a plain `/score` read is "trust the API"; only the credit product carries an off-chain HMAC attestation). Covenant wraps each read in an independently-verifiable attestation (which pubkey, which score, at which slot, signed and anchored), turning "trust FairScale's API" into "verifiable that this read happened and has not been tampered." Same discipline as Krexa: a labeled soft signal that sits beside Covenant's audit-derived reputation, never blended into it, never a gate.

---

## 2. Why FairScale, and why now

- **Existing warm relationship.** We already integrate one direction (the export bridge, merged to `main` 2026-05-27, deployed as `covenant-fairscale-bridge` on Render). This is not a cold partner. Adding the consume side closes the loop into a real two-way integration.
- **Operator trust preference.** The operator trusts FairScale over Krexa and wants FairScale as the preferred reputation/credit signal. That is the driver. (See §8 for the honest read on the "Krexa copied FairScale" belief: no public evidence found, treated as unverified. It does not change the plan.)
- **x402-native on our rail.** FairScale bills reads in USDC over x402 on Solana mainnet. Covenant already has the x402 Solana payer (`covenant-x402` `SolanaSigner`), so we consume FairScore by paying for it the same way an agent pays for anything else. No API-key custody required.
- **Reciprocity is real.** FairScale's `work_history` pillar already consumes our conduct events and names Kamiyo. Co-marketing writes itself.

---

## 3. What FairScale exposes (the consume surface)

| Product | Read | Score | Notes |
|---|---|---|---|
| Reputation | `GET api.fairscale.xyz/score?wallet=<pubkey>` | `fairscore` 0-100 (tiers bronze→diamond) | 15 on-chain behavioral features + social/peer/identity pillars. A shortcut `/fairScore` returns a ~0-1000 scale (verify before wiring). |
| Agent Trust | `GET agent-api.fairscale.xyz/v1/trust-gate?wallet=&min_score=` | decision `allow`/`deny` + `fairscore` + verification {said, erc8004, sati} | Cleanest binary consume. Five pillars incl. `work_history` (credits Kamiyo/Dexter). |
| Credit | `GET agent-api.fairscale.xyz/v1/credit?wallet=&amount=` | `credit_score` 0-100 + risk band + `lending_terms` | Underwriting **opinion** only, no funds move (unlike Krexa). Carries an off-chain HMAC attestation. |

- **Auth:** a `fairkey` header (`zpka_...`, free tier 1k/mo) **or** x402 pay-per-read (no account). We prefer x402.
- **x402:** $0.005 USDC reputation/agent, $0.50 USDC credit. Solana mainnet, USDC mint `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`, facilitator Dexter `https://x402.dexter.cash`, payTo `fairAUEuR1SCcHL254Vb3F3XpUWLruJ2a11f6QfANEN`.
- **Off-chain.** No program id, PDA, or on-chain score account. Cannot be read trustlessly from chain. (Contrast with `covenant-krexa`, which decodes an on-chain `KrexitScore` account on devnet.)

---

## 4. The gap we fill

A consumer reading a FairScore has no way to prove it is genuine: the plain `/score` response is unsigned ("trust the TLS endpoint"), and even the credit attestation is a **shared-secret HMAC** that only FairScale can verify (send the hash back, they confirm), off-chain, not a public-key signature the world can check. So:

1. **No portable provenance on a score read.** Nothing binds "this FairScore, for this pubkey, was really returned by FairScale at this time" into a tamper-evident, independently-checkable record.
2. **No on-chain verifiability.** The score lives entirely behind the API.
3. **Circularity, unmanaged.** FairScore's `work_history` pillar is partly computed from Covenant's own exported conduct events, so consuming the composite back is a feedback loop. It is a routing prior, not an independent correctness check, and must be labeled as such.

Covenant closes #1 and #2 by attesting the read, and manages #3 by keeping the signal labeled and un-blended.

---

## 5. The product — `fairscale.score`, a provenance-wrapped soft signal

Mirror `covenant-krexa` exactly:

- **MCP tool `fairscale.score { pubkey }`** returns a labeled projection of the FairScore (reputation `fairscore` + tier, optionally the agent `trust-gate` decision and the credit band/terms), tagged `TRUST_LABEL = "fairscale-attested (third-party REST), soft signal"`, alongside the raw upstream blob.
- **Consumed over x402** by default (pay $0.005 USDC on Solana through the daemon's x402 Solana payer + Dexter facilitator), with a `fairkey` header fallback for free-tier reads.
- **Provenance wrap:** each read emits a Covenant attestation binding `(reader, pubkey queried, endpoint, score returned, upstream response hash, x402 settlement sig, slot)` via `covenant-attestation`, so the read itself is verifiable even though FairScale's score is not. This is the value-add over the Krexa oracle.
- **Kept separate** from Covenant's audit-derived reputation (the pinned `Response::Reputation` wire shape), never blended, per the Krexa design decision. spend-authz is the more useful blend target than reputation (a risk input to a spend decision).

FairScale becomes the **preferred** third-party reputation/credit signal; `krexa.score` stays as a second, lower-weight source. Consuming both is consistent and non-conflicting.

---

## 6. Architecture — mirror covenant-krexa, reuse x402

| Need | Existing crate / asset | New work |
|---|---|---|
| REST client for the three reads + typed structs | pattern from `covenant-krexa/src/client.rs` | `fairscale/src/client.rs` (score/trust-gate/credit) |
| Pay for the read over x402 (Solana, Dexter) | `covenant-x402` `SolanaSigner`, the daemon x402 dispatch | Dexter-facilitator read path |
| MCP `fairscale.score` labeled soft signal | `covenant-mcp`, `covenant-krexa/src/tools.rs` | `fairscale/src/tools.rs` |
| Provenance attestation on each read | `covenant-attestation`, `covenant-audit` | read-provenance envelope |
| Daemon wiring (enable flag, from_env, tool_call) | the merged krexa wiring in `covenantd` | `fairscale_from_env` + `fairscale.` route |

New crate: **`covenant-fairscale`** (`version = "0.0.0"`), REST-only + x402, no on-chain decode (FairScale is off-chain). Modules: `client`, `config`, `tools`, `provenance`, `lib`. Off by default behind `COVENANT_FAIRSCALE_ENABLED`.

---

## 7. Phases (Covenant does the lifting)

**Phase 0 — read-only oracle (ship first, low risk).**
`fairscale.score` MCP tool: reputation + trust-gate reads, labeled soft signal, x402-paid, with the provenance wrap. No funds beyond the $0.005 read fee. Deliverables + loop tasks `fs-01..05`. Exit bar: a real FairScore read for a live pubkey, paid over x402 on Solana mainnet, its provenance attestation independently verifiable.

**Phase 1 — credit read (still low risk; FairScale credit moves no funds).**
Add the `/v1/credit` underwriting read as a labeled advisory signal (band, suggested APR, max line) at $0.50/read. Unlike Krexa's credit (which disbursed USDC through a PDA), FairScale credit is opinion-only, so no gating of a funds path is needed. Keep it labeled and behind a per-call cap.

**Phase 2 — shared, partnership-dependent.**
- Covenant provenance as a verifiability layer *for FairScale's own reads* (co-marketed: "FairScore reads you can prove").
- Two-way attestation: our conduct-event export (already live) plus FairScore consume, both provenance-bound, so the loop is auditable end to end.
- List Covenant in FairScale's `/v1/directory` (their x402 skill directory).

---

## 8. FairScale vs Krexa — the honest read

The operator believes Krexa ripped off FairScale's trust/credit models and trusts FairScale more. On the facts:

- **No public evidence of copying** was found (X, GitHub, news, community). Treat the ripoff belief as **unverified**; do not put it in any plan or outreach copy.
- They are **direct competitors in the same lane** ("credit for AI agents on Solana"), which likely fuels the suspicion, but the products are architecturally distinct: FairScale scores 0-100 and **underwrites** (advisory), Krexa scores 200-850 (FICO-like) and **disburses** USDC with auto-repayment. Different scale, different scope, Krexa is multi-chain (Solana + Monad).
- The trust preference is a legitimate operator call and drives this integration. Both can be consumed as labeled soft signals; FairScale is weighted preferred. No need to remove `krexa.score` — this adds a preferred source beside it.

---

## 9. Constraints honored

- **Labeled soft signal, never a gate.** Same as Krexa. Stays beside the audit-derived reputation, never blended into the pinned wire shape.
- **Circularity acknowledged in the label.** FairScore partly derives from our exported data; consuming it back is a routing prior, not a correctness check. The `TRUST_LABEL` says third-party/soft.
- **Token-decoupled.** `$FAIR` went through a futarchy liquidation and looks wound down; the API bills in **USDC**. Never depend on, reference, or touch `$FAIR`. `$CVNT` stays Solana-canonical and untouched.
- **Never touch payments beyond the read fee.** We pay the x402 read like any metered resource; we never intermediate FairScale's flow.
- **Plan-first; reuse infra; onchain framing; verifiable-as-property; contact@opencovenant.org.**

---

## 10. De-risking / verify before building

- **Score scale inconsistency is real.** `/score.fairscore` is 0-100; `/fairScore.fair_score` is ~0-1000. Confirm the exact scale against a live response before wiring any threshold.
- **`@fairscale/sdk` is not on npm** (404). Consume via raw REST/x402, which is fully specified. Do not assume an installable SDK.
- **Off-chain only.** No trustless on-chain read exists (unlike the Krexa devnet path). Provenance comes from Covenant's own attestation of the read, not from chain.
- **Token defunct, product alive.** Build against the API; ignore `$FAIR`.
- **Team anonymous.** "Rishee" is the export-bridge contact but is unconfirmed as FairScale core (a `RisheeA` GitHub authored a community skill, empty profile). Verify who we are actually talking to before outreach commitments.
- **HMAC attestation is FairScale-only-verifiable.** If we surface FairScale's credit attestation, label it as "FairScale can verify," not as a public proof.

## 11. Outreach (warm; when operator says go)

To the FairScale contact (via the existing bridge relationship): "We already feed your `work_history` pillar; now we consume FairScore inbound and pay for it over your x402 rail. Covenant wraps each read with independently-verifiable provenance, so a FairScore read becomes something a third party can check, not just trust. Point us at the current score scale for `/score` and confirm the x402 read is live for our payer, and we'll show you the first provenance-wrapped FairScore read on mainnet." (States what is shipping + one concrete ask; leans on the existing relationship; no `$FAIR`, no unverified claims about competitors.)
