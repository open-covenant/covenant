# Metaplex integration

Covenant has registered selected MPL Core assets with Metaplex's **MPL Agent
registry** and published AppData commitments under a configured data authority. The registry
binds a PDA to an asset; it does not establish the real-world operator or make
the asset trustworthy. AppData authority attributes bytes to a configured key;
it does not establish claim truth or log completeness. The public reader uses
configured DAS/RPC providers and reports those dependencies explicitly.

Live on mainnet:

| What | Address |
|---|---|
| Covenant Agents collection (MPL Core) | `Duqs6dq1wXPcRqJVUCgSZxrkLRdg3oBfZ3ViER1kt6gC` |
| Configured agent asset (subject) | `9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH` |
| Its 014 Registry record (PDA) | `D3ezfMUeBuXQjdDkKmiY9mHeQxARxsEgrbjRgtPWA14g` |
| Configured AppData data authority | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Historical AppData commitment | `7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH` |

Inspect configured-provider observations at
[opencovenant.org/agents](https://opencovenant.org/agents), or on the
[Metaplex agent directory](https://www.metaplex.com/agents/9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH).

## Architecture

```
covenantd ── DaemonMetaplexBridge (covenantd/src/metaplex.rs)
   └── reads → covenant-metaplex::das (HTTP → configured DAS provider) [no key]

explicit manual/development use
   └── covenant-metaplex-signer (subprocess, solana-sdk 3.x) [key holder]
       └── mpl-core (AppData) · mpl-agent-identity
```

The production daemon filters funded Metaplex names from discovery and refuses
them before capability lookup, sidecar construction, RPC, or key access. The
lower-level sidecar remains available for explicit manual and development use.
It is process separation, not a security isolation boundary: both processes run
on the same host under one operator. The sidecar pins both program ids,
validates its accepted payload shape, applies a per-action lamport cap, and
reports after RPC confirmation.

## Tool surface

The read tools are capability-gated (`tool.call.metaplex.*`) and namespaced
`metaplex.*`. The two legacy funded names are retained by the lower-level crate
but are neither advertised nor callable through the daemon:

| Tool | Kind | Needs |
|---|---|---|
| `metaplex.das.get_asset` | read | `COVENANT_METAPLEX_DAS_URL` |
| `metaplex.das.assets_by_owner` | read | 〃 |
| `metaplex.das.search` | read | 〃 |
| `metaplex.das.get_asset_proof` | read | 〃 |
| `metaplex.attest.audit_root` | parked daemon write | explicit lower-level use only |
| `metaplex.identity.register` | parked daemon write | explicit lower-level use only |

`metaplex.identity.register` creates an MPL Core asset and calls the
registry's `register_identity_v1` in one transaction, producing the
tamper-evident PDA↔asset binding (`["agent_identity", asset]` under
`1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p`). An optional
`registrationUri` (`https://` or `ar://`) is untrusted onchain input that may
point to an application registration document. Readers must not treat the URI
or its contents as identity proof.

## Autonomous anchoring

`COVENANT_METAPLEX_AUTO_ATTEST=1` no longer starts a driver. The daemon logs a
stable parked warning instead. Timer-driven funded anchoring remains off until
it has a cumulative budget, durable reservation, and idempotent reconciliation.
The lower-level driver code is retained for tests and future redesign; it is not
a second independent witness and the production binary does not launch it.

## Configuration

```sh
COVENANT_METAPLEX_ENABLED=true
COVENANT_METAPLEX_CLUSTER=mainnet-beta          # devnet | mainnet-beta
COVENANT_METAPLEX_DAS_URL=                      # enables reads (Helius/Triton/QuickNode)
COVENANT_METAPLEX_RPC_URL=                      # lower-level/manual writes only
COVENANT_METAPLEX_SIGNER_BIN=                   # lower-level/manual sidecar
COVENANT_METAPLEX_KEYPAIR=                      # lower-level/manual keypair path
COVENANT_METAPLEX_COLLECTION=                   # MPL Core collection to mint into
COVENANT_METAPLEX_AGENT_ASSET=                  # identity asset named by the commitment
COVENANT_METAPLEX_AGENT_REGISTRATION=           # its 014 registry PDA
COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS=0     # 0/unset/invalid disables daemon write tools
COVENANT_METAPLEX_ALLOW=                        # tool-slug allowlist; empty = all
COVENANT_METAPLEX_AUTO_ATTEST=false             # true logs parked; no driver starts
COVENANT_METAPLEX_ATTEST_INTERVAL_SECS=900
```

The daemon never advertises or invokes write tools. Independently, its legacy
sidecar configuration considers writes disabled unless the per-action cap is a
positive integer. Zero, an unset value, or an invalid value fails closed. When
the signer binary is run directly for explicit manual development, an unset cap
uses its built-in
20,000,000-lamport ceiling; an explicit zero or invalid value is rejected before
the keypair is loaded.

## Historical AppData commitment format — `covenant.audit-root.appdata.v2`

Each historical record is a fresh MPL Core asset whose **AppData** external
plugin holds one Covenant-specific JSON document. Its `type` URI borrows an
[ERC-8004](https://eips.ethereum.org/EIPS/eip-8004) label, but the payload was
not written through the ERC-8004 Validation Registry and the matching shape
does not create ERC-8004 interoperability or generic-wallet semantics:

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
| `type` | Historical application type tag. It does not by itself provide ERC-8004 interoperability. |
| `schema` | Always `covenant.audit-root.appdata.v2`. Bumped if the field set changes. |
| `subject` | The agent named by the commitment: 014 `registry` slug, identity `asset`, registration PDA. `asset`/`registration` are omitted when the daemon isn't configured with them. |
| `validator` | A historical payload field expected to equal the configured AppData data authority. The authority field, not this string, is the key-attribution signal. |
| `hashAlg` | Always `sha256-merkle`. ERC-8004 commitments are keccak256 `bytes32`; this declares ours is a SHA-256 merkle root. |
| `responseHash` | The agent's audit-chain head: 64 lowercase hex chars. |
| `tag` | Historical application categorization; mirrors `covenant.releaseScope`. |
| `covenant` | Domain identifiers naming what the root covers — never audit-log contents. |
| `recordedAt` | Unix seconds, stamped by the daemon. |

Notes for consumers:

- The on-chain bytes are exactly the JSON above (camelCase). Some DAS
  indexers re-case keys when indexing (Helius returns `response_hash`);
  accept both.
- A directly decoded Core account can show which AppData data authority was
  authorized to write the stored bytes. A DAS response is a provider report of
  that state. Either observation attributes bytes to a configured key; neither
  proves the claim, the operator behind the key, or evidence completeness.

### Inspecting a commitment

1. **Authority observation** — fetch the asset and compare the AppData data
   authority with the configured Covenant key. Label a DAS result as
   provider-backed unless the Core account is decoded or proven independently.
2. **Collection** (identity assets) — the agent identity is grouped under the
   Covenant Agents collection; check the grouping. Audit-root commitments are
   standalone assets. Collection membership is classification, not trust.
3. **Optional supplied-log check** — if the event lines and chain sidecar are
   separately obtained, recompute
   ([`/audit/events.jsonl`](https://opencovenant.org/audit/events.jsonl)) and
   its hash-chain sidecar
   ([`/audit/events.chain.jsonl`](https://opencovenant.org/audit/events.chain.jsonl)),
   then recompute: `event_hash = SHA-256(event line)`,
   `chain_hash = SHA-256(previous_chain_hash + "\n" + event_hash)` (first
   `previous` is 64 zeros). A matching root only covers the supplied lines and
   does not prove that the log is complete or runtime-mediated. The current
   `/agents` page does not perform this recomputation.
4. **Registry binding** (identity assets) — derive
   `["agent_identity", asset]` under the registry program and check the
   account exists, is owned by the registry program, is exactly 40 bytes, and
   stores the same asset key after its 8-byte header.

### Relation to official `mpl-agent` work

The official `metaplex-foundation/mpl-agent` repository now contains
`mpl-agent-validation` and `mpl-agent-reputation`, which its README describes
as not yet finalized. No migration compatibility is promised. Future work
should first agree on a concrete missing interface with upstream maintainers;
the historical Covenant payload must not be presented as the future standard.
