# Hatcher GPU smoke deployment

Historical preparation snapshot, written 2026-10-18. Service versions and rollout
steps below describe the original preparation, not current production state.
For the subsequent lost-response incident and automatic recovery rollout, see
[Hatcher automatic recovery](hatcher-automatic-recovery.md). Keep the dispatch
queue paused unless a new paid run is explicitly authorized.

## Services and source

The inspected Render deployment was split across old versions:

| Component | Render service ID | Inspected source/version |
| --- | --- | --- |
| Connector | `srv-d8nspicvikkc73d9b430` | `main`, commit `333544f` |
| Coding gateway | `srv-d8o032ho3t8c73d9do5g` | `feat/hatcher-compute`, commit `020846a` |
| Daemon | `srv-d8nu8preo5us738e1dug` | Confirm deployed commit in Render before updating |

The prepared patch branch is `codex/hatcher-gpu-smoke`. Publish it first, record
its reviewed full commit SHA, then deploy that same revision to all three
services. A local branch, successful build, or WebSocket upgrade is not proof of
the running version. Record each successful Render deployment ID and commit.

Preserve each service's existing root directory and service type. The gateway
root is `services/coding-gateway`; the connector root is
`services/hatcher-connector`; the daemon builds `deploy/Dockerfile.daemon` from
the repository root. Use the tested Node 24 runtime. The connector uses `ws`
to inspect rejected upgrades and honor `Retry-After`; reconnects have a
30-second minimum and a five-minute fallback cap, while a longer server
cooldown takes precedence. Use each package's scoped
installation and build, not an unrelated monorepo build. The existing scoped
pnpm lockfiles omit some package dependencies; refresh them before requiring
frozen installs, or retain the reviewed Render installation command that permits
the existing lockfile drift. Verify the final deployed build, not just a prior
local test result.

## Deploy with GPU gates off

1. Stop sending Hatcher dispatches. Do not run any intent against the old gateway
   while its compute token is loaded: the inspected service had no gateway auth,
   no E2B isolation, and no persistent disk. Old builds may not honor the new GPU
   feature flags.
2. Configure the gateway prerequisites below and deploy the reviewed patch with
   `COMPUTE_GPU_ENABLED=0`. Keep the connector at `HATCHER_GPU_ENABLED=false`.
3. Deploy the daemon's typed execution and real stop support from the same
   revision, with the correct gateway URL and bearer key. Confirm its Hermes
   coder agent is installed and routable.
4. Deploy the connector patch with its GPU gate still off. Complete the
   credential handoff and non-billable readiness checks below.
5. Enable the gateway GPU gate only after readiness passes; verify its reported
   bounds. Enable the connector gate last. Credentials and an open socket alone
   never authorize a GPU booking: a dispatch still needs the explicit grant.

## Gateway configuration

Keep this service private. The inspected deployment uses `NODE_ENV=internal`
and `CODER_BACKEND=anthropic`. Retain that intended internal configuration; do
not weaken the separately defined `NODE_ENV=production` contract to make this
deployment pass.

Add a persistent Render disk mounted at `/data`, with a single gateway writer.
The inspected Render UI quoted **$0.25 per GB per month**, minimum **1 GB**;
the user approved that 1 GB recurring cost on 2026-10-18. Provisioning is in
progress; this document does not confirm the disk has been attached. Confirm
the displayed price and the resulting mount. This storage cost is separate
from the run budget. Setting a path or `COMPUTE_DURABLE_STORAGE=1` without an
actual persistent mount does not establish durability.

Set these nonsecret values:

```dotenv
NODE_ENV=internal
CODER_BACKEND=anthropic
CODER_MODEL=claude-sonnet-4-6
CODER_PER_RUN_USD_MAX=0.50
CODER_MAX_CONCURRENT=1
CODER_WALL_MS=600000
LEDGER_PATH=/data/ledger.json
RUN_STORE_PATH=/data/runs.json
COMPUTE_JOURNAL_PATH=/data/compute.json
COMPUTE_DURABLE_STORAGE=1
COMPUTE_GPU_ENABLED=0
COMPUTE_MAX_USDC_MICROS=200000
COMPUTE_MAX_DURATION_SECS=300
COMPUTE_MAX_LAUNCHES=1
COMPUTE_API_URL=https://compute.opencovenant.org
```

Enter secrets directly into Render's secret environment fields: a new random
`CODER_AUTH_TOKEN` of at least 32 characters, `ANTHROPIC_API_KEY`, `E2B_API_KEY`,
and the authorized `COMPUTE_API_TOKEN`. Never put their values in this document,
source control, screenshots, logs, or command arguments. The gateway auth token
must also be entered as the daemon's `HERMES_API_KEY`.

E2B isolation is required before **any** run while compute credentials are
configured, including an ordinary non-GPU coding run. The new gateway rejects
such runs on its local provider. In `NODE_ENV=internal`, an `E2B_API_KEY` selects
that isolated provider; an immutable template and verified tariff are not
required by the boot checks or `/readyz` when no `E2B_TEMPLATE_ID` is configured.
In that case the reported tariff dependency is a no-op, not verified billing
evidence. Setting `E2B_TEMPLATE_ID` activates the matching tariff probe.

Before the paid smoke run, verify the sandbox reservation against the current
template and pricing: configure a verified `E2B_TEMPLATE_ID`,
its matching `E2B_EXPECTED_CPU_COUNT` and `E2B_EXPECTED_MEMORY_MB`, a currently
valid `CODER_E2B_TARIFF_REF`, and a verified conservative
`CODER_E2B_WORST_CASE_USD_PER_SEC`. This is a stronger paid-run release gate than
the internal-mode code requires. Do not invent tariff evidence or describe the
default rate as a verified $0.50 overall spending bound. Template IDs can have
multiple builds; the gateway does not pin a build ID. Recheck the resolved
template's CPU and memory identity before enabling the paid run. The independent GPU
booking ceiling remains $0.20 for one 300-second booking. Keep
`E2B_EGRESS_ALLOW` limited to the approved package/GitHub hosts actually needed;
the GPU workspace URL is not an allowed sandbox destination.

## Daemon and connector wiring

On the daemon, set:

```dotenv
HERMES_API_BASE_URL=http://covenant-hatcher-gateway:10000/v1
COVENANT_LIVE_TRACE=1
```

Set secret `HERMES_API_KEY` to exactly the gateway's new `CODER_AUTH_TOKEN`.
These are the actual runtime keys; `HERMES_API_TOKEN` is not read. The runtime
sends `HERMES_API_KEY` as a Bearer credential.

Confirm `$COVENANT_HOME/agents/coder/agent.toml` exists and declares agent id
`coder`, `runtime="hermes"`, and required capabilities `intent.subscribe`,
`tool.code`, and `memory.write`. The deploy entrypoint seeds the coder package
when `HERMES_API_BASE_URL` is set, but it does not replace an existing package.
Inspect an existing manifest rather than assuming a deploy corrected it. Typed
execution submissions require the intent to route to a Hermes coding agent
before the GPU capability check; a phase-0 echo or demo agent is not a GPU test.

On the connector, set:

```dotenv
HATCHER_MESH_URL=wss://api.hatcher.host/integrations/covenant/connectors/ws
HATCHER_AGENT_ID=cmp2dohkb00jtlsvsq1m5hriq
HATCHER_CONNECTOR_ID=cmvdhgaqc0000rfvsv37mezmh
HATCHER_GPU_ENABLED=false
HATCHER_MAX_CONCURRENT_DISPATCH=1
HATCHER_DEFAULT_DEADLINE_MS=600000
```

Preserve the verified private `COVENANT_DAEMON_URL` for the identified Hatcher
daemon. Keep its existing authenticated daemon credential binding intact:
`COVENANT_CONNECTOR_TOKEN` is preferred when a properly provisioned scoped peer
exists; otherwise the existing operator bootstrap uses
`COVENANT_OPERATOR_TOKEN`. Do not invent a scoped peer token or imply that an
operator token is least-privilege. Verify the connector can mint its required
per-dispatch capabilities under the selected peer. For an explicitly authorized
GPU dispatch, these include `compute.gpu_workspace` plus the native coder's
`intent.subscribe` and `tool.code`, each expiring at the dispatch deadline.
The two native coder grants use
`{"version":1,"run_key":"<execution.run_key>"}`. The patched daemon must enforce
that exact run key; these grants cannot authorize another dispatch or an
ordinary/legacy intent. Ordinary dispatches receive no new coder grants. Keep
the existing `memory.write` baseline unchanged.

The existing deployment entrypoint also grants broad baseline
`intent.subscribe` and, when Hermes is configured, `tool.code` to the operator.
The daemon deliberately continues accepting existing unrestricted grants;
the new run-key check constrains scoped grants and does not revoke the broad
operator bootstrap. Inspect the selected peer's actual capabilities, do not
describe this bootstrap as least-privilege, and do not add broad grants to
work around a failed dispatch. GPU execution still requires its independently
validated `compute.gpu_workspace` grant and exact booking policy.

Rotate/update `HATCHER_CONNECTOR_TOKEN` through the Render secret field and use
a current pairing code only if Hatcher requires one. The present browser flow
requires the user to enter replacement authentication secrets directly; hand
that field to the user rather than extracting or echoing credentials. Resume
after the field is saved and the intended deployment completes. No credential
values belong in this runbook.

## Non-billable readiness gates

Perform these checks from the private network using the appropriate bearer
credentials, without submitting an intent or creating a sandbox/GPU job:

- Gateway `GET /healthz`: `provider="e2b"`, `persistentRuns=true`, and
  `storageReady=true`.
- Authenticated gateway `GET /readyz`: HTTP 200, `ready=true`, and both
  ledger/run-store storage checks true. Explicitly require
  `dependencies.compute.ok=true` before declaring GPU readiness: compute is
  deliberately non-blocking for ordinary coding runs, so `ready=true` can
  coexist with `failed:["compute"]`. Require no failed dependencies for this
  smoke release. Model catalog and E2B control-plane probes must pass; the
  tariff probe must be configured and pass before the paid run as described
  above. An unconfigured tariff probe is not pricing verification.
- Gateway exact `GET /v1/capabilities` without an Authorization header: HTTP 204,
  zero response bytes, `Cache-Control: private, no-store`, and
  `Vary: Authorization`. This compatibility response serves Render's legacy
  health probe and establishes liveness only. An empty or incorrect Authorization
  header must return 401. Anonymous `GET /readyz` and `GET /v1/budget` must return
  401. Check these GET routes; do not submit a run as an auth test.
- Authenticated gateway `GET /v1/capabilities`: HTTP 200 and valid capability JSON,
  initially reporting `gpu_workspace=false`.
  Verify the actual persistent mount and writable storage, then set
  `COMPUTE_GPU_ENABLED=1` and redeploy. After readiness and recovery complete,
  authenticated capabilities must report `gpu_workspace=true`, GPU budget
  `$0.20`, `maxLaunches=1`, and maximum/default duration `300` seconds.
  This capability flag establishes the local execution gates, not live compute
  availability; retain the separate successful compute probe requirement.
- Daemon health is up and its configured private Hermes URL is reachable with
  the mirrored bearer credential. Confirm the deployed typed execution/stop
  implementation and Hermes agent manifest without submitting paid work.
- Connector `GET /healthz`: daemon reachable, socket open, zero in-flight
  dispatches. `mesh.authentication="unverified"` is deliberately honest: the
  pinned mesh contract has no confirmed authentication acknowledgement. Socket
  open and a sent `hello` are not proof of accepted auth or pairing. Confirm
  Hatcher's paired-agent view separately.

Only after all gates pass, set `HATCHER_GPU_ENABLED=true`, redeploy, and verify
the connector reports `gpu_enabled=true`. Then explicitly confirm to Hatcher
that Covenant is ready for the agreed single smoke dispatch. Until that
confirmation, Hatcher remains blocked from testing this integration.

## Exact consent and smoke request

Use one new dispatch id for the agreed run; replays of that run must reuse it.
The connector derives the stable run key from the full agent id and dispatch
id. Do not change the dispatch id to retry an uncertain or still-stopping run.
Before enabling the paid test, set the gateway's `CODER_ALLOWED_RUN_KEY` to the
lowercase SHA-256 digest of the UTF-8 string
`JSON.stringify([fullAgentId, exactDispatchId])` for that agreed dispatch, then
redeploy. The pin is unset by default; a configured empty or malformed value
fails startup. A pin also requires a service auth token of at least 32 characters
and absolute `RUN_STORE_PATH` and `LEDGER_PATH` values.

With the pin set, every other run key is rejected before admission or provider
work, including ordinary requests without a GPU grant. Matching identical
requests return the existing run; changed requests conflict instead of receiving
a fresh allowance. The pinned session is retained during receipt pruning and
reloaded after restarts. Keep the same pin and mounted data for the whole test;
deleting the data, removing the pin, or changing it is not a permitted retry.
After completion or failure, a replay retrieves that result and does not start a
new attempt. A fresh test requires new authorization. Verify rejection of another
key and replay of the original key without another sandbox or GPU booking.

The following is the Hatcher mesh payload, not a command to execute now:

```json
{
  "v": 1,
  "type": "dispatch",
  "dispatch_id": "hatcher-gpu-smoke-2026-10-19-01",
  "agent_id": "cmp2dohkb00jtlsvsq1m5hriq",
  "intent": {
    "text": "Run a coding GPU smoke test. Launch exactly one gpu_workspace for 300 seconds. Poll status with bounded waits. Record its job id and status, then cancel it and return the final receipt. Do not launch a replacement or attempt to connect from this sandbox to the workspace URL.",
    "context": {
      "max_cost_usd": 0.50,
      "gpu": { "duration_secs": 300 }
    }
  },
  "grants": [
    {
      "scope": "compute.gpu_workspace",
      "constraints": {
        "max_usdc_micros": 200000,
        "duration_secs": 300,
        "max_launches": 1
      }
    }
  ]
}
```

The proposed **$0.50 total reservation** allocates the **$0.20 GPU booking
ceiling**, a sandbox reservation, and model use. It is not a $0.20 all-in run.
Treat the overall bound as verified only after the sandbox identity and
conservative tariff have been checked; the default internal-mode rate alone
does not establish the actual sandbox charge.
For example, a verified sandbox ceiling of `$0.0002/second` and a `600` second
gateway wall limit reserve `$0.12`, leaving `$0.18` for the model. Use the actual
verified tariff when calculating admission; lower remaining model allowance
may stop the coding loop before the GPU becomes ready.

Only the explicit grant authorizes GPU execution. `intent.context.gpu` is a
validated hint, not permission. Extra, duplicate, malformed, or wider GPU grants
are rejected. The connector forwards a typed `/intent` execution object with
the stable key, total cap, and exact GPU limits; ordinary coding intents do not
inherit GPU access from a gateway compute token.

## Required smoke evidence and stopping

For the one authorized paid test, record the deployment SHAs, dispatch/intent/
gateway run/job ids, the booked maximum, final terminal status, and full safe
receipt fields. Do not persist a workspace access URL or token. Confirm there
was only one booked GPU, and that replaying the same dispatch cannot create a
second booking. An uncertain submission must be reconciled, not retried under
a fresh id.

A Hatcher cancel frame for the same dispatch must reach daemon
`POST /intents/:id/stop` and then the gateway's stop operation. A `stopping`
response acknowledges the request; it does not prove resource cleanup. Keep
polling the original result. A terminal result must include completed cleanup
and receipts whose charged plus refunded amounts reconcile to the booking
maximum. `cleanup_pending`, missing receipts, or unresolved launch keys mean
the test is incomplete, even if the socket, coding loop, or model has stopped.

The first test must verify the control plane/provider's actual readiness and
billing timestamps and receipt accounting. Do not promise that billing begins
only when reachable, that every cancellation is immediate, or that refunds are
prorated until that live evidence exists. Local tests and non-billable probes
do not establish those provider outcomes.

If readiness or cleanup fails, turn the connector GPU gate off first and keep
new Hatcher dispatches paused. Disable new GPU admissions at the gateway while
retaining the patched service, credentials needed for cleanup, persistent disk,
and journals until existing jobs are reconciled. Do not delete journals or roll
back to the old gateway while an admitted GPU run remains unresolved.
