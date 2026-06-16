# Escrow completion proofs

Covenant as the trust layer for an external marketplace escrow (e.g. Orbserv's
OrbMarket holding funds in OrbWallet). An escrow funds a task, the worker runs
under Covenant, and when the work is done the daemon issues a **signed
completion proof** the escrow releases against. After releasing, the escrow
reports the payout back so it joins the proof in the audit chain.

Covenant holds no funds and moves none. It produces the release signal and
records it. This is the Phase 2 counterpart to the spend-authorization surface
(`docs/spend-authorization.md`); it reuses the same audit log, settlement
receipts, and ed25519 identity.

Opt-in and off by default (`COVENANT_ESCROW_ENABLED`).

## Enabling

The operator opts in at boot and grants the calling identity the capabilities:

```
export COVENANT_ESCROW_ENABLED=1
covenant capabilities grant escrow.completion.prove
covenant capabilities grant escrow.release.record
```

## The loop

1. The escrow funds a task and the worker runs under Covenant.
2. On completion the escrow (or the worker's operator) calls `POST /escrow/prove`.
3. The daemon binds the reported facts to its audit chain root, signs the
   envelope with its identity, records it, and returns the signed proof.
4. The escrow **verifies the signature** against the daemon pubkey and releases
   funds to `worker_pubkey` when `validation_passed` is true.
5. After the transfer lands, the escrow calls `POST /escrow/release` to record
   the payout. That row joins the proof by `proof_id`, closing the loop in the
   audit chain.

## `POST /escrow/prove`

Requires capability `escrow.completion.prove`. No funds move.

Request:

```json
{
  "task_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
  "worker_pubkey": "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStpvxbiyN",
  "provider": "orbserv",
  "result_hash_hex": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
  "validation_passed": true
}
```

| Field | Type | Meaning |
|---|---|---|
| `task_id` | uuid | The task/job the escrow is funding. |
| `worker_pubkey` | string | bs58 ed25519 key of the agent the escrow should pay. |
| `result_hash_hex` | string | Hash of the delivered result (the value the audit chain carries on the worker's `IntentDispatched`). |
| `validation_passed` | bool | Whether the work validated. Release only when true. |

Response:

```json
{
  "kind": "completion_proven",
  "proof_json": "{\"proof_id\":\"...\",\"task_id\":\"...\",\"worker_pubkey\":\"...\",\"provider\":\"orbserv\",\"result_hash_hex\":\"...\",\"validation_passed\":true,\"audit_root_hex\":\"...\",\"proven_at\":1718553600000}",
  "signature_b58": "5h4...",
  "signer_pubkey_b58": "8x…"
}
```

### Verifying the proof (escrow side)

`proof_json` is the **exact** canonical message the daemon signed. Do not
re-serialize it — verify the signature over its raw bytes, then parse it:

```
ed25519_verify(
  pubkey  = base58_decode(signer_pubkey_b58),   // 32 bytes
  message = utf8_bytes(proof_json),
  sig     = base58_decode(signature_b58),        // 64 bytes
)
```

On success, trust the parsed fields and release escrow to `worker_pubkey` when
`validation_passed` is true. `audit_root_hex` is Covenant's tamper-evident
audit chain root at proof time, so the proof is anchored to a verifiable work
history, not a bare assertion.

## `POST /escrow/release`

Requires capability `escrow.release.record`. Records the payout the escrow made
with its own custody.

Request:

```json
{
  "proof_id": "…",
  "provider": "orbserv",
  "network": "eip155:8453",
  "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  "amount": "80000",
  "tx_sig": "0x…"
}
```

| Field | Type | Meaning |
|---|---|---|
| `proof_id` | uuid | The `proof_id` from the `/escrow/prove` response this release acted on. |
| `amount` | string | Atomic amount released, decimal string. |
| `tx_sig` | string, optional | On-chain transaction signature or hash. |

Response: `{ "kind": "escrow_released", "receipt_id": "…", "proof_id": "…" }`.

Idempotent on `proof_id`: retry freely. A repeat returns the **same**
`receipt_id` without writing a second `escrow_released` row, so one release
yields exactly one receipt. Covenant debits no budget here — the escrow holds
the funds, Covenant only records the payout.

## Audit

`escrow_completion_proven` carries the `proof_id`, `task_id`, `worker_pubkey`,
`result_hash_hex`, `validation_passed`, `audit_root_hex`, and the
`signature_b58`, so each row is independently re-verifiable. `escrow_released`
carries the same `proof_id` plus the `receipt_id` and `tx_sig`, so the proof
and the payout that acted on it read back as a linked pair. Read with
`covenant audit recent` or `GET /audit/recent`; verify chain integrity with
`GET /audit/verify`.

## `GET /reputation/:worker_pubkey`

Requires capability `reputation.read`. A worker's standing, computed entirely
from the escrow rows above — not self-reported. `worker_pubkey` is the bs58 key
the completion proofs name (URL-safe, so it rides the path).

```json
{
  "kind": "reputation",
  "worker_pubkey": "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStpvxbiyN",
  "proofs_total": 3,
  "validations_passed": 2,
  "validations_failed": 1,
  "releases": 2,
  "completion_rate_bps": 6666,
  "computed_audit_root_hex": "…"
}
```

| Field | Meaning |
|---|---|
| `proofs_total` | `escrow_completion_proven` rows naming this worker. |
| `validations_passed` / `validations_failed` | of those, the validation outcome. |
| `releases` | `escrow_released` rows whose proof was one of this worker's. |
| `completion_rate_bps` | `validations_passed / proofs_total`, in basis points. |
| `computed_audit_root_hex` | the chain root the score was read over. |

The score is reproducible: recompute over the same audit chain and the numbers
match, so neither the worker nor the operator can inflate it. This is
Covenant's half. A marketplace combines it with the escrow's earnings ledger
for the full picture.

## Not yet

The proof binds the facts the authenticated, capability-gated caller reports;
the daemon does not yet re-derive `result_hash_hex` from its own
`IntentDispatched`/run record. Binding the proof to that internal record is a
planned tightening — the same enforcement-vs-record boundary
`spend-authorization.md` notes for settlement. Disputes are not yet a primitive
(no dispute row to count), so they are out of the reputation cut for now.
