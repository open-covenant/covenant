# Agent Validation Records — Parked Research Note

Status: Parked. This document was never submitted as a Metaplex Improvement Proposal and has not been endorsed by Metaplex, the Solana Foundation, or either community. It has no standards standing.

The original draft explored representing validator-authored commitments as Metaplex Core AppData and optionally attaching a Core Oracle plugin to one asset lifecycle event. Current Covenant product work does not depend on this format and does not present it as an agent-validation standard.

## Scope boundary

The prototype demonstrates a limited set of structural observations:

- a configured DAS response can report a Core asset, an AppData payload, and its data authority;
- a verifier can compare those reported fields with an expected validator key and schema;
- supplied event lines can be folded into a hash-chain root and compared with a stored commitment; and
- a configured Core Oracle plugin can veto the selected Core asset lifecycle event, which the prototype set to transfer.

Those observations do not establish that the underlying claim is true, that the supplied event log is complete, that an agent runtime followed the log, or that a wallet or payment was mediated. They do not enforce W009 or W011. The Oracle experiment applies to a Core asset transfer only, not agent execution, transaction signing, tool use, or payment authorization.

## Why this is parked

- A standalone Covenant record format would create a parallel validation surface before the Metaplex agent-validation work is settled.
- A URI or similarly shaped payload does not create ERC-8004 interoperability. The historical ERC-8004 URI used by the prototype is only an application-defined type tag.
- AppData authority proves which configured key could write the observed bytes; it does not prove service quality, reputation, delivery, or semantic correctness.
- DAS is an indexer view that may be stale, incomplete, or unavailable. Direct RPC observation can reduce indexer dependence but still proves only account structure and bytes at an observed slot.
- A universal verified/accountable score would collapse distinct evidence and local policy into an unjustified global verdict.

Any future standards work should begin with the relevant Metaplex maintainers and existing Solana attestation or agent-validation primitives. A MIP should be considered only after a concrete missing interface is demonstrated by multiple independent implementations.

## Current upstream path

As of 31 July 2026, the official
[`metaplex-foundation/mpl-agent`](https://github.com/metaplex-foundation/mpl-agent)
repository already contains `mpl-agent-validation` and `mpl-agent-reputation`
programs, which its README describes as not yet finalized. The validation
program currently provides registration scaffolding rather than the record and
evidence semantics explored here.

The next standards step should therefore be an upstream design discussion and,
if invited, a focused contribution to that repository. It should define one
concrete missing interface, include independent consumers and test vectors, and
avoid creating a parallel Covenant-specific standard. Preparing a new MIP is
premature unless the upstream maintainers identify a gap that cannot be handled
inside the existing program.

## Historical prototype format

The experiment stored a standalone Core asset with one JSON AppData plugin. Its data authority was treated as the expected validator key. The illustrative payload was:

```json
{
  "type": "<application-defined record type>",
  "schema": "<namespaced schema and version>",
  "subject": { "registry": "mpl-agent-014", "asset": "<agent Core asset>" },
  "validator": "<expected AppData data authority>",
  "hashAlg": "<commitment hash algorithm>",
  "responseHash": "<validator-authored commitment>",
  "tag": "<optional categorization>",
  "recordedAt": "<optional unix seconds>"
}
```

A configured reader can check that:

1. the supplied asset view contains the expected AppData structure;
2. the reported AppData authority equals its configured validator key;
3. the mirrored validator field equals that same key;
4. the type, schema, and hash algorithm are on its allowlist; and
5. the commitment is well formed for the declared algorithm.

Passing these checks means only that the supplied record is structurally consistent with the configured expectations. It does not validate the evidence behind the commitment or produce a trust decision for a buyer.

## Historical mainnet observations

These addresses remain useful for reproducing the prototype's structural checks. Their presence is not an endorsement or a claim of general agent enforcement.

| Observed object | Address |
|---|---|
| Configured AppData authority | `DKxXrxxCzAwLSXRUWzUouiW46GNf4PR2mjjhAbtCAkcK` |
| Subject Core asset | `4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc` |
| Agent Registry program | `1DREGFgysWYxLnRnKQnwrxnJQeSMk2HmGaC6whw2B2p` |
| Derived Agent Registry PDA | `FLt6bxnQfxVVJ77naw83KrcZeFyJvApKdmEWKWwG8CVx` |
| AppData record asset | `4A2fdNqmPiQrv3iYv6WY2mQ9eSQuBERhdeg4vk7G8vGG` |
| Prototype Core Oracle program | `2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD` |
| Prototype Oracle account | `4iQbGGLyLXed6aoKfrPAPUd7wxHaS3SPCUURVb3gUho3` |

The verified-build record for the prototype program can show that deployed bytes matched published source at a recorded commit. It does not show that a broader agent runtime or payment path used the program.

## Superseding direction

Covenant now treats trust as a local decision over portable evidence. Near-term work targets exact payment preflight and signer-bound enforcement: canonicalize a proposed payment, apply local policy or explicit approval, make the signer validate the final transaction, consume authorization atomically, and emit a decision receipt linked to settlement evidence.

Identity records and attestations may be inputs to that local decision. They are not substitutes for payment authorization, signer isolation, exact transaction validation, or independently demonstrated service quality.
