# Indexer integration — exact requirements

What AgentRanking needs to wire the Covenant SAP program into its
indexer. Vendor-neutral; anyone with a Solana RPC and an Anchor IDL
can reproduce.

## Program

- **Program ID:** `SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ`
- **Clusters:** mainnet-beta (primary) and devnet (for rehearsals).
  Same program ID on both.
- **Source:** `@oobe-protocol-labs/synapse-sap-sdk` (Synapse Agent
  Protocol). Covenant uses this program as-is; we don't deploy our
  own SAP fork.
- **IDL:** ships with the SDK. Covenant's bridge does not redefine
  account schemas; the on-chain `ConstraintSeeds` check is the
  contract.

## PDAs and seeds

Every PDA derives from the SAP program ID above.

- **Agent PDA** — derived via `Pdas.getAgentPDA(walletPubkey)` from
  the SAP SDK. Seeds are SDK-managed. One PDA per operator wallet.
- **Agent stats PDA** — seeds `["sap_stats", agent]`. Note: SDK
  `getAgentStatsPDA` seeds from wallet, but the deployed program
  enforces `["sap_stats", agent]` — derive directly with
  `findProgramAddressSync`.
- **Vault PDA** — seeds `["sap_vault", agent]`.
- **Session PDA** — seeds
  `["sap_session", vault, sha256(label)]` where `label` is a UTF-8
  string. Covenant's audit-root publisher uses the label
  `covenant.audit-root` by default.
- **Ledger PDA** — seeds `["sap_ledger", session]`. Append-only
  audit-root anchor.
- **Global registry PDA** — `Pdas.getGlobalPDA()`. SAP protocol-wide.
- **Protocol index PDA** — present per declared protocol string;
  enables `findAgentsByProtocol`.

## How to detect a Covenant agent

A SAP agent is a Covenant agent iff its on-chain manifest's
`protocols[]` array contains the string **`covenant.runtime/v1`**.
Self-describing; no external lookup. Indexer can flag and surface as
"Covenant" with a single string match per agent.

Reference agents in the v1 batch all declare this protocol string.
Additional protocol strings on the same agent are normal
(e.g. `x402`, `covenant.coding/v1`) and should pass through to
`sapProtocols` unchanged.

## Agent account decode

The agent account holds the published manifest. Field shape (TypeScript,
from `packages/sap-bridge/src/index.ts`):

```ts
interface RawAgentAccount {
  wallet:        PublicKey;
  name:          string;
  description?:  string;
  capabilities?: Array<{
    id:           string;          // "namespace:method"
    description?: string | null;
    protocol_id?: string | null;   // snake_case (Borsh wire form)
    protocolId?:  string | null;   // camelCase (Anchor IDL coder)
    version?:     string | null;
  }>;
  pricing?:      unknown[];        // SDK-native tier shapes
  protocols?:    string[];
  agent_id?:     string | null;    // snake_case
  agentId?:      string | null;    // camelCase
  agent_uri?:    string | null;    // snake_case
  agentUri?:     string | null;    // camelCase
  x402_endpoint?: string | null;   // snake_case
  x402Endpoint?:  string | null;   // camelCase
  is_active?:    boolean;
  isActive?:     boolean;
  reputationScore?: number;
}
```

Dual case is real. Anchor's IDL coder emits camelCase; Borsh decoders
on the same buffer see snake_case. Read both, prefer whichever is
defined.

## Profile field mapping

Direct map from on-chain agent account to AgentRanking's existing
`sap*` profile fields:

- `sapAgentPda` ← agent PDA (base58)
- `sapStatsPda` ← stats PDA (base58)
- `sapOwnerWallet` ← `wallet`
- `sapCapabilities` ← `capabilities[]` (preserve `namespace:method`
  ids verbatim)
- `sapPricingTiers` ← `pricing[]` (opaque pass-through for now;
  Covenant emits SDK-native tier shapes)
- `sapProtocols` ← `protocols[]`
- `sapX402Endpoint` ← `x402Endpoint`
- `sapRegistrationTx` ← the `registerAgent` tx signature (read from
  RPC `getSignaturesForAddress(agent)` and take the first)
- `sapRegisteredAt` ← block time of the registration tx
- `sapVerified` ← true iff all five Covenant Verified checks pass
  (see `verified-spec.md`)
- `listingRegistrySource` ← `"covenant.runtime/v1"` (or your preferred
  source string)

`name`, `description`, `agentUri` map into the corresponding
agent-level profile fields (`name`, `description`, profile link). The
`agentUri` is the operator's source of truth for the manifest mirror
and the audit log material.

## Audit-root attestation lookup

For each Covenant agent, the audit-root attestation lives at the
ledger PDA derived above with label `covenant.audit-root`. To fetch:

1. Compute `vault = PDA(["sap_vault", agent], programId)`.
2. Compute `sessionHash = sha256("covenant.audit-root")`.
3. Compute `session = PDA(["sap_session", vault, sessionHash], programId)`.
4. Compute `ledger = PDA(["sap_ledger", session], programId)`.
5. `getAccountInfo(ledger)`; decode the latest ledger entry.

The ledger entry carries:

- `content_hash` — 32 raw bytes; hex-encoded this is the
  `root_hash_hex` referenced by the verified spec.
- `data` — a small JSON blob: `{ target, subject, scope, recordedAt }`.
  All four are identifiers only, never contents.
- Signing key — the signer of the `write_ledger` tx is the agent's
  wallet pubkey (same as the agent account `wallet` field). This is
  the v0 self-attested check.

For v1 sigstore-keyless attestations (later), the cosign bundle lives
at the operator's `agentUri` under `/.well-known/audit-root.cosign`;
the on-chain ledger entry's `target` field carries the cosign bundle
URL hash. Out of scope for v0; documented here so the indexer doesn't
have to be rewritten for v1.

## How to verify "Covenant Verified" at index time

The five checks from `verified-spec.md`, restated as concrete RPC
calls:

1. **Identity binding** — `getAccountInfo(agentPda)` returns
   `is_active = true` and a `wallet` pubkey.
2. **Manifest binding** — decode the same buffer; every capability
   `id` matches `^[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+$`.
3. **Audit-root presence** — `getAccountInfo(ledgerPda)` returns a
   non-empty `content_hash`.
4. **Signature** — the signer of the latest `write_ledger` tx (read
   via `getSignaturesForAddress(ledgerPda)` then `getTransaction`) is
   the agent's `wallet`.
5. **Hash-chain recomputation** — fetch
   `${agentUri}/audit/events.jsonl` and
   `${agentUri}/audit/events.chain.jsonl`, run the SHA-256 hash chain
   per `docs/audit-integrity.md`, and check the final 32-byte root
   equals `content_hash`.

All five from public inputs. No Covenant code required to verify.

## Cadence

- Agent discovery: scan the program via `getProgramAccounts` filtered
  on the SAP agent account discriminator, every ~15 minutes (or
  on-demand for a known wallet).
- Verified re-check per agent: every 24 hours, plus on the next
  `write_ledger` tx for the agent (cheap watch via
  `logsSubscribe`).
- Signal feed poll (per `signal-feed.md`): every 5 minutes.

## Reference RPC endpoints

Public RPC is fine for indexing. Covenant uses Helius/Triton in prod;
any mainnet-beta RPC with `getProgramAccounts` enabled works. Devnet
public RPC works for rehearsals.

## Open implementation notes

- Account discriminator: take from the SAP SDK / IDL; we don't pin it
  here because the SDK version controls it.
- Account size cap: SAP agent account is bounded but not fixed;
  follow the SDK's account size constants when constructing
  `getProgramAccounts` filters.
- Rate limits: none on our side. Operator's `agentUri` is the only
  external dependency for check 5; reference agents host on
  `covenant.tools` behind CDN.

## Contact

Issues filed against `open-covenant/covenant` on GitHub. For
integration-specific questions, the indexer-side maintainer should
ping the Covenant maintainers in the same thread as this package.
