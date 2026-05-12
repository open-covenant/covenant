# Hermes-backed agent (example)

A Covenant agent whose execution runs inside a [Hermes](https://github.com/NousResearch/hermes-agent) gateway. Covenant owns the capability check, intent dispatch, and audit chain; Hermes owns the agent stepping.

## Prereqs

A running Hermes gateway with the API server enabled:

```bash
hermes serve --api  # binds 127.0.0.1:8642 by default
```

If you set an `API_SERVER_KEY`, point Covenant at it via `HERMES_API_KEY`.

## Register

```bash
mkdir -p ~/.covenant/agents
cp -R covenant/examples/hermes-agent ~/.covenant/agents/hermes
```

## Run

```bash
export HERMES_API_BASE_URL=http://127.0.0.1:8642/v1
export HERMES_API_KEY=...          # only if your gateway requires auth
covenantd                          # in one shell

covenant capabilities grant intent.subscribe
covenant capabilities grant memory.write
covenant intent "summarise today's research notes"
```

Covenant verifies the agent has its declared capabilities, then issues
`POST /v1/runs` to the Hermes gateway with the intent text as `input` and
the intent UUID as both `session_id` and `Idempotency-Key`. The runner
polls `GET /v1/runs/{id}` until terminal state, returns the run's
`output` as the intent result, and writes one `intent_dispatched` row
into the hash-chained audit log.

## What does *not* happen yet

- Hermes's per-step event stream (`tool.started`, `tool.completed`,
  `approval.request`) is not yet folded into the Covenant audit log.
  That arrives as a follow-up — tracked as slice 3 of the integration.
- Tool gating on the Hermes side is still server-configured. Covenant
  enforces the agent-level capability set; if you need per-tool gating
  inside Hermes, configure it via the Hermes CLI for now.
