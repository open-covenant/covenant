# Compute chain observer and public status

The observer is read-only and off by default. It starts with Solana `LeaseTerms`
accounts before consulting the coordinator's journal, so a funded lease without
any local job is visible. It never sends a transaction, dispatches a job, retries
a payment, changes fees, or repairs the journal. Enabling it does not enable or
change the separately configured lease meter.

`GET /status` is public, versioned JSON with `Cache-Control: no-store`. The native
website uses it for `/compute-status`. The response includes generation time,
public deployment identities, journal writability, matchable capacity, the latest
recorded lease payout time, observation freshness and finding counts, aggregate
alert delivery state, and source-reviewed incidents. It contains no buyer or job
identity, RPC URLs, credentials, SSH details or alert destination. The funding
renter and lease coordinator pins are public service identities, not buyer keys.

## Reading the status

- `healthy` means no discrepancy was observed in the configured scan scope. It
  does not certify custody solvency, transaction finality, GPU execution, or
  service-wide availability.
- `degraded` means a discrepancy was observed. Alerts wait for three observations
  of the same finding category before notifying an operator.
- `blind` means RPC, network identity, program or account validation prevented a
  complete readable observation. It is not evidence that money disappeared.
- `stale` means the latest recorded scan started more than three observation
  intervals ago; `not_observed` means the first scan has not finished; `disabled`
  means no observer is configured.
- `coverage: no_leases` explicitly identifies a successful empty scan. It is not
  evidence that a real settlement has been checked. `leases_observed` identifies
  a nonempty complete scan, and `incomplete` identifies missing visibility.

Empty matchable capacity and no recent payout do not prove an outage. A journal
with failed durable writes is reported separately; an in-memory test process is
labelled `journal_persistence: memory`. `latest_recorded_lease_payout_at_ms` is the
local recording time, not a claimed block timestamp. A recorded signature alone
is not a finality proof.

## Explicit deployment pins

| Variable | Meaning / default |
| --- | --- |
| `COVENANT_COMPUTE_OBSERVER` | `0` (off); `1` requires all pins below |
| `COVENANT_COMPUTE_OBSERVER_GENESIS_HASH` | Exact expected L1 genesis hash |
| `COVENANT_COMPUTE_OBSERVER_RPC_URL` | L1 Solana JSON-RPC endpoint |
| `COVENANT_COMPUTE_OBSERVER_ER_RPC_URL` | Ephemeral-rollup JSON-RPC endpoint |
| `COVENANT_COMPUTE_OBSERVER_PROGRAM` | Deployed lease or settlement program |
| `COVENANT_COMPUTE_OBSERVER_COORDINATOR` | Coordinator public key in chain lease terms |
| `COVENANT_COMPUTE_OBSERVER_RENTER` | Sidecar vault-funding renter public key |
| `COVENANT_COMPUTE_OBSERVER_MINT` | Settlement token mint |
| `COVENANT_COMPUTE_OBSERVER_VALIDATOR` | Pinned ER validator identity |
| `COVENANT_COMPUTE_OBSERVER_INTERVAL_SECS` | `60` |
| `COVENANT_COMPUTE_OBSERVER_GRACE_SECS` | `120`; ordinary open/settle handoff grace |

No signing key belongs in these variables. Each scan verifies L1 genesis and an
executable program even when there are zero accounts. Scans use `confirmed`
commitment, a two-minute deadline, a 16 MiB response limit and a 20,000-account
limit. Oversize, timeout and regressing L1 slots report incomplete visibility. The scan
slot must have an RPC block time no more than five minutes old or 30 seconds in
the future, preventing an indefinitely cached successful scan from staying green.
RPC redirects are refused; errors never include endpoint URLs or response bodies.

The account scan is scoped to the configured coordinator and renter. It verifies
program ownership, exact supported account layout, discriminator, parties,
mint, validator and PDA seeds. Terms and meter are re-read from one L1 bank at
least as recent as the scan. Delegation zeroes the L1 meter and changes its owner;
the observer then checks the pinned ER identity and reads the delegated meter
there. ER elapsed time is never treated as proof of an L1 payout. Settlement of
an expired still-delegated lease uses zero usage, matching the program rule.

Findings cover open and settled chain leases missing from the journal, overdue
settlement, incomplete split payouts, contradictory local/chain terminal states,
changed terms, missing local settlement evidence and mismatched charges. Split
operator/renter payouts are a supported program state: a partial settlement is
an attention category after grace and repeated observations, not an impossible
state. The observer does not assert that every live local lease must have a
chain account: current journal records lack a durable successful chain-open
marker and the existing meter permits off-chain fallback. Historical scope is
limited to retained chain accounts and this deployment's retained local journal.

## Alerts and recovery

`observer-alert-state.json` lives beside the coordinator journal, with independent
atomic replacement and file/directory fsync. It is bound to the six public chain
pins. Unknown/corrupt state or a different deployment binding is rejected rather
than silently resetting prior alerts. The file contains aggregate category
state and opaque event IDs, not job/account identities or destinations.

A category must occur in three observations before firing, and then be absent
from three fully readable complete scans before recovery. Blind or partial scans
cannot resolve a demonstrated discrepancy. Repeated findings do not issue new
notifications. Restart resumes the same pending event IDs and confirmation
state; a gap exceeding three intervals resets unfinished confirmation sequences.
Transitions are durably queued before delivery, with a maximum of 64 pending
events. A full queue or failed persistence is exposed through status/metrics and
never acknowledges a lost transition.

By default, transitions go to structured operator logs. Optional HTTPS webhook:

- `COVENANT_COMPUTE_OBSERVER_ALERT_WEBHOOK`

Or configure the Covenant Cloudflare Email Service for email (mutually exclusive):

- `COVENANT_COMPUTE_OBSERVER_ALERT_CLOUDFLARE_API_TOKEN` — Email Sending permission
- `COVENANT_COMPUTE_OBSERVER_ALERT_CLOUDFLARE_ACCOUNT_ID` — owning account ID
- `COVENANT_COMPUTE_OBSERVER_ALERT_FROM` — plain, provider-verified sender mailbox
- `COVENANT_COMPUTE_OBSERVER_ALERT_TO` — plain recipient mailbox

The email origin is fixed to `https://api.cloudflare.com`, using the configured
account’s `/client/v4/accounts/{account_id}/email/sending/send` endpoint.
Payloads contain
only category, aggregate count, observation time, event ID and public status link.
Requests have five-second deadlines, refuse redirects, and use the event ID as
`Idempotency-Key`. At most four deliveries are attempted per sweep; an unknown
or rejected outcome remains queued. Cloudflare must return `success: true` and
name the recipient in `delivered` or
`queued`, without a permanent bounce or suppression. Responses are capped at
16 KiB. Accepted delivery does not prove inbox arrival. Delivery is at least
once: Cloudflare does not document provider idempotency guarantees, so a lost
acknowledgement can lead to a duplicate despite the stable event ID. Receivers
of generic webhooks should deduplicate by event ID. The
[Cloudflare REST contract](https://developers.cloudflare.com/email-service/api/send-emails/rest-api/)
defines recipient acceptance. No secret or destination is logged or exposed publicly.

Prometheus `/metrics` includes enabled, complete, latest observation timestamp,
scanned leases, visibility, finding categories, active/pending alerts and alert
persistence health. Alert on stale timestamp, blind visibility, failed alert
persistence, or a growing pending queue. Do not label missing visibility as
insolvency. The observer shares the coordinator process; independent external
HTTP monitoring is required to detect a stopped process or an unreachable site.

## Reviewed incident record

`agent-os/crates/covenant-compute-coordinator/incidents.json` is versioned source
(`schema_version: 1`). Each incident needs a unique ID, start time, optional
resolution time, effect, cause, fix, and ordered timestamped updates inside its
window. Unknown cause/fix must explicitly say investigation is ongoing. Tests
reject missing disclosure fields, duplicate IDs, unknown versions and incoherent
timelines. The initially empty reviewed record is not a claim of zero historical
incidents. Automated findings never publish unreviewed incident narratives.

## Validation and promotion

From `agent-os`:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p covenant-compute-coordinator --all-targets
```

An isolated live-RPC harness starts no server, journal, sidecar or financial
worker and sends no transaction. Configure only the observer's public pins and
RPC endpoints, then run:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo run -p covenant-compute-coordinator --example observer-once -- --empty-journal
```

Its intentionally empty local books should flag real chain leases as missing
local records. Do not present these harness findings as production incidents.
Wrong-genesis, missing-program and unavailable-RPC controls must be `blind`.
Before enabling public status, verify the actual deployed revision, production
journal comparison, alert receipt, persistence over restart, and recovery. No
funded rental is required. Disable only the observer on rollback; preserve its
state file and leave existing settlement/cleanup workers running.
