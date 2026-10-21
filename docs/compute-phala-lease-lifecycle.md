# Phala CPU lease lifecycle

## Ephemeral no-KMS native integration

The separate `phala-cpu-ephemeral` executor serves only
`phala-cpu-ephemeral-dstack061`. It uses a new VM and disk per signed lease,
the independently verified `phala_ephemeral_dstack061` profile, and an outbound
SSH connection through the coordinator. Managed KMS remains disabled and the
served trust ceiling remains **`Open`**. The earlier
[isolated live acceptance](compute-ephemeral-phala-acceptance.md) qualifies the
pinned guest; its evidence remains separate from the native paid path.

Coordinator admission and the signed relay are deployed at source
`6598fe15663ad88fe256ad739ded703d05e1ad43`. Both bounded paid attempts passed
guest/buyer verification and native transfer, but failed provider-deletion
confirmation: the first exceeded its 60-second gate, and the October 21 retry
exceeded its fixed 300-second cutoff by 18.926 seconds. Both VMs were later
confirmed absent, with zero charge/payout and finalized full escrow returns.
The [native acceptance report](compute-native-phala-acceptance.md) preserves
each attempt separately. A local poll-only deletion repair is not deployed;
successful paid production qualification remains outstanding. The coordinator
capability remains enabled, both one-use allowances are consumed, and the test
node has been stopped. Node creation remains disabled by default. No third
attempt, standing capacity or replacement allowance is authorized.

Buyer preparation uses `lease prepare-phala-ephemeral` with the exact HTTPS
coordinator origin as `--relay-origin`; generation is `phala-ephemeral-{job-id}-1`.
The node derives the profile and workload from the signed envelope, verifies a
fresh Intel quote, and pins the verified boot app and instance before publishing
access. A second coordinator challenge is independently verified and saved.
The buyer re-verifies the complete evidence and requires the relay metadata to
match the original lease and cryptographically verified app and instance.

The coordinator requires `COVENANT_COMPUTE_HARDWARE_LEASE_PINS=1`,
`COVENANT_COMPUTE_EPHEMERAL_RELAY=1` and
`COVENANT_COMPUTE_EPHEMERAL_RELAY_ORIGIN` set to that exact origin. It retains
relay tombstones in `journal.relay.json` beside its main persistent journal.
Restart refuses to reconstruct an old session or reuse its credentials; the
operator retains its original provider cleanup obligation. Buyer and operator
requests use signed Covenant identities; no buyer bearer secret enters the VM.

The node additionally requires `COVENANT_PHALA_ALLOW_EPHEMERAL_CREATION=1`,
`COVENANT_PHALA_RELAY_ORIGIN`, and the independently selected production
`COVENANT_PHALA_EXPECTED_DEVICE_ID`. Placement, digest and the one-use creation
allowance remain explicit. `COVENANT_PHALA_KMS_ID` is not used by this executor.
Unresolved resources retain capacity; exhausting the allowance advertises zero
availability. The implementation does not automatically grant a new allowance.

Each lease permits one SSH stream. Open it before the original hardware
challenge's 120-second opening deadline; the saved verdict's longer lifetime
does not extend that deadline. A diagnostic probe consumes the stream. Native
workspace restore uses one SSH command with stdin, and `lease transfer-phala`
provides a fixed synthetic-file transfer with guest byte count and SHA-256.
Both require strict attested host-key checking. Source data is limited to
8 MiB, relay ciphertext to 16 MiB, and relay streams to 300 seconds; the native
operation also retains its shorter deadline. A workspace demanding a trust
class above `Open` is refused before download or decryption.

Buyer close and original signed expiry stop the meter. Close during VM startup
or evidence collection cancels admission and queues deletion of the original
resource. A successful ephemeral execution receipt requires durable confirmed
deletion: HTTP 204 alone is insufficient, and GET must confirm absence.
The deployed ephemeral envelope reserves up to 300 seconds for provider deletion and a
separate 60 seconds for result delivery in each new signed ephemeral envelope.
Guest expiry, access, allowance runtime and price ceiling remain unchanged.
The first observed close fixes a durable cleanup cutoff, capped by the original
result deadline minus its delivery reserve. Polling or restarting cannot renew
that cutoff; old closing rows without the new metadata remain cleanup-only.
Deletion must be observed and durably saved within that cutoff. An unresolved
delete produces a failed execution and refund while durable cleanup continues.
Cleanup latency cannot extend the paid meter. Receipt settlement, provider
absence, and final provider USD billing are separately retained evidence.

The local, undeployed deletion repair persists an unknown-outcome marker before
the first DELETE and acceptance after success, then uses GET-only reconciliation
for that original UUID. Prior attempts, lost acknowledgements and restarts do
not authorize another DELETE. An exact-owned provider response already showing
deletion in progress is also adopted as poll-only. A definite refusal remains
an unresolved obligation requiring operator reconciliation. No provider progress
status substitutes for confirmed absence or changes the original cutoff. The
live retry issued 101 DELETEs; their contribution to the delay is unproven.

## Managed-KMS diagnostic integration

The `phala-cpu` node executor connects Covenant's signed lease, provider
lifecycle, genuine hardware evidence and buyer SSH/workspace paths. It serves
only `phala-cpu-dstack061`, a CPU lease profile. The compiled trust ceiling is
**`Open`**. New managed-KMS creation is disabled by default and remains disabled
pending the [acceptance decision](compute-hardware-acceptance.md).

This is the implementation and operating contract. The updated guest image
passed real Docker SSH/storage tests and a fresh TDX-bound memory/swap check on
the existing diagnostic CVM on 2026-10-20; see the
[memory diagnostic record](compute-memory-diagnostic-evidence.md).
No new resource was created. Neither that check nor the earlier
[CPU diagnostic](compute-hardware-diagnostic-evidence.md) establishes acceptance
of the integrated paid flow or a production attested-rental release.

## Signed lifetime and workload

The buyer signs one job UUID, operator, generation, workload SHA-256 and
absolute expiry. Generation is `phala-{job-id}-1`. Expiry is the original
`issued_at_ms + duration_secs * 1000`, with duration between 1 and 3,600 seconds.
Acceptance delay, boot, readiness, recovery and evidence refresh cannot extend
it. For example, a 600-second signed window accepted 60 seconds after issuance
has at most 540 seconds left. Live metering stops at the signed expiry; previously
persisted settlement stamps and legacy lease behavior are unchanged.

The deterministic workload uses the reviewed production wrapper, a
digest-pinned guest image, buyer SSH public key, exact operator/buyer/job scope,
fixed gateway and signed expiry. The node must reproduce its exact hash before
provisioning. Matcher and node admission require the pinned operator and refuse
future-issued or expired requests; reoffering cannot substitute another operator.
The wrapper must explicitly encode `"swap_size": "0"` as a string. Missing,
numeric or nonzero values are rejected. The provider's normalized Compose
hash must still equal the buyer's exact signed workload hash before creation;
API normalization cannot silently change the swap policy.

The external SSH address is the attested instance ID plus
`-1022.dstack-pha-prod5.phala.network:443`, user `covenant`. The collector uses
instance port `18090`, mapped to container port `8090`; native dstack reserves
host port `8090`. TLS gateway identity and the guest's SSH host key are separate
checks, and both are bound into the hardware scope.

## Guest memory and runtime checks

The current source uses a 128-MiB `noswap` tmpfs for `/workspace`. SSH `/run`
and `/tmp` use 8-MiB and 16-MiB `noswap` tmpfs mounts. A new
`host-public-memory-v1` volume provides a 1-MiB `noswap` tmpfs at `/host-public`,
avoiding reuse of the earlier disk-backed public-key volume during an update.
The collector mounts it read-only; it contains the public host key and runtime
status. Private SSH keys stay in `/run`; the collector replay journal remains
durable. Workspace plaintext does not persist across recreation of its tmpfs.

The SSH service sets both `mem_limit` and `memswap_limit` to 768 MiB; the
collector sets both to 256 MiB. Both set core soft/hard limits to zero.
Runtime checks require no guest swap, cgroup v2 `memory.swap.max=0` and
`memory.swap.current=0`, the exact memory limits, actual tmpfs mount properties,
and a non-piped kernel core handler. Missing/ambiguous kernel evidence or
unsupported controls stop admission to SSH/collection; there is no fallback.

Before generating a host key, SSH verifies these controls. Its watchdog
continues checking, publishes short-lived status bound to the lease and public
key, and stops SSH while invalidating status on failure. The collector checks
its own controls and that status before and after guest information RPCs and
quote collection. Thus a blocking RPC cannot reuse an earlier safe status
after SSH has stopped. The diagnostic verified the actual SSH-side kernel
controls and collected a quote through the guarded collector. This establishes
the recorded runtime behavior; it does not establish provider-independent
memory or secret custody.

## Operator configuration

Keep existing node identity, coordinator, payment and journal configuration.
The additional settings are:

| Variable | Required value or behavior |
| --- | --- |
| `COVENANT_COMPUTE_NODE_EXECUTOR` | `phala-cpu` |
| `COVENANT_COMPUTE_NODE_MAX_IN_FLIGHT` | `1`; unresolved resources retain capacity |
| `COVENANT_COMPUTE_NODE_MODELS` | Omit for this executor's default, or set only `phala-cpu-dstack061` |
| `COVENANT_PHALA_API_KEY_FILE` | Existing owner-only regular file, at most 4 KiB; never place its contents in a command, log or review record |
| `COVENANT_PHALA_TEEPOD_ID` | Positive, explicitly selected live **legacy `Teepod.id`**, not the newer `node_id` |
| `COVENANT_PHALA_OS_IMAGE` | Exact reviewed production OS identifier |
| `COVENANT_PHALA_KMS_ID` | Exact reviewed KMS identifier |
| `COVENANT_PHALA_GUEST_IMAGE_DIGEST` | Qualified `sha256:` digest, matching the buyer's workload |
| `COVENANT_PHALA_ALLOW_MANAGED_KMS_CREATION` | `0` or unset for no creation; `1` only for a separately authorized finite synthetic pilot at `Open`, not production-trust qualification |
| `COVENANT_PHALA_CREATION_ALLOWANCE_FILE` | Reviewed JSON below, at most 16 KiB; required in addition to the creation switch |

Configure the coordinator with
`COVENANT_COMPUTE_HARDWARE_LEASE_PINS=1`. Do not also configure a nonempty
`COVENANT_COMPUTE_HARDWARE_WORKLOAD_SHA256`; that belongs to the separate legacy
diagnostic mode. Use the reviewed journal namespace for the selected mode.

Select current placement through the authenticated account. The adapter does
not choose an available Teepod automatically. It requests production
`prefer_dev=false`, 1 vCPU, 2,048 MiB and a 20-GB disk, and rejects a provision
response selecting another Teepod or KMS. It authenticates the live CPU catalog,
checks the exact instance type and rate against the allowance, and rechecks
that the rate is unchanged before create. A capacity failure is not permission
to substitute a different type, OS, placement or price.

### One-use creation allowance

The JSON object has exactly these fields; numeric descriptions below are
placeholders, not approved values:

| Field | Constraint |
| --- | --- |
| `schema_version` | `1` |
| `review_id` | Unique reviewed identifier; 1–128 ASCII letters/digits or `-_.` |
| `operator_pubkey_b58` | The configured operator identity |
| `instance_type` | Exact live selected type; same identifier syntax |
| `max_hourly_micro_usd` | Explicitly approved positive catalog-rate ceiling |
| `max_runtime_secs` | Approved duration, 1–3,600 |
| `max_new_resources` | Exactly `1` |
| `planning_reserve_micro_usd` | At least `ceil(max_hourly_micro_usd * max_runtime_secs / 3600)` |
| `valid_until_ms` | Finite reviewed expiry, no earlier than the signed guest expiry |

The durable planning row consumes the review ID even if provisioning is
abandoned or refused. Deletion does not restore that use or refund a planning
reserve. Another attempt requires a new explicit review; do not rewrite the
journal or reuse an ID. The reserve bounds admission planning, not the
provider's invoice: storage, ancillary charges or delayed deletion may exceed
it.

This object is **not** KMS policy acceptance. It contains no CA/app/workload
acceptance fields. Retain the separate, finite, scope-bound decision required
by the [KMS review](../deploy/compute-attested-cpu/kms-policy-review.md).
The KMS production assessment is **rejected**: managed prod5, the diagnostic
account's selected managed prod7 endpoint, and all 16 registered Base/Ethereum
nodes reported `os_image_verification=false` (18 of 18 assessed endpoints).
The two managed endpoints share a CA root; compute-host placement and KMS
endpoint selection are distinct.
Accepting the current managed service is limited to a finite synthetic pilot
at `Open`, not a production-trust alternative. Production requires provider
remediation with verified measured configuration/governance or a separately
qualified customer-controlled KMS deployment. Neither the creation switch nor
a valid allowance changes those facts or raises trust.

## Durable lifecycle and evidence

The guest's persistent replay journal uses a job-specific named volume.
Updating a diagnostic CVM for a new job must preserve the earlier journal and
select that new job's volume; never clear or reset an existing replay journal.

The private node journal is `<node-home>/phala-leases`; evidence is stored
under `<node-home>/phala-evidence/<job-id>`. Preserve both across restart.
Recovery runs before fresh admission checks, including when creation is
disabled. The existing user-created diagnostic CVM is not adopted into this
journal.

The node fsyncs a provision intent before allocating app configuration, then
persists the app ID, compose hash, OS, environment-encryption key, type, rate,
Teepod and KMS returned by provisioning. It fsyncs create intent before the
single create attempt. An uncertain create is reconciled using the originally
allocated app and unique generated name. An absent lookup after an unknown
create does not prove the request failed and does not authorize another create.

Readiness requires the expected provider state and collector identity:
instance, OS, measured workload, one SSH host key and typed TLS channel.
Access is published before the operator requests the coordinator challenge.
The collector quote request is recorded before nonce consumption; genuine
quote and bounded current Intel collateral are checked locally before the
coordinator independently verifies them. The evidence run retains original
raw responses, verification and submission results. Unknown quote/submission
outcomes are not silently retried with a new challenge or overwritten directory.
An instance or key change after readiness is not silently repinned.

Close, expiry, cancellation and restart cleanup operate on the original
journal-owned immutable CVM UUID. Cleanup uses resource identity metadata;
workload drift or an unavailable compose-detail endpoint must not prevent
deletion of an already owned resource. HTTP 204 means deletion initiated.
A subsequent authenticated GET 404 confirms absence. Unknown outcomes and
cleanup obligations remain durable; stopped application services alone do not
end provider billing. Shutdown reports unresolved resource obligations
explicitly; draining accepted jobs alone cannot establish clean provider
teardown.

## Buyer commands

These commands describe the entry path; they do not authorize spending.
Use the intended existing buyer identity/home. Prepare the operator, qualified
digest, public key, duration and approved native rate before starting the
120-second submission window.

```sh
covenant-compute lease prepare-phala \
  --job-id <UUID> \
  --operator <OPERATOR_PUBLIC_KEY> \
  --image-digest sha256:<QUALIFIED_DIGEST> \
  --duration-secs <APPROVED_SECONDS> \
  --rate <APPROVED_MICRO_USDC_PER_SECOND> \
  --ssh-key <BUYER_PUBLIC_KEY_FILE> \
  --output <NEW_PRIVATE_DIRECTORY>
```

Preparation is offline. It does not reserve budget, call an API, buy a lease or
create a CVM. It creates a new owner-only directory with `workload.json`
(exact compact bytes) and `originalsignedenvelope.json`, using the current
issuance time. Preserve both; reformatting the workload changes its hash.
The duration × rate ceiling must fit the configured per-call limit.

Submission requires the configured `COVENANT_COMPUTE_COORDINATOR_URL`,
`COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC` and existing durable budget. Optional
`COVENANT_COMPUTE_BUDGET_HOME` selects its location, and
`COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` supplies the per-call cap. These values
must come from the approved native Compute budget, separately from provider
USD exposure; this guide supplies no new limits.

```sh
covenant-compute lease submit-phala \
  --envelope <NEW_PRIVATE_DIRECTORY>/originalsignedenvelope.json
covenant-compute lease view <UUID>
```

`submit-phala` validates the original signature, buyer, lease pin, cap and time
before budget reconciliation/reservation, then revalidates before the HTTP
attempt. Future issuance, age over 120 seconds or original expiry is rejected.
It reserves through `DurableBudget` and submits once without automatic retry,
redirect, resigning or ID replacement. Every previously reserved job ID stays
non-reusable. An uncertain response retains the hold.

```sh
covenant-compute budget status
covenant-compute budget reconcile
covenant-compute lease view <UUID>
```

Use those reads/reconciliation with the original envelope after uncertainty.
Stale preparation is never renewed automatically. Reconcile any original
attempt before explicitly reviewing and preparing a different purchase.

After readiness and fresh evidence:

```sh
covenant-compute lease probe-phala \
  --envelope <NEW_PRIVATE_DIRECTORY>/originalsignedenvelope.json \
  --ssh-key <BUYER_PRIVATE_KEY_FILE>
covenant-compute workspace restore-to-lease <WORKSPACE_UUID> \
  --job-envelope <NEW_PRIVATE_DIRECTORY>/originalsignedenvelope.json \
  --ssh-key <BUYER_PRIVATE_KEY_FILE>
covenant-compute lease close <UUID>
```

The probe is explicitly diagnostic at `Open`. It reads current lease scope,
independently verifies hardware and probes the attested channel; it does not
purchase another lease or upload a workspace. Native TLS validates the exact
gateway hostname; OpenSSH checks only the attested guest key using isolated
configuration and a one-use local relay.

Restore first verifies the live lease/evidence, decrypts into a private local
location, then re-reads the scope and verifies again immediately before
upload. It honors the workspace's trust floor and refuses stronger floors at
`Open`. Live acceptance must use synthetic data only. A changed or expired
lease must not reuse an earlier grant, and a failed transfer is not an
instruction to overwrite or retry automatically.

Closing the Covenant lease and reconciling its USDC settlement are separate
from confirming provider deletion and USD charges. Retain both sets of evidence.
The earlier 0.75-USDC x402 allowance does not cover either new purchase here.
