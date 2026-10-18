# Public inference deployment

This is the release container for the inbound x402 V2 Solana USDC seller. It
contains the Rust gateway, the official x402 verifier, a normal Covenant Compute
operator and its private inference provider adapter. Only the gateway binds the
public port. It uses the existing coordinator and staking/settlement rules.

Build `deploy/Dockerfile.compute-x402` from the repository root. The dedicated
image workflow publishes an immutable `x402-<revision>` tag inside the existing
public Compute node package. Deploy its resolved digest only after an anonymous
registry manifest/blob read succeeds. The runtime is Node24 plus the pinned Rust
build; no Docker socket or GPU access is required for this provider-backed node.

## State and secrets

Run one instance with a persistent volume at `/data`. The supervisor creates
separate gateway, operator, buyer-budget and provider directories, with private
permissions. Identity seeds are restored only onto an empty volume; conflicting
buyer/fee-payer seeds refuse startup. A conflicting optional operator seed locks
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

Customer authorization sponsorship is capped at two signatures, 60,000 compute
units and 1,000 microlamports per unit: at most 10,060 lamports per transaction.
The installed client uses 60,000 units because the full request-bound memo
consumed 43,642 units in a read-only mainnet simulation. The official SDK's default
20,000 units cannot execute that memo. Do not shorten the binding, bypass
simulation, alter an existing authorization, or raise the priority fee to work
around a rejected payment.

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
   the merchant/asset/network/fee payer against the public 402 quote.
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
Changing it refuses startup, just like changing the buyer or fee-payer key.

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
