#!/usr/bin/env bash
# Tear the alpha stack down from the recorded pids and confirm nothing is left behind.
# The receipts log is kept — it is the record of what the alpha served.
set -euo pipefail

STATE="${COVENANT_ALPHA_STATE:-$HOME/.local/state/covenant-inference-alpha}"
ENV_FILE="$STATE/alpha.env"
PIDS="$STATE/pids"
RECEIPTS_LOG="$STATE/receipts.jsonl"
PORTS=(28080 28090 28091 27443)
[ -f "$ENV_FILE" ] && { . "$ENV_FILE"; }

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '   %s\n' "$*"; }

stop() { # name
  local name="$1" f pid
  f="$PIDS/$name.pid"
  [ -f "$f" ] || { info "$name: no pidfile"; return; }
  pid="$(cat "$f")"
  if ! kill -0 "$pid" 2>/dev/null; then
    info "$name (pid $pid): already stopped"
    rm -f "$f"; return
  fi
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.25; done
  if kill -0 "$pid" 2>/dev/null; then
    kill -9 "$pid" 2>/dev/null || true
    info "$name (pid $pid): force-killed"
  else
    info "$name (pid $pid): stopped"
  fi
  rm -f "$f"
}

say "stopping the stack (tunnel, heartbeat, gateway, node serve, llama)"
for name in tunnel heartbeat gateway node-serve llama; do stop "$name"; done
sleep 1

say "checking for strays"
strays=0
for port in "${PORTS[@]}"; do
  held="$(lsof -ti "tcp:$port" 2>/dev/null || true)"
  if [ -n "$held" ]; then
    strays=$((strays + 1))
    info "port $port still held by pid(s): $held — killing"
    kill -9 $held 2>/dev/null || true
  fi
done
leftover="$(pgrep -fl 'covenant-inferd|covenant-inference-gateway' 2>/dev/null | grep -v pgrep || true)"
if [ -n "$leftover" ]; then
  info "note: covenant-inference processes still present (may belong to another run):"
  printf '     %s\n' "$leftover"
  strays=$((strays + 1))
fi

if [ "$strays" -eq 0 ]; then
  say "teardown clean — no strays on ports ${PORTS[*]}, no covenant-inference processes left"
else
  say "teardown done with $strays stray check(s) flagged above"
fi

if [ -f "$RECEIPTS_LOG" ]; then
  info "receipts log kept: $RECEIPTS_LOG ($(wc -l < "$RECEIPTS_LOG" | tr -d ' ') receipts)"
fi
