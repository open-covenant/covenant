# Metaplex integration

Covenant agents carry an on-chain identity in Metaplex's **MPL Agent registry**
(the "014 Registry") and anchor their audit roots as **MPL Core AppData
attestations** — indexed by DAS, checkable by anyone, with no Covenant
infrastructure in the trust path.

Live on mainnet:

| What | Address |
|---|---|
| Covenant Agents collection (MPL Core) | `Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC` |
| Production agent identity asset (subject) | `9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH` |
| Its 014 Registry record (PDA) | `D3ezfMUeBuXQjdDkKmiY9mHeQxARxsEgrbjRgtPWA14g` |
| Covenant attestation authority (validator) | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Audit-root attestation (ERC-8004 v2) | `7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH` |

Verify either of the first two at
[opencovenant.org/agents](https://opencovenant.org/agents), or on the
[Metaplex agent directory](https://www.metaplex.com/agents/9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH).

## Architecture

```
covenantd ── DaemonMetaplexBridge (covenantd/src/metaplex.rs)
   ├── reads  → covenant-metaplex::das   (HTTP → any DAS provider)      [no key]
   └── writes → covenant-metaplex-signer (subprocess, solana-sdk 3.x)   [isolated minting key]
                  └── mpl-core (AppData) · mpl-agent-identity
```

The daemon never holds the minting key or a `solana-sdk` dependency. Writes
are delegated to the standalone `covenant-metaplex-signer` sidecar over JSON
stdin/stdout — the same isolation pattern as the x402 funding-key signer. The
sidecar pins both program ids, validates payloads before sending, enforces a
per-action lamport cap, and only reports success after the transaction is
confirmed on-chain.

## Tool surface

All tools are capability-gated (`tool.call.metaplex.*`) and namespaced
`metaplex.*`:

| Tool | Kind | Needs |
|---|---|---|
| `metaplex.das.get_asset` | read | `COVENANT_METAPLEX_DAS_URL` |
| `metaplex.das.assets_by_owner` | read | 〃 |
| `metaplex.das.search` | read | 〃 |
| `metaplex.das.get_asset_proof` | read | 〃 |
| `metaplex.attest.audit_root` | write | signer + RPC + keypair |
| `metaplex.identity.register` | write | 〃 |

`metaplex.identity.register` creates an MPL Core asset and calls the
registry's `register_identity_v1` in one transaction, producing the
tamper-evident PDA↔asset binding (`["agent_identity", asset]` under
`1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p`). An optional
`registrationUri` (`https://` or `ar://`) points the registration at an
ERC-8004 registration document so directories can render the agent.

## Autonomous anchoring

With `COVENANT_METAPLEX_AUTO_ATTEST=1`, the daemon anchors its
audit-integrity root to MPL Core on a timer — only when the root has
changed, never for the empty chain, with its own last-root tracking on
disk so restarts don't re-anchor. This is the second anchor beside the
SAP ledger: one attestation source, independent sinks, either can fail
and retry without gating the other.

## Configuration

```sh
COVENANT_METAPLEX_ENABLED=true
COVENANT_METAPLEX_CLUSTER=mainnet-beta          # devnet | mainnet-beta
COVENANT_METAPLEX_DAS_URL=                      # enables reads (Helius/Triton/QuickNode)
COVENANT_METAPLEX_RPC_URL=                      # required for writes
COVENANT_METAPLEX_SIGNER_BIN=                   # path to covenant-metaplex-signer
COVENANT_METAPLEX_KEYPAIR=                      # minting keypair; read by the sidecar only
COVENANT_METAPLEX_COLLECTION=                   # MPL Core collection to mint into
COVENANT_METAPLEX_AGENT_ASSET=                  # this agent's identity asset (attestation subject)
COVENANT_METAPLEX_AGENT_REGISTRATION=           # its 014 registry PDA (attestation subject)
COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS=0     # refuse writes above this cost
COVENANT_METAPLEX_ALLOW=                        # tool-slug allowlist; empty = all
COVENANT_METAPLEX_AUTO_ATTEST=false             # anchor changed audit roots on a timer
COVENANT_METAPLEX_ATTEST_INTERVAL_SECS=900
```

## Attestation schema — `covenant.audit-root.appdata.v2`

Each attestation is a fresh MPL Core asset in the Covenant collection whose
**AppData** external plugin holds one JSON document, shaped as an
[ERC-8004](https://eips.ethereum.org/EIPS/eip-8004) validation response (a
`validator` publishing a `responseHash` commitment about a `subject` agent)
so any wallet/explorer decodes it without Covenant-specific code:

```json
{
  "type": "https://eips.ethereum.org/EIPS/eip-8004#validation-v1",
  "schema": "covenant.audit-root.appdata.v2",
  "subject": {
    "registry": "mpl-agent-014",
    "asset": "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH",
    "registration": "D3ezfMUeBuXQjdDkKmiY9mHeQxARxsEgrbjRgtPWA14g"
  },
  "validator": "DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK",
  "hashAlg": "sha256-merkle",
  "responseHash": "7c375d0e0a749966541c7543b87b76f61fd4b64d41ff12473d68f3ff45caef26",
  "tag": "audit",
  "covenant": {
    "releaseTarget": "covenant",
    "releaseSubject": "witness-loop",
    "releaseScope": "audit"
  },
  "recordedAt": 1781078761
}
```

| Field | Meaning |
|---|---|
| `type` | ERC-8004 validation discriminator; tells a generic reader how to decode the envelope. |
| `schema` | Always `covenant.audit-root.appdata.v2`. Bumped if the field set changes. |
| `subject` | The agent this attests to: 014 `registry` slug, identity `asset`, `registration` PDA. `asset`/`registration` are omitted when the daemon isn't configured with them. |
| `validator` | The attesting authority, stamped by the signer with the key that actually signs. Equals the AppData `data_authority`. |
| `hashAlg` | Always `sha256-merkle`. ERC-8004 commitments are keccak256 `bytes32`; this declares ours is a SHA-256 merkle root. |
| `responseHash` | The agent's audit-chain head: 64 lowercase hex chars. |
| `tag` | ERC-8004 categorization; mirrors `covenant.releaseScope`. |
| `covenant` | Domain identifiers naming what the root covers — never audit-log contents. |
| `recordedAt` | Unix seconds, stamped by the daemon. |

Notes for consumers:

- The on-chain bytes are exactly the JSON above (camelCase). Some DAS
  indexers re-case keys when indexing (Helius returns `response_hash`);
  accept both.
- **Authorship is a chain fact, not a payload claim.** MPL Core only lets
  the AppData `data_authority` write the data, and the signer stamps
  `validator` with that same key. An attestation is Covenant-authored iff
  that authority is the Covenant attestation authority listed above —
  `validator` is a convenience mirror, not the source of trust.

### Verifying an attestation

1. **Authority** — fetch the asset (DAS `getAsset` or raw account) and check
   the AppData plugin's authority address against the Covenant attestation
   authority.
2. **Collection** (identity assets) — the agent identity is grouped under the
   Covenant Agents collection; check the grouping. Audit-root attestations are
   standalone assets, so they are trusted by the authority check above plus the
   reproduced root below, not by collection membership.
3. **Reproduce the root** — fetch the published audit log
   ([`/audit/events.jsonl`](https://opencovenant.org/audit/events.jsonl)) and
   its hash-chain sidecar
   ([`/audit/events.chain.jsonl`](https://opencovenant.org/audit/events.chain.jsonl)),
   then recompute: `event_hash = SHA-256(event line)`,
   `chain_hash = SHA-256(previous_chain_hash + "\n" + event_hash)` (first
   `previous` is 64 zeros). The attested `responseHash` must equal one of the
   recomputed `chain_hash` values. [opencovenant.org/agents](https://opencovenant.org/agents)
   runs exactly this in the browser.
4. **Registry binding** (identity assets) — derive
   `["agent_identity", asset]` under the registry program and check the
   account exists and is owned by it.

### Relation to `mpl-agent-validation`

The registry's validation program (`VALREG…`) is not yet deployed. Its
design carries the same validation semantics this payload already encodes
(subject agent, validator, response commitment). When it ships, Covenant
attestations migrate onto it with the same fields; the v2 envelope is
deliberately ERC-8004 validation-shaped so that migration is a re-anchor,
not a redesign.
