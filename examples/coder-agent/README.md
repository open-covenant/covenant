# Coder agent

A coding agent for the public sandbox. It declares the `tool.code` capability
(so build/coding intents route to it) and `runtime = "hermes"`, delegating
execution to the coding gateway — a Hermes `/v1`-compatible service that runs
the coding loop inside an ephemeral, secret-free, egress-capped sandbox and
streams tool and file events back through the daemon's audit chain.

## Wiring

1. Stand up the coding gateway and set `HERMES_API_BASE_URL` (and
   `HERMES_API_KEY` if it requires auth) on the daemon.
2. The deploy entrypoint then seeds this package into
   `$COVENANT_HOME/agents/coder` and grants `tool.code`. Until
   `HERMES_API_BASE_URL` is set the agent stays dormant — without a gateway a
   coding intent would only return `HermesUnconfigured`, which is worse than
   the honest "no agent" default.

There is no on-disk binary: hermes-runtime agents execute remotely, so there is
nothing to build or install alongside this manifest.
