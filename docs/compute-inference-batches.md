# Batch inference

Prism package 12 introduces independent prompts under one paid call. This
Covenant implementation adds `POST /v1/inference/batches` to the existing x402 Solana
USDC gateway and adds `purchaseBatch` / `purchase_batch` to its JavaScript and
Python clients. Source `8ed36c4fe075bb097e90bfe8a009ca638083f0f3` passed one
installed-client two-child production acceptance on 2026-10-21, and fresh
admissions are enabled. Public batch SDK publication and discovery are separate. See the [release evidence](compute-inference-batches-release-evidence.md).

## Request and price

```json
{
  "version": 1,
  "requests": [
    {"model":"claude-haiku-4.5","messages":[{"role":"user","content":"Summarize A."}],"max_tokens":256,"pricing":"model_tariff_v1"},
    {"model":"claude-haiku-4.5","messages":[{"role":"user","content":"Summarize B."}],"max_tokens":256,"pricing":"model_tariff_v1"}
  ]
}
```

Fresh purchases accept **one or two** independent text-chat requests. Each child
uses the existing model, input, output-cap and generation-parameter validation.
Nested batches, streaming, tools and vision are rejected. The whole HTTP request
must fit the existing 128 KiB limit. The generic proof and recovery format is
bounded at 32 items; this release does not activate purchases of that size.

The exact retail price is the sum of the child prices. Each tariff child costs
`base_micro_usdc + ceil(max_tokens * output_cap_micro_usdc_per_million / 1000000)`.
Omitting `pricing` retains the configured legacy flat price for that child.
Prices are fixed by requested output cap; generated token counts do not reprice
the request. There is no batch discount or throughput claim.

The 402 response freezes `extra.batchPricing`: version, batch UUID and the ordered
child job UUIDs, models, output caps, pricing modes and exact retail amounts.
The gateway persists every original signed native job before returning the quote.
The verifier independently checks each child price against its loaded policy and
requires their sum to equal the single payment amount. It also binds the batch
resource URL, merchant, asset, fee payer and request-bound memo. A single-request
authorization cannot be used on the batch route.

## Deadlines and execution

The existing provider operator serves one job at a time, so this release executes
children sequentially. Freshness and completion are separate: a child must be
dispatched while its original signature is fresh, and finish within its signed
deadline. Admission reserves 45 seconds per child and a further settlement margin,
and checks tariff validity too. With the current 120-second freshness and
180-second deadline, a two-child quote lasts at most 30 seconds. Always honor the
returned expiry; wallets and clients must not refresh a signed authorization.

Capacity is observed before quoting. The sum of native child costs must fit both
the merchant reserve and the durable buyer budget. These checks do not reserve
provider capacity. Each native dispatch uses the existing durable budget and
settlement path; concurrent budget consumers can make later work unavailable,
without authorizing overspend or a replacement job.

The customer payment is broadcast only after every child has a verified successful
native result. A definitive child failure leaves the customer uncharged; any
already-earned operator payments remain paid. The gateway conservatively charges
the full native batch ceiling to its seller-loss reserve on partial failure.
An uncertain child outcome remains pending and is reconciled using the original
job identity. A pending customer transaction similarly retains its original
signed bytes and transaction ID.

The timing margins do not extend a Solana recent blockhash lifetime. If work or
settlement runs too slowly, the original customer payment may become unlandable;
the merchant bears the completed native costs. Retain the unresolved payment
record for reconciliation and never sign a replacement to collect it. The live
two-child acceptance must record actual timing and original transaction finality.

Completed child outputs are fsynced in a private per-batch journal. Restart skips
completed children. After the main journal durably records settlement or terminal
failure, the temporary progress copy is removed; recovery also cleans copies left
by a crash between those operations. Settled plaintext follows the existing
five-minute retention period while financial tombstones remain.

## Result verification and client recovery

The response contains the ordered native outputs and an RFC6962 Merkle tree.
Each domain-separated leaf binds the batch UUID, canonical parent request hash,
position, child job UUID and canonical output hash. Clients verify the paid quote's
batch/child IDs, every inclusion path and each original signed native receipt,
including model, generation parameters, native tariff and approved operator.
These proofs establish provenance and membership, not factual correctness.

Both clients share their existing SQLite journal, request-ID namespace and spend
ceiling between single and batch purchases. Retrying the same request ID and body
recovers the original authorization or cached completed result; changing the body
under that ID is rejected. See the [JavaScript](../sdk/compute-x402-client/README.md)
and [Python](../sdk/compute-x402-python/README.md) examples.

Limits are explicit: 256 KiB per retained child output, 9 MiB aggregate proof
output, 10 MiB progress file, 64 MiB main gateway journal, 10 MiB client batch
response and 128 KiB private verifier request. The single-client response limit
remains 4 MiB. Journal admission reserves output headroom before paid execution.

## Activation and rollback

`COVENANT_GATEWAY_BATCH_ENABLED=true` is required in both the gateway and private
verifier. Leave it false until release qualification is complete. The free model
catalog, discovery manifest and OpenAPI continue to advertise only the accepted
single-chat product; batch discovery/indexing is a separate rollout step.

This binary migrates gateway journals v2/v3/v4/v5 to v6 without replacing identities,
prices, balances, budgets, latches, Base payment options or original payment records.
Base remains available on single requests; this batch release offers Solana only.
The independent pre-release batch v5 and production Base v5 fields both migrate
without reinterpretation. It preserves each
saved quote's memo binding: legacy v2 memos remain long, and v3 memos retain the
32-character request binding accepted by standard x402 wallets. Fresh batch
quotes use that same short binding over the ordered batch request. An older binary
cannot recover the new journal. Roll back by disabling batch admissions on this
binary, retaining the verifier and volume so admitted work can reconcile. Inspect
the live journal size and pending work before deployment. Do not restore a stale
financial snapshot or downgrade its version number.

The deployed immutable Linux image passed isolated persistent-volume restart
smoke. Its installed-client two-child test verified finalized retail and native
settlement, full before/after accounting and zero-call completed replay. A private
harness logging error after HTTP 200 required local recovery from the saved
original response and finalized transaction through the normal SDK journal path;
no second payment request or signature was used. The optional live restart and
direct server replay were not run. Fresh admissions were restored after review
without a restart, additional payment or financial change. Preserve original
identities and unknown outcomes if a step
fails; no automatic retry of a new purchase is permitted. Deployment normally
runs a paid provider startup probe, which requires its own approved scope.
