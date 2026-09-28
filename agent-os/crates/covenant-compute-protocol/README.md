# covenant-compute-protocol

Wire protocol for the Covenant compute network: the signed messages a
buyer, a coordinator and an operator node exchange, and nothing else.
Pure types and signing helpers — no I/O, no transport, `#![deny(unsafe_code)]`.
The network-level map lives at
[docs/compute-network.md](../../../docs/compute-network.md).

Both sides of every connection depend on this crate, so the wire shape
cannot drift: the node and the coordinator literally share the structs
they serialize.

## What's in the box

- **`SignedJobEnvelope`** — a buyer's job: kind, input, price, deadline,
  capability requirements, all under one ed25519 signature. The envelope
  is the buyer's offer and, once matched, the exact record both sides
  hold each other to.
- **`GenerationParams`** — the buyer's sampling knobs (`temperature`,
  `top_p`, `max_tokens`, `seed`, `stop`), signed inside the envelope's
  input block and bounds-checked at parse, so a malformed ask refuses
  before any money moves; `chat_input`/`ChatMessage` are the companion
  multi-turn input shape.
- **`SignedWorkReceipt`** — the operator's countersigned result: status,
  output hash, meter readings, and the root of the operator's own
  hash-chained audit log at signing time. A receipt re-verifies locally;
  nobody has to trust the relay that carried it.
- **`CapabilityProfile`** / **`HardwareClass`** / **`PriceAsk`** — what an
  operator claims to be able to run and at what price, signed into its
  `RegisterRequest`.
- **`JobKind`** — `inference_call`, `batch_job`, `lease_session`.
- **`StreamChunk`** / **`StreamPush`** — a streaming job's live output
  feed (the envelope's opt-in `stream` flag): seq-numbered text pieces
  the node batches up while it works. Deliberately unsigned — chunks
  are a preview, and the receipt over the final output's hash is what
  settles; a buyer can hold the assembled feed against that verified
  output afterwards.
- **`RegisterRequest`** / **`HeartbeatRequest`** — self-authenticating
  operator lifecycle messages.
- **`DisputeRequest`** — a buyer's signed, time-bounded complaint against
  a concluded job.
- **`UnbondRequest`** and the bond memos — an operator's stake (C5
  phase 2): a bond post attributes itself on-chain with
  `compute-bond:v1:<operator>` (the deposit memo's contract), and the
  signed unbond request takes unslashed stake back out, its refund
  memo-tagged `compute-bond-refund:v1:<operator>:<id>`. The request
  reserves nothing — stake stays slashable until the refund leaves.
- **`WithdrawalRequest`** — a buyer's signed money-out. The recipient
  wallet rides under the buyer's own signature (a relay cannot re-point
  it), the buyer-chosen id is the idempotency key end to end, and the
  transfer memo-tags itself `compute-withdrawal:v1:<buyer>:<id>`.
- **Payout linkage** — `payout_memo_for` derives from the receipt the
  SPL memo its transfer must carry
  (`compute-payout:v1:<job id>:<receipt signature>`), and
  `verify_payout_transaction` holds a fetched transaction to exactly
  that memo and reads the amount and recipient off the chain's own
  record — what the node's `earnings verify` and the buyer's
  `compute.verify` both build on, with no trust in the coordinator.
- **`FederationEscrow`** — the hold/release/refund trait a coordinator
  implements; `EscrowStatus` and `RefundReason` are the shared vocabulary
  for where the money stands.
- **`FundingSource`** — every hold is tagged `organic` (real buyer money)
  or `bootstrap` (subsidized), so subsidy can be capped and audited
  instead of leaking into revenue.
- **`MarketplaceFee`** — the basis-points take disclosed to operators at
  registration and applied at payout.
- **Signed reads** — helpers for GET endpoints whose response is private
  to one key (balances, job history): the caller proves possession of
  the pubkey the path names.
- **`SealedSecret`** / `vault_seal` / `vault_open` — a secret encrypted
  under a key only the buyer holds (XChaCha20-Poly1305, fresh nonce per
  seal), so the coordinator's vault stores ciphertext it cannot read.
  `sign_vault` authorizes a vault request the way a signed read does,
  and additionally binds the exact body a store writes.
- **`PROTOCOL_VERSION`** — the wire version every party declares in the
  `x-compute-protocol` header, bumped only on a breaking wire change.
  It exists so a coordinator deployed ahead of a stale node can refuse
  with "upgrade" and both numbers instead of a parse error; the
  coordinator's configurable version floor is the enforcing end.
  `validate_address_b58` is the companion entry-check every path that
  will eventually transfer money runs on its addresses.

## Conventions

- All money is integer micro-USDC; settlement currencies are SOL/USDC.
- All signatures are ed25519 over canonical serializations; every signed
  type carries its own `verify()`.
- Timestamps are Unix milliseconds, with explicit skew windows where
  freshness matters.

Used by [`covenant-compute-coordinator`](../covenant-compute-coordinator),
[`covenant-compute-node`](../covenant-compute-node) and
[`covenant-compute-buyer`](../covenant-compute-buyer).
