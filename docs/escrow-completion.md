# Escrow completion statements

> **Experimental and not a release authorization.** This surface is opt-in and
> off by default (`COVENANT_ESCROW_ENABLED`). It signs two different classes of
> data: `result_hash_hex` and whether a local `intent_dispatched` row has status
> `ok` are derived from the daemon's audit log; `escrow_id`, hirer, worker,
> amount, asset, network, and provider are supplied by the caller. The daemon
> does not bind those caller-supplied fields to a prior escrow lock, inspect the
> work, validate output quality, or verify a payment onchain.

The returned blob can attribute this mixed statement to a pinned daemon key.
It must not be used by itself to release funds. A consuming escrow would need a
precommitted record of the exact job, payee, amount, asset, network, and escrow
identifier; it must compare every field with that record and apply its own
acceptance policy. The current Covenant endpoint does not create or verify that
binding.

## Enabling

The operator opts in at boot and grants the capabilities only to a trusted
escrow integration. Do not grant `escrow.completion.prove` to a hirer or worker
and do not enable this surface for automatic payouts:

```
export COVENANT_ESCROW_ENABLED=1
covenant capabilities grant escrow.completion.prove
covenant capabilities grant reputation.read
```

## The loop

1. An external escrow stores a precommitted context for a `job_id`.
2. Covenant records an `intent_dispatched` row for that job.
3. The trusted escrow integration calls `POST /escrow/prove` with the same
   context.
4. The daemon echoes the supplied context, derives the local result hash and
   `status == "ok"` observation, signs the mixed statement, and records it.
5. The escrow pins the expected daemon key, verifies the signature, compares
   every context field with its own precommit, and evaluates delivery or quality
   independently. The current statement alone is insufficient to release.
6. If the escrow pays under its own policy, it records and reconciles that
   payout in its own independently verified ledger. Covenant's legacy
   `POST /escrow/release` route is parked and admits no payout fact.

> A `job_id` with no local `intent_dispatched` row is denied. The existence of a
> successful row does not prove which external worker, amount, or escrow it
> belongs to and does not establish delivery quality.

## `POST /escrow/prove`

Requires capability `escrow.completion.prove`. No funds move.

Request:

```json
{
  "escrow_id": "escrow_xyz",
  "job_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
  "hirer_address": "0x0fA12125753428C58aE439E57fab3A94Bd93C78b",
  "worker_address": "0x7A4D3Ae53E9F96599143e1BF057ba11A7e09Ab3E",
  "amount": "10000000",
  "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  "network": "eip155:84532",
  "provider": "orbserv"
}
```

The daemon finds the newest `intent_dispatched` row for `job_id`, copies its
`result_hash_hex`, sets `validation_passed` to whether the row's status equals
`ok`, echoes all other request fields, and signs. Despite the legacy field name,
`validation_passed` is a local dispatch-status observation, not independent
validation of completion, delivery, or quality.

Response:

```json
{
  "kind": "completion_proven",
  "decision_id": "…",
  "proof": "<base64>",
  "worker_address": "0x7A4D3Ae53E9F96599143e1BF057ba11A7e09Ab3E",
  "issued_at": "1718553600000"
}
```

### Verifying the proof (escrow side)

`proof` is one opaque base64 token. Decode it, require the exact
`covenant.escrow-completion.v1\n` domain, reject the bundle unless its
`signer_pubkey_b58` equals a key obtained through a trusted channel, then verify
the ed25519 signature over the UTF-8 bytes of `domain || proof_json`. Do not
re-serialize `proof_json` and do not trust the key carried inside the bundle by
itself:

```
bundle = json_decode(base64_decode(proof))
assert bundle.domain == "covenant.escrow-completion.v1\n"
assert bundle.signer_pubkey_b58 == PINNED_COVENANT_PUBKEY
ed25519_verify(
  pubkey  = base58_decode(PINNED_COVENANT_PUBKEY),
  message = utf8_bytes(bundle.domain + bundle.proof_json),
  sig     = base58_decode(bundle.signature_b58),        // 64 bytes
)
proof = json_decode(bundle.proof_json)
```

After signature verification, compare `escrow_id`, `job_id`, hirer, worker,
amount, asset, network, and provider byte-for-byte with the escrow's own
precommitted record. Treat `result_hash_hex`, `validation_passed`, and
`audit_root_hex` as claims from that daemon. The local hash-chain root can show
later modification of included rows; it does not prove the log is complete or
that a runtime produced the claimed result. This contract still needs an
independent delivery/quality decision before any release.

## `POST /escrow/release`

This legacy compatibility route is parked. It rejects every request and writes
no settlement receipt, accounting entry, or audit event. Caller-supplied payout
fields cannot safely become Covenant facts until the daemon independently
verifies the transfer and atomically binds it to a prior authorization.

Request:

```json
{
  "escrow_id": "escrow_xyz",
  "decision_id": "…",
  "hirer_address": "0x0fA1…",
  "worker_address": "0x7A4D…",
  "amount": "10000000",
  "asset": "0x036CbD…",
  "network": "eip155:84532",
  "provider": "orbserv",
  "tx_sig": "0x…"
}
```

`decision_id` is the value from the `/escrow/prove` response. An enabled daemon
returns an error containing `escrow release reporting is disabled until payout
facts are independently verified and bound to the completion statement`.
The legacy `escrow_released` success response remains in the IPC schema only
for wire compatibility and is not emitted by this path.

## `GET /reputation/:worker_address`

Requires capability `reputation.read`. This legacy endpoint summarizes local
escrow statement and release rows. Those rows include caller-supplied context
and unverified payout reports, so the result is an audit-log heuristic, not
reputation or independent evidence of work. It returns `proofs_total`,
`validations_passed`, `validations_failed`, `releases`, `completion_rate_bps`,
and `computed_audit_root_hex`.

## Audit

`escrow_completion_proven` carries `decision_id` (the `proof_id`), `escrow_id`,
`job_id`, `hirer_address`, `worker_address`, `amount`/`asset`/`network`, the
derived `result_hash_hex` + `validation_passed`, `audit_root_hex`, and
`signature_b58`, so a consumer with the pinned daemon key and required domain
can authenticate the included statement. `escrow_released` is a historical
audit kind retained for compatibility with old logs; the parked release route
does not write it. Read retained rows with `covenant audit recent` or
`GET /audit/recent`; verify chain integrity with `GET /audit/verify`.

## Not yet

The format lacks a daemon-verified precommit that binds the job to the external
escrow context. It also lacks independent work validation, chain verification
of releases, signer-key discovery/rotation, power-loss guarantees, and dispute
handling. Safe release reporting additionally needs a verified
authorization-to-payout binding and an atomic or recoverable write protocol.
Until those exist, the statement must not authorize a payout and the release
reporter remains disabled.
