# AceData integration — Attested Generative Capabilities

Status: phases 1–3 shipped (provider + provenance + governance + on-chain-by-construction)
Branch: `feat/acedatacloud-integration`
Crate: `agent-os/crates/covenant-acedata`

## Summary

Wire [AceData Cloud](https://acedata.cloud) — a unified gateway to 50+ generative AI
services (image, video, music, TTS, search, LLM) — into Covenant as a first-class
**capability provider**. The value Covenant adds is not tool access; it is the
**governance wrapper**: every generation an agent performs through AceData is
capability-scoped, budget-bounded, recorded in the hash-chained audit log, and
optionally anchored on-chain. The output is *verifiable AI generation* — a provable
record of **who** generated **what**, with **which model**, under **whose authority**,
at **what cost**, with a **content hash** — that neither party produces alone.

AceData is the capability layer. Covenant is the trust layer. They compose; they do
not overlap.

## Why this, and why it does not conflict

Covenant already occupies a clear set of integration surfaces. The naive AceData
integration — "add AceData as another x402 paid-HTTP provider" — would collide head-on
with two of them and add nothing differentiated. We deliberately avoid that.

| Surface | Owner | This integration |
| --- | --- | --- |
| Generic x402 paid-HTTP client | `covenant-x402` (xona) | **reused** optionally for crypto pay-per-call; not duplicated |
| DeFi x402 provider profile (OpenAPI → tools) | `covenant-hyre` (hatcherlabs) | different shape; **not touched** |
| On-chain audit-root anchoring | `covenant-sap-bridge` (synapse/sap) | **reused** to anchor provenance; not duplicated |
| External attestation bridge | `covenant-gitlawb` | complementary; not touched |
| Native MCP tool registry | `covenant-mcp` | extended with new tools (its intended use) |
| **Generative AI capability + content provenance** | **— (empty)** | **AceData fills it** |

The new crate sits in the one empty quadrant of the map: outbound generative capability
bound to content provenance. Payment is treated as *orthogonal* — the default path is
AceData's Bearer-token billing, which never touches the x402 surface at all. Crypto
pay-per-call, if enabled, is a **reuse** of the existing xona x402 client (AceData
speaks x402 on Solana), so even the payment layer composes rather than conflicts.

## Why it is valuable to both sides

- **For Covenant:** plays directly to its differentiators (scoped capabilities, hash-chained
  audit, settlement ledger, on-chain anchoring). The integration is mostly *wiring an
  existing-shaped provider into existing-shaped governance* — low technical risk, high
  narrative payoff. It gives Covenant a demoable, market-relevant hero feature:
  C2PA-style provenance for AI media, enforced by an agent OS and anchored on-chain.
- **For AceData:** a flagship "trust/provenance" reference that differentiates them from
  raw API resellers and aligns with their own crypto-native posture ($ACE on Solana, x402,
  X402Guard spend control). They are a partner and have signalled they will build to
  whatever we propose; this gives them a distribution + credibility story.

## Architecture

New crate: `agent-os/crates/covenant-acedata/` (deliberately **not** `-x402`; this is a
Bearer-token generative provider, not a new payment surface).

```
covenant-acedata/
├── lib.rs         AceDataConfig (api_key, base_url, enabled, model allowlists, budget caps); exports
├── client.rs      async REST client → api.acedata.cloud (Bearer ACEDATACLOUD_API_TOKEN);
│                  task submit + poll (their generation APIs are task-based)
├── catalog.rs     curated tool set + per-tool cost class / input schema
├── tools.rs       impl covenant_mcp::Tool per tool; call() → client → provenance record
└── provenance.rs  AceDataGeneration record: grant id, intent id, model, sha256(prompt),
                   output ref + sha256(output), cost, timestamp — shaped for the audit
                   hash-chain and for optional SAP on-chain anchoring
```

Wiring (points confirmed against current `main` at build time):

- **Tool registry:** extend `tools_vec` at daemon startup (`covenantd/src/main.rs`), gated by
  `[acedata] enabled` in `secrets.toml`. Tool calls auto-emit audit events — no special path.
- **Capability scope:** add `acedata_generate_scope_allows(scope, model, est_cost)` to
  `covenant-permissions`, wired into the dispatch-time `tool_call_scope_allows` path. A grant
  can then say "this agent may call image generation, Flux only, ≤ $2 / session."
- **Audit:** add `AuditKind::AceDataGeneration { intent_id, family, model, prompt_sha256,
  output_ref, output_sha256, cost, settlement_tx }` to `covenant-audit`.
- **Settlement:** emit a `SettlementReceipt` per call into `covenant-settlement`; surface in
  the web console.
- **On-chain (optional):** route the provenance/audit root through the existing
  `covenant-sap-bridge` to anchor a batch of generations on Solana → a verifiable provenance
  certificate. Reuse, not new.
- **Payment (optional):** default is Bearer billing. If an operator wants crypto pay-per-call,
  route AceData's `402` through the existing `covenant-x402` (xona) Solana signer. Reuse, not new.

## Curated first tool set

Start narrow and high-signal rather than exposing all 50+ services:

- `acedata.image.generate` — Flux / Midjourney
- `acedata.music.generate` — Suno
- `acedata.search` — Google SERP
- (phase 2) `acedata.video.generate` — Sora / Veo / Kling · `acedata.tts` — Fish · `acedata.chat` — LLM gateway

## Configuration (phase 1)

Off by default. The daemon reads these env vars at startup (the API key is a Bearer billing
credential, kept off-repo — see `.env.example`):

- `COVENANT_ACEDATA_ENABLED` — `1` / `true` / `yes` to register the tools
- `COVENANT_ACEDATA_API_KEY` — token from the platform.acedata.cloud console (required)
- `COVENANT_ACEDATA_BASE_URL` — API host override (default `https://api.acedata.cloud`)
- `COVENANT_ACEDATA_ALLOW` — comma-separated tool allowlist (empty = all three)
- `COVENANT_ACEDATA_IMAGE_MODEL` / `COVENANT_ACEDATA_MUSIC_MODEL` — default models

Tools registered: `acedata.image.generate` (Flux), `acedata.music.generate` (Suno),
`acedata.search` (Google SERP). Each call returns two result blocks — the raw API response,
then a `provenance` record: `{ provider, tool, model, prompt_sha256, output_sha256, assets,
task_id }`. The output hash is taken over the canonical JSON (JCS) of the response; hashing the
asset bytes themselves is a phase-2 refinement. Because the tools register into the daemon's
tool registry, every call is already capability-gated and audited like any other tool — no new
enforcement path was needed for phase 1.

## Phasing

1. **MVP — shipped.** `covenant-acedata` crate: Bearer client, 3 flagship tools (image, music,
   search), per-call provenance records, env-gated daemon wiring, unit tests + a live smoke
   example. Governed via the existing tool-registry capability path; audited via the existing
   tool-call trail. No chain.
2. **Provenance surfaced + tightened governance — shipped.** A dedicated
   `AuditKind::AceDataGeneration` event (provenance becomes a first-class, hash-chained row, not a
   generic tool-call), a `acedata_generate_scope_allows` capability predicate (a grant may pin a
   `{"models": [...]}` allowlist), enforced by a per-call model gate in an `acedata.*` daemon
   dispatch branch that also records a `ResourceKind::Tool` settlement receipt — all bound to the
   calling agent. Plus a "Generations" panel in `covenant-web` (`/generations`) that reads the
   audit feed and shows model, prompt/output hashes, assets, and task id per generation. Per-call
   cost in the receipt is `0` until AceData returns cost metadata (a partner ask).
3. **On-chain provenance certificate — achieved by construction.** The SAP bridge already anchors
   the *whole* audit-log Merkle root on-chain (`covenant-sap-bridge` `publish_audit_root`, fed by
   the daemon's `verify_integrity` root). Because every generation is an `AceDataGeneration` event
   *in* that log, its provenance is included under the anchored root the moment the SAP bridge is
   enabled (`COVENANT_SAP_ENABLED=1`) — no AceData-specific on-chain code. A verifiable-generation
   certificate is therefore: the `AceDataGeneration` row + the audit integrity proof + the SAP
   on-chain root that covers it.
   - **Per-generation inclusion proof — shipped.** `covenant-audit` now exports an
     `AuditInclusionProof` (`AuditLog::prove_inclusion`) and an offline `verify_inclusion_proof`.
     The audit log is a linear SHA-256 hash chain, so a proof carries the generation's exact event
     line, the chain hash before it, and the per-event hashes after it; folding them reproduces the
     anchored root. Fetch it for any generation via `GET /audit/inclusion/:event_id` (operator-only;
     the `/generations` panel shows the event id), and verify it without trusting the daemon via
     `cargo run -p covenant-audit --example verify_inclusion -- proof.json`. Then compare
     `computed_root_hash_hex` to the SAP on-chain anchor. Remaining optional work: x402-via-xona
     Solana pay-per-call.
4. **North star (optional):** attested AI-media *resale* — Covenant already models resale
   descriptors; an agent-generated asset becomes a verifiably-sourced, tradeable artifact.

## Rejected alternatives

- **AceData as x402 provider profile (a `covenant-hyre` clone):** collides with xona-x402 and
  hatcherlabs; redundant; adds no differentiated value. This is exactly the conflict to avoid.
- **Register AceData's hosted MCP servers as external MCP endpoints (config only):** trivial, but
  it is not really an integration — no capability/audit/settlement leverage, nothing
  Covenant-specific, and under-delivers on a partnership. Viable as a convenience add-on, not the core.
- **Skills-only (`npx skills add acedatacloud/skills`):** Covenant is a Rust daemon, not a
  SKILL.md host; skills are documentation for the agents the runtime *spawns*, not for the daemon.
  Worth shipping to spawned agents in a later phase, but not the spine of the integration.

## x402 pay-per-call (verified live on mainnet, 2026-06-23)

AceData exposes an optional keyless x402 path alongside credit billing.
Discovery: `GET https://platform.acedata.cloud/.well-known/x402`.

A POST to a billed endpoint without an API key returns HTTP 402 with a
mainline x402 **v2** challenge offering Base, Solana, and SKALE. The Solana
accept is USDC with **no `extra.feePayer`**, so the caller funds gas
(self-paid scheme).

Verified flow (`serp/google`, 0.000952 USDC charged, 10 results returned):

1. POST the endpoint unpaid → 402, read the `network:"solana"` accept.
2. Sign with `covenant-x402-signer` (self-paid `SolanaSigner` path), passing
   the requirement with the **CAIP-2** network so the signer accepts it.
3. The signer returns a v1 envelope; re-wrap the signed `payload.transaction`
   as **`{x402Version:2, scheme:"exact", network:"solana", payload}`** — the
   short network id `"solana"`, NOT CAIP-2. AceData rejects CAIP-2 with
   `unsupported_kind`.
4. Retry with the `X-PAYMENT` header → 200; the response carries
   `cost:{amount, currency:"usdc", settlement:"authorized"}`, which the
   provenance record captures.

No API key, no facilitator fee-payer (caller funds gas). The signer's signed
transaction is accepted unchanged; only the envelope version/network wrap
differs from the sponsored (zauth/EarnFi) path.
