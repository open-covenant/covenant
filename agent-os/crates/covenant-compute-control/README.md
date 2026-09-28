# covenant-compute-control

The buyer side of Covenant Compute: a bearer-token HTTP API that rents a
GPU workspace by the second. A customer presents a beta token, browses a
catalog of apps and the GPU offers that can run them, and launches a
bounded session that bills for the seconds it runs. The
[node](../covenant-compute-node/README.md) is the supply this sells; the
network-level map lives at
[docs/compute-network.md](../../../docs/compute-network.md).

It is a thin front end. The money, the escrow, the operator market and
the metered settlement all live in the coordinator's lease engine; the
control plane translates a launch into a signed lease and reads the
meter back out as a job. Two things live here that the engine does not:
the app catalog (a curated set of runnable images and their resource
floors) and the beta-token identity a customer authenticates with.

## The API

Every `/v1` route needs `Authorization: Bearer <token>`; `GET /healthz`
is the one exception, for a load balancer. Every error answers with the
same envelope, `{ "error": { "code", "message" } }`, so a client keys on
the stable `code` and shows the `message`.

| Route | What it does |
| --- | --- |
| `GET /healthz` | Liveness, no token. |
| `GET /v1/apps` | The apps this deployment will launch. |
| `GET /v1/offers` | The GPU supply on the market right now. |
| `POST /v1/plans` | Resolve a request into a concrete, priced plan. Moves no money. |
| `POST /v1/jobs` | Launch a plan. Needs an `Idempotency-Key` header. |
| `GET /v1/jobs` | The caller's jobs, without access credentials. |
| `GET /v1/jobs/:id` | One job, refreshed against the engine while it is live. |
| `DELETE /v1/jobs/:id` | End a live session and return its settled account. |

The path from nothing to a running workspace is three steps:

1. `GET /v1/apps` lists what is launchable, each with the window and
   budget defaults a client can present and the floors an offer must
   clear (`min_vram_mib`, `min_trust`). Only an app marked `available`
   carries a launch image; a `preview` entry is shown but cannot run.
2. `POST /v1/plans` turns a request into a plan. The body names the app,
   the window in seconds, the most the caller will spend, and optionally
   a minimum trust class. The control plane screens the live market for
   the offers that clear the app's GPU-memory and trust floors and the
   caller's budget, then prices the cheapest survivor at the same rate a
   launch will escrow, and returns that plan. Nothing is committed and no
   money moves, so the caller reviews the exact offer and ceiling first.
3. `POST /v1/jobs` commits a plan. The plan carries the app and offer
   whole, so the control plane re-checks that the plan still matches the
   released catalog and that the offer is still on the market, reserves
   the ceiling against the caller's spend cap, and opens the lease. An
   `Idempotency-Key` header makes the launch safe to retry: the same key
   returns the same job rather than opening a second machine.

A client that already knows the offer it wants can skip the plan step
and `POST /v1/jobs` a plan it built itself; the launch runs the same
checks either way. `POST /v1/plans` exists so a client does not have to
reproduce the market screening and pricing to get one right.

### Resolving a plan

`POST /v1/plans` is the one-call quote. Given a request, it either
returns a launchable plan or explains why the market could not meet it:

- `no_matching_offer` (409): nothing online clears the app's GPU memory
  and the requested trust class. The trust floor is the stronger of the
  app's own minimum and any the request names; a request can raise it,
  never drop below the app's.
- `over_budget` (409): an offer meets the requirements, but the cheapest
  match costs more than the request allowed for the window. The message
  names the micro-USDC the cheapest match needs, so the caller knows what
  to raise.
- `unknown_app`, `app_unavailable`, `invalid_duration` (422): the request
  itself is the problem, before the market is consulted.

Ties between equally priced offers break on the offer id, so the same
market resolves the same plan every time.

## Jobs

A launched session moves through `funding` (the spend is reserved, the
machine not yet reached), `provisioning`, `running`, and `stopping` to a
terminal `completed`, `cancelled`, or `failed`. `GET /v1/jobs/:id`
refreshes a live job against the engine on each read and returns a
terminal one from the record; `GET /v1/jobs` lists the caller's jobs
from the record without refreshing.

`access_url` carries the workspace credential and appears only on the
response that just obtained it: the launch, and a read of a running job.
It is never in a listing, and never once the session is stopping or
terminal. Reach the session while it runs, then close it.

`DELETE /v1/jobs/:id` ends the job. A job still `provisioning`, before any
operator has accepted it, cancels at once with the whole escrowed ceiling
refunded. A running session closes instead: the close is recorded and
carried out as the machine is released, so the job comes back `stopping`
and settles on the next read, reading as `completed` once the meter is
final (or `cancelled` if nothing was metered), its receipt filled in
then. Closing is safe to repeat, and a job already terminal comes back
untouched. The receipt reconciles against the lease it settles:
`charged_usdc_micros + refunded_usdc_micros` equals the escrowed ceiling,
and an early close bills the seconds served, not the whole window. A
session whose workload fails reads as `failed` and is refunded in full,
its receipt showing nothing charged, so every terminal job carries the
money it settled to. The customer is billed from the moment the machine
is reachable; the operator absorbs provisioning and teardown, so a slow
boot is never the renter's cost.

Spend is bounded twice. Each beta owner has a spend cap the control plane
reserves against at launch (`spend_cap_exceeded` when a launch would
exceed it), and the control plane's own buyer balance on the coordinator
is the ceiling on what all its owners can hold open at once.

> This control plane keeps its job index and spend accounting in memory,
> which is deliberate for the beta: a restart forgets the index while the
> coordinator keeps settling the leases themselves. Durable buyer-side
> bookkeeping is a later step.

## Configuration

The server reads its configuration from the environment at boot and
refuses to start with a required value missing, named. `serve` also
confirms the catalog and probes the market once at startup, so a
misconfigured catalog or an unreachable coordinator surfaces in the boot
log rather than at the first request.

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_BETA_TOKENS_JSON` | (required) | The beta roster: a JSON array of `{ "owner", "token", "spend_cap_usdc_micros" }`. Each token authenticates one owner and caps what it can hold open. |
| `COVENANT_COMPUTE_COORDINATOR_URL` | (required) | The coordinator whose lease engine this plane buys from. |
| `COVENANT_COMPUTE_BUYER_IDENTITY` | (required) | Path to the buyer identity that funds every lease. Created if absent; its coordinator balance bounds total open spend. |
| `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` | (required) | The most one lease may escrow. A launch quoting above it is refused before any money moves. |
| `COVENANT_COMPUTE_CATALOG_JSON` | (built-in) | The app catalog, as a JSON array of apps. Every entry is validated and a released app must pin its image to a sha256 digest; leave unset to serve the built-in catalog. |
| `COVENANT_COMPUTE_MIN_REPUTATION_BPS` | (none) | A reputation floor, in basis points, applied to the operators a lease will match. |
| `COVENANT_COMPUTE_BIND` | `127.0.0.1:8787` | The address the HTTP surface listens on. |

On SIGTERM the server drains in-flight requests before exiting, so a
launch is never killed between opening a lease and recording it.
