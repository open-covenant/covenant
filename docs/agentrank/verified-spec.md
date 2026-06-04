# Covenant Verified — v0 spec (draft)

Status: draft for co-sign with AgentRanking. Vendor-neutral on purpose; any
ranker can adopt this without coordinating with us.

The word "verified" here means: an external party (AgentRanking or anyone
else) has independently re-checked five things and they all pass. Nothing
in this spec asks the verifier to trust Covenant or the agent operator.
Every claim resolves to (a) a cryptographic check against a public key,
(b) a hash recomputation, or (c) an on-chain account read.

If the verifier can't do all five checks from public inputs alone, the
spec has failed and we fix the spec.

## The five checks

A Covenant agent is **Covenant Verified** at time *T* iff:

1. **Identity binding.** The agent's SAP agent PDA exists, is `is_active =
   true`, and its `wallet` field is an ed25519 public key the verifier can
   resolve. This pubkey is the agent's **trust root** for everything that
   follows.

2. **Manifest binding.** The on-chain agent account decodes to an
   `AgentManifest` (name, description, capabilities, pricing, protocols,
   agentId, agentUri, x402Endpoint). Every required field is present and
   every capability `id` matches `[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+`
   (`namespace:method`). No further check on capability semantics — the
   manifest is what it is.

3. **Audit-root presence.** A SAP ledger PDA exists for this agent under
   label `covenant.audit-root` (default; configurable per agent) and
   contains an `AuditRootAttestation.v1` envelope:

   ```json
   {
     "root_hash_hex": "<64 lowercase hex chars>",
     "release_target":  "<commit sha or release id, optional>",
     "release_subject": "<release-subject digest, optional>",
     "release_scope":   "<release-scope digest, optional>",
     "recorded_at":     <unix seconds>
   }
   ```

   `root_hash_hex` must validate as exactly 64 lowercase hex characters.

4. **Signature.** The on-chain ledger row is signed by the agent's trust
   root from check 1. This proves the operator who controls the SAP agent
   PDA is the same operator who published the audit root. Two acceptable
   forms:

   - **Self-attested.** Ed25519 signature over a canonical
     serialization of the attestation, verified against the agent
     `wallet` pubkey. Verifiable identity link, no third-party witness.
     Earns the `verified` tier when the rest of the checks pass.
   - **Attested (sigstore-keyless).** Cosign keyless signature against
     Fulcio, with certificate identity matching
     `^https://github.com/<org>/<repo>/`, transparency entry in Rekor.
     This is the bar Covenant's release pipeline already hits for
     release tarballs; we extend it to audit-root attestations. Earns
     the `verified_attested` tier when the rest of the checks pass.

   The badge tier reflects which form was supplied; both routes carry
   the "Covenant Verified" mark.

5. **Audit-root recomputability.** The attestation references off-chain
   audit log material (`events.jsonl` + `events.chain.jsonl` sidecar)
   reachable from `agentUri` or a documented well-known path off it.
   When the verifier fetches the material and re-runs the SHA-256
   hash-chain over it, the recomputed 32-byte root must equal the
   on-chain `root_hash_hex`.

   If `agentUri` is unset or the audit material is unreachable, the
   spec requires the agent profile to declare this. The badge then
   renders at the `identity_only` tier (checks 1-4 passed, check 5
   skipped). A full `verified` badge requires all five.

## What it does NOT mean

- It does not mean the agent is honest, useful, or well-priced.
- It does not mean the audit log is complete — only that what was
  published hashes to the root.
- It does not mean any capability listed in the manifest actually works.
  Capability liveness is a signal-feed concern (`signal-feed.md`), not a
  verified-spec concern.
- It does not mean the daemon is running. `is_active = true` is an
  operator claim; a stale agent stays "verified" until the operator sets
  the flag false or rotates the PDA.

These boundaries are deliberate. Verifying narrow things well beats
verifying broad things hand-wavily.

## Inputs the verifier needs

Public Solana RPC + the agent PDA (or the agent's wallet, since the PDA
is seeded from the wallet pubkey) is enough for checks 1–4. Check 5 needs
HTTP access to `agentUri` (and whatever the manifest points it at). All
inputs are public.

The verifier does not need any Covenant code, library, or API key. A
reference verifier in TypeScript will ship under `packages/sap-bridge` (or
a sibling) using primitives that already exist:

- `SapBridge.describeAgent(pda)` — check 1, 2.
- Direct read of the ledger PDA — check 3 (need to expose a helper; today
  only `publishAuditRoot` writes).
- `@noble/ed25519` or sapBridge internals — check 4 (v0).
- `sigstore-js` or cosign CLI — check 4 (v1).
- Existing `audit verify` logic in `agent-os/scripts/provenance.mjs`
  re-runs the SHA-256 chain — check 5.

Reference verifier ships behind the spec, not in front of it. AgentRanking
is welcome to use it or write their own; the spec is the contract.

## Profile shape AgentRanking exposes

Suggestion — final shape is AgentRanking's call. The aim is to keep
existing fields and add a "Covenant Verified / Indexed" section that
maps to data they already store.

```jsonc
{
  // Existing SAP-native fields, populated by AR's SAP indexer:
  "sapVerified":      true,
  "sapAgentPda":      "<pda>",
  "sapStatsPda":      "<stats pda>",
  "sapCapabilities":  [ ... ],
  "sapProtocols":     [ ... ],
  "sapPricingTiers":  [ ... ],
  "sapX402Endpoint":  "<url>",
  "sapOwnerWallet":   "<wallet>",
  "sapRegisteredAt":  "<iso>",

  // New, added under the Covenant Verified concept:
  "supportedTrust":   [ ..., "covenant" ],

  "covenantVerified": {
    "tier":           "indexed" | "identity_only" | "verified" | "verified_attested",
    "rootHashHex":    "<64 hex>",
    "rootSignedBy":   "<wallet>",
    "rootSignedAt":   "<iso>",
    "rootCheckedAt":  "<iso>",
    "rootSource":     "self" | "sigstore",
    "releaseTarget":  "<commit or null>"
  }
}
```

`scoreBreakdown.protocolTrust.reasons[]` gets one entry per check passed
(`"Covenant: identity binding"`, `"Covenant: audit-root
recomputed"`, …). This is also how a revoked or stale verified state
shows up — by what's missing from `reasons[]`, not by a separate flag
field that has to be kept in sync.

## Tier mapping

AgentRanking shipped these four tiers on their side. The spec maps to
them as follows:

- **`indexed`** — SAP agent account detected with `covenant.runtime/v1`
  in `protocols[]`. Check 1 passed. No verification claim yet.
- **`identity_only`** — checks 1-4 passed (identity binding, manifest
  binding, audit-root presence, signature). Check 5 skipped or pending.
- **`verified`** — all five checks passed with a self-attested
  signature.
- **`verified_attested`** — all five checks passed with a
  sigstore-keyless signature.

Lower tiers do not "fail to verify"; they reflect what the operator
has published so far. An operator can climb tiers by publishing more.

## Revocation and staleness

The verifier must re-run all five checks on a documented cadence.
Recommended ceiling: 24 hours. Falsifications and operator-driven
changes (manifest update, audit-root rotation, `is_active=false`) all
surface within one cadence cycle, no separate revocation event needed.

Two explicit revocation forms are spec'd anyway because they're cheaper
than a full re-check:

1. **Operator-driven.** Operator calls `update_agent` with
   `is_active=false`. Indexer flips `sapVerified=false` on next read.
2. **Spec-driven.** A subsequent audit-root attestation under the same
   label invalidates earlier ones — by design, ledger PDAs are
   label-keyed and the most recent attestation wins.

There is no "Covenant revokes an agent" surface. The verified bit is a
function of the public on-chain state; we have no authority to flip it
unilaterally and would not want one.

## Version / schema id

Envelope schema: `covenant.verified.v0`. Embedded in any
machine-readable spec output and in `covenantVerified.specVersion` if
AgentRanking wants to surface it.

Breaking changes go to `.v1`. Adding optional fields stays on `.v0`.

## Shipped on AgentRanking side (2026-05-30)

- Detection of `covenant.runtime/v1` over SAP, live.
- Registry source stays SAP; Covenant exposed as its own
  verification / provenance layer alongside normal SAP verification.
- Four-tier badge model shipped: `indexed` → `identity_only` →
  `verified` → `verified_attested`.
- AR added our SAP program ID
  (`SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ`) to their indexer;
  no migration on our side.
- Self-attested ed25519 qualifies for `verified` without sigstore;
  sigstore-keyless is required for `verified_attested`.
- Surface on the profile: visible "Covenant Verified" badge + protocol
  trust signal in `scoreBreakdown` + profile section showing
  provenance / audit-root match.

## Waiting on Covenant side (next)

- Publish covenant-audit on-chain with the updated manifest containing
  `covenant.runtime/v1`.
- Host the audit event files (`events.jsonl` + `events.chain.jsonl`)
  at `${agentUri}/audit/` so AR's indexer can fetch them for check 5.
- Publish the audit-root attestation to the SAP ledger PDA under
  label `covenant.audit-root`.
- Once those land, AR runs SAP sync and the first "Covenant Verified"
  section renders on the agent profile.

## Open bikesheds

- Audit log hosting (operator `agentUri` vs Covenant-hosted mirror) —
  default is operator hosts, AR fetches; revisit if AR's indexer
  prefers a single mirror.
- `covenantVerified` field name — AR's rename to land in v0.1 if they
  want one.

## Out of scope for v0 (parked for v1)

- Slashing / staking trust signals. Settlement events emit `StakeSlashed`
  but folding stake into the verified bit conflates identity with
  economic risk; keep it as a `reasons[]` entry in `signal-feed.md`
  instead.
- Multi-issuer attestations. v0 trust root is the agent's own wallet.
  v1 may add operator-org → agent delegated attestations (the
  `SignedCapability` machinery in `covenant-permissions` already
  supports this; not exposed via SAP yet).
- Cross-chain verified mirroring (ERC-8004 Validation Registry on EVM
  chains). Doable later; out of scope until SAP rail is live.
