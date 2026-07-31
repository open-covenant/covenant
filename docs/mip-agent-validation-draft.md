# Agent Validation Records for MIP-014 Identities

Status: Community proposal draft. No MIP number has been assigned.

Implementation note: The proposer is ready, willing, and able to maintain the
schema, vectors, and verifier as a community implementation. The proposed
record format requires no change to MPL Core or to the deployed MIP-014
identity program.

## Summary

MIP-014 gives an agent a stable identity: an MPL Core asset bound to an Agent
Registry PDA. It intentionally leaves validation and reputation to later
layers.

This proposal defines an additive **Agent Validation Record v1** profile. A
record is JSON in an MPL Core AppData external plugin. It binds:

- a MIP-014 subject asset;
- a validator key;
- a namespaced validation profile;
- a 32-byte response commitment; and
- the validator-asserted time of the observation.

The security property is narrow and explicit: a consumer that pins validator
key `V` can prove that the AppData adapter whose `data_authority` is
`PluginAuthority::Address { address: V }` currently contains the record. The
record does not prove that the validator is honest, that the committed evidence
is correct, or that an agent is safe. Those remain consumer policy.

Direct Solana account reads and MPL Core decoding are the normative
verification path. DAS is an indexed discovery and read convenience with
freshness, omission, provider, and pagination assumptions.

Oracle-based lifecycle enforcement is not part of record conformance. It is
specified separately as an optional, experimental extension.

## Requested decision and implementation

This proposal asks Metaplex governance to adopt the v1 type identifier, JSON
Schema, authority-verification procedure, and conformance vectors as the
standard validation-record profile for MIP-014 identities. Approval would
standardize the envelope and verification contract; it would not deploy or
modify an on-chain program.

The proposer requests Community Implementation designation and will maintain
the schema, vectors, and reference verifier in a public repository. The
canonical artifacts submitted for review are versioned together. Any future
program-mediated transport, record version, or Core or registry change
requires separate review.

## Motivation

An identity registry answers “which on-chain identity names this agent?” It
does not answer “which validator made this claim about the agent, under which
schema, and what evidence did the validator commit to?”

Without a shared envelope, each directory and marketplace must learn a
validator-specific payload and can easily make unsafe assumptions:

- treating the first AppData adapter as a validation record even though an
  agent asset may carry multiple validation or application AppData adapters;
- trusting a payload `validator` string without checking the AppData
  `data_authority`;
- trusting an adapter's top-level configuration authority instead of the
  authority that can actually write its data;
- reading one partial DAS page and presenting it as a complete history; or
- treating a signed commitment as a positive verdict.

A small record profile gives indexers, wallets, marketplaces, and agents the
same fail-closed verification contract while leaving validator selection and
validation semantics open to competition.

## Current implementation landscape

This proposal builds on MIP-014 and acknowledges the implementation already
present in the official `metaplex-foundation/mpl-agent` repository.

At repository commit
`326b76a46aa3b0dd6400f7a318992d537470c57c`, that repository contains:

- an `mpl-agent-validation` program that declares program ID
  `VALREGY66A9ieJfFUNs5GrxFTy498KUoSU7TbmSePQi`;
- an `AgentValidationV1` PDA derived from
  `["agent_validation", asset]`;
- a `RegisterValidationV1` instruction;
- a generated IDL and JavaScript and Rust clients; and
- JavaScript and Rust registration tests.

The repository describes its validation and reputation programs as not yet
finalized. The current validation instruction creates the PDA and installs a
**Binary** AppData adapter on the subject asset whose `data_authority` is that
PDA. Its current IDL does not define a response-write instruction or the JSON
payload in this proposal.

This proposal does not replace, rename, or claim wire compatibility with that
work. It standardizes a direct-key JSON record profile that can ship without a
new program. A future program-mediated profile can reuse the envelope only
after it defines how program state binds a validator to the PDA and how
responses are authorized and written. That profile would have a different
authority-verification procedure.

## Goals

- Define an unambiguous JSON envelope for a validator-authored claim about a
  MIP-014 agent.
- Make the on-chain AppData `data_authority` the authorship fact.
- Require consumers to pin both the validator and the validation profile.
- Keep direct RPC and account decoding sufficient for verification of a known
  record.
- Make DAS useful for indexing without making it the trust root.
- Remain additive to MIP-014, MPL Core, and the existing
  `mpl-agent-validation` implementation.
- Publish a machine-readable JSON Schema, valid and invalid vectors, and a
  dependency-free reference verifier.

## Non-goals

- Selecting which validators a consumer should trust.
- Assigning a universal score or declaring an agent safe.
- Proving the truth of evidence committed by `responseHash`.
- Defining validator incentives, slashing, or dispute resolution.
- Defining a request/response workflow.
- Gating agent execution or MPL Core lifecycle events.
- Claiming Metaplex Foundation or Solana Foundation endorsement.
- Providing automatic interoperability with ERC-8004.

## Conventions

The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted
as described in RFC 2119 and RFC 8174.

`base58(pubkey)` means Solana's base58 text encoding of exactly 32 bytes.
Comparisons of public keys, type identifiers, schema identifiers, and algorithm
identifiers are exact and case-sensitive.

## Terminology

- **Agent identity**: an MPL Core asset registered under MIP-014.
- **Record asset**: the MPL Core asset whose AppData adapter carries a
  validation record.
- **Validator**: the key a consumer has chosen as a trust root for a validation
  profile.
- **Profile**: the namespaced semantics identified by the payload `schema`.
- **Record-authentic**: the record is well-formed and its AppData
  `data_authority` is the `PluginAuthority::Address` variant whose address
  equals the pinned validator.
- **Evidence-verified**: the profile-specific evidence recomputes to
  `responseHash`.
- **Policy-accepted**: a consumer has decided the validator, profile, evidence,
  freshness, and result satisfy its own policy.

These states are distinct. Record authenticity alone MUST NOT be displayed as
evidence verification or policy acceptance.

## Specification

### Record representation

A v1 record is a JSON-encoded AppData external plugin on an MPL Core asset. Its
AppData adapter MUST use `ExternalPluginAdapterSchema::Json`.

MPL Core permits multiple AppData adapters on one asset, each keyed by a
different data authority. A consumer MUST enumerate the adapters and select by
all of:

1. external adapter type `AppData`;
2. JSON encoding;
3. the `PluginAuthority::Address` data-authority variant and locally pinned
   address;
4. payload `type`; and
5. the locally pinned payload `schema`.

A consumer MUST NOT select the first AppData adapter. It MUST NOT classify an
asset as a validation record merely because the asset has AppData. MIP-014
agent assets and unrelated applications can legitimately carry their own
AppData.

Writers SHOULD mint a new record asset for each observation instead of
overwriting an earlier record. AppData remains mutable by its data authority,
so a consumer that needs historical non-equivocation SHOULD also retain the
record transaction signature, slot, and observed account bytes.

### Canonical payload

The proposed v1 type identifier is:

```text
mpl.agent.validation-record.v1
```

It is proposed by this document and is not an assigned MIP identifier.

```json
{
  "type": "mpl.agent.validation-record.v1",
  "schema": "org.example.validation-profile.v1",
  "subject": {
    "registryProgram": "1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p",
    "asset": "<MIP-014 agent Core asset>",
    "registration": "<optional canonical Agent Identity PDA>"
  },
  "validator": "<validator public key>",
  "hashAlg": "<profile-defined commitment algorithm>",
  "responseHash": "<32-byte lowercase hexadecimal commitment>",
  "tag": "<optional profile categorization>",
  "recordedAt": 1782120658,
  "extensions": {
    "org.example": {}
  }
}
```

The canonical on-chain keys are camelCase. An indexer MAY return re-cased
fields such as `hash_alg`, `response_hash`, and `recorded_at`. A DAS client MAY
accept those aliases at the indexer boundary, but a writer MUST write the
canonical camelCase form. A verifier MUST reject an object that contains both
forms of the same field.

The normative JSON Schema is
[agent-validation-record-v1.schema.json](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/agent-validation-record-v1.schema.json).

#### Fields

- `type` (required): exactly `mpl.agent.validation-record.v1`.
- `schema` (required): the validation profile and version. A verifier MUST be
  configured with the exact expected value and MUST reject unknown profiles.
- `subject.registryProgram` (required): the MIP-014 registry program the
  verifier expects on the selected cluster.
- `subject.asset` (required): the registered MPL Core agent asset.
- `subject.registration` (optional): the canonical Agent Identity PDA. If
  present, it
  MUST equal the canonical PDA derived from
  `["agent_identity", subject.asset]` under `subject.registryProgram`.
- `validator` (required): a convenience mirror of the AppData
  `data_authority`. It MUST equal the locally pinned validator and the
  on-chain authority.
- `hashAlg` (required): a profile-defined identifier. A verifier MUST reject an
  algorithm it has not implemented for that profile.
- `responseHash` (required): exactly 32 bytes encoded as 64 lowercase
  hexadecimal characters.
- `tag` (optional): profile-defined categorization.
- `recordedAt` (required): non-negative Unix seconds asserted by the
  validator. It is not a block timestamp and MUST NOT be treated as one.
- `extensions` (optional): profile-specific fields keyed by a namespace
  controlled by the extension author.

Unknown top-level keys are invalid in v1. New portable fields require a new
record version; profile-specific data belongs under `extensions`.

### MIP-014 subject verification

A verifier MUST establish that the subject is a registered MIP-014 identity,
not merely trust the payload:

1. fetch `subject.asset` from the same explicit Solana cluster;
2. require the account owner to be the expected MPL Core program and decode it
   as a Core asset;
3. require the asset to carry the MPL Core `AgentIdentity` external plugin;
4. derive the canonical Agent Identity PDA from
   `["agent_identity", subject.asset]` under the pinned registry program;
5. fetch that PDA and require its owner to equal the pinned registry program;
6. decode a locally supported Agent Identity account version and require its
   stored asset to equal `subject.asset`; the current official source contains
   both `AgentIdentityV1` and `AgentIdentityV2`, so a verifier MUST check the
   discriminator before selecting the layout; and
7. if `subject.registration` is present, require exact equality with the
   derived PDA.

Account existence alone is insufficient.

### Normative verification by direct RPC

Verification of a known record asset MUST be possible without DAS:

1. Select the Solana cluster, finality, MPL Core program, MIP-014 registry
   program, expected validator, expected record type, expected profile, and
   supported hash algorithms as local policy inputs.
2. Call `getAccountInfo` for the record asset at the selected finality.
3. Require the account owner to equal the expected MPL Core program.
4. Decode the account with the corresponding MPL Core account decoder.
5. Enumerate external adapters. Select the AppData key equivalent to
   `ExternalPluginAdapterKey::AppData(PluginAuthority::Address {
address: expected_validator })`.
6. Require the selected adapter's schema to be JSON and decode its bytes as
   UTF-8 JSON.
7. Require the exact expected `type` and `schema`, then apply the JSON Schema
   and all field checks above.
8. Require `validator == expected_validator`.
9. Perform the MIP-014 subject verification.
10. If claiming evidence verification, run the profile's exact `hashAlg`
    algorithm and compare the result with `responseHash`.

The verifier MUST fail closed on an absent adapter, malformed account,
unsupported layout, unsupported algorithm, ambiguous candidate, wrong owner,
wrong PDA, or unknown profile.

The record transaction signature and slot are useful observation metadata but
are not fields authenticated by the current AppData payload. A consumer that
uses them MUST obtain them from chain history or a separately trusted index.

### DAS indexed read path

DAS can make records easy to discover and render. It is not the normative
trust root.

The v1 envelope makes a discovered record unambiguous; it does not create a
globally complete subject-to-record index. Complete discovery requires an
indexer that scans the relevant MPL Core updates, a directory that receives
record IDs, or a future registry extension. A verifier given a record ID does
not need to trust that discovery service to authenticate the current account.

For a DAS `getAsset` response, a verifier MUST inspect each
`external_plugins[]` entry and require:

```text
plugin.type == "AppData"
plugin.adapter_config.schema == "Json"
plugin.adapter_config.data_authority.type == "Address"
plugin.adapter_config.data_authority.address == expected_validator
plugin.data.type == "mpl.agent.validation-record.v1"
plugin.data.schema == expected_profile
```

Equivalent camelCase field names MAY be accepted when a provider uses them.
The verifier MUST pin both the exact `Address` discriminant and
`adapter_config.data_authority.address`. A response that omits the discriminant
may be used for discovery but MUST NOT establish record authenticity.
Conflicting snake_case and camelCase aliases MUST be rejected. The verifier
MUST NOT use the plugin's top-level `authority.address` as authorship evidence;
the plugin configuration authority and the AppData write authority are
different roles.

A DAS provider can omit a record, return stale state, lag an update, re-case a
payload unexpectedly, or be unavailable. A high-assurance consumer SHOULD
confirm the selected record through direct RPC before a payment, signature, or
other consequential action.

Discovery queries MUST traverse the complete result set. A page-based client
must continue until an empty or short page using stable sort parameters. A
cursor-based client must follow the returned cursor to exhaustion and use the
provider-required ID sort. An implementation MUST NOT silently stop after a
fixed number of pages and call the result complete.

`getAssetsByOwner(validator)` is only a discovery heuristic. Ownership of the
record asset is not authorship, record assets can be transferred, and a
validator can write AppData on an asset it does not own. Security derives from
the pinned AppData `data_authority`, not from asset ownership.

### Ordering, freshness, and revocation

`recordedAt` is authenticated only as part of the current
validator-controlled AppData payload. It is not a detached signature or
consensus time. Consumers SHOULD combine it with the observed slot and
transaction signature.

Records are not self-revoking. A profile that requires expiry, supersession, or
revocation MUST define those semantics inside its namespaced version and must
specify how ties and conflicting records are resolved. A generic verifier MUST
NOT infer current standing by taking the largest `recordedAt` across unknown
profiles.

Burning the record asset removes its current account state. Transferring it can
break owner-based discovery. Overwriting AppData can replace the earlier
payload. Consumers that require durable historical evidence need a transaction
archive or an independently anchored snapshot.

## Relationship to ERC-8004

ERC-8004 is currently a Draft Standards Track ERC. It is design inspiration,
not a compatibility claim.

This proposal borrows the general subject–validator–response-commitment shape
from ERC-8004's Validation Registry. ERC-8004 defines EVM contracts, events,
request hashes, response values, response URIs, and `bytes32` commitments. It
does not define the AppData record transport in this proposal, does not define
`mpl.agent.validation-record.v1`, and does not define the legacy
`https://eips.ethereum.org/EIPS/eip-8004#validation-v1` string as an official
JSON validation-record discriminator.

An implementation MUST NOT advertise these AppData records as interoperable
with ERC-8004 solely because the field names are similar. A bridge or adapter
would need to define chain identity, request/response mapping, validator
identity, hash construction, finality, and replay rules separately.

## Covenant audit-chain profile

Covenant is an independent reference implementation, not a Foundation-endorsed
implementation.

For new v1 records, Covenant's profile is:

```text
schema  = org.opencovenant.audit-chain.v1
hashAlg = sha256-chain-v1
```

The commitment is a linear SHA-256 hash chain, not a Merkle tree:

```text
ZERO = "0000000000000000000000000000000000000000000000000000000000000000"

event_hash_hex[0] = lowercase_hex(
  SHA256(exact_utf8_jsonl_event_bytes_without_line_ending[0])
)
chain_hash_hex[0] = lowercase_hex(
  SHA256(utf8(ZERO + "\n" + event_hash_hex[0]))
)

event_hash_hex[i] = lowercase_hex(
  SHA256(exact_utf8_jsonl_event_bytes_without_line_ending[i])
)
chain_hash_hex[i] = lowercase_hex(
  SHA256(utf8(chain_hash_hex[i - 1] + "\n" + event_hash_hex[i]))
)

responseHash = chain_hash_hex[last]
```

A conforming `sha256-chain-v1` evidence set MUST contain at least one event.
An empty evidence set has no v1 `responseHash` and MUST NOT use `ZERO` as a
completed commitment.

The hash input at each chain step is the 64-character lowercase hexadecimal
previous hash, one ASCII LF byte (`0x0a`), and the 64-character lowercase
hexadecimal event hash. The previous digest bytes are not decoded before the
fold. There is no leaf pairing, tree balancing, duplication, or Merkle proof.

### Legacy deployed record and migration

The current mainnet Covenant record predates this v1 proposal:

| Item                     | Address                                        |
| ------------------------ | ---------------------------------------------- |
| Validator data authority | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Subject MIP-014 agent    | `4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc` |
| Subject registration PDA | `FLt6bxnQfxVVJ77naw83KrcZeFyJvApKdmEWKWwG8CVx` |
| Legacy record asset      | `4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG` |

On 2026-07-31 at mainnet finalized slot `436319347`, direct RPC returned the
record and subject accounts owned by MPL Core and the 104-byte registration
account owned by the MIP-014 registry program. This is a point-in-time
observation, not a substitute for a verifier's current read.

The legacy payload uses:

```text
type    = https://eips.ethereum.org/EIPS/eip-8004#validation-v1
schema  = covenant.audit-root.appdata.v2
hashAlg = sha256-merkle
```

`sha256-merkle` is a legacy mislabel. Its deployed `responseHash` was produced
by the linear hash-chain algorithm specified above. It MUST NOT be interpreted
as a Merkle root.

The legacy asset demonstrates the Core/AppData authority model, but it is not a
conforming v1 vector. Migration is additive:

1. leave the existing record address unchanged as historical evidence;
2. issue a new record with type `mpl.agent.validation-record.v1`;
3. use profile `org.opencovenant.audit-chain.v1`;
4. use algorithm identifier `sha256-chain-v1`; and
5. publish a profile-specific link between the legacy and replacement records
   under `extensions`.

A compatibility verifier MAY accept `sha256-merkle` only when the caller
explicitly enables the legacy Covenant profile. It MUST interpret the value
with the hash-chain algorithm above and MUST NOT enable that alias for other
schemas.

The
[current public verification endpoint](https://opencovenant.org/api/agents/4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc/verify)
reports the legacy record separately.

That endpoint is a convenience view, not part of the normative verification
path.

## Optional Oracle enforcement

Oracle enforcement is defined in the separate, non-normative
[Oracle enforcement extension](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/agent-validation-oracle-extension.md).

It is intentionally outside record conformance because it adds a program,
verdict authority, program-upgrade policy, account-availability dependency, and
asset-lockout risk. A consumer can verify or use records without enabling
Oracle enforcement.

## Security considerations

### Validator trust and key compromise

The AppData `data_authority` proves which key can write the payload; it does not
prove that the key belongs to a competent or honest validator. Consumers MUST
configure accepted validators out of band. A compromised validator key can
author or overwrite records that pass authorship checks.

Validator rotation creates a new trust root. A payload that merely announces a
new key is insufficient. Directories need an explicit rotation policy, such as
a separately signed transition accepted by local governance.

### Adapter authority confusion

The AppData write authority and the external plugin's configuration authority
are distinct. A malicious asset creator can set a cosmetic or configuration
authority address without giving that address control of the data. DAS
verifiers MUST pin the `Address` discriminant and
`adapter_config.data_authority.address`; direct decoders MUST pin the exact
`PluginAuthority::Address` variant and address inside the AppData key.

### Multiple AppData adapters and type confusion

An agent asset can carry validation AppData and unrelated application AppData
in addition to its distinct `AgentIdentity` external plugin. Selecting the
first AppData adapter, selecting only by adapter type, or selecting only by
payload `type` permits misclassification. Authority, adapter schema, record
type, and profile schema are all required selection keys.

### PDA squatting and reinitialization

An attacker can transfer lamports to a predictable PDA before initialization.
Initializers must handle pre-funded system accounts safely or fail without
accepting them as initialized. They must require canonical seeds and bump,
correct owner, correct discriminator, expected data length, and one-time
initialization.

Verifiers MUST NOT treat a PDA's existence or non-zero balance as registration.
They must check owner, derivation, discriminator, and decoded subject binding.
The same rule applies to MIP-014 identity PDAs, the current
`AgentValidationV1` PDA, and any Oracle PDA.

### Program upgrade and source-verification risk

The direct-key record profile adds no new on-chain program, but it depends on
the selected versions of MPL Core and the MIP-014 registry. A program-mediated
validation profile would also depend on its program ID, current deployed bytes,
upgrade authority, and governance.

A source-verified build proves that deployed bytes matched source at an
observation time. It does not make an upgradeable program immutable. Consumers
whose policy depends on program behavior must inspect the current programdata
account and decide whether its upgrade authority is acceptable.

### Record mutation, transfer, burn, and freeze

The validator can overwrite its AppData. The asset owner can transfer the
record, which can break owner-based indexing. An authorized burn removes the
live account. Freeze and lifecycle plugins can prevent legitimate transfer or
recovery.

Applications must not equate NFT ownership, collection membership, frozen
state, or continued DAS visibility with validator authorship. Consequential
uses should retain the exact verified bytes, slot, and transaction signature.

### DAS trust and completeness

DAS is indexed state, not consensus. A provider can be stale, incomplete,
malicious, rate-limited, or differently configured. Complete pagination reduces
accidental omission but does not make the provider trusted. Use direct RPC for
the final record and subject checks when the decision is consequential.

### Commitment semantics

`responseHash` is a commitment, not a truth oracle. A record can be
record-authentic while its evidence is unavailable, malformed, false, or
irrelevant. Profiles must define canonical input bytes and recomputation.
Consumer interfaces must keep authorship, evidence verification, and policy
acceptance visibly separate.

### Time, replay, and cross-cluster confusion

`recordedAt` is validator-supplied. Consumers must use an explicit cluster and
must not replay a record from devnet, testnet, a local validator, or a fork into
mainnet policy. Profiles that authorize actions need a domain separator,
audience, nonce or sequence, expiry, and replay rules inside the committed
evidence.

## Backwards compatibility

This proposal is additive:

- no MIP-014 account, PDA, instruction, or agent asset changes;
- no MPL Core program change;
- no change to the current `mpl-agent-validation` IDL or clients;
- no requirement for existing assets to add an adapter; and
- no effect on consumers that ignore this record type.

Legacy Covenant records remain readable under an explicit compatibility mode
but are not mislabeled as v1-conforming records.

## Reference artifacts and conformance

The proposal includes:

- [JSON Schema](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/agent-validation-record-v1.schema.json)
- [valid and invalid DAS-shaped vectors](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/agent-validation-record-v1.vectors.json)
- [dependency-free DAS-envelope verifier](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/verify-agent-validation-record.mjs)
- [separate Oracle extension](https://github.com/open-covenant/covenant/blob/b066eb352851448529744cd0e97f53bf17b95953/docs/standards/agent-validation-oracle-extension.md)

Except for the pinned MIP-014 registry program, the vector assets and public
keys are synthetic, off-chain conformance fixtures. They are not live accounts
and do not reuse the deployed Covenant record identifiers in the legacy-record
section.

Run the vectors with:

```bash
node docs/standards/verify-agent-validation-record.mjs --test
```

The dependency-free verifier is a pure function over a supplied DAS-shaped
asset. It exercises adapter selection and payload validation without network
access. Before executing the vectors, the runner mechanically checks that the
schema's required fields, property sets, patterns, bounds, and nested subject
contract match the verifier's contract. A complete verifier must also perform
the direct-RPC Core ownership, `AgentIdentity` plugin, registration PDA,
account discriminator, and profile-specific evidence checks specified above.

The verifier deliberately includes these negative cases:

- an unrelated AppData adapter appears first;
- top-level plugin authority tries to impersonate the data authority;
- an agent application AppData payload is presented as a record;
- payload validator and data authority differ;
- the response commitment is malformed;
- an unknown profile or unsupported algorithm is presented under a trusted
  validator;
- the subject names a registry program other than the locally pinned MIP-014
  program;
- `Owner`, `UpdateAuthority`, and missing data-authority discriminants try to
  satisfy an `Address` authority pin;
- conflicting adapter-configuration, data-authority, or payload aliases are
  supplied together;
- an unknown top-level payload field is supplied;
- non-string identifiers and malformed verifier policy inputs are supplied;
  and
- a required payload field is missing.

## Rationale

### Why direct-key AppData?

It provides a useful authorship primitive today without introducing a new
program or claiming that the existing program already implements response
semantics. A future program-mediated version can add richer authorization and
aggregation after those semantics are finalized.

### Why pin the profile?

The same validator can make different kinds of claims. Accepting arbitrary
schemas turns “trusted validator” into “trusted for every future claim.” The
consumer must opt into the semantics it understands.

### Why a separate record asset?

A new asset per observation preserves a simple append-oriented model and avoids
making the subject agent's identity adapter carry unrelated validator state.
It also avoids treating every AppData-bearing agent asset as a record. Profiles
may later define subject-attached or program-mediated representations under a
new version.

### Why not make DAS normative?

MPL Core account bytes are the chain state. DAS improves discovery and
developer experience, but different providers can lag or omit results.
Separating discovery from verification keeps the security boundary explicit.

## Official references

- [Metaplex MIP repository and submission route](https://github.com/metaplex-foundation/mip)
- [MIP governance procedure](https://github.com/metaplex-foundation/mip/blob/8aaffc118ab01e293666397b72ef67bb015bafa1/mip-5.md)
- [MIP-014](https://github.com/metaplex-foundation/mip/blob/8aaffc118ab01e293666397b72ef67bb015bafa1/mip-014.md)
- [Current Agent Identity source](https://github.com/metaplex-foundation/mpl-agent/tree/326b76a46aa3b0dd6400f7a318992d537470c57c/programs/mpl-agent-identity)
- [Current Agent Identity documentation](https://developers.metaplex.com/smart-contracts/mpl-agent/identity)
- [Current `mpl-agent-validation` program](https://github.com/metaplex-foundation/mpl-agent/tree/326b76a46aa3b0dd6400f7a318992d537470c57c/programs/mpl-agent-validation)
- [Current validation IDL](https://github.com/metaplex-foundation/mpl-agent/blob/326b76a46aa3b0dd6400f7a318992d537470c57c/idls/mpl_agent_validation.json)
- [Current JavaScript validation test](https://github.com/metaplex-foundation/mpl-agent/blob/326b76a46aa3b0dd6400f7a318992d537470c57c/clients/js/test/validation/register.test.ts)
- [Current Rust validation test](https://github.com/metaplex-foundation/mpl-agent/blob/326b76a46aa3b0dd6400f7a318992d537470c57c/clients/rust-validation/tests/create.rs)
- [MPL Core AppData](https://developers.metaplex.com/smart-contracts/core/external-plugins/app-data)
- [MPL Core account deserialization](https://developers.metaplex.com/smart-contracts/core/deserialization)
- [DAS pagination](https://developers.metaplex.com/dev-tools/das-api/guides/pagination)
- [ERC-8004 Validation Registry](https://eips.ethereum.org/EIPS/eip-8004#validation-registry)

## Submission note

This document intentionally has no MIP number. The current official repository
directs community proposals to the
[MIP submission portal](https://mip.metaplex.com/); MIP-5 says a number is
assigned after the community feedback period if the proposal passes initial
screening. Submission through the portal and any wallet or payment requirements
remain an external proposer action. At the official MIP repository snapshot
cited above, MIP-5 states that submission requires proving ownership of 200
MPLX and paying 10 USDC, refundable for a good-faith submission. The live
portal is authoritative if those parameters change.
