# @covenant/coding-gateway

A Hermes `/v1`-compatible gateway that turns a coding task into real work at the
quality bar of a desktop interactive coding agent, then streams the result back
through covenantd's audit chain.

covenantd's `HermesRunner` already speaks this protocol, so the daemon runtime is
unchanged: the `coder` agent declares `runtime = "hermes"`, and the daemon points
at this gateway via `HERMES_API_BASE_URL`.

## What it does per run

1. Provisions an **ephemeral sandbox** (`SandboxProvider`) — no host secrets, an
   egress allowlist, and cpu/memory/disk/wall caps, torn down on completion,
   timeout, or stop.
2. Drives a **pluggable coding backend** (`CodingBackend`) — an Anthropic or
   OpenAI coding-agent SDK, selected per run — with read/write/edit/terminal
   tools bound to the sandbox.
3. Maps backend events onto Hermes SSE frames (`tool.started` / `tool.completed`
   / `approval.request`) that the daemon folds into audit, plus `message` /
   `reasoning` / `file.written` for the live UI (WS3).

## Hermes `/v1` surface

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/v1/runs` | start a run (`{input, session_id}` → `{run_id}`) |
| GET | `/v1/runs/{id}` | poll status / output |
| GET | `/v1/runs/{id}/events` | SSE event stream |
| POST | `/v1/runs/{id}/stop` | cancel + tear down |
| GET | `/v1/capabilities` | advertised features the daemon gates on |

See `src/types.ts` for the full contract and the `CodingBackend` /
`SandboxProvider` interfaces.

## Operator controls

Per-run admission is gated by a USD spend ledger with hard daily and monthly
caps and a global concurrency cap. The ledger reserves the per-run maximum at
admission and commits the actual cost on completion, so concurrent bursts can't
overshoot the cap.

| Env var | Default | Meaning |
|---|---|---|
| `CODER_DAILY_USD` | `6` | Hard daily spend cap. |
| `CODER_MONTHLY_USD` | `200` | Hard monthly spend cap. |
| `CODER_PER_RUN_USD_MAX` | `2` | Per-run reservation ceiling. |
| `CODER_MAX_CONCURRENT` | `2` | Concurrent run cap. |
| `CODER_IP_MAX_PER_IP` | `1` | Per-IP in-flight admission cap. Stops a single anonymous client from occupying every concurrency slot. Set to `0` to disable (only safe behind an upstream rate limiter). |
| `CODER_IP_REFILL_MS` | `60000` | Minimum delay between one IP's release and its next admission. Rate-limits a rapid-cycle client that drains the daily cap with cheap no-op runs. |
| `CODER_WALL_MS` | `600000` | Per-run wall-clock ceiling. The gateway aborts at this deadline and passes the same value to the sandbox as its self-destruct backstop, so a crashed gateway still has a hard end to leftover microVM spend. Also the deadline horizon `LEDGER_PATH` uses for warm-recovery reservations. |
| `TRUSTED_PROXY_HOPS` | `0` | Trust the right-most N entries of `X-Forwarded-For` as proxy hops the operator controls; everything left is treated as client-supplied. **Picking too large** lets a client rotate IPs via the header — set it to the exact number of trusted proxies between the gateway and the public internet (1 for a single Cloudflare/Fly/Render edge; 2 for an edge plus an internal load balancer). Default `0` uses the socket peer, which is safe for any deployment but collapses every visitor behind shared NAT or a single edge to one address. |
| `LEDGER_PATH` | _(none)_ | If set, committed spend AND in-flight reservations persist to this file so caps survive a restart. Must point at **persistent** storage (not `tmpfs` / a container volume that resets on reboot) or the cap silently restarts at $0. |
| `CODER_LEDGER_RESET_PENDING` | _(unset)_ | One-shot operator override: when set to `1` at boot, the ledger drops every persisted pending reservation instead of reinstating it. Use only when a crash-loop has wedged admission with stale markers (see "Warm recovery" below); unset it after the next clean boot or every subsequent restart loses real in-flight reservations. |
| `CODER_EXEMPT_IPS` | _(none)_ | Comma-separated list of IPs that bypass the per-IP bucket **and** the daily/monthly USD spend caps. The kill-switch, the concurrency cap, and observability still apply, so an exempt run still surfaces on `/v1/budget` and still tears down when the kill-switch fires. Intended for the operator's own IP so the intentionally-low public daily cap doesn't block diagnostic / development traffic from the people maintaining the deployment. The address format must match exactly what `sourceIp` resolves to under the current `TRUSTED_PROXY_HOPS` setting. |

`GET /v1/budget` returns the live snapshot: `dailyUsd`, `monthlyUsd`,
`reserved`, `active`, `killed`, the configured caps, `outcomes` counters
(`completed`, `failed`, `cancelled`), and an `ipBucket` block with the live
per-IP gate state (`active`, `inflight`, `rejected`, configured `maxPerIp` /
`refillMs`) so operators can see abuse volume from one snapshot.

**Kill-switch.** Sending `SIGUSR1` to the gateway process (`kill -USR1 <pid>`)
refuses every new reservation and aborts every in-flight run's
`AbortController`, tearing down sandboxes so spend stops immediately. The
switch is idempotent and has no HTTP surface, so there is no auth path to get
wrong.

For a clean shutdown, send `SIGUSR1` first and wait for `GET /v1/budget` to
report `active: 0` before sending `SIGTERM`. Each in-flight run still has to
finish its `sandbox.destroy()` round-trip after the abort lands; `SIGTERM`
during that window can orphan a microVM until its own wall-clock budget
self-destructs at the provider.

**Warm recovery.** A reservation appended to `LEDGER_PATH` at admission time
carries the wall-clock deadline (`now + CODER_WALL_MS`). On boot the gateway
treats every unexpired entry as a live reservation against the daily cap and
concurrency slot, so a crash-restart cannot admit a fresh max-spend wave on
top of microVMs that are still burning wallet until their own self-destruct.
Expired entries are pruned from disk at load; `commit` removes the entry the
caller's run owned, so the file size stays proportional to live runs, not
lifetime runs. If a crash-loop wedges admission with stale markers — say a
process aborts before the sandbox actually starts but after the marker is
written — boot once with `CODER_LEDGER_RESET_PENDING=1` to drop every
pending entry, then unset it before normal operation resumes.

## Status

Design + interface stubs (coder-03). Implementation slices: gateway core +
Anthropic backend (coder-04), OpenAI backend (coder-05), ephemeral sandbox
provider (coder-06/07). Needs `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` and a
sandbox-provider credential; a hard spend cap gates public exposure.
