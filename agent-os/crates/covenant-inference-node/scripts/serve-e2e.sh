#!/usr/bin/env bash
# End-to-end proof that `covenant-inferd serve` fronts a real llama.cpp engine:
# resolve the qwen3 GGUF from ollama, boot llama-server, put serve in front of it,
# and drive a real completion + the identity-bearing /health through the serve hop.
#
# Requires: llama-server (llama.cpp), ollama with `qwen3:8b` pulled.
set -euo pipefail

MODEL="${MODEL:-qwen3:8b}"
ENGINE_PORT="${ENGINE_PORT:-8080}"
SERVE_PORT="${SERVE_PORT:-8090}"
ENGINE_BIN="${ENGINE_BIN:-llama-server}"
CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

GGUF="$(ollama show "$MODEL" --modelfile | awk '/^FROM /{print $2; exit}')"
[[ -f "$GGUF" ]] || { echo "could not resolve GGUF for $MODEL" >&2; exit 1; }
echo "weights: $GGUF"

engine_pid=""
serve_pid=""
cleanup() {
  [[ -n "$serve_pid" ]] && kill "$serve_pid" 2>/dev/null || true
  [[ -n "$engine_pid" ]] && kill "$engine_pid" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

wait_health() {
  local url="$1" label="$2" deadline=$(( SECONDS + ${3:-120} ))
  until curl -fsS "$url" >/dev/null 2>&1; do
    (( SECONDS >= deadline )) && { echo "$label did not become ready" >&2; exit 1; }
    sleep 1
  done
  echo "$label ready"
}

echo "== building covenant-inferd =="
( cd "$CRATE_DIR" && cargo build --quiet )
INFERD="$CRATE_DIR/target/debug/covenant-inferd"

echo "== starting llama-server on :$ENGINE_PORT =="
"$ENGINE_BIN" -m "$GGUF" --port "$ENGINE_PORT" -c 4096 --parallel 1 >/tmp/llama-e2e.log 2>&1 &
engine_pid=$!
wait_health "http://127.0.0.1:$ENGINE_PORT/health" "llama-server" 180

echo "== starting serve on :$SERVE_PORT =="
"$INFERD" serve \
  --engine-url "http://127.0.0.1:$ENGINE_PORT" \
  --weights "$GGUF" \
  --model "$MODEL" \
  --listen "127.0.0.1:$SERVE_PORT" >/tmp/serve-e2e.log 2>&1 &
serve_pid=$!
# serve hashes the multi-GB weights before it binds; allow for a cold-cache read.
wait_health "http://127.0.0.1:$SERVE_PORT/health" "serve" 300

echo "== chat completion through serve =="
# qwen3 is a reasoning model; `/no_think` keeps the answer in `content` instead of
# burning the token budget on a reasoning trace.
completion="$(curl -fsS "http://127.0.0.1:$SERVE_PORT/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"say hi in exactly three words /no_think\"}],\"max_tokens\":64,\"temperature\":0}")"
echo "$completion"
content="$(echo "$completion" | python3 -c 'import json,sys; m=json.load(sys.stdin)["choices"][0]["message"]; print((m.get("content") or m.get("reasoning_content") or "").strip())')"
[[ -n "$content" ]] || { echo "empty completion" >&2; exit 1; }
echo "assistant said: $content"

echo "== /health (identity digest) =="
curl -fsS "http://127.0.0.1:$SERVE_PORT/health"; echo

echo "e2e ok"
