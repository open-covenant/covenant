# Covenant Compute control plane

This binary serves the authenticated desktop API and uses SQLite as the
authority for job ownership, idempotency, immutable quotes, and spend
reservations.

## Build and run

From `agent-os/`:

```bash
cargo build -p covenant-compute-control --locked
```

That builds only this crate and its dependencies. On a warm target directory it
takes a few seconds; a cold full build takes roughly forty seconds. The binary
lands at `target/debug/covenant-compute-control`.

A minimal run, with the API key read from a file. The database directory has to
exist; the process creates the file, not the directory:

```bash
mkdir -p "$HOME/.local/share/covenant"

COVENANT_COMPUTE_BIND=127.0.0.1:8787 \
COVENANT_COMPUTE_DATABASE_PATH=$HOME/.local/share/covenant/compute.sqlite3 \
COVENANT_COMPUTE_PROVIDER=vast \
COVENANT_COMPUTE_BETA_TOKENS_JSON='[{"owner":"beta-user","token":"replace-with-a-random-token-of-16-plus-characters","spend_cap_usdc_micros":5000000}]' \
COVENANT_VAST_API_KEY_FILE=$HOME/.config/covenant/covenant-vast-api-key \
  ./target/debug/covenant-compute-control
```

Startup logs the Vast search constraints in force, probes the market once, and
reports how many offers survived each filter:

```text
INFO Vast offer search constraints api_url=https://console.vast.ai/api/v0/
     gpu_models=L40S,L40,RTX 6000Ada,RTX A6000,A40,A100 PCIE,A100 SXM4
     min_gpu_memory_mib=40000 max_hourly_micros=1000000
     max_inet_cost_micros=50000 disk_gb=16
INFO compute offer search completed provider_offers=27 dropped_bandwidth_cost=0
     dropped_host_evidence=0 dropped_gpu_class=0 dropped_price_ceiling=10
     admitted=17
INFO compute offer probe completed offers=17
INFO compute control plane listening address=127.0.0.1:8787 provider=vast
```

Every offer query logs the same counters, and drops to `WARN` with the
configured ceilings attached when nothing is admitted. If `provider_offers` is
zero the search matched nothing at Vast, so the constraint to move is one of
the search inputs printed at startup.

Startup failures print the message and its causes and exit non-zero, for
example `could not read the Vast credential file /path/to/key` followed by
`caused by: No such file or directory (os error 2)`.

Every transition that moves money is logged at `INFO` against the job id: the
first running observation, the deadline, a cancellation request, the confirmed
provider teardown, and the settled totals. Access URLs and workspace tokens are
never logged.

```text
INFO compute job is running and billing has started
     job_id=0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77 provider_job_id=27260661
     duration_secs=1800
INFO compute job cancellation requested job_id=0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77
INFO compute provider teardown confirmed
     job_id=0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77 provider_job_id=27260661
INFO compute job settled job_id=0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77
     provider_job_id=27260661 status="cancelled" runtime_secs=25
     charged_usdc_micros=2346 refunded_usdc_micros=166543
     provisioning_secs=144 provisioning_usdc_micros=13512
     commitment=vast:instance:27260661:destroyed
```

## Configuration

Required:

| Variable | Effect |
| --- | --- |
| `COVENANT_COMPUTE_DATABASE_PATH` | SQLite file holding jobs, quotes, and reservations. An in-memory path is refused. |
| `COVENANT_COMPUTE_PROVIDER` | Must be `vast`. Any other value is refused at startup. |
| `COVENANT_COMPUTE_BETA_TOKENS_JSON` | JSON array of `{"owner","token","spend_cap_usdc_micros"}`. Tokens are at least 16 characters; owners and tokens must be unique. |
| `COVENANT_VAST_API_KEY_FILE` or `COVENANT_VAST_API_KEY` | Vast API credential, read from a file or supplied inline. The file path wins when both are set. The deploy blueprint uses the inline form. |

Optional, with the shipped defaults:

| Variable | Default | Effect |
| --- | --- | --- |
| `COVENANT_COMPUTE_BIND` | `127.0.0.1:8787` | Listen address. |
| `COVENANT_COMPUTE_PROVISIONING_TIMEOUT_SECS` | `600` | How long a job may sit before its first running observation. Past it the job is cancelled and fully refunded. Accepts 1 to 86400. |
| `COVENANT_VAST_API_URL` | `https://console.vast.ai/api/v0/` | Vast API root. HTTPS only outside loopback, no credentials, no query, trailing slash required. |
| `COVENANT_VAST_MAX_HOURLY_MICROS` | `1000000` | Highest hourly price, in USD micros, an offer may carry. $1.00/hr matches the 30-minute, $0.50 allowance the catalog ships and clears the 40 GB class, which traded between $0.34 and $1.00 per hour when this default was set. Accepts 1 to 10000000. |
| `COVENANT_VAST_GPU_MODELS` | `L40S,L40,RTX 6000Ada,RTX A6000,A40,A100 PCIE,A100 SXM4` | Comma-separated Vast `gpu_name` values. Names must match Vast exactly; a name no host uses simply never matches. |
| `COVENANT_VAST_MIN_GPU_MEMORY_MIB` | `40000` | Minimum GPU memory. Set to keep the admitted fleet in one class; the workspace app itself only requires 16384. Accepts 1024 to 1048576. |
| `COVENANT_VAST_MAX_INET_COST_MICROS` | `50000` | Highest per-GB transfer price, in USD micros, a host may charge. Accepts 0 to 1000000. |
| `COVENANT_VAST_DISK_GB` | `16` | Disk requested and priced for each instance. Accepts 16 to 2048. |

Raising `COVENANT_VAST_MAX_HOURLY_MICROS` widens the admitted fleet and raises
the maximum a launch can cost. It is the first ceiling to check when
`/v1/offers` comes back empty.

Run exactly one control-plane process for a database and provider namespace.
Launch and cancellation coordination is process-local; multi-replica leader
election and distributed allocation locking are not implemented in this alpha.

## HTTP API

`GET /healthz` is open. Every `/v1` route requires
`Authorization: Bearer <token>` from `COVENANT_COMPUTE_BETA_TOKENS_JSON`.
Configured tokens are hashed in memory and are never written to SQLite or logs.

| Route | Purpose |
| --- | --- |
| `GET /v1/apps` | The released catalog. |
| `GET /v1/offers` | Live offers that clear every configured constraint. |
| `POST /v1/jobs` | Launch. Requires an `Idempotency-Key` header. |
| `GET /v1/jobs` | This owner's jobs, newest first, capped at 100. |
| `GET /v1/jobs/{id}` | One job, refreshed against the provider. |
| `DELETE /v1/jobs/{id}` | Cancel and settle. |

`{id}` is the UUID returned by the launch. Anything else is refused with
`invalid_job_id`, and a job belonging to another owner is `job_not_found`.

### Launching

A launch plan is checked against the catalog and against a live offer, so build
it from the two read endpoints rather than from a transcribed literal.

1. `GET /v1/apps`, and take the app object whose `availability` is `available`.
   Send it back unchanged: every field is compared byte for byte.
2. `GET /v1/offers`, and take one offer object. Send it back unchanged too.
3. Choose `duration_secs`, at least 300 and at most the app's
   `max_duration_secs`. Allocating a GPU and starting the workspace takes
   minutes, so a shorter session would expire before the workspace answers.
4. Set `maximum_usdc_micros` to
   `ceil(rate_usdc_micros_per_hour * duration_secs / 3600)`. A mismatch is
   refused, and the error names the value the control plane expected.

```bash
curl -sX POST http://127.0.0.1:8787/v1/jobs \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "idempotency-key: launch-2026-08-27-a" \
  -d '{
    "app": {
      "id": "gpu-workspace",
      "name": "GPU Workspace",
      "summary": "Open a bounded CUDA and Jupyter workspace on a dedicated GPU.",
      "kind": "workspace",
      "availability": "available",
      "image": "docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10",
      "min_vram_mib": 16384,
      "min_trust": "open",
      "default_duration_secs": 1800,
      "max_duration_secs": 21600,
      "default_max_usdc_micros": 500000
    },
    "offer": {
      "id": "vast:47876011:32049",
      "gpu": {"model": "A40", "vram_mib": 46068, "cuda_major": 12},
      "rate_usdc_micros_per_hour": 337778,
      "trust_class": "open",
      "online": true
    },
    "duration_secs": 1800,
    "maximum_usdc_micros": 168889
  }'
```

The response is the job:

```json
{
  "id": "0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77",
  "app_id": "gpu-workspace",
  "offer_id": "vast:47876011:32049",
  "status": "provisioning",
  "maximum_usdc_micros": 168889,
  "access_url": null,
  "error": null,
  "receipt": null
}
```

A launch that reaches the provider answers `provisioning`, then moves through
`running` and `stopping` to `completed`, `cancelled`, or `failed`. `funding`
means the reservation is held and the provider has not been reached yet: a
client sees it on `GET /v1/jobs` after a launch answered `provider_unavailable`,
until the next reconciliation pass retries the allocation. `access_url` carries
the workspace credential and is returned only by the response that obtained it;
it is never stored and never appears in `GET /v1/jobs`.

The `Idempotency-Key` header is mandatory. Repeating a key with the same plan
returns the same job; repeating it with a different plan is refused with
`idempotency_conflict`, whether or not the original offer still exists.

### Errors

Every error, including 404 and 405, uses one envelope:

```json
{"error": {"code": "missing_idempotency_key", "message": "Idempotency-Key is required"}}
```

| Status | Code |
| --- | --- |
| 400 | `missing_idempotency_key`, `invalid_idempotency_key`, `malformed_json`, `invalid_content_type`, `invalid_request_body`, `invalid_job_id` |
| 401 | `unauthorized` |
| 404 | `job_not_found`, `unknown_route` |
| 405 | `method_not_allowed` |
| 409 | `stale_offer`, `idempotency_conflict`, `spend_cap_exceeded`, `spend_cap_below_commitments` |
| 422 | `unknown_app`, `invalid_launch_plan`, `app_unavailable`, `invalid_duration`, `offer_offline`, `insufficient_gpu_memory`, `insufficient_trust`, `invalid_offer_rate`, `invalid_maximum_usdc_micros` |
| 500 | `internal_error` |
| 503 | `provider_unavailable` |

The 422 codes each name one field. `invalid_launch_plan` means the app object
does not match the catalog, which usually means the client is holding a stale
copy. `invalid_duration` and `invalid_maximum_usdc_micros` carry what the
control plane expected, for example
`duration_secs must be between 300 and 21600` and
`maximum_usdc_micros must be 168889 for this offer and duration`.

The three body codes separate three different fixes. `malformed_json` means the
bytes are not JSON. `invalid_content_type` means the request did not declare
`content-type: application/json`. `invalid_request_body` means the body parsed
but is not a launch plan, and its message names the field at fault, such as a
body that is missing the field `app.id`.

## Receipts and cost

A settled job carries a receipt:

```json
{
  "id": "vast-0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77",
  "job_id": "0f0c1d9e-2f2f-4a51-9a1f-6d1f5e2a2b77",
  "app_id": "gpu-workspace",
  "provider": "vast",
  "runtime_secs": 25,
  "provisioning_secs": 144,
  "provisioning_usdc_micros": 13512,
  "charged_usdc_micros": 2346,
  "refunded_usdc_micros": 166543,
  "commitment": "vast:instance:27260661:destroyed",
  "transaction": null
}
```

`runtime_secs` is billed from the first observation that the provider reports
the instance running with complete facts. The workspace can take a further
moment to answer after that. `provisioning_secs` reports the window before it,
from job creation to that first running observation, and is zero for a job that
never became ready. `provisioning_usdc_micros` prices that window at the same
hourly rate as the charge. The provider bills it, the operator absorbs it, and
the customer is not charged for it.

That window is large next to a short session. It is bounded by
`COVENANT_COMPUTE_PROVISIONING_TIMEOUT_SECS`, 600 seconds by default, so against
the 300-second minimum duration the absorbed cost can reach twice the charge,
and further above it when a session is cancelled early. The receipt above
absorbed 144 seconds against the 25 seconds it charged.

The allowance also does not cover transfer. `COVENANT_VAST_MAX_INET_COST_MICROS`
caps the per-GB price of an admitted host at $0.05 by default, and hosts in this
class no longer offer free transfer, so a zero ceiling admits nothing. A token
holder who moves a large amount of data can therefore create operator cost
outside the time-based allowance, bounded only by that per-GB ceiling.

Receipt usage comes from the provider's immutable offer and the control plane's
durable runtime boundary. It records what the control plane authorized and
settled. Nothing in it measures the machine independently. The quoted maximum
bounds beta-account usage and future escrow settlement; the Vast invoice is
separate. Vast exposes no per-instance billing
deadline, so the control plane requests deletion at the selected duration and
retries failures, and operator cost can continue until Vast confirms it.
Receipts stay evidence until Solana settlement replaces this boundary; only then
do they prove on-chain payment.

## Provider evidence

The Vast adapter requires returned offer evidence for host verification,
reliability, availability, direct-port capacity, architecture, and CUDA 12.4
compatibility before allocation. It prices the configured disk allocation and
refuses any offer whose per-byte upload or download charge is missing, invalid,
or above the configured ceiling. The API's `cuda_major` value is the major
component of Vast's returned maximum host CUDA compatibility, not a runtime
probe. Jupyter readiness and the exact port mapping can only be checked after
the provider creates the instance.
