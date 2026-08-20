# Escrow completion proofs

Covenant as the trust layer for an external marketplace escrow (e.g. Orbserv's
OrbMarket holding funds in OrbWallet on Base). A hirer locks funds against a
job, the worker runs under Covenant, and the escrow asks the daemon to prove
completion. **Covenant does not take the caller's word that the work is done**:
it looks the job up in its own audit chain, derives the result hash and
validation outcome from the worker's actual run, and signs a proof carrying
those derived facts plus the escrow context. The escrow verifies the signature
against the daemon's published pubkey and releases funds to the worker when
`validation_passed`. Covenant holds no funds and moves none — it produces the
release signal and records it.

Because the facts are derived from Covenant's own records, it is safe for the
hirer wallet itself to call prove: it cannot forge a result the chain does not
show. After releasing, the escrow reports the payout back so it joins the proof
in the audit chain, idempotent on `decision_id`.

Opt-in and off by default (`COVENANT_ESCROW_ENABLED`).

## Enabling

The operator opts in at boot and grants the calling identity the capabilities:

```
export COVENANT_ESCROW_ENABLED=1
covenant capabilities grant escrow.completion.prove
covenant capabilities grant escrow.release.record
covenant capabilities grant reputation.read
```

## The loop

1. The hirer locks funds in escrow against a `job_id`. The worker then runs
   that job under Covenant, which records an `intent_dispatched` row.
2. The escrow (or the hirer wallet) calls `POST /escrow/prove` with the
   `job_id`.
3. The daemon looks up the job's run, derives `result_hash`/`validation`,
   signs the proof, records it, and returns an opaque proof blob.
4. The escrow **verifies the blob** and releases funds to `worker_address` when
   `validation_passed`.
5. The escrow calls `POST /escrow/release` to record the payout, joined to the
   proof by `decision_id`.

> The `job_id` must be the Covenant job/intent uuid the worker actually ran
> under — that is the key the lookup matches. A `job_id` with no run in the
> chain is denied.

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

The daemon finds the `intent_dispatched` row for `job_id`, sets
`result_hash_hex` from it and `validation_passed` from whether that run
finished `ok`, binds the escrow context, and signs. A job with no run returns
an error; a job whose run failed is proven with `validation_passed: false` so
the escrow simply does not release.

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

`proof` is one opaque base64 token. Decode it, then verify the ed25519
signature over the **exact** inner `proof_json` bytes — do not re-serialize:

```
bundle = json_decode(base64_decode(proof))   // {proof_json, signature_b58, signer_pubkey_b58}
ed25519_verify(
  pubkey  = base58_decode(bundle.signer_pubkey_b58),   // 32 bytes
  message = utf8_bytes(bundle.proof_json),
  sig     = base58_decode(bundle.signature_b58),        // 64 bytes
)
proof = json_decode(bundle.proof_json)
```

On success, trust the parsed `proof` fields (`job_id`, `worker_address`,
`amount`, `result_hash_hex`, `validation_passed`, `audit_root_hex`) and release
to `worker_address` when `validation_passed` is true. `audit_root_hex` is
Covenant's tamper-evident chain root at proof time, anchoring the proof to a
verifiable work history.

## `POST /escrow/release`

Requires capability `escrow.release.record`. Records the payout the escrow made
with its own custody.

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

`decision_id` is the value from the `/escrow/prove` response. Response:
`{ "kind": "escrow_released", "recorded_at": "<epoch-ms>" }`.

Idempotent on `decision_id`: retry freely. A repeat writes no second
`escrow_released` row and moves no funds (Covenant custodies nothing). Safe for
a fire-and-forget background record after the payout lands.

## `GET /reputation/:worker_address`

Requires capability `reputation.read`. A worker's standing, computed from the
escrow rows in the audit chain — not self-reported. Returns `proofs_total`,
`validations_passed`, `validations_failed`, `releases`, `completion_rate_bps`,
and `computed_audit_root_hex` (the chain root the score was read over, so it is
reproducible). This is Covenant's half; combine it with the escrow's earnings
ledger for the full picture.

## Audit

`escrow_completion_proven` carries `decision_id` (the `proof_id`), `escrow_id`,
`job_id`, `hirer_address`, `worker_address`, `amount`/`asset`/`network`, the
derived `result_hash_hex` + `validation_passed`, `audit_root_hex`, and
`signature_b58`, so each row is independently re-verifiable. `escrow_released`
carries the same `decision_id` plus the `receipt_id` and `tx_sig`, so the proof
and the payout read back as a linked pair. Read with `covenant audit recent` or
`GET /audit/recent`; verify chain integrity with `GET /audit/verify`.

## Not yet

The lookup keys on a single daemon's audit chain: the worker must have run
under the same daemon that issues the proof. Disputes are not yet a primitive
(no dispute row to count), so they are out of the reputation cut.
