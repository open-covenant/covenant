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

## Status

Design + interface stubs (coder-03). Implementation slices: gateway core +
Anthropic backend (coder-04), OpenAI backend (coder-05), ephemeral sandbox
provider (coder-06/07). Needs `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` and a
sandbox-provider credential; a hard spend cap gates public exposure.
