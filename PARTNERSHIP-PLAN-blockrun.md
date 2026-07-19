# Covenant × BlockRun — Partnership & Integration Plan

*Status: PLAN ONLY (local). Nothing built or pushed. Follows the partner-integration convention: nested worktree `covenant-blockrun`, branch `feat/blockrun`, new crate `agent-os/crates/covenant-blockrun`.*

---

## 1. Thesis (one paragraph)

BlockRun solved **payment trust** for the agent economy: x402 makes every agent action an atomic, onchain USDC settlement on Base and Solana — "the wallet is the identity, no accounts, no KYC," 14.65M transactions settled, 1M+ API calls/month. What BlockRun has **zero** of is **delivery trust, identity trust, and decision trust**. Every one of their flagship autonomous-money products implicitly needs a receipt binding *who acted, what they intended, what they were served, and what they paid* — and none produces one. They even market the word "provenance" while shipping only payment metering. Covenant is precisely that missing layer: verifiable agent identity, provenance, and reputation, built on Solana, x402-native on both chains, with a hard discipline of sitting **alongside** payments and never touching them. The integration turns every BlockRun x402 settlement into a Covenant-attested, portable, onchain reputation event. Covenant does 100% of the Phase 1 lifting as a client-side overlay requiring **zero code changes from BlockRun**; BlockRun co-builds the native rail in Phase 2.

---

## 2. Strategic fit — why this is the cleanest partner in the pipeline

| Dimension | BlockRun | Covenant | Fit |
|---|---|---|---|
| Rail | x402 on Base + Solana | x402-native on Base + Solana (`covenant-x402`, `-x402-signer`, `-x402-signer-evm`) | Same rail, same two chains — no bridging work |
| Settlement token | USDC | USDC (no token entanglement; $CVNT stays Solana-canonical, untouched) | No token conflict |
| Identity model | "The wallet is the identity" (no reputation) | Agent identity + reputation (`covenant-identity`, `-metaplex`, `-sns`, PROOF) | Covenant *upgrades* their primitive, doesn't replace it |
| Routing | ClawRouter/Franklin router, savings claims self-reported | `covenant-router` + attestation | Covenant makes the claim verifiable |
| Ecosystem trust slot | **Empty category. Open invitation to list.** | This is Covenant's entire product | Uncontested |
| Discipline | Payments are theirs | "Trust ALONGSIDE payments, never touch payments" | No overlap, no threat |

BlockRun's partner roster (Circle, Coinbase/CDP, Base, Solana, x402 Foundation, thirdweb, PayAI, Surf, Exa, 0x, Tatum, ElevenLabs, Bland/Twilio) contains **no trust, identity, reputation, verification, or provenance partner**. The category is empty and their partners page ends with an open invitation. Covenant walks into an uncontested seat.

---

## 3. The gap we fill (BlockRun's unsolved trust problems)

From their own docs and repos — every autonomous-money demo needs a receipt none of them produces:

1. **Paying agent → service honesty.** x402 proves the agent *paid*; nothing proves the service *delivered what was paid for*. An agent buying a "premium model" response or "$0.01 sentiment" cannot verify it got the claimed model, fresh data, or honest routing.
2. **Service → agent legitimacy.** The only identity is a wallet; Sybil = a new wallet. Services can't distinguish a well-behaved Franklin from a hostile fork, and have no abuse history to price against.
3. **Routing/savings claims are unfalsifiable.** "78% / 89% / 92% savings" (three different published figures) is asserted by the router, which earns the margin on whatever it routes to — a direct incentive conflict with no proof of which model actually served the request.
4. **Guardrail integrity is honor-system.** `alpha-mcp`'s "cannot be overridden" risk limits are open-source client code — trivially forkable with the limits removed. Any counterparty relying on them has no proof the unmodified guardrails ran.
5. **Decision provenance is local and mutable.** Franklin-Trading's persona debate (Analyst → Bull/Bear → Trader → Risk → Compliance) lands in `~/.blockrun/sessions/*.jsonl`; polymarket-agent's 3-model consensus lands in `/tmp` ("data lost on redeployment"). The reasoning that justifies a trade is self-reported by the same process that trades.
6. **No standard receipt.** On-chain USDC receipts exist per fill, but nothing binds `(agent wallet, intent, inputs seen, decision, payment, output hash)` into one tamper-evident artifact.

Covenant's receipt slots into exactly this seam.

---

## 4. The product — **Covenant Receipts for x402**

One artifact, three consumers. A **Covenant Receipt** is a signed, hash-bound attestation of a single x402 interaction:

```
Receipt {
  agent:        <paying wallet> → bound to a Covenant identity (Metaplex agent-registry / .sol / ERC-8004)
  intent:       hash(intent | outcome | budget)
  requested:    { service, model_requested, params }
  served:       { model_served, provider, routing_profile, savings_claimed }
  io:           { input_hash, output_hash }
  payment:      { tx_hash (from X-Payment-Receipt), amount, asset, network }
  verdict:      delivery_ok | mismatch | disputed
  sig:          agent-side (Phase 1) → co-signed service-side (Phase 2)
  anchor:       Solana attestation (covenant-attestation / covenant-settlement)
}
```

- **For the paying agent:** proof it got the model/data it paid for; a portable reputation it accrues across every marketplace, not just BlockRun.
- **For the service (and BlockRun's router):** a verifiable delivery record and a reputation signal on the counterparty — anti-Sybil, priced abuse history.
- **For third parties (LPs, enterprises, auditors):** the missing dispute/audit artifact behind every autonomous-money claim. BlockRun's Enterprise tier already advertises "audit logs" — this makes them cryptographic.

Framing discipline: reputation is a **system property** ("BlockRun spend is verifiable"), never a chore handed to the reader ("go verify it yourself").

---

## 5. Architecture — reuse, don't reinvent

Phase 1 is ~80% composition of crates that already ship in `agent-os/crates/`:

| Need | Existing crate | New work |
|---|---|---|
| x402 client + signing (Base + Solana) | `covenant-x402`, `covenant-x402-signer`, `covenant-x402-signer-evm` | Parse `X-Payment-Receipt`, capture round-trip |
| The receipt attestation + Solana anchor | `covenant-attestation`, `covenant-settlement` | Receipt schema + hashing |
| Bind paying wallet → agent identity | `covenant-identity`, `covenant-metaplex`, `covenant-sns` | Wallet→identity resolver |
| Verifiable routing | `covenant-router` | Attest requested-vs-served model |
| Reputation aggregation | `covenant-proof` (worktree), `covenant-audit` | Receipt → score rollup |
| Spend/budget parity | `covenant-budget`, `covenant-spend-permission` | Map BlockRun `delegate`/`report` |
| MCP trust tools | `covenant-mcp`, `covenant-guard` (mcp.opencovenant.org) | `covenant_attest/verify/reputation` tools |

New crate: **`covenant-blockrun`** — the thin composition layer + wrappers. The only genuinely new code is the receipt schema, the wallet→identity resolver, the SDK decorators, and the demo.

Injection points BlockRun already exposes (so Phase 1 needs nothing from them):
- **Python:** `blockrun-llm`'s `AnthropicClient` uses a custom httpx transport for x402 — wrap the transport.
- **TypeScript:** `blockrun-llm-ts` handles x402 in the fetch layer — wrap fetch.
- **MCP:** `blockrun-mcp` is open source (MIT-ish, verify) and forkable; ship Covenant trust tools **alongside** it, keyed to the same `~/.blockrun/` wallet and the `agent_id` field already on `blockrun_chat`/`blockrun_wallet delegate`.

---

## 6. Phase 1 — Covenant does ALL the lifting (zero BlockRun code change)

**Goal:** a BlockRun agent gets Covenant receipts + reputation today, without BlockRun shipping a line. Pure client-side overlay.

**Deliverables**
1. `covenant-blockrun` crate (`agent-os/crates/covenant-blockrun`) — receipt schema, wallet→identity resolver, x402 round-trip capture, Solana anchor via `covenant-attestation`.
2. **`@covenant-org/blockrun` (npm) + `covenant-blockrun` (Python)** decorator SDKs — wrap the BlockRun client; every call emits a receipt. Drop-in: `wrapBlockRun(client)`.
3. **Covenant trust MCP tools** alongside `blockrun-mcp`: `covenant_attest`, `covenant_verify`, `covenant_reputation` — so any Claude Code / Franklin user with `blockrun-mcp` installed gets receipts for their spend.
4. **Three x402-paid Covenant endpoints** (Covenant is a *service on BlockRun's own rail* — dogfoods x402, real USDC yield): `POST /attest`, `POST /verify`, `GET /reputation/:agent`. A BlockRun agent pays Covenant in USDC the same way it pays for a model. Covenant lists itself on BlockRun's ecosystem page as the trust provider.
5. **Verifiable-routing receipt** — the wedge. Using ClawRouter's exposed `.model` / `.routing.savings` / debug headers, attest "requested `auto`, served model X at price Y." Turns the unfalsifiable savings claim into a Covenant-verifiable receipt. Directly repairs their credibility gap.
6. **Flagship demo** (see §11).

**Tasks (loop-seedable)**
- `bru-01` receipt schema + hashing + Solana anchor (reuse `covenant-attestation`)
- `bru-02` x402 round-trip capture + `X-Payment-Receipt` parse (verify header casing against live 402 first)
- `bru-03` wallet→Covenant-identity resolver (Metaplex/.sol/ERC-8004)
- `bru-04` TS decorator `@covenant-org/blockrun` around fetch layer
- `bru-05` Python decorator around `AnthropicClient` transport
- `bru-06` MCP trust tools alongside `blockrun-mcp`
- `bru-07` three x402-paid endpoints on covenantd
- `bru-08` verifiable-routing receipt from ClawRouter headers
- `bru-09` reputation rollup via PROOF
- `bru-10` flagship demo + live-verified receipt on mainnet

**Exit bar (interface-correct → production-correct):** a real Franklin/Franklin-Trading run on mainnet emits a receipt whose payment `tx_hash` resolves onchain and whose identity + model attestation verify independently. `live_*` demo naming, failure-mode bullets, honest mock/live ratio.

---

## 7. Phase 2 — BlockRun co-builds the native rail

Now the lifting is shared. BlockRun adds server-side hooks so receipts become **bidirectional** (service attests delivery, not just agent asserting it):

1. **`X-Covenant-Attestation` response header** emitted by BlockRun's gateway/CDP-facilitator path — the service co-signs delivery. Closes gap #1 (service honesty) cryptographically.
2. **Reputation-gated access/pricing** — services can require a minimum Covenant reputation or price by abuse history. Closes gap #2 (Sybil / hostile fork).
3. **Covenant as an ecosystem "Trust" family** alongside Intelligence / Routing / Creation / Trading — the empty category, filled.
4. **Guardrail attestation for `alpha-mcp`** — attest the unmodified guardrail path executed before a fill. Closes gap #4.

---

## 8. Phase 3 — deep

- **Verifiable decision provenance** for Franklin-Trading (persona debate) and polymarket-agent (3-model consensus): attest the reasoning ran *before* the fill, replacing mutable `*.jsonl` / `/tmp`.
- **Dispute / arbitration** layer over receipts (Covenant already has settlement + audit primitives).
- **Cross-marketplace portable reputation** — a receipt earned on BlockRun counts on any other x402 marketplace (PayAI, Xona, etc. already in Covenant's orbit).

---

## 9. Value exchange (both sides win)

**BlockRun gains**
- The delivery/identity/reputation layer their entire autonomous-money pitch implicitly needs.
- "Provenance" becomes real (they already market the word).
- Verifiable routing-savings — fixes the self-reported 78/89/92% credibility problem.
- Anti-Sybil + priced abuse history for services.
- Cryptographic audit logs for their Enterprise tier.
- Fills the empty ecosystem trust category with a partner, not a competitor to payments.

**Covenant gains**
- Distribution at scale: 14.65M settled txns, 1M+ calls/month, ClawRouter (6.6k★), blockrun-mcp (475★), Franklin — each x402 call a potential Covenant attestation.
- A canonical, high-volume x402 counterparty on **both** Base and Solana.
- PROOF/reputation stack validated at real volume.
- Real USDC yield: BlockRun agents pay Covenant for verification over x402 — Covenant becomes a paid service on the largest agent-payment rail.

---

## 10. Constraints honored (guardrails)

- **Never touch payments.** Receipts overlay settlement; Covenant never intermediates USDC flow. (PayAI/Xona discipline.)
- **No new token.** USDC + attestation only. $CVNT stays Solana-canonical and untouched; BlockRun has no token — keep it that way.
- **Plan-first.** This document is the plan; no code, no worktree, no push until operator go.
- **Reuse own infra.** ~80% existing crates; no rewrites.
- **Onchain framing, contact@opencovenant.org** in any outbound copy; don't recap BlockRun's product back at them.
- **Verifiable-as-property**, never "verify it yourself."

---

## 11. Flagship demo (the postworthy artifact)

**"A verifiable Franklin run."** Franklin executes a real coding-or-trading outcome, spending USDC across several models via x402. The Covenant overlay emits one receipt chain binding: Franklin's wallet → Covenant identity, its intent/budget, each requested-vs-served model (including the router's savings claim), and every payment tx hash — all anchored on Solana, independently verifiable. Side-by-side: BlockRun's local mutable `session.jsonl` vs. the Covenant onchain receipt. One is asserted; one is verifiable. That contrast is the whole story.

Meets the x402 bar: novel, demoable, onchain, story-driven.

---

## 12. De-risking / open questions (verify before building)

- Pin exact 402 header casing against a **live** 402 response (`X-Payment-Required` vs `PAYMENT-REQUIRED`; `PAYMENT-SIGNATURE` vs `Payment-Signature`) — docs are inconsistent.
- Confirm the `X-Payment-Receipt` schema actually carries tx hash (+ amount/timestamp?) — one research pass saw it on `/docs/x402/endpoints`, another couldn't confirm the full schema.
- Confirm `blockrun-mcp` and `circle-nanopayment-sample` licenses before forking.
- Verify Solana-side x402 receipt shape (USDC mint `EPjF…Dt1v`) matches Base-side.
- Confirm ClawRouter debug headers are stable enough to attest against.

## 13. The one concrete outreach ask (when operator says go)

"Covenant ships the trust receipt layer as a zero-integration overlay on your x402 rail — you don't write a line for Phase 1. Give us a live 402 endpoint to pin headers against and a slot in your ecosystem trust category, and the first verifiable Franklin run is yours to demo." (No meeting ask; states what's shipping + one concrete ask.)
