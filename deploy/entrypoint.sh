#!/usr/bin/env bash
## Demo-mode entrypoint for covenantd on Render.
##
## - Honors COVENANT_OPERATOR_TOKEN env (b58 string) by writing it to the
##   on-disk operator.token before the daemon starts, so both services share
##   the same bearer credential.
## - Copies the bundled demo-agent (Haiku-backed) into $COVENANT_HOME/agents.
## - Waits for the daemon HTTP gateway to come up, then grants baseline
##   capabilities so a fresh container is immediately useful.

set -euo pipefail

COVENANT_HOME="${COVENANT_HOME:-/data}"
HTTP_PORT="${COVENANT_HTTP_PORT:-8421}"
HTTP_HOST="${COVENANT_HTTP_BIND_ADDR:-0.0.0.0}"

mkdir -p "$COVENANT_HOME/peers" "$COVENANT_HOME/agents" "$COVENANT_HOME/audit" \
         "$COVENANT_HOME/memory" "$COVENANT_HOME/capabilities" \
         "$COVENANT_HOME/receipts" "$COVENANT_HOME/a2a" "$COVENANT_HOME/budget" \
         "$COVENANT_HOME/identity"

if [[ -n "${COVENANT_OPERATOR_TOKEN:-}" ]]; then
  printf '%s' "$COVENANT_OPERATOR_TOKEN" > "$COVENANT_HOME/peers/operator.token"
  chmod 600 "$COVENANT_HOME/peers/operator.token"
  echo "entrypoint: wrote operator token from env (mode 0600)"
fi

# Seed the demo-agent by default. Set COVENANT_SEED_DEMO_AGENT=0 for a clean
# no-agent daemon (intents fall through to the phase-0 echo path instead of
# matching an agent) — used by execution-layer connectors that only need a
# trusted-local echo/audit backend, not the Haiku demo agent.
if [[ "${COVENANT_SEED_DEMO_AGENT:-1}" == "1" ]]; then
  if [[ ! -d "$COVENANT_HOME/agents/demo" ]]; then
    cp -R /opt/covenant/demo-agent "$COVENANT_HOME/agents/demo"
    echo "entrypoint: seeded demo-agent"
  fi
else
  echo "entrypoint: demo-agent seeding disabled (COVENANT_SEED_DEMO_AGENT=${COVENANT_SEED_DEMO_AGENT})"
fi

# The coder agent uses runtime=hermes and is only useful once a coding gateway
# is configured. Without HERMES_API_BASE_URL it would route coding intents to a
# HermesUnconfigured error, so seed it (and grant tool.code) only when the
# gateway URL is set — it stays dormant until the gateway is wired.
CODER_CAP=""
if [[ -n "${HERMES_API_BASE_URL:-}" ]]; then
  if [[ ! -d "$COVENANT_HOME/agents/coder" ]]; then
    cp -R /opt/covenant/coder-agent "$COVENANT_HOME/agents/coder"
    echo "entrypoint: seeded coder-agent (HERMES_API_BASE_URL set)"
  fi
  CODER_CAP="tool.code"
fi

echo "entrypoint: starting covenantd on $HTTP_HOST:$HTTP_PORT"
covenantd &
DAEMON_PID=$!

# Wait for the HTTP gateway to accept requests.
for _ in $(seq 1 30); do
  if curl -fs "http://127.0.0.1:$HTTP_PORT/health" >/dev/null 2>&1; then
    echo "entrypoint: daemon ready"
    break
  fi
  sleep 0.5
done

# Idempotent capability seeding via the operator CLI (which reads the same
# operator.token we just wrote). Failures are non-fatal — the daemon stays up.
for action in memory.write intent.subscribe memory.read $CODER_CAP; do
  if covenant capabilities grant "$action" >/dev/null 2>&1; then
    echo "entrypoint: granted $action"
  else
    echo "entrypoint: grant $action skipped (already granted, or daemon not ready yet)"
  fi
done

# Hand the foreground over to the daemon so Render can SIGTERM it cleanly.
wait "$DAEMON_PID"
