#!/usr/bin/env bash
# Demo script recorded by asciinema and rendered to assets/demo.gif via agg.
# Drives a single intent round trip through covenantd against an isolated
# $COVENANT_HOME, then prints the audit row and verifies the hash chain.
#
# Prereqs: workspace built (`cargo build --workspace --exclude covenant-settlement-program`)
# and jq on PATH. Run from the repository root.

set -euo pipefail

DIM=$'\033[38;5;245m'
RESET=$'\033[0m'

type_cmd() {
  local cmd="$1"
  printf '%s$%s ' "$DIM" "$RESET"
  for ((i=0; i<${#cmd}; i++)); do
    printf '%s' "${cmd:$i:1}"
    sleep 0.025
  done
  printf '\n'
  sleep 0.35
}

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export COVENANT_HOME="$(mktemp -d)/covenant"
export COVENANT_HTTP_PORT=0
mkdir -p "$COVENANT_HOME/agents"
cp -R ./examples/hello-agent "$COVENANT_HOME/agents/hello"

BIN=./agent-os/target/debug
set +m
"$BIN/covenantd" >/dev/null 2>&1 &
DAEMON_PID=$!
disown 2>/dev/null || true
cleanup() {
  { kill -TERM "$DAEMON_PID" 2>/dev/null; wait "$DAEMON_PID" 2>/dev/null; } >/dev/null 2>&1
  rm -rf "$COVENANT_HOME"
}
trap cleanup EXIT

for _ in 1 2 3 4 5 6 7 8 9 10; do
  if "$BIN/covenant" ping >/dev/null 2>&1; then break; fi
  sleep 0.3
done

# Out-of-frame setup so the GIF stays on the round trip itself.
"$BIN/covenant" capabilities grant memory.write >/dev/null 2>&1

clear
sleep 0.4

type_cmd "covenant ping"
"$BIN/covenant" ping
sleep 0.8

type_cmd 'covenant intent "say hello"'
"$BIN/covenant" intent "say hello"
sleep 1.2

type_cmd "covenant audit recent -n 1 --json | jq ."
"$BIN/covenant" audit recent -n 1 --json | jq .
sleep 1.8

type_cmd "covenant audit verify --json | jq ."
"$BIN/covenant" audit verify --json | jq .
sleep 2.4
