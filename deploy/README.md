# Covenant public sandbox — Render deploy

This directory ships the Covenant operator console as a public, writable
sandbox on Render. Two services share an internal network and a single
operator token; destructive actions are gated at the proxy layer; a daily
cron restarts the daemon to keep state clean.

## Layout

- `deploy/Dockerfile.daemon` — multi-stage Rust build for `covenantd`,
  produces a slim Debian runtime with the daemon, the operator CLI, and
  a Python 3 toolchain for the bundled hello-agent.
- `deploy/entrypoint.sh` — bootstraps the operator token from env, copies
  the hello-agent into `$COVENANT_HOME/agents`, starts the daemon, and
  seeds baseline capabilities (`memory.write`, `intent.subscribe`,
  `memory.read`) via the CLI.
- `render.yaml` (repo root) — two services + a shared env group + a
  12-hour reset cron.

## Architecture

```
visitor ──HTTPS──▶ covenant-demo-web (Next.js)
                         │
                         │ private network (Render internal)
                         ▼
                  covenant-demo-daemon (covenantd HTTP)
```

The daemon binds to `0.0.0.0:8421` on the Render private network only.
It is not exposed publicly. All visitor traffic goes through the Next.js
proxy, which:

- Injects the shared `COVENANT_OPERATOR_TOKEN` as a bearer header.
- Blocks destructive routes when `NEXT_PUBLIC_DEMO_MODE=1`
  (`peers/rotate`, `*/purge`, `capabilities/revoke`, etc.).
- Rate-limits writes per IP (10/min for intent dispatch, 5/min for grants).
- Renders a "Public sandbox" banner so visitors know state is shared.

## First deploy

1. Push the branch to a GitHub repo connected to your Render workspace.
2. Render reads `render.yaml` and creates both services + the env group.
3. The env group generates `COVENANT_OPERATOR_TOKEN` once and pins it.
4. The daemon entrypoint writes that token to `$COVENANT_HOME/peers/operator.token`
   with mode 0600 on first boot, satisfying the daemon's permission check.

## After first deploy

Copy the daemon service ID from the Render dashboard into the
`DAEMON_SVC_ID` env var on the `covenant-demo-reset` cron, and create a
workspace API key and set it as `RENDER_API_KEY`. The cron runs every
12 hours and restarts the daemon via the Render REST API; because the
disk is persistent, state survives the restart unless you remove the
disk volume from `render.yaml` (set `disk:` to null for ephemeral state).

## Local dev unchanged

Both `COVENANT_DAEMON_URL` and `COVENANT_OPERATOR_TOKEN` are env-driven
with fallbacks. When unset, the Next.js app talks to `127.0.0.1:8421`
and reads `~/.covenant/peers/operator.token` just like before. Demo mode
is off unless `NEXT_PUBLIC_DEMO_MODE=1`.

## Hardening notes

- **Single Next.js instance**: the rate limiter is in-process. If
  Render autoscales the web service, swap the in-memory map for Redis
  (Upstash works fine via env vars).
- **Disk**: the 1 GB Render persistent disk caps audit log + memory
  growth. Twelve-hour restarts will eventually fill it; tune
  `examples/hello-agent` budget or add an audit-log cap in the daemon.
- **No outbound network**: the Docker image only has Python + curl. The
  hello-agent has `network = "off"` in its manifest, so even an attempted
  prompt-injection cannot reach the internet.
