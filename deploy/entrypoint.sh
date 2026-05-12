#!/usr/bin/env bash
## Demo-mode entrypoint for covenantd on Render.
##
## - Honors COVENANT_OPERATOR_TOKEN env (b58 string) by writing it to the
##   on-disk operator.token before the daemon starts, so both services share
##   the same bearer credential.
## - Copies the bundled hello-agent into $COVENANT_HOME/agents.
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

if [[ ! -d "$COVENANT_HOME/agents/hello" ]]; then
  cp -R /opt/covenant/hello-agent "$COVENANT_HOME/agents/hello"
  echo "entrypoint: seeded hello-agent"
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
for action in memory.write intent.subscribe memory.read; do
  if covenant capabilities grant "$action" >/dev/null 2>&1; then
    echo "entrypoint: granted $action"
  else
    echo "entrypoint: grant $action skipped (already granted, or daemon not ready yet)"
  fi
done

# Hand the foreground over to the daemon so Render can SIGTERM it cleanly.
wait "$DAEMON_PID"
