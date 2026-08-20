#!/usr/bin/env bash
# Orchestrates the local determinism spike: starts llama-server in two serving
# configurations (single-stream and concurrent/batched), runs the harness phases
# against each, then merges into results/local-run.json. Tears servers down at end.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"
mkdir -p results phase

# Resolve the GGUF blob from ollama at runtime (keeps absolute user paths out of source).
BLOB="$(ollama show qwen3:8b --modelfile | awk '/^FROM /{print $2; exit}')"
SERVER="/opt/homebrew/bin/llama-server"
PORT_SERIAL=8099
PORT_CONC=8098
LLAMACPP_VERSION="$($SERVER --version 2>&1 | grep -m1 version | tr -d '\n')"

PIDS=()
cleanup() {
  echo ">> teardown"
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  lsof -ti tcp:$PORT_SERIAL 2>/dev/null | xargs kill -9 2>/dev/null
  lsof -ti tcp:$PORT_CONC   2>/dev/null | xargs kill -9 2>/dev/null
}
trap cleanup EXIT

start_server() {
  local port="$1"; shift
  local logf="$1"; shift
  lsof -ti tcp:"$port" 2>/dev/null | xargs kill -9 2>/dev/null
  nohup "$SERVER" -m "$BLOB" --port "$port" "$@" > "$logf" 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  LAST_PID="$pid"
  echo ">> started server pid=$pid port=$port flags: $*"
  for i in $(seq 1 90); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$port/health 2>/dev/null)" = "200" ]; then
      echo ">> port $port READY after ${i}s"; return 0
    fi
    sleep 1
  done
  echo "!! port $port failed to become ready"; tail -20 "$logf"; return 1
}

# ---- Phase A: single-stream, --parallel 1 (noise floor + seed control) -------
SERIAL_FLAGS=(-ngl 99 -c 4096 --parallel 1 --no-webui)
start_server $PORT_SERIAL results/server_serial.log "${SERIAL_FLAGS[@]}" || exit 1

echo ">> exp1_serial"
python3 harness.py exp1_serial     --port $PORT_SERIAL --runs 5 --seed 42 > phase/exp1_serial.json
echo ">> exp2_seeds"
python3 harness.py exp2_seeds      --port $PORT_SERIAL                    > phase/exp2_seeds.json

kill "$LAST_PID" 2>/dev/null; lsof -ti tcp:$PORT_SERIAL | xargs kill -9 2>/dev/null; sleep 2

# ---- Phase B: concurrent, --parallel 4 + continuous batching -----------------
CONC_FLAGS=(-ngl 99 -c 4096 --parallel 4 --cont-batching --no-webui)
start_server $PORT_CONC results/server_concurrent.log "${CONC_FLAGS[@]}" || exit 1

echo ">> exp1_concurrent"
python3 harness.py exp1_concurrent --port $PORT_CONC --runs 5 --seed 42 > phase/exp1_concurrent.json

kill "$LAST_PID" 2>/dev/null; lsof -ti tcp:$PORT_CONC | xargs kill -9 2>/dev/null; sleep 1

# ---- Merge -------------------------------------------------------------------
echo ">> merge"
SERIAL_FLAGS_STR="${SERIAL_FLAGS[*]}" CONC_FLAGS_STR="${CONC_FLAGS[*]}" \
  LLAMACPP_VERSION="$LLAMACPP_VERSION" MODEL_BLOB="$BLOB" \
  python3 merge.py

echo ">> done -> results/local-run.json"
