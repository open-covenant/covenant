# Hatcher automatic cleanup and receipt recovery

## Failure and recovery contract

A GPU launch can be durably accepted by Compute even when its HTTP response is
lost or reports a transient provider failure. The gateway already saves the
launch idempotency key before making the request. Previously, a run that lost
the job ID could remain in `stopping` indefinitely because cleanup only knew how
to cancel by job ID.

Compute now exposes an authenticated, owner-scoped operation:

```http
DELETE /v1/jobs/by-key/{idempotency_key}
```

Its response echoes `idempotency_key`, sets `cancellation_recorded: true`, and
contains the existing `job` or `null`. The operation durably seals that key even
when no job exists. A delayed launch with the same owner and key cannot create a
job after absence was confirmed. A cancellation request is not itself proof of
provider cleanup; an existing job must still reach a terminal state with a valid
receipt. The launch and cancellation paths share the existing per-job lock.

The gateway reaper uses this operation only for closed runs with unresolved
launch keys. It validates the echoed key, acknowledgement, and booking identity
before updating the existing reservation. A matching job follows normal
cancellation and receipt settlement. A confirmed sealed absence releases the
reservation without creating a GPU charge. Errors and malformed responses remain
pending and are retried. Recovery never submits another launch, resets the
booking count, or changes the run's authorization or spending limits.

Compute also returns the persisted job after ambiguous provider launch errors,
allowing callers to retain its ID immediately. Definitive provider rejection
produces a control-plane receipt with zero charge and the full reservation
refunded. That receipt is explicitly identified as a control-plane rejection,
not a provider billing record.

## Final result and accounting

The gateway persists the terminal result only after GPU cleanup is confirmed.
The result includes GPU receipts and an itemized accounting summary. Model costs
are estimates from usage when available; unknown usage retains its conservative
hold. E2B is reported as the reserved maximum, not an actual supplier invoice.
Runs created before the accounting basis was persisted report unknown component
breakdowns rather than applying today's configuration to historical usage.

The connector keeps a finished dispatch tracked while sending its cached result
and proof. If the local transport rejects the send, it retries delivery without
submitting the intent again. Transport acceptance is not a Hatcher application
acknowledgement: the current wire protocol has no such acknowledgement or durable
cross-process delivery outbox.

## Rollout

1. Publish and deploy the reviewed Compute control-plane revision first. Its
   SQLite migration adds cancellation seals without deleting jobs or spend data.
2. Deploy the reviewed gateway revision with the existing persistent disk, run
   store, journal, ledger, credentials, and allowed run key unchanged.
3. Deploy the connector revision after the gateway is healthy. Drain existing
   tracked dispatches first because connector delivery tracking is in memory.
4. Record the exact live commits and Render deployment IDs. Build success alone
   is not deployment evidence.

Do not authorize a replacement dispatch, change the consumed one-booking limit,
or start a paid test as part of this rollout. Keep the 300-second booking,
$0.20 GPU and $0.50 total operating limits. Covenant's previously agreed coverage
of provider startup or cleanup overruns is unchanged.

## Verification without a new paid run

Local tests cover lost launch responses, sealed absence, late launch races,
owner isolation, restart durability, hostile response validation, terminal
accounting, and connector delivery retry.

For live integration verification, use a temporary copy of a closed gateway
journal and the already terminated incident's original launch key. Restrict
outbound requests to cancellation by that exact key and observation/cancellation
of its known job. Reject all launch, offer, model, and sandbox creation requests.
Run the deployed reaper against that temporary journal and require the existing
terminal receipt. Confirm production journal, run store, and ledger are
unchanged. The control plane will permanently seal that already-consumed launch
key; this is a production cancellation record, not a read-only query. This
verifies compatibility of the deployed recovery path without allocating a
resource or repeating a dispatch.

Check gateway readiness, zero active runs and reservations, unchanged authorization
and budget limits, and an online connector with zero tracked dispatches. Preserve
the verification output with the deployment evidence; do not publish secrets.

## Rollback

Rollback code only; retain the SQLite cancellation seals and all persistent
gateway state. Old gateway code cannot automatically resolve an unknown job ID,
so a rollback can restore the original pending-cleanup behavior. Never clear a
reservation, delete a seal, replace an idempotency key, or reset a budget to make
a rollback appear complete. Keep new dispatches paused until the recovery-capable
versions are restored.
