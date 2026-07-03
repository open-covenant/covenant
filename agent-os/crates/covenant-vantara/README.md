# covenant-vantara

A read-only provenance connector between [Vantara](https://www.vantara.services) — a
decentralized inference network — and Covenant's trust layer.

Vantara nodes run agent jobs, get paid in USDC on Solana, and publish a
content-free provenance record for every job: a job id, the node, the model, a
SHA-256 of the output, a signed receipt, a settlement flag, and a completion
time. No inputs, outputs, or wallets ever leave the network. This crate reads
that ledger, verifies each record's signature against the network key, and
mints a Covenant attestation. It never runs a job and never touches Vantara's
payment rail.

## Tools

Registered into the daemon's `ToolRegistry` when `COVENANT_VANTARA_ENABLED` is set.

- **`vantara.jobs`** — list recent provenance records and a verified Covenant
  attestation for each. Args: `limit` (1–50, default 20), `offset`.
- **`vantara.attest`** — resolve one job and return its content-free
  attestation. Select it by `jobId`, by `outputHash`, or by `output` (hashed
  locally to its SHA-256 — the output never leaves the host).
- **`vantara.payouts`** — read the aggregate on-chain settlement ledger:
  per-wallet USDC payouts, each with a Solana transaction signature, plus
  lifetime totals.

An attestation is a deterministic function of the record and the feed's signing
block — no wall clock, no content — so the same job always attests to the same
object, safe to hash, log, or anchor on-chain:

```json
{
  "provider": "vantara",
  "cluster": "mainnet-beta",
  "jobId": "8e71e03f-d994-4b09-8444-53039ca2839e",
  "model": "claude",
  "outputHash": "881f47e19aac2cdddbc30b1fa29284158ee680c1c2f84ddc73b5fa3606c9b3b5",
  "hash": { "algorithm": "sha256", "valid": true },
  "signature": { "status": "verified" },
  "settlement": { "settled": false },
  "completedAt": "2026-07-01T19:19:04.083Z"
}
```

## Verification

Two axes, both real:

- **Output hash** — checked against the feed's declared algorithm. For `sha256`,
  that it is 64 lowercase hex characters. An unknown algorithm is reported as
  such, never silently passed.
- **Receipt signature** — every record carries `vantaraSignature`, an ed25519
  signature over the canonical string
  `vantara-job-receipt-v1|<jobId>|<nodeId or '-'>|<model>|<outputHash>|<completedAt>`.
  The feed is self-describing: its `signing` block carries the public key
  (base58), the encodings, and the canonical template, so the verifier rebuilds
  the exact bytes and checks the signature. The attestation reports `verified`,
  `absent`, or `invalid` with a reason.

**Trust anchor.** By default the verifier pins the provider key to
`providerCallback.publicKey` from the `/.well-known/mpp` discovery doc, resolved
once at startup through a different endpoint than the feed. So `verified` means
verified against a key sanctioned out of band, not one the feed asserted about
itself, and a feed presenting a different signing key is rejected. Set
`COVENANT_VANTARA_PROVIDER_PUBKEY` to hard-pin a specific key instead; with
neither, verification falls back to the self-describing feed key.

`proofSignature` is a separate, network-internal node-to-orchestrator MAC. It is
verified server-side, intentionally not third-party verifiable, and left opaque
here — the connector never attests over it. Per-node public keys are a future
Vantara phase; until then the network key signs every receipt.

## Payment anchor

Each record carries `settlement` (`settled`, `settledAt` at hour precision): a
content-free confirmation that the job's node reward settled on-chain, with no
amount, signature, or wallet. That is the per-job payment binding, and it is
excluded from the signed receipt because it mutates when the reward settles.

`vantara.payouts` additionally surfaces the aggregate settlement ledger, whose
`txSignature`s are independently verifiable on any Solana mainnet explorer.
A job is never joined to a wallet — that would deanonymize node operators — so
wallet-level binding is out of scope by design.

## Configuration

```
COVENANT_VANTARA_ENABLED=true
COVENANT_VANTARA_BASE_URL=https://www.vantara.services   # optional
COVENANT_VANTARA_ALLOW=vantara.attest,vantara.payouts    # optional; empty = all
COVENANT_VANTARA_PROVIDER_PUBKEY=                        # optional; hard-pin the signing key (base58)
```

## Live check

Reads the real explorer and verifies every record, no keys, no spend:

```
cargo run -p covenant-vantara --example live_smoke
```
