# [MIP-XXX] Agent Validation Records

Status: Draft (not yet submitted). Builds on MIP-014 (Agent Registry + Core Agent Identity Plugin).
Reference implementation: Covenant, live on Solana mainnet (addresses below). Every on-chain claim in this document is checkable against those addresses.

## Summary

MIP-014 gives an agent a verifiable *identity*: a Metaplex Core asset bound to a registration in the Agent Registry. It is minimal by design and leaves *accountability* — evidence of what an agent has done, and whether it is fit to act — out of scope.

This proposal defines a **validation record**: a signed, indexer-readable claim, written to MPL Core AppData, that a named validator attests a response about a subject agent. It is shaped after the ERC-8004 Validation registry so the two ecosystems interoperate, and it is verifiable by anyone with a single DAS query — trusting neither the agent, the validator's servers, nor this proposal's code.

It also specifies an optional **enforcement** layer: the same verdict can gate a subject agent's Core lifecycle events (the reference gates transfer) on-chain via the Core Oracle external plugin, so an agent can only move while it is in policy.

A complete reference implementation — records, a DAS-only verifier, and a source-verified gating program — is live on mainnet today.

## Motivation

The registry tells a buyer an agent *exists*. Before delegating a mandate — spend, sign, transact — a buyer needs to know the agent's standing is real, current, and checkable without trusting the agent or any single server. There is no standard, on-chain, indexer-readable way to express "validator V attests claim C about agent A."

MIP-014 scopes accountability out explicitly, and a dedicated validation registry program is anticipated but unbuilt. This proposal fills the gap now, with a record format that already produces and verifies on mainnet, transports over infrastructure that already exists (Core AppData + DAS), and migrates onto a validation program unchanged when one ships.

## Goals

- Express "validator V attests response R about agent A" as a single on-chain object, readable by any DAS indexer.
- Make authorship a chain fact, not a payload claim, so a record cannot be forged without the validator's key.
- Verifiable by a pure function over public DAS output — no validator infrastructure, no program call, no proposer's code in the trust path.
- Interoperate with ERC-8004 so an agent's record is meaningful across ecosystems.
- Change nothing already shipped under MIP-014; an unaware consumer ignores records.
- Offer optional on-chain enforcement for agents that should be constrained by their own verdict.

## Conventions

The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY in this document are to be interpreted as described in RFC 2119.

## Terminology

- **Validator** — the key authorized to make claims. On-chain it is the AppData `data_authority`.
- **Subject** — the agent a record is about; a Core asset registered under MIP-014.
- **Validation record** — a standalone Core asset carrying one AppData plugin with the payload defined below.
- **Response** — the validator's claim, committed as `responseHash` under a declared `hashAlg`.
- **Gated agent** — an agent whose asset carries the Oracle external plugin from the Enforcement section, binding its lifecycle to the verdict.
- **AppData schema** — MPL Core's AppData plugin stores its bytes under one encoding of `Binary | Json | MsgPack`. Records use `Json`, which is why DAS indexes the payload as queryable JSON (and why the casing note in Encoding applies). This is the plugin's encoding, distinct from the payload's own `schema` field.

## Specification

### Validation record

A validation record is a standalone Metaplex Core asset carrying a single **AppData** external plugin (`Json` encoding) whose `data_authority` is the validator. MPL Core permits only that authority to write the plugin's data, so authorship is established by the chain, not asserted in the payload.

The canonical payload (on-chain keys are camelCase; see Encoding):

```json
{
  "type": "https://eips.ethereum.org/EIPS/eip-8004#validation-v1",
  "schema": "<namespaced record schema and version>",
  "subject": { "registry": "mpl-agent-014", "asset": "<agent Core asset>" },
  "validator": "<validator pubkey; equals the AppData data_authority>",
  "hashAlg": "<commitment hash algorithm>",
  "responseHash": "<the validator's commitment>",
  "tag": "<optional categorization>",
  "recordedAt": <unix seconds>
}
```

- `type` (required) — the ERC-8004 validation discriminator; lets a generic reader decode the envelope without registry-specific code.
- `schema` (required) — namespaced record schema and version, so the response semantics are unambiguous.
- `subject.registry` (required) — `"mpl-agent-014"`.
- `subject.asset` (required) — the agent this record is about, binding the record to a MIP-014 identity.
- `subject.registration` (optional) — the agent's Agent Registry PDA. It is derivable from `subject.asset` (the `["agent_identity", asset]` PDA under the registry program), so it MAY be omitted; a reader that needs it derives it.
- `validator` (required) — the attesting key. It MUST equal the on-chain AppData `data_authority`; a reader that finds a mismatch MUST reject the record. Mirroring it in the payload lets an offline reader check the binding without a second account fetch.
- `hashAlg` + `responseHash` (required) — the validation commitment. `hashAlg` names the algorithm; `responseHash` MUST be in that algorithm's canonical encoding. ERC-8004 commits with keccak256 over `bytes32`; a record MAY declare another algorithm (the reference uses `sha256-merkle`, encoded as 64 lowercase hex).
- `tag` (optional) — free categorization, e.g. the kind of validation.
- `recordedAt` (optional) — unix seconds; lets a reader order records.

Implementations MAY carry additional data under a namespaced object (e.g. a vendor key); conforming readers MUST ignore unknown fields.

### Encoding

On-chain, AppData is written with camelCase keys (`responseHash`, `hashAlg`, `recordedAt`). The Metaplex DAS API re-cases nested keys to snake_case (`response_hash`, `hash_alg`, `recorded_at`) when it indexes the JSON; single-word keys (`type`, `schema`, `subject`, `validator`, `tag`) are unchanged. A conforming verifier MUST accept either casing per field. The reference verifier reads `snake ?? camel`.

### Reading an asset as record or agent

An asset that carries an AppData plugin is read as a validation record; an asset without one is read as an agent (its records live on other assets — see Verification). A record carries exactly one AppData plugin; a reader that encounters more than one uses the first. An agent asset SHOULD NOT also carry record AppData, since it would then be read as a record.

### Verification (DAS-only)

A record is **valid** for a validator V iff, from its DAS `getAsset`:

1. it carries an AppData external plugin;
2. `type`, `schema`, and `hashAlg` equal the constants the verifier expects (a verifier MUST reject a `hashAlg` it does not implement);
3. `responseHash` is well-formed for that `hashAlg` (for `sha256-merkle`, 64 lowercase hex);
4. the AppData `authority.address` equals V; and
5. the payload `validator` equals V (mirrors 4).

A verifier MUST pin V — the expected validator key — and reject any record bound to a different authority. (An HTTP endpoint additionally requires the asset interface to be `MplCoreAsset`; the record check itself operates on the AppData plugin.)

An agent A is **accountable** under V iff at least one valid record has `subject.asset == A`. A reader finds these by paging V's owned assets over DAS (`getAssetsByOwner`) and matching `subject.asset`. Record recency is ordered by `recordedAt`; ties SHOULD be broken deterministically (e.g. by record asset id), since DAS does not guarantee page order. For *current* standing, prefer the enforced verdict (below) over record recency.

No registry program call and no validator infrastructure are required. The check is a pure function over public DAS output; the only trust anchor is the on-chain `data_authority`, which MPL Core enforces.

### Optional enforcement: gating lifecycle events on the verdict

A record is passive: a reader decides what to do with it. For an agent that should be *constrained* by its verdict, the same signal can gate the agent asset's Core lifecycle events on-chain, with no new trust surface, using the **Core Oracle external plugin**.

A small program owns one oracle account per agent at the `["oracle", asset]` PDA and exposes one authority-gated value, an `OracleValidation`. The agent asset carries an Oracle external plugin with:

- `base_address` = that PDA,
- `results_offset = ValidationResultsOffset::Anchor` (Core reads the result at byte 8, after the 8-byte account discriminator), and
- `lifecycle_checks = { transfer: [CanReject] }`.

Core's hookable lifecycle events are create, transfer, update, and burn; the reference gates transfer. The validator flips the verdict between in policy and out of policy; MPL Core enforces it — an out-of-policy verdict makes Core veto the gated event (the transfer fails with a custom program error). The stored value is byte-compatible with `mpl-core`'s `OracleValidation` at the `Anchor` offset, so Core reads it directly with no adapter:

```
account: [0..8)  Anchor account discriminator
         [8]     OracleValidation tag       (1 = V1)
         [9]     create   : ExternalValidationResult
         [10]    transfer : ExternalValidationResult
         [11]    burn     : ExternalValidationResult
         [12]    update   : ExternalValidationResult

ExternalValidationResult:  Approved = 0,  Rejected = 1,  Pass = 2

in policy      -> transfer = Pass (2)      the oracle abstains; Core defers to the owner authority
out of policy  -> transfer = Rejected (1)  the oracle vetoes the transfer
```

Agent *execution* is not a Core lifecycle event; an application that runs the agent reads the same verdict before acting. That is an application-level binding, outside Core, and outside this proposal's on-chain guarantees.

Enforcement is additive and opt-in: an agent with no Oracle plugin behaves exactly as before; an agent with one is bound to its verdict, and the binding is a chain fact anyone reads from the asset's plugins. The gating program SHOULD be a verified build, so the enforcement logic is auditable from source rather than trusted.

## Reference implementation (live on Solana mainnet)

Covenant produces, verifies, and enforces these records today. Every row is checkable on mainnet.

| What | Address |
|---|---|
| Validator (AppData data_authority) | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Subject agent (MIP-014, gated) | `4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc` |
| Agent Registry program | `1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p` |
| Agent Registry PDA for the subject (derived) | `FLt6bxnQfxVVJ77naw83KrcZeFyJvApKdmEWKWwG8CVx` |
| Validation record | `4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG` |
| Gating program (source-verified) | `2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD` |
| Oracle account (verdict) | `4iQbGGLyLXed6aoKfrPAPUd7wxHaS3SPCUURVb3gUho3` |

Reference constants: `type = https://eips.ethereum.org/EIPS/eip-8004#validation-v1`, `schema = covenant.audit-root.appdata.v2`, `hashAlg = sha256-merkle`. The reference `responseHash` is an audit-chain Merkle root.

- **Verifier** — a pure function over DAS output. Reference: the Rust `covenant_metaplex::verify` engine (`verify_attestation`, `verify_agent`) and its TypeScript port backing the public endpoint (`verifyAttestation`, `findAccountability`). No key, no Covenant infrastructure in the trust path.
- **Gate** — the `covenant-oracle` program gates the subject agent's transfer on its verdict via the Oracle plugin. It is a verified build: `https://verify.osec.io/status/2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD` reports the on-chain bytes match the published source at the recorded commit.
- **Writer (operational)** — an isolated signer process holds the validator key; the daemon never does. This is a property of the reference deployment, not a protocol requirement.

### Reproduce every check

- **Record** — `getAsset 4A2fd…`: AppData `authority.address` and `data.validator` both equal the validator; `type`/`schema`/`hashAlg` match; `responseHash` is 64 hex.
- **Accountability** — `getAssetsByOwner DKxXr…`: a record with `subject.asset == 4XtUr…`.
- **Identity** — the `["agent_identity", 4XtUr…]` PDA under the Agent Registry program (`1DREG…`) derives to `FLt6bxnQ…` and is owned by that program.
- **Gate** — `getAsset 4XtUr…` shows an Oracle plugin with `base_address = 4iQb…`; `getAccountInfo 4iQb…` byte 10 is the live transfer verdict.
- **Program** — `verify.osec.io/status/2PJF…` reports `is_verified: true`.
- **One call for all of it** — `GET https://opencovenant.org/api/agents/4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc/verify` returns the accountability and gate verdict; the human view is `https://opencovenant.org/agents/4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc`.

## Security considerations

- **Authorship is the trust anchor.** MPL Core permits only the AppData `data_authority` to write the plugin data, so a record's authorship is a chain fact. A verifier MUST pin the expected validator key and reject any record whose `data_authority` (or mirrored `validator`) differs. Without that pin, a record proves only "someone wrote JSON."
- **Trust-root selection is out of scope.** This proposal defines how to verify a record *against a given validator key*, not how a directory chooses that key. Which validator(s) a consumer trusts, and how it discovers them, is a directory policy decision; a consumer MAY trust one or many validators.
- **DAS is a convenience, not the trust root.** The verdict derives from on-chain account data (the AppData authority, the oracle account). DAS is a faster read path; a verifier can read the same accounts directly over RPC to remove the indexer. A malicious indexer can withhold or stale data but cannot forge a passing record, because the authority binding is on-chain.
- **`responseHash` is a commitment, not the evidence.** Validity means "the validator attested this commitment," not "the underlying work is correct." What the commitment binds to (e.g. a published, recomputable audit chain) is the validator's semantics, declared by `schema`/`hashAlg`, and out of scope for this envelope.
- **Freshness and revocation.** Records are additive; supersession is by `recordedAt` and, for enforcement, by flipping the live oracle verdict. A consumer that needs current standing reads the enforced verdict rather than a record's age.
- **Enforcement authority.** The oracle verdict is controlled by the gating program's authority; whoever holds it can open or close the gate. That authority SHOULD be the validator (or its governance), and the program SHOULD be a verified build so the control surface is auditable. A gate is pinned to a specific program by deriving `base_address` from `["oracle", asset]` under the expected program id — a different program is a different gate.
- **No new program for records.** Records add no program and no new signer; they reuse Core's AppData authority model, so their attack surface is Core's, already audited.

## Backwards compatibility

Additive. No change to MIP-014, the Core Agent Identity plugin, or any existing asset. Records and gated agents are ordinary Core assets; a consumer that does not understand them ignores the extra plugin and behaves as before.

## Relationship to a validation registry program

When a dedicated validation registry program ships, these records migrate onto it with the same fields — the envelope is deliberately validation-shaped, so migration is a re-anchor, not a redesign. Until then, AppData + DAS is the transport, and the record is already useful: a directory can surface a "verified / accountable" signal, and an agent can be gated on its verdict, today.
