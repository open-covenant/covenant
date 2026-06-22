# MIP-XXX (draft): Agent Validation Records

**Status:** Draft (for discussion)
**Builds on:** MIP-014 (Agent Registry + Core Agent Identity Plugin)
**Reference implementation:** Covenant — live on mainnet (see below)

## Abstract

MIP-014 gives a Solana agent a verifiable *identity* (a Core asset in the 014
registry) and a registration document. It is minimal by design and leaves
*accountability* — proof of what an agent has actually done — out of scope.

This proposal defines a minimal, indexer-readable **validation record**: a
signed claim, written to MPL Core AppData, that a named **validator** attests
a **response** about a **subject agent**. It is shaped after the ERC-8004
Validation registry so the two ecosystems interoperate, and it is verifiable
by anyone through a standard DAS query with no off-chain trust.

It changes nothing already shipped under MIP-014; it sits beside it.

## Motivation

The agent registry tells a buyer an agent *exists*. Before delegating a
mandate (spend, act, transact), a buyer needs to know the agent's *record* is
real and checkable. Today there is no standard, on-chain, indexer-readable way
to express "validator V attests claim C about agent A." The `VALREG`
validation program id is reserved but unbuilt. This proposal fills that gap
with a record format that is already producing and verifying on mainnet.

## Specification

A validation record is an MPL Core **AppData** external plugin (`schema: Json`)
on a standalone Core asset, whose `data_authority` is the **validator** key.
MPL Core enforces that only that authority may write the data, so authorship is
a chain fact, not a payload claim. The payload:

```json
{
  "type": "https://eips.ethereum.org/EIPS/eip-8004#validation-v1",
  "schema": "<namespaced record schema + version>",
  "subject": { "registry": "mpl-agent-014", "asset": "<agent Core asset>", "registration": "<014 PDA>" },
  "validator": "<validator pubkey; mirrors the AppData data_authority>",
  "hashAlg": "<commitment hash algorithm>",
  "responseHash": "<the validation commitment, e.g. an audit-chain root>",
  "tag": "<categorization>",
  "recordedAt": <unix seconds>
}
```

- `type` — ERC-8004 validation discriminator; lets a generic reader decode the
  envelope without registry-specific code.
- `subject.asset` — the agent the record is about, binding it to the 014 registry.
- `validator` — the attesting authority; **must equal** the on-chain AppData
  `data_authority`. A reader that finds a mismatch rejects the record.
- `hashAlg` + `responseHash` — the validation commitment. ERC-8004 uses
  keccak256 `bytes32`; a record may declare another algorithm (Covenant uses a
  SHA-256 audit-chain root).

### Verification (DAS-only)

For a record asset:
1. Fetch via DAS `getAsset`. Read its AppData `data` and `authority.address`.
2. Accept iff: `type`/`schema`/`hashAlg` are the expected constants,
   `responseHash` is well-formed, the AppData `authority.address` equals the
   expected validator, and the payload `validator` mirrors it.

For an agent: it is **accountable** iff at least one record asset authored by a
trusted validator has `subject.asset == <agent>` and passes the above.

No registry program call and no validator infrastructure are required — the
check is a pure function over public DAS output.

## Reference implementation (live on mainnet)

Covenant produces and verifies these records today:

| What | Address |
|---|---|
| Validator (attestation authority) | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Agent identity (subject) | `4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc` |
| Validation record | `4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG` |

- Writer: `covenant-metaplex-signer` (isolated minting key, solana-sdk 3.x).
- Verifier: `covenant_metaplex::verify` — `verify_attestation` (pure, over a
  DAS asset) and `verify_agent` (DAS-backed accountability), exposed as the
  capability-gated `metaplex.verify.*` MCP tools. No key, no Covenant infra.

## Relationship to the validation program

When the `VALREG` validation program ships, these records migrate onto it with
the same fields — the envelope is deliberately validation-shaped so migration
is a re-anchor, not a redesign. Until then, AppData + DAS is the transport, and
the record is already useful: a directory can surface a "verified / accountable"
signal by running the verification above.

## Backwards compatibility

Additive. No change to MIP-014, the Core Agent Identity plugin, or any existing
asset. Records are standalone Core assets; consumers that do not understand them
ignore them.
