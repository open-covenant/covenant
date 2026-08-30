# @covenant/coding-gateway

A Hermes `/v1` gateway that runs a coding task inside an ephemeral sandbox and
streams the run back through covenantd's audit chain.

covenantd's `HermesRunner` already speaks this protocol, so the daemon runtime is
unchanged: the `coder` agent declares `runtime = "hermes"`, and the daemon points
at this gateway via `HERMES_API_BASE_URL`.

## What it does per run

1. Provisions a sandbox (`SandboxProvider`) and tears it down on completion,
   timeout, or stop. Which provider you get depends on the environment; see
   [Sandbox providers](#sandbox-providers).
2. Drives a coding backend (`CodingBackend`) chosen by `CODER_BACKEND`:
   `anthropic`, `openai`, or `usepod`. The backend gets `read_file`,
   `write_file`, `edit_file`, and `bash`, all bound to that sandbox.
3. Maps backend events onto Hermes SSE frames. The `tool.*` and `approval.*`
   frames are what the daemon folds into audit; `message`, `reasoning`, and
   `file.written` drive the live UI.

## Run it

```sh
pnpm install
pnpm build   # tsc, src/ -> dist/
pnpm start   # node dist/main.js
```

`dist/` is a build artifact and is not kept in step with `src/`, so build before
every start or you will run stale code. `PORT` selects the listen port and
defaults to `8642` (`GATEWAY_PORT` is accepted as an alias). A rejected
environment value prints one line to stderr and exits 1, with no stack trace.

The smallest working setup is one model credential and a port:

```sh
export ANTHROPIC_API_KEY=sk-ant-...   # or CODER_BACKEND=openai + OPENAI_API_KEY
export PORT=47920
pnpm build && pnpm start
```

That boots on the `local` sandbox provider, with GPU workspaces off and no
authentication on the run API. Read [Sandbox providers](#sandbox-providers) and
[Exposure and authentication](#exposure-and-authentication) before you point
anything at it. To turn on GPU workspaces, add the control-plane credential and
restart:

```sh
export COMPUTE_API_TOKEN=...
export COMPUTE_API_URL=https://compute.opencovenant.org
```

The boot line reports the model, the sandbox provider, and whether GPU
workspaces are on, and `GET /v1/capabilities` reports the same in JSON:

```
coding-gateway listening on :47920 (model=claude-sonnet-4-6, effort=low, sandbox=local, compute=on ($0.20/run, 1 launch max, 1800s max, compute.opencovenant.org))
```

## HTTP surface

| Method | Path                   | Auth   | Purpose                                                        |
| ------ | ---------------------- | ------ | -------------------------------------------------------------- |
| GET    | `/healthz`             | none   | liveness: process is up, backend and sandbox provider ids, whether run and ledger storage is writable. Does not call any provider. |
| GET    | `/readyz`              | bearer | cached dependency evidence (model, balance, sandbox, tariff, compute) and the storage state. 200 when every required dependency is healthy, 503 otherwise. Compute is reported but never blocking: renting a GPU is optional, so a control-plane outage leaves `gpu_workspace` calls failing while the gateway keeps accepting runs. |
| POST   | `/v1/runs`             | bearer | start a run (`{input, session_id, max_cost_usd}` → `{run_id}`) |
| GET    | `/v1/runs/{id}`        | bearer | poll status / output                                           |
| GET    | `/v1/runs/{id}/events` | bearer | SSE event stream                                               |
| POST   | `/v1/runs/{id}/stop`   | bearer | cancel + tear down                                             |
| GET    | `/v1/capabilities`     | bearer | advertised features the daemon gates on, the effective GPU bounds, and the active sandbox provider |
| GET    | `/v1/budget`           | bearer | live spend snapshot                                            |

"bearer" means `Authorization: Bearer $CODER_AUTH_TOKEN` **when
`CODER_AUTH_TOKEN` is set**. When it is unset, none of these paths check
anything; see [Exposure and authentication](#exposure-and-authentication).

See `src/types.ts` for the full contract and the `CodingBackend` /
`SandboxProvider` interfaces.

## Sandbox providers

`E2B_API_KEY` decides which provider a run gets, and the two are not
interchangeable.

**`e2b`** (`E2B_API_KEY` set). An ephemeral Firecracker microVM: no host
secrets, an egress allowlist, cpu/memory/disk/wall caps, and a self-destruct
deadline at the provider. This is the isolation boundary every other claim in
this document assumes, and production boots only on it.

**`local`** (`E2B_API_KEY` unset). A scratch directory plus `child_process`
exec **on the gateway host**, implemented in `src/sandbox/local.ts`. It enforces
none of the above: no egress allowlist, no cpu/memory/disk caps, no secret
stripping. The model holds a `bash` tool, so anything the gateway's own user can
do, a run can do. It exists so the gateway is runnable on a laptop without an
E2B account. Do not run it anywhere the run API is reachable by anyone you would
not hand a shell.

The active provider appears in the boot line, in `GET /healthz`, and in
`GET /v1/capabilities` as `sandbox.provider`. Repository runs on the `local`
provider are refused unless `ALLOW_LOCAL_REPOSITORY_RUNS=1`.

## Exposure and authentication

`POST /v1/runs` starts a real, paid run. With `CODER_AUTH_TOKEN` unset there is
no authentication on it, so any caller that can open a socket to the port can
spend the daily cap. The gateway logs a warning at boot when the token is unset.

Deploy it as a private service. In the Blueprint it is a Render `pserv` with no
public URL, reachable only as `covenant-coding-gateway:10000` from the daemon.
If you expose it any other way, set `CODER_AUTH_TOKEN` (production refuses to
boot without one of at least 32 characters) and keep `/healthz` as the only
unauthenticated path.

## Operator controls

Admission runs through a USD spend ledger with hard daily and monthly caps and a
global concurrency cap. Every request supplies `max_cost_usd`, and the ledger
reserves that exact all-in cap before a sandbox starts. The gateway holds back
the maximum sandbox charge first, gives the model provider what is left, and
derives an output-token limit for each paid turn from that remainder and the
configured per-million-token price ceilings.

The installed E2B SDK exposes no authoritative billing receipt, so every
attempted sandbox create is charged for the full wall-clock reservation. The
sandbox figure in `/v1/budget` is a conservative accounting charge, not a claim
about the provider invoice.

| Env var                              | Default                   | Meaning                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------ | ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `CODER_DAILY_USD`                    | `6`                       | Hard daily spend cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `CODER_MONTHLY_USD`                  | `200`                     | Hard monthly spend cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `CODER_PER_RUN_USD_MAX`              | `2`                       | Per-run reservation ceiling.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `CODER_MAX_CONCURRENT`               | `2`                       | Concurrent run cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `CODER_IP_MAX_PER_IP`                | `1`                       | Per-IP in-flight admission cap. Stops a single anonymous client from occupying every concurrency slot. Set to `0` to disable (only safe behind an upstream rate limiter).                                                                                                                                                                                                                                                                                                                                                                      |
| `CODER_IP_REFILL_MS`                 | `60000`                   | Minimum delay between one IP's release and its next admission. Rate-limits a rapid-cycle client that drains the daily cap with cheap no-op runs.                                                                                                                                                                                                                                                                                                                                                                                               |
| `CODER_WALL_MS`                      | `600000`                  | Per-run wall-clock ceiling. The gateway aborts at this deadline and passes the same value to the sandbox as its self-destruct backstop, so a crashed gateway still has a hard end to leftover microVM spend. The deadline is retained in pending ledger records as audit context; restart accounting still charges every unresolved reservation at its full maximum.                                                                                                                                                                           |
| `USEPOD_BASE_URL`                    | `https://api.usepod.ai`   | HTTPS origin used to construct the documented token-in-path proxy URL. Production rejects embedded credentials and pre-tokenized paths.                                                                                                                                                                                                                                                                                                                                                                                                        |
| `USEPOD_INPUT_USD_PER_MILLION`       | `0.2`                     | Decimal input-price estimate for reporting. Production requires an explicit value at least as high as the request ceiling and rejects exponent notation, non-finite values, zero, and more than six decimal places.                                                                                                                                                                                                                                                                                                                            |
| `USEPOD_OUTPUT_USD_PER_MILLION`      | `0.4`                     | Decimal output-price estimate with the same validation. Actual gateway accounting uses the integer request ceilings below for every model.                                                                                                                                                                                                                                                                                                                                                                                                     |
| `USEPOD_MAX_INPUT_PRICE_MICROUNITS`  | `200000`                  | Per-million input-token price ceiling sent on every UsePod request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `USEPOD_MAX_OUTPUT_PRICE_MICROUNITS` | `400000`                  | Per-million output-token price ceiling sent on every UsePod request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `USEPOD_MIN_BALANCE`                 | `1`                       | Intake floor in whole USDC microunits (`4000000` = 4 USDC). Production requires an explicit value. Readiness checks the documented, non-billable token balance endpoint and requires its unique JSON value to match a unique `X-Balance-Remaining` header before comparing it with this floor.                                                                                                                                                                                                                                                 |
| `E2B_TEMPLATE_ID`                    | _(none)_                  | Immutable E2B template ID. Production rejects the mutable `E2B_TEMPLATE` alias. Every create reads E2B's returned template ID, CPU count, and memory and kills the sandbox before any model call if they differ from the configured identity.                                                                                                                                                                                                                                                                                                  |
| `E2B_EXPECTED_CPU_COUNT`             | `4` in development        | Expected vCPU count returned by E2B. Must be explicit in production.                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `E2B_EXPECTED_MEMORY_MB`             | `4096` in development     | Expected memory in MiB returned by E2B. Must be explicit in production.                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `E2B_EGRESS_ALLOW`                   | deny all                  | Immutable comma-separated sandbox policy parsed once at boot. Only the reviewed GitHub, npm, Yarn, PyPI, and crates.io distribution hosts are accepted; unknown, duplicate, or malformed hosts fail boot. Every sandbox run declares an explicit subset, and E2B denies all other outbound traffic. Provider control-plane calls originate from the gateway and are not added to the untrusted sandbox policy.                                                                                                                                 |
| `CODER_E2B_WORST_CASE_USD_PER_SEC`   | `0.0002` in development   | Pinned worst-case sandbox tariff, bounded to `(0, 0.01]`. Must be explicit in production and must cover the formula in the current tariff evidence. The gateway reserves `CODER_WALL_MS / 1000 × rate` before any provider call and retains it for every attempted E2B create.                                                                                                                                                                                                                                                                 |
| `CODER_E2B_TARIFF_REF`               | unverified in development | Fetchable HTTPS tariff-evidence JSON ending in `#sha256=<digest>`. Production readiness downloads at most 64 KiB, verifies the digest and schema, binds it to the exact template ID/CPU/memory/rate, fetches and hashes the official E2B source rate card, recomputes the resource-price formula, and rejects evidence older than or valid for more than seven days.                                                                                                                                                                           |
| `TRUSTED_PROXY_HOPS`                 | `0`                       | Trust the right-most N entries of `X-Forwarded-For` as proxy hops the operator controls; everything left is treated as client-supplied. Picking too large lets a client rotate IPs via the header. Set it to the exact number of trusted proxies between the gateway and the public internet (1 for a single Cloudflare/Fly/Render edge; 2 for an edge plus an internal load balancer). Default `0` uses the socket peer, which is safe for any deployment but collapses every visitor behind shared NAT or a single edge to one address. |
| `LEDGER_PATH`                        | _(none)_                  | If set, committed spend and in-flight reservations both persist to this file, so caps survive a restart. It must point at storage that outlives the container (not `tmpfs`, and not a volume that resets on reboot), or the cap silently restarts at $0.                                                                                                                                                                                                                                                                                                           |
| `CODER_EXEMPT_IPS`                   | _(none)_                  | Comma-separated list of IPs that bypass only the per-IP bucket. Daily and monthly USD caps, the kill switch, concurrency, sandbox reservation, and observability always apply. Intended for trusted operational probes that must not contend with a public per-IP bucket. The address format must match exactly what `sourceIp` resolves to under the current `TRUSTED_PROXY_HOPS` setting.                                                                                                                                                    |

GPU workspaces have their own controls; see [GPU workspaces](#gpu-workspaces).

`GET /v1/budget` returns the live snapshot: `dailyUsd`, `monthlyUsd`,
`reserved`, `active`, `killed`, the configured caps, `outcomes` counters
(`completed`, `failed`, `cancelled`), and an `ipBucket` block with the live
per-IP gate state (`active`, `inflight`, `rejected`, configured `maxPerIp` /
`refillMs`) so operators can see abuse volume from one snapshot. Its
`accounting.sandbox` block labels the method as `pinned-worst-case-tariff`,
publishes the rate, evidence reference, exact sandbox identity and maximum
per-run charge, and reports that no authoritative E2B billing receipt is
available.

**Kill-switch.** Sending `SIGUSR1` to the gateway process (`kill -USR1 <pid>`)
refuses every new reservation and aborts every in-flight run's
`AbortController`, tearing down sandboxes so spend stops immediately. The
switch is idempotent and has no HTTP surface, so there is no auth path to get
wrong. Its state persists across restarts and still blocks exempt IPs. There is
no runtime reset endpoint. To reset it, an operator must take the service
offline, back up and reconcile the provider account against the ledger, set only
the ledger document's `killed` field to `false`, preserve its spend counters and
pending entries, and then restart the service. Deleting or replacing the ledger
is not a valid reset because it would erase cap history.

For a clean shutdown, send `SIGUSR1` first and wait for `GET /v1/budget` to
report `active: 0` before sending `SIGTERM`. Each in-flight run still has to
finish its `sandbox.destroy()` round-trip after the abort lands; `SIGTERM`
during that window can orphan a microVM until its own wall-clock budget
self-destructs at the provider.

**Crash recovery.** A reservation appended to `LEDGER_PATH` at admission time
carries the caller's exact cap. On boot the gateway charges every surviving
reservation at that full amount and records a failed outcome before accepting
new work. This remains conservative even when the process cannot prove whether
the provider or sandbox stopped before the crash. A normal terminal commit
removes its reservation, so the file stays proportional to live runs rather
than lifetime runs.

## GPU workspaces

A run can rent a dedicated GPU (CUDA + Jupyter) on the Covenant compute market
through a `gpu_workspace` tool. The feature is off unless `COMPUTE_API_TOKEN` is
set: with no token the agent's tool list is exactly `read_file`, `write_file`,
`edit_file`, `bash`. `GET /v1/capabilities` reports whether it is on and the
bounds in force.

The gateway enforces the bounds and holds the control-plane token, which never
enters the sandbox. The sandbox's egress allowlist does not include the
workspace either, so the agent books a GPU for the person who asked and cannot
use it itself.

| Env var                     | Default                             | Meaning                                                                                                                                                                    |
| --------------------------- | ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `COMPUTE_API_TOKEN`         | _(none)_                            | Control-plane bearer token. Unset leaves `gpu_workspace` out of the model's tool list entirely.                                                                              |
| `COMPUTE_API_URL`           | `https://compute.opencovenant.org`  | Control-plane origin, including the scheme. `https` is required; `http` is accepted only for `127.0.0.1`, `::1`, and `localhost`, because the token is sent on every request. |
| `COMPUTE_MAX_USDC_MICROS`   | `200000`                            | Whole-run GPU budget in micro-USDC (`200000` = $0.20). Accepted range `1` to `10000000`, the beta token's own $10 cap.                                                       |
| `COMPUTE_MAX_DURATION_SECS` | `1800`                              | Longest booking one run may request. Accepted range `60` to `21600`.                                                                                                        |
| `COMPUTE_MAX_LAUNCHES`      | `1`                                 | Most workspaces one run may launch. Accepted range `1` to `20`.                                                                                                             |

**Boot-time validation is not request-time clamping.** Every value in the table
is validated when the process starts: outside its accepted range, the gateway
prints the range and exits 1. What the *model* asks for is a separate thing. A
`duration_secs` above `COMPUTE_MAX_DURATION_SECS` is clamped down to it, one
below 60 seconds is refused, and a launch that names no duration books 1800
seconds (or `COMPUTE_MAX_DURATION_SECS`, whichever is lower).

**The three bounds are one budget.** A launch books the cheapest online offer
and commits its full booking maximum, `ceil(rate_per_hour * duration / 3600)`,
against `COMPUTE_MAX_USDC_MICROS`. Cancelling returns neither the launch nor the
committed budget. So the launch count you can actually reach is

```
COMPUTE_MAX_LAUNCHES, but no more than
COMPUTE_MAX_USDC_MICROS / (cheapest_rate_per_hour * COMPUTE_MAX_DURATION_SECS / 3600)
```

The defaults are sized against the live market. At the cheapest offers seen when
they were set, around 380000 micro-USDC per GPU-hour, one 1800-second booking
commits roughly 190000. So $0.20 funds exactly one launch, with headroom up to
400000 micro-USDC per GPU-hour. Raising `COMPUTE_MAX_LAUNCHES` alone changes
nothing: raise the budget with it, or shorten the booking window. When no offer
fits what is left, the launch is refused with the remaining budget and the model
is told to try a shorter `duration_secs`.

**GPU spend counts against the run.** Committed GPU spend is added to the run's
`costUsd` on both the completed and failed paths, so it lands in the same daily
and monthly ledger as model and sandbox spend and counts against the caller's
`max_cost_usd`. A run whose total exceeds its reservation engages the kill
switch, so size `max_cost_usd` to cover the GPU budget as well as the model
budget, or lower `COMPUTE_MAX_USDC_MICROS`.

**Teardown at run end is best effort.** When a run reaches any terminal state
(completed, failed, aborted at `CODER_WALL_MS`, stopped through
`POST /v1/runs/{id}/stop`, or aborted by the kill switch), the gateway cancels
every workspace that run launched and did not already cancel, with a 5-second
budget per cancel. Confirmed cancellations are named in the run's output. A
workspace whose cancel does not confirm stays uncancelled and bills until its
own booking deadline, at most `COMPUTE_MAX_DURATION_SECS` from launch. The
gateway logs the job id and stops trying when the run ends. A gateway that dies
outright cancels nothing, and the booking deadline is the only backstop. The
control plane's own job list is the authority on what is still running.

**Readiness reports the control plane without gating on it.** With the feature
on, `/readyz` carries a `compute` dependency: a non-billable read of the control
plane's offer list with the configured token, on the same cached refresh
interval as the other probes. A wrong token or an unreachable origin shows up in
`dependencies.compute` and in `failed`, but the gateway stays ready and keeps
accepting runs, because renting a GPU is optional. A `gpu_workspace` call made
while the control plane is down fails on its own and reports why.

The `tool.started` frame for a `gpu_workspace` call carries the action and its
outcome (job id, offer id, and booked maximum on launch; charged and refunded on
cancel), which is what the daemon folds into audit. It never carries
`access_url`: that is a live credential for the workspace, and it belongs only
in the run's final answer.

## Production contract

Production boots only with the UsePod backend, one pinned model, explicit price
ceilings, a funded token, an immutable E2B template identity, current hashed
tariff evidence, restricted egress, and durable ledger and run-receipt paths.

Readiness uses only non-billable control-plane checks: UsePod's model catalog and
token-balance endpoint, E2B's sandbox-list API, the public tariff evidence, and,
when GPU workspaces are on, the compute control plane's offer list. The balance
response must contain one safe-integer `usdc_balance` in USDC microunits, and
when `X-Balance-Remaining` is present it must match exactly. Malformed,
duplicate, conflicting, or below-floor evidence closes intake. Paid model and
sandbox probes are forbidden, because repeated readiness polling would spend
outside the job ledger. Run admission fails closed while cached readiness
evidence is unhealthy.

Each validated paid turn persists non-secret route data, token usage, price
ceilings, and an accounted cost before the next turn begins. The accounted cost
is the greater of the ceiling-derived usage cost and any provider-reported cost,
so a missing provider cost report does not stop a valid multi-turn run. A missing
or mismatched receipt retains the full reservation. A receipt above the
reservation stays visible in the ledger, permanently engages the kill switch, and
blocks new admission until an operator reconciles the provider account against
the ledger.

The balance contract comes from UsePod's
[documented credit-verification endpoint](https://docs.usepod.ai/api/deposit-on-chain/#verifying-the-credit)
and its guarantee that every proxied response carries the balance header. Before
opening production intake, capture a `/readyz` response with
`dependencies.balance.ok: true` against the dedicated funded token. Production
requires `CODER_AUTH_TOKEN`, so that request carries the bearer. Unit tests
use protocol fixtures and are not evidence that a live provider response still
matches the documented contract.

The tariff evidence document has this exact schema:

```json
{
  "schema": "mizuki.e2b-tariff.v1",
  "provider": "e2b",
  "effectiveAt": "2026-08-23T20:32:23Z",
  "validUntil": "2026-08-29T20:32:23Z",
  "sourceUrl": "https://e2b.dev/pricing",
  "sourceSha256": "28e0e81c35b2d6e8def4bab24d105e5b39d31330c39be20f5411b51df664bbc7",
  "templateId": "aaj2iho3gnyf5fcvln83",
  "cpuCount": 4,
  "memoryMb": 4096,
  "cpuUsdPerCoreSecond": 0.000014,
  "memoryUsdPerGibSecond": 0.0000045,
  "fixedUsdPerSecond": 0,
  "safetyMultiplier": 2,
  "worstCaseUsdPerSecond": 0.0002
}
```

The current reviewed asset is
[`infra/mizuki/evidence/e2b-tariff-2026-08-23.json`](../../infra/mizuki/evidence/e2b-tariff-2026-08-23.json).
The Blueprint publishes its exact SHA-256 in `CODER_E2B_TARIFF_REF`. The rates
are an operator-reviewed transcription bound to the official source bytes, not
a provider-authenticated invoice. Refresh and content-address a new asset before
`validUntil`; readiness will otherwise close intake.
