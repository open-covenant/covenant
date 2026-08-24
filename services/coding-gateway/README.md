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
retains the maximum sandbox charge before giving the model provider a budget,
then derives a conservative output-token limit for every paid turn from the
remaining budget and configured per-million-token price ceilings. E2B does not
expose an authoritative billing receipt in the installed SDK, so an attempted
sandbox create is always charged for the full wall-clock reservation. The
reported sandbox amount is a conservative accounting charge, not a claim about
the provider invoice.

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
| `TRUSTED_PROXY_HOPS`                 | `0`                       | Trust the right-most N entries of `X-Forwarded-For` as proxy hops the operator controls; everything left is treated as client-supplied. **Picking too large** lets a client rotate IPs via the header — set it to the exact number of trusted proxies between the gateway and the public internet (1 for a single Cloudflare/Fly/Render edge; 2 for an edge plus an internal load balancer). Default `0` uses the socket peer, which is safe for any deployment but collapses every visitor behind shared NAT or a single edge to one address. |
| `LEDGER_PATH`                        | _(none)_                  | If set, committed spend AND in-flight reservations persist to this file so caps survive a restart. Must point at **persistent** storage (not `tmpfs` / a container volume that resets on reboot) or the cap silently restarts at $0.                                                                                                                                                                                                                                                                                                           |
| `CODER_EXEMPT_IPS`                   | _(none)_                  | Comma-separated list of IPs that bypass only the per-IP bucket. Daily and monthly USD caps, the kill switch, concurrency, sandbox reservation, and observability always apply. Intended for trusted operational probes that must not contend with a public per-IP bucket. The address format must match exactly what `sourceIp` resolves to under the current `TRUSTED_PROXY_HOPS` setting.                                                                                                                                                    |

`GET /v1/budget` returns the live snapshot: `dailyUsd`, `monthlyUsd`,
`reserved`, `active`, `killed`, the configured caps, `outcomes` counters
(`completed`, `failed`, `cancelled`), and an `ipBucket` block with the live
per-IP gate state (`active`, `inflight`, `rejected`, configured `maxPerIp` /
`refillMs`) so operators can see abuse volume from one snapshot. Its
`accounting.sandbox` block labels the method as `pinned-worst-case-tariff`,
publishes the rate, evidence reference, exact sandbox identity and maximum
per-run charge, and explicitly reports that no authoritative E2B billing
receipt is available.

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
ceilings, a funded token, an immutable E2B template identity, current hashed
tariff evidence, restricted egress, and durable ledger and run-receipt paths.
Readiness uses only non-billable control-plane checks: UsePod's model catalog
and token-balance endpoint, E2B's sandbox-list API, and the public tariff
evidence. The balance response must contain one safe-integer `usdc_balance` in
USDC microunits and one matching `X-Balance-Remaining` value. Missing, malformed,
duplicate, conflicting, or below-floor evidence closes intake. Paid model and
sandbox probes are intentionally forbidden because repeated readiness polling
would sit outside the job ledger. Run admission fails closed while that cached
readiness evidence is unhealthy. Each validated paid turn persists non-secret route data,
token usage, price ceilings, and an accounted cost before another turn begins.
The accounted cost is the maximum of the ceiling-derived usage cost and an
optional provider-reported cost. Missing provider cost reports therefore do not
stop a valid multi-turn run. Missing or mismatched accounted receipts retain the
full reservation. A receipt above the reservation remains visible in the ledger, permanently
engages the kill switch, and prevents new admission until an operator
reconciles the provider account and ledger.

The balance contract comes from UsePod's
[documented credit-verification endpoint](https://docs.usepod.ai/api/deposit-on-chain/#verifying-the-credit)
and its guarantee that every proxied response carries the balance header. Before
opening production intake, capture an authenticated `/readyz` response with
`dependencies.balance.ok: true` against the dedicated funded token. Unit tests
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
