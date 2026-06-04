# AgentRanking ↔ Covenant Integration

Branch: `feat/agentrank-verified`. Working scope for the chiefy7 / AgentRanking
collaboration. Plan-only — no code lands on this branch until the verified spec
is co-signed with AgentRanking and the SAP program-id question is resolved.

## What we're building

Three surfaces, each owned by one side, glued by SAP on Solana:

1. **Covenant Verified spec** (Covenant owns) — vendor-neutral criteria for
   what it means to be a verified Covenant agent. Provenance + on-chain
   audit-root match, not self-reported compatibility. See `verified-spec.md`.
2. **Signal feed** (Covenant owns) — runtime signals AgentRanking polls to
   power reputation, separate from the static verified badge. See
   `signal-feed.md`.
3. **AgentRanking surfacing** (AgentRanking owns) — "Covenant Verified /
   Indexed" section on the agent page, mapping Covenant signals into the
   existing `scoreBreakdown.protocolTrust.reasons[]` slot and adding
   `covenant` to `supportedTrust`. We don't dictate UI; we hand them a clean
   schema.

## Why SAP is enough to start

AgentRanking already runs a SAP indexer (Solana identity contract
`8oo4dC4JvBLwy5tGgiH3WwK4B9PWxL9Z4XjA2jzkQMbQ`, chainId `900001`) and the
profile schema already has first-class SAP fields: `sapVerified`,
`sapAgentPda`, `sapStatsPda`, `sapCapabilities`, `sapPricingTiers`,
`sapProtocols`, `sapX402Endpoint`, `sapRegistrationTx`, `sapOwnerWallet`,
`sapRegisteredAt`, `sapOperatorClaimed`, `sapTwinMatch`,
`sapClaimAgentDbId`, `listingRegistrySource`.

Covenant's `packages/sap-bridge` already writes a full `AgentManifest` to a
SAP agent PDA (name, description, capabilities, pricing, protocols,
agentId, agentUri, x402Endpoint) and a 32-byte audit-root hash to a SAP
ledger PDA. **So discovery needs no new endpoint** — we publish reference
agents over SAP, AgentRanking's existing indexer picks them up, and the
"verified" section is layered on top of fields they already store.

The only delta to surface today is "verified": AgentRanking has KYA ($99,
first-party) but no documented partner-verified schema. That's the thing
we're co-defining.

## Resolved: SAP program id

AgentRanking will add our program ID
(`SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ`) as a recognized SAP
source in their indexer rather than ask us to re-register against
`8oo4dC4JvBLwy5tGgiH3WwK4B9PWxL9Z4XjA2jzkQMbQ`. Our existing reference
agent (PDA `CkyhgJ…`, wallet `AdChc…`) stays where it is, no migration.

Concrete inputs they need to wire this are in
`indexer-integration.md`.

## Phasing

**Phase 0 — alignment (this branch, no code shipped).**
- Co-sign `verified-spec.md` minimum criteria with AgentRanking.
- Resolve the SAP program-id question.
- Map each verified criterion to a profile field or
  `scoreBreakdown.protocolTrust.reasons[]` entry.

**Phase 1 — reference agents on SAP devnet.**
- Register the agents in `reference-agents.md` against whichever SAP
  program AgentRanking indexes.
- Confirm each lands as a profile on AgentRanking with the expected
  `sapCapabilities` / `sapProtocols` / `sapPricingTiers`.

**Phase 2 — verified signal.**
- Publish audit-root attestations for each reference agent.
- AgentRanking's SAP indexer reads the ledger PDA, validates per
  `verified-spec.md`, sets the verified flag, and renders the
  "Covenant Verified" section.

**Phase 3 — signal feed.**
- Stand up the read-only HTTP service in `signal-feed.md` (wraps
  sap-bridge `describeAgent` + indexer settlement aggregates + FairScale
  conduct events). AgentRanking polls it on a documented cadence and
  surfaces signals as `reasons[]` in `scoreBreakdown`.

**Phase 4 — public discovery.**
- Cross-link: Covenant registry links out to AgentRanking profiles;
  AgentRanking lists Covenant Verified agents in a public showcase
  row.

## Ownership boundary

Covenant owns:
- SAP agent registration (write). Already shipped in `packages/sap-bridge`.
- Audit-root attestation (write). Already shipped; currently unsigned,
  see verified-spec §3.
- Signal feed HTTP service. New thin service; wraps existing data
  sources.

AgentRanking owns:
- Verified validation logic (re-checking). They re-validate per the
  spec — we don't ask them to trust us.
- Profile UI, verified badge, ranking weight. We supply data; they
  decide presentation.
- Mapping signal values into `scoreBreakdown.protocolTrust.reasons[]`,
  per a documented contract we agree on.

Co-owned:
- The verified spec text and criteria. Vendor-neutral so any ranker
  can adopt.

## Files on this branch

- `docs/agentrank/README.md` — this file.
- `docs/agentrank/verified-spec.md` — Covenant Verified v0 draft.
- `docs/agentrank/signal-feed.md` — signal feed schema + service plan.
- `docs/agentrank/reference-agents.md` — first batch of reference agents
  to publish.

## Questions for chiefy7

Carried forward to `verified-spec.md` and `signal-feed.md` per topic. Top
five blocking ones, consolidated:

1. **SAP program id** — `8oo4dC4J…` vs `SAPpUhsW…`. Same or different?
2. **Verified badge surfacing** — can AgentRanking add a partner-verified
   badge (analogous to the gold check) keyed off the SAP audit-root
   attestation, or do we ride `metadata[].key/value` and
   `scoreBreakdown.protocolTrust.reasons[]` only?
3. **`supportedTrust` enum** — does adding `covenant` require config on
   your end, or is the enum open?
4. **Signal feed cadence / shape** — what poll interval, payload size
   cap, and pagination style does the indexer prefer? Cursor or
   since-timestamp?
5. **Reference-agent slugs** — do we get to pick the slugs under
   `/agents/900001/<slug>` or are they assigned at index time from the
   on-chain agent id?
