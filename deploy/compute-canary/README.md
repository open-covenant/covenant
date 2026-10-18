# Production rental checks

`scripts/compute-rental-canary.py` drives the installed, version-pinned buyer CLI
against the public HTTPS coordinator. A passing check discovers affordable
capacity, opens one bounded lease, authenticates the SSH host key, inspects the
GPU with a fixed `nvidia-smi` command, closes the lease, verifies settlement on
Solana, reconciles the buyer balance, and confirms provider instance deletion.

This is a sampled rental check. It does not certify every operator, attest GPU
hardware, or guarantee uninterrupted availability. Preflight, recovery after a
failed job, and refunded rentals are never successful GPU checks. Local tests
are not production acceptance.

## Spending and recovery

The approved pilot admits at most **$2 in a rolling 24 hours and $20 total**.
Each planned 360-second lease at 334 micro-USDC per second reserves:

| Component | Reservation |
| --- | ---: |
| Buyer lease ceiling | 0.12024 USDC |
| Provider allocation | $0.35 |
| Network fee allocation | $0.02 |
| Storage allocation | $0.001 |
| Total admission reservation | $0.49124 |

Four full reservations total $1.96496. Provider, fee, and storage allocations
remain counted permanently against the pilot. Only the buyer component can
fall to its actual charge, after verified settlement, balance reconciliation,
and authenticated provider cleanup. Operator revenue is never subtracted.
Uncertain outcomes keep their full reservation and remain in the daily window
regardless of age. No new rental is admitted while one is unresolved. An
attributable authenticated provider charge above its allocation increases the
journal total and permanently blocks new admissions.

These are hard **new-spend admission** limits. The provider allocation is
conservative; deletion outages can still increase external invoices. It is not
a provider billing guarantee. Configured price/runtime/transfer limits and
current affordable capacity must pass before dispatch. The runner does not
raise its price or send funds when balance or capacity is insufficient.

The pilot uses a 240-second readiness limit, a $1/hour provider price ceiling,
and $0.004/GB ingress/egress admission ceilings. Lease startup is inside the
360-second session window: the executor starts its clock before provisioning
and subtracts that elapsed time. Teardown's six 30-second request deadlines plus
94 seconds of backoff allow roughly 274 seconds of additional attempted cleanup.
One 634-second instance at the hourly ceiling plus a 16 GB transfer allowance is
about $0.24012, leaving margin inside the $0.35 provider allocation. The 16 GB
figure is a transfer planning allowance, not an enforced byte quota; provider
outages still have the invoice limitation above. The canary's buyer-scoped broker
path stops after a created instance fails SSH-key attachment instead of paying
for another creation. Pre-creation offer-race retries remain allowed.

`budget.json` is atomically replaced and fsynced. It contains the full recovery
record and fixed job UUID before `lease open --job-id UUID` is called. That CLI
path durably reserves the UUID and sends at most one POST. Lost responses use
signed reads and provider evidence; open is never repeated. Limits and buyer /
coordinator identity are pinned in the journal. The buyer pin includes SHA-256
of its existing regular 32-byte seed file, so replacing a key at the same path
cannot reuse the budget. The seed is never published. Missing or invalid state fails
closed. `active.json` is only a secondary view: deleting it cannot erase the
reservation. Never remove the monetary journal to restart the pilot.

`run.lock` covers the whole rental. At most one purchase is admitted per UTC
six-hour window. Recovery only closes, settles, and verifies deletion of the
existing job; it cannot start a replacement. Its verdict is `recovered`, not
`success`. SIGTERM/SIGINT attempt bounded reconciliation; SIGKILL leaves durable
state for the next invocation. An unwritable/full disk prevents dispatch.
The supervisor uses `--recover-only` for this path, including when scheduling is
disabled. If the pending record was already resolved, recovery refuses a new
purchase. A missing or corrupt supervisor admission journal cannot renew a
completed one-shot authorization; its separate recovery clock still permits
cleanup without replacing the damaged admission history.

The buyer must be dedicated: unrelated spending, deposits, or withdrawals
fail exact balance reconciliation. Prefunding is separately authorized. An
optional `deposit_signature` only claims that explicit, already-broadcast
transfer. Unused lease escrow is an internal credit, not another on-chain refund.

## Read-only evidence adapter

The trusted adapter is an argv executable with protected read-only provider
credentials. It receives JSON on stdin and emits JSON on stdout. It must never
create, delete, close, or fund resources. Mutations use the normal
buyer/coordinator/broker path.

Requests have `version:1`, `operation`, `run_id`, and `coordinator_url`.
Access/cleanup also carry `job_id`, the captured `provider_instance_id` if known,
and `lease_view`. Responses repeat version, operation, and exact job ID. Access
and cleanup require a fresh `observed_at_unix` within 90 seconds.

Preflight response:

```json
{"version":1,"operation":"preflight","host_key_binding":"authenticated","cleanup":"provider_readback","provider_limits_checked":true,"provider_reservation_micro_usd":350000}
```

`provider_limits_checked` means admission configuration and current affordable
capacity were checked. It does not promise a provider cleanup SLA or invoice cap.

Access response:

```json
{"version":1,"operation":"access","job_id":"EXACT-UUID","observed_at_unix":1790000000,"provider_instance_id":"vast:123456","binding":{"verified":true,"source":"provider_authenticated","job_id":"EXACT-UUID"},"ssh":{"host":"ssh.example.com","port":2201,"user":"root","public_key":"ssh-ed25519 BASE64"}}
```

The adapter authenticates the exact job, instance, lifecycle, and host public
key before reporting `verified:true`. Independently verified
`operator_signed_ready` is also supported. Keyscan, TOFU, and keys learned from
the SSH session itself are insufficient. The runner matches host/user/port to
the signed lease view, pins the key in a private known-hosts file, and requires
strict SSH checking without global SSH configuration or password fallback.
Both `ssh user@host -p N` and the provider's `ssh -pN user@host` forms are parsed;
extra arguments are rejected. GPU model and usable-memory threshold are
explicit. Marketed VRAM size is not treated as usable `nvidia-smi` memory.

Cleanup response:

```json
{"version":1,"operation":"cleanup","job_id":"EXACT-UUID","observed_at_unix":1790000010,"provider_instance_id":"vast:123456","binding_verified":true,"source":"provider_readback","exists":false}
```

Cleanup requires authenticated provider state and the exact job-to-instance
mapping even when readiness failed. An unknown mapping, missing permission,
incomplete list, stopped instance, or unavailable provider is not deletion.
Optional `provider_cost_verified:true` plus integer `provider_cost_micro_usd`
may report attributable authenticated billing. A shared account balance
movement alone does not establish attribution. Billing proof can increase the
reservation and latch a breach; it never releases the provider allocation.

## Scheduling, public state, and alerts

Production uses the existing coordinator host and persistent disk under an
isolated Unix user. No new hosting subscription is required. The supervisor
runs `--once` at 00:00, 06:00, 12:00, and 18:00 UTC, and retries unresolved
recovery every five minutes. Run `--preflight` and an approved live check before
enabling the schedule. Preflight exits 2 and is never a passing rental verdict.

An independent supervisor/watchdog runs `--monitor` every 60 seconds, including
while a rental holds `run.lock`. It uses a separate lock, reads durable state,
publishes a heartbeat, and counts absent or unsuccessful six-hour windows.
Three windows trigger an alert. Success resets the streak; rapid retries and
preflight cannot erase missing windows. A durable success anchor survives
bounded record pruning. After downtime the next tick counts the missed windows;
it does not invent rental verdicts. To notify while this entire host is down,
an external monitor must watch the public feed's staleness. API freshness and
rental verdict are separate facts.

The alert adapter receives `{schema:1,type,state}` with `type` either
`rental_canary_missing_or_unsuccessful` or `rental_canary_recovered`, and returns
zero only after delivery. Failed alerts retry on later ticks; successful
failure alerts are limited to once per six-hour window. Explicit budget pauses
disable new admissions. `--monitor --disabled` publishes intentional shutdown.
The systemd examples are optional for compatible hosts; Render uses its own
supervisor. Source files alone install, fund, and activate nothing.

Private state lives in `/data/canary-private` (0700). Deployment separately
creates `/data/canary-public` (0755), writable by the canary user.
`public_state_path` is atomically replaced as 0644 and served by the coordinator
at `/rental-checks` after strict validation.

Schema 1 contains integer Unix timestamps, `enabled`, `cadence_seconds:21600`,
nullable `next_due_at_unix`, `pause_reason`, budget totals, monitor counters,
nullable `latest`, and at most 40 history rows (including latest). A row has
run/job IDs, timestamps, verdict, stage, safe reason, GPU model, charge/unused
escrow, and booleans for SSH identity, GPU probe, on-chain settlement, balance
reconciliation, provider cleanup, and recovery-only status. No host/IP, key,
raw error, signed URL, private path, provider credential, or SSH transcript is
published. Monetary entries are bounded to 128 and never pruned; private run
records are bounded to 1,024. Release evidence is copied before retention expiry.

## Verification

```sh
python3 -m unittest discover -s scripts/tests -p 'test_compute_rental_canary.py' -v
```

Tests use independent fake CLI, evidence, and SSH processes for current CLI
JSON shapes, fixed-ID dispatch, strict SSH, exact GPU model, durable budgets,
unknown outcomes, interrupted recovery, concurrency, refused spending, balance
and on-chain verification, missing windows, and public-data sanitization.
Production additionally requires installed CLI provenance, authenticated
provider acceptance, actual settlement and deletion, public stale-state checks,
and delivered alerts.

## Existing-service supervisor

The opt-in coordinator supervisor requires the existing coordinator journal at
`/data/mainnet`. It never relocates or reinterprets a journal. The mount root is
owned by root with traversal allowed; coordinator and canary homes are separate
0700 directories owned by their respective Unix users. Signing keys must remain
beneath the coordinator's private home. The canary receives only its own buyer,
SSH identity, read-only provider evidence credential and approved alert settings.

`COVENANT_CANARY_MODE` selects `preflight`, `once`, `scheduled` or `disabled`.
Omitting it preserves the legacy coordinator entrypoint. Configured `disabled`
prevents new admissions but continues recovery of unresolved rentals. The
supervisor durably records a schedule attempt before launching the runner;
recovery uses `--recover-only` and cannot become a fresh purchase.

The supervisor installs the anonymous immutable Compute 0.3.1 archive whose
SHA-256 is pinned in `COVENANT_CANARY_RELEASE_SHA256`. Promotion must first
verify its signature, provenance and installed contents. A root-only persistent
archive cache is rehashed at every boot so a GitHub outage cannot prevent
recovery. Invalid cached bytes fail closed. No build-tree binary substitutes for
the released client.
