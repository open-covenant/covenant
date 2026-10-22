# Public inference deployment

This is the release container for the inbound x402 V2 Solana USDC seller. It
contains the Rust gateway, the official x402 verifier, a normal Covenant Compute
operator and its private inference provider adapter. Only the gateway binds the
public port. It uses the existing coordinator and staking/settlement rules.

The [batch inference candidate](../../docs/compute-inference-batches.md) is a
separate opt-in. It uses journal v6, so rollback must retain the capable binary
and disable batch admissions rather than downgrade the financial journal.

Build `deploy/Dockerfile.compute-x402` from the repository root. The dedicated
image workflow publishes an immutable `x402-<revision>` tag inside the existing
public Compute node package. Deploy its resolved digest only after an anonymous
registry manifest/blob read succeeds. The runtime is Node24 plus the pinned Rust
build; no Docker socket or GPU access is required for this provider-backed node.

## State and secrets

Run one instance with a persistent volume at `/data`. The supervisor creates
separate gateway, operator, buyer-budget and provider directories, with private
permissions. Identity seeds are restored only onto an empty volume; a conflicting
buyer seed refuses startup. Fee payer keys are kept one file per address, so a
new fee payer seeds a new file; a key that does not match
`COVENANT_GATEWAY_FEE_PAYER` refuses startup before anything is written. A conflicting optional operator seed locks
new admissions while payment recovery stays available. Fee-payer keys and all existing monetary journals must
survive redeployment. Do not scale this container to multiple independent disks.

Copy the names in `environment.example` to the deployment's secret store. The
example deliberately disables operator registration and public admissions.
Configure the real merchant, fee payer and payout identities. Create the merchant
USDC associated token account and fund the dedicated fee payer with a small,
explicit SOL allowance. Keep merchant, client and funding-wallet private keys
outside the public service. Only its dedicated fee payer, native buyer and node
signing seeds belong in this container.

The operator needs the deployment's existing CVNT stake before it can be matched.
Stake approval is independent of the USDC test/reserve cap. Do not disable the
coordinator's stake requirement or reuse an active operator identity for a second
service.

## Initial acceptance allowance

The approved aggregate allowance is **10 USDC** for live tests and initial gateway
reserve. The proposed initial allocation is deliberately smaller:

| Purpose | Maximum counted against approval |
| --- | ---: |
| Native gateway prefunding and lifetime compute ceiling | 3 USDC |
| Local provider qualification | 1 USDC |
| Deployed provider lifetime ceiling | 1 USDC |
| AgentKit test wallet/customer payments | 0.5 USDC |
| Transaction fees and account creation | At most 1.5 USDC equivalent |

Local and deployed provider journals use separate allocations; their combined
ceiling remains 2 USDC. Keep both as evidence. A response-usage upper bound is not
an observed provider debit. Existing unrelated provider-account activity is not
part of this release allowance. Reconcile unknown outcomes before any retry or
budget increase. Unspent gateway balance remains owned by Covenant; the gateway
does not automatically recycle customer receipts into a fresh spending ceiling.

Covenant pays the Solana fee for customer payments with at most two signatures,
60,000 compute units and 1,000 microlamports per unit, so at most 10,060 lamports
per transaction; anything outside those limits is rejected.

Every quote from the current gateway includes a memo of 32 lowercase hexadecimal
characters, and the payment transaction must carry it unchanged. The memo is the
first 16 bytes of the SHA-256 hash of `covenant.x402.v3:<quote ID>:<request hash>`.
The quote states both inputs: the quote ID (a UUID) in `extra.quoteId` and the
request hash in `extra.requestSha256`, so anyone holding the quote can recompute
the memo. The request hash is the SHA-256 of the request body as the gateway
received it, serialized as compact UTF-8 JSON with object keys sorted at every
level; Covenant's SDKs compute the same hash and refuse a quote whose hash
differs. For example, quote ID `00000000-0000-4000-8000-000000000001` with a
request hash of 64 `a` characters gives the memo
`8bfb7fde3cb99c6b0f16b7a18d83a0e1`. In a read-only mainnet simulation, a payment
built by the official x402 client (`@x402/svm`) used 13,180 compute units,
within the 20,000 the client always sets, so standard wallets need no changes.

Earlier gateway versions issue the 118-byte memo
`covenant.x402.v2:<quote ID>:<request hash>` instead, and paying it needs 43,642
compute units. The official client always sets 20,000, so standard wallets cannot
pay those quotes; the memo's length tells the two kinds apart. Covenant's
JavaScript SDK from 0.4.0 and Python SDK from 0.3.0 sign them with a 60,000
compute-unit limit, which is within the limits Covenant pays fees for. Because the
verifier accepts both memo formats, a payment signed against an older gateway still
settles after that gateway is upgraded. Earlier SDK versions (JavaScript 0.3.0 and
older, Python 0.2.0 and older) refuse 32-character memos before signing, so upgrade
them before or together with the gateway.

If a payment is rejected, do not disable the verifier's simulation check, edit the
signed transaction or raise the priority-fee cap to push it through. The customer
can request a new quote and pay that instead.

After a rejected authorization, retain the original client and gateway records.
Before a separate acceptance attempt, verify the original gateway entry is still
quoted (no admitted job/payment), its blockhash is expired, its fully signed
transaction ID has no historical signature status, and customer balances are
unchanged. A diagnostic simulation with `replaceRecentBlockhash` and signature
verification disabled is useful for diagnosis only; it is not payment acceptance
and must never be broadcast or substituted into the original purchase.

## Promotion

1. Verify the image revision, dependency locks, private bridge tests, installed
   AgentKit package and Rust payment/recovery tests.
2. Provision the service with a persistent disk and admissions disabled. Confirm
   secrets survive a restart without changing identities or resetting journals.
3. Confirm the provider can serve the exact configured model within price and
   latency limits. Enable the staked operator and confirm live inference capacity.
4. Fund the gateway's dedicated buyer and read back its credited balance. Verify
   that the merchant, asset, network and fee payer match the public 402 quote
   and, when Base is offered, that the Base receiving address and USDC contract
   do too.
5. Under the remaining test cap, run the installed AgentKit demo. Retain signed
   request, native job/receipt, exact output, transaction readback and budget
   records. Repeat the original authorization and verify no second purchase.
6. Restart and inspect the same journals, balances and identities. Publish the
   page/video only with this evidence and the deployed revision.

For rollback, set `COVENANT_GATEWAY_ADMISSIONS_ENABLED=false`. Leave the gateway,
verifier and persistent volume running so pending settlement can recover. Do not
delete a journal, reset a volume, replace a payment authorization or reverse
already-earned operator pay. Process `/health` alone does not prove capacity or
successful paid inference.

## Recovery when an optional worker stops

The public gateway starts before the optional provider/operator in recovery-only
mode. `/data/gateway/admissions.starting` blocks quotes and new payment admissions
until the provider reports ready and the exact local operator has a fresh,
matchable registration. `/health` returns `admissions_enabled` separately from
process health; a recovery-only service still returns HTTP 200.

A failed provider or operator writes `/data/gateway/admissions.disabled` and
syncs it to the persistent disk before optional workers stop. Payment verification,
reconciliation and already-admitted result delivery keep running. The failure
latch survives redeployment and is never automatically removed. While it exists,
optional workers are not restarted and no new paid qualification probe runs.
Hung optional workers are terminated after a bounded shutdown window without
stopping payment recovery. After reviewing
and fixing the failure, explicitly remove that one latch file and restart the
service; readiness must pass again. Do not remove any payment/provider journal,
identity file or budget record. Disabling the local operator leaves admissions
closed while recovery remains available.

The private bridge authentication/seal secret is pinned in
`/data/gateway/bridge-secret` with mode 0600. Keep its configured value identical
across revisions: existing pending payments carry seals made with that key.
Changing it refuses startup, just like changing the buyer key. The fee payer
may change only when no open payment, including a result still held for
retries, used the old one; otherwise the gateway refuses to start.

## Immutable-container smoke qualification

After publishing the image, the image workflow pulls that exact digest and runs
its real entrypoint with a temporary persistent Docker volume and a 512 MiB
memory limit. Optional operator/provider processes and new admissions are
explicitly disabled. Random unfunded test identities are used; no production
keys, provider credentials or payment authorizations enter the test.

The host reads Solana mainnet's genesis hash once. The image then runs on a Docker
`--internal` network without published ports. A separate fixture, using the same
image, returns that observed genesis hash and rejects/counts every other RPC or
coordinator request. It has no forwarding path. The check verifies the image and
HTTP revision, disabled admissions, a refused inference request, private file
permissions, zero jobs/reservations and unchanged buyer/fee/seal keys and journal
identity after restart. Only the recovery clock may advance. No key bytes, private
fingerprints, journal contents or app Docker logs are printed. On failure, bounded
logs and process state are printed only for the separate RPC fixture, which has
never received keys or credentials.

Run it on a Linux Docker host with Node 24 and an accessible immutable image:

```sh
SMOKE_IMAGE=ghcr.io/open-covenant/covenant/compute-node-public@sha256:<digest> \
SMOKE_REVISION=<full-source-revision> node deploy/compute-x402/smoke.mjs
```

This is a startup and empty-volume continuity check. Funded settlement recovery,
real provider/operator readiness and memory usage with all workers enabled remain
separate acceptance checks.

To retest an already-published image without rebuilding Rust, dispatch the existing
`compute-x402-image` workflow with `smoke_image` set to its full immutable package
reference and `smoke_revision` set to its embedded 40-character source revision.
This runs only the `smoke-existing` job; the build job is skipped. The harness is
checked out from the dispatched ref, so fixture fixes can qualify an older image.
Smoke-only runs use a separate concurrency group from image builds.

## External rental-check observer

The optional `COVENANT_CANARY_WATCHDOG_ENABLED=true` process observes the separate
coordinator's fixed public `/rental-checks` endpoint every 60 seconds. It uses the
existing `/data/rental-watchdog` directory and no additional service. The child
receives only its journal path and the four `COVENANT_CANARY_ALERT_*` email
settings documented in `environment.example`; no buyer, operator or provider
credentials are passed to it. Its only network operations are a bounded public
GET and Cloudflare's email-sending API for the configured verified destination
behind the approved alert route. Deployment pins that destination to current
routing evidence; only an exact recipient match acknowledges delivery.

After 18 continuous hours without a current heartbeat, it queues one reporting
outage notification. A current disabled or budget-paused heartbeat remains
visible. A stale disabled snapshot, absent configured feed, invalid response or
network failure does not suppress an outage. Set the observer's own explicit
switch to `false` to stop observing intentionally. This checks feed availability;
the coordinator's monitor separately counts failed or missed rental windows.

Reporting restoration queues a separate notice. It never describes a fresh
heartbeat as a passed GPU check. The private journal and exact outbox event are
synced before email delivery. An unknown outcome retries the same event ID and
message; provider idempotency governs duplicate delivery. The outbox retains up
to 32 undelivered events and refuses to discard them automatically. Corrupt or
unreadable state is not reset. Inspect and preserve this journal during recovery.

The observer is protected by a crash-releasing process lock and restarted
separately from the financial worker group. Observer failure cannot lock
admissions, kill settlement workers, or repeat a paid qualification probe. This
provides a different instance from the coordinator; a failure affecting both
Render services or their shared infrastructure can still prevent observation.
Before enabling, confirm fresh journal timestamps and exercise delivery with
isolated fixtures. No paid rental is needed to qualify this observer.

## Model-specific quoted prices

Activate tariffs only with the full `compute-x402-image` build, which also includes
the rental observer. The observer-only `compute-rental-watchdog-image` overlay
preserves its pinned legacy payment runtime and cannot activate tariffs. Any
tariff setting on that overlay locks new admissions while legacy payment recovery
continues; its original flat-price operation remains available without tariffs.

Set `COVENANT_GATEWAY_TARIFFS_JSON` to an approved array of paired `retail` and
`native` versioned tariffs. The supervisor writes immutable configuration files
for both gateway and node. Native tariff matching must be deployed on the
coordinator before enabling the pair. Only the currently qualified provider model
and output limit are accepted; this does not add unqualified models.

New clients opt in with `pricing: "model_tariff_v1"`. Requests without that field
retain the original flat prices and receipt shape. The public `/v1/models` lists
the configured retail schedule while admissions are open. Each new 402 freezes
its retail amount and signed native execution before returning the quote. Saved
quotes never use a changed schedule. Malformed optional pricing configuration
latches new admissions off while the essential gateway and payment verifier
continue recovery. Repair configuration and review the latch before clearing it;
do not reset identities, budgets or journals.

The gateway upgrades journals from versions 2 through 5 to version 6, preserving
the namespace, financial entries, signed records, original memo form and Base
payment options. Version 3 freezes tariffs; version 4 preserves the short or long
payment memo; production version 5 preserves Base quotes and payments. The batch
candidate also used version 5 before deployment. Version 6 preserves both feature
fields and prevents either older binary from discarding recovery state. To stop
taking work, disable new admissions on this binary and retain settlement recovery.
Do not deploy an older binary or restore an old monetary snapshot over the journal.
See [pricing and limits](../../docs/compute-inference-tariffs.md) and the
[release evidence](../../docs/compute-model-pricing-release-evidence.md).
