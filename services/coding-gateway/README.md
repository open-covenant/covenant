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

| Method | Path                   | Purpose                                                        |
| ------ | ---------------------- | -------------------------------------------------------------- |
| POST   | `/v1/runs`             | start a run (`{input, session_id, max_cost_usd}` → `{run_id}`) |
| GET    | `/v1/runs/{id}`        | poll status / output                                           |
| GET    | `/v1/runs/{id}/events` | SSE event stream                                               |
| POST   | `/v1/runs/{id}/stop`   | cancel + tear down                                             |
| GET    | `/v1/capabilities`     | advertised features the daemon gates on                        |

See `src/types.ts` for the full contract and the `CodingBackend` /
`SandboxProvider` interfaces.

## Operator controls

Per-run admission is gated by a USD spend ledger with hard daily and monthly
caps and a global concurrency cap. Every request supplies `max_cost_usd`; the
ledger reserves that exact all-in cap before starting a sandbox. The gateway
deducts the maximum sandbox charge before giving the provider a budget, then
derives a conservative output-token limit for every paid turn from the
remaining budget and configured per-million-token price ceilings.

| Env var                              | Default                 | Meaning                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------ | ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `CODER_DAILY_USD`                    | `6`                     | Hard daily spend cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `CODER_MONTHLY_USD`                  | `200`                   | Hard monthly spend cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `CODER_PER_RUN_USD_MAX`              | `2`                     | Per-run reservation ceiling.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `CODER_MAX_CONCURRENT`               | `2`                     | Concurrent run cap.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `CODER_IP_MAX_PER_IP`                | `1`                     | Per-IP in-flight admission cap. Stops a single anonymous client from occupying every concurrency slot. Set to `0` to disable (only safe behind an upstream rate limiter).                                                                                                                                                                                                                                                                                                                                                                                           |
| `CODER_IP_REFILL_MS`                 | `60000`                 | Minimum delay between one IP's release and its next admission. Rate-limits a rapid-cycle client that drains the daily cap with cheap no-op runs.                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `CODER_WALL_MS`                      | `600000`                | Per-run wall-clock ceiling. The gateway aborts at this deadline and passes the same value to the sandbox as its self-destruct backstop, so a crashed gateway still has a hard end to leftover microVM spend. The deadline is retained in pending ledger records as audit context; restart accounting still charges every unresolved reservation at its full maximum.                                                                                                                                                                                                |
| `USEPOD_BASE_URL`                    | `https://api.usepod.ai` | HTTPS origin used to construct the documented token-in-path proxy URL. Production rejects embedded credentials and pre-tokenized paths.                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `USEPOD_MAX_INPUT_PRICE_MICROUNITS`  | `200000`                | Per-million input-token price ceiling sent on every UsePod request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `USEPOD_MAX_OUTPUT_PRICE_MICROUNITS` | `400000`                | Per-million output-token price ceiling sent on every UsePod request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `E2B_TEMPLATE`                       | _(none)_                | Pinned sandbox template. Required in production.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `TRUSTED_PROXY_HOPS`                 | `0`                     | Trust the right-most N entries of `X-Forwarded-For` as proxy hops the operator controls; everything left is treated as client-supplied. **Picking too large** lets a client rotate IPs via the header — set it to the exact number of trusted proxies between the gateway and the public internet (1 for a single Cloudflare/Fly/Render edge; 2 for an edge plus an internal load balancer). Default `0` uses the socket peer, which is safe for any deployment but collapses every visitor behind shared NAT or a single edge to one address.                      |
| `LEDGER_PATH`                        | _(none)_                | If set, committed spend AND in-flight reservations persist to this file so caps survive a restart. Must point at **persistent** storage (not `tmpfs` / a container volume that resets on reboot) or the cap silently restarts at $0.                                                                                                                                                                                                                                                                                                                                |
| `CODER_EXEMPT_IPS`                   | _(none)_                | Comma-separated list of IPs that bypass the per-IP bucket **and** the daily/monthly USD spend caps. The kill-switch, the concurrency cap, and observability still apply, so an exempt run still surfaces on `/v1/budget` and still tears down when the kill-switch fires. Intended for the operator's own IP so the intentionally-low public daily cap doesn't block diagnostic / development traffic from the people maintaining the deployment. The address format must match exactly what `sourceIp` resolves to under the current `TRUSTED_PROXY_HOPS` setting. |

`GET /v1/budget` returns the live snapshot: `dailyUsd`, `monthlyUsd`,
`reserved`, `active`, `killed`, the configured caps, `outcomes` counters
(`completed`, `failed`, `cancelled`), and an `ipBucket` block with the live
per-IP gate state (`active`, `inflight`, `rejected`, configured `maxPerIp` /
`refillMs`) so operators can see abuse volume from one snapshot.

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

## Production contract

Production boots only with the UsePod backend, one pinned model, explicit price
ceilings, a funded token, a pinned E2B template, restricted egress, and durable
ledger and run-receipt paths. Readiness executes a forced tool call on that exact
model and proves funded marketplace routing from the response headers. Each run
persists the same non-secret route and cost evidence before completion is
visible. Missing provider cost evidence stops the run before another paid turn
and charges the full reservation. A receipt above the reservation remains
visible in the ledger, permanently engages the kill switch, and prevents new
admission until an operator reconciles the provider account and ledger.
