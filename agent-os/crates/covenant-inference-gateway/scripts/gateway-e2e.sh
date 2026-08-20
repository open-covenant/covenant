#!/usr/bin/env bash
# End-to-end proof of the full path: an OpenAI client hits the gateway, which routes
# by model over a node's outbound mTLS tunnel to the node's serve surface and on to a
# real llama.cpp engine running the qwen3 gguf. A completion coming back is proof the
# whole gateway -> tunnel -> node -> engine chain is live.
#
# Everything runs on loopback with a throwaway CA minted here. No state escapes the
# temp dir, and every process is torn down on exit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATEWAY_CRATE="$(cd "$SCRIPT_DIR/.." && pwd)"
NODE_CRATE="$(cd "$GATEWAY_CRATE/../covenant-inference-node" && pwd)"

MODEL="qwen3:8b"
LLAMA_PORT=18080
NODE_PORT=18090
GW_HTTP=18091
GW_TUNNEL=17443

WORK="$(mktemp -d)"
PIDS=()

cleanup() {
  set +e
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
  done
  sleep 0.5
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

wait_for() { # url [jq-filter] [expected]  — polls until the endpoint answers as wanted
  local url="$1" filter="${2:-}" want="${3:-}" i
  for i in $(seq 1 180); do
    if body="$(curl -fsS --max-time 5 "$url" 2>/dev/null)"; then
      if [ -z "$filter" ]; then return 0; fi
      got="$(printf '%s' "$body" | jq -r "$filter" 2>/dev/null || true)"
      if [ -n "$want" ]; then
        [ "$got" = "$want" ] && return 0
      else
        [ -n "$got" ] && [ "$got" != "null" ] && [ "$got" != "0" ] && return 0
      fi
    fi
    sleep 1
  done
  return 1
}

# Release build: the node hashes the multi-gigabyte gguf into its model identity at
# startup, and debug sha2 crawls through it. Optimised, it is a few seconds.
say "building gateway + node (release)"
cargo build --release --quiet --manifest-path "$GATEWAY_CRATE/Cargo.toml"
cargo build --release --quiet --manifest-path "$NODE_CRATE/Cargo.toml"
GW_BIN="$GATEWAY_CRATE/target/release/covenant-inference-gateway"
NODE_BIN="$NODE_CRATE/target/release/covenant-inferd"

say "resolving $MODEL weights via ollama"
GGUF="$(ollama show "$MODEL" --modelfile | awk '/^FROM /{print $2; exit}')"
[ -f "$GGUF" ] || die "could not resolve gguf for $MODEL (got: $GGUF)"
echo "gguf: $GGUF"

say "minting throwaway CA + node/gateway certs"
cd "$WORK"
cat > ca.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = covenant-inference-test-ca
[v3_ca]
basicConstraints = critical,CA:true
keyUsage = critical,keyCertSign,cRLSign
EOF
cat > server.cnf <<'EOF'
[req]
distinguished_name = dn
req_extensions = v3_req
prompt = no
[dn]
CN = localhost
[v3_req]
basicConstraints = CA:false
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = @alt
[alt]
DNS.1 = localhost
IP.1 = 127.0.0.1
EOF
cat > client.cnf <<'EOF'
[req]
distinguished_name = dn
req_extensions = v3_req
prompt = no
[dn]
CN = covenant-inference-node
[v3_req]
basicConstraints = CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
EOF
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 2 -config ca.cnf >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -config server.cnf >/dev/null 2>&1
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 2 \
  -extfile server.cnf -extensions v3_req >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -config client.cnf >/dev/null 2>&1
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 2 \
  -extfile client.cnf -extensions v3_req >/dev/null 2>&1

say "starting llama.cpp on the gguf (port $LLAMA_PORT)"
llama-server -m "$GGUF" --host 127.0.0.1 --port "$LLAMA_PORT" -c 4096 --parallel 2 \
  > "$WORK/llama.log" 2>&1 &
PIDS+=($!)
wait_for "http://127.0.0.1:$LLAMA_PORT/health" || die "llama-server never became ready (see $WORK/llama.log)"

# Identity fields the node serve surface bakes into the model identity. Passed to
# enroll too so the enrolled digest matches what the node actually serves.
WEIGHTS_HASH="$(shasum -a 256 "$GGUF" | awk '{print $1}')"
RV="e2e-$(llama-server --version 2>&1 | grep -oE '[0-9]{3,}' | head -1 || echo 0)"

say "starting node serve surface (port $NODE_PORT)"
"$NODE_BIN" serve \
  --listen "127.0.0.1:$NODE_PORT" \
  --engine-url "http://127.0.0.1:$LLAMA_PORT" \
  --model "$MODEL" --weights "$GGUF" \
  --quantization q4_k_m --runtime llama.cpp --runtime-version "$RV" \
  --temperature 0 --top-p 1 --seed 0 --sampling-max-tokens 512 \
  > "$WORK/node-serve.log" 2>&1 &
PIDS+=($!)
wait_for "http://127.0.0.1:$NODE_PORT/health" '.engine_ready' || die "node serve never became ready (see $WORK/node-serve.log)"
DIGEST="$(curl -fsS "http://127.0.0.1:$NODE_PORT/v1/models" | jq -r '.data[0].model_identity_digest')"
echo "served model identity digest: $DIGEST"

say "starting gateway (http $GW_HTTP, tunnel $GW_TUNNEL)"
"$GW_BIN" \
  --http-listen "127.0.0.1:$GW_HTTP" \
  --tunnel-listen "127.0.0.1:$GW_TUNNEL" \
  --tls-cert server.crt --tls-key server.key --client-ca ca.crt \
  > "$WORK/gateway.log" 2>&1 &
PIDS+=($!)
wait_for "http://127.0.0.1:$GW_HTTP/health" || die "gateway never became ready (see $WORK/gateway.log)"

say "creating node identity + enrolling"
NODE_ID="$("$NODE_BIN" create-identity --path "$WORK/device.json")"
echo "node id: $NODE_ID"
"$NODE_BIN" enroll \
  --identity "$WORK/device.json" \
  --control-plane "http://127.0.0.1:$GW_HTTP" \
  --operator-wallet 8xbXHAh15QHqvS8Hh7cGxtVQD1TKtqZgQ2n7bK9K1Zop \
  --payout-wallet 5vJRzKtcpwzT8Vq3fY8f7pQe9uW1cDp2b6z3s4t5u6v7 \
  --weights-hash "$WEIGHTS_HASH" \
  --quantization q4_k_m --runtime llama.cpp --runtime-version "$RV" \
  --temperature 0 --top-p 1 --seed 0 --sampling-max-tokens 512 \
  --max-tokens-per-second 200 --rate-per-second 1000

say "starting heartbeat + outbound tunnel"
"$NODE_BIN" heartbeat \
  --identity "$WORK/device.json" \
  --control-plane "http://127.0.0.1:$GW_HTTP" \
  --tunnel-connected --served-model-digest "$DIGEST" \
  --interval-seconds 5 > "$WORK/heartbeat.log" 2>&1 &
PIDS+=($!)
"$NODE_BIN" tunnel \
  --identity "$WORK/device.json" \
  --gateway "127.0.0.1:$GW_TUNNEL" --server-name localhost \
  --ca-certificate ca.crt --client-certificate client.crt --client-key client.key \
  --connection-id e2e --target "127.0.0.1:$NODE_PORT" --slots 4 \
  > "$WORK/tunnel.log" 2>&1 &
PIDS+=($!)

wait_for "http://127.0.0.1:$GW_HTTP/health" '.online_nodes' \
  || die "node never showed online at the gateway (heartbeat: $WORK/heartbeat.log, tunnel: $WORK/tunnel.log)"
echo "gateway health: $(curl -fsS http://127.0.0.1:$GW_HTTP/health)"

say "routing a chat completion through gateway -> tunnel -> node -> engine"
RESP="$(curl -fsS --max-time 120 "http://127.0.0.1:$GW_HTTP/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"model":"'"$MODEL"'","messages":[{"role":"user","content":"say hi in three words /no_think"}],"max_tokens":16,"temperature":0}')"
echo "raw response: $RESP"
CONTENT="$(printf '%s' "$RESP" | jq -r '.choices[0].message.content')"
[ -n "$CONTENT" ] && [ "$CONTENT" != "null" ] || die "no completion content routed back"

say "checking /v1/models union"
MODELS="$(curl -fsS "http://127.0.0.1:$GW_HTTP/v1/models")"
echo "models: $MODELS"
GW_DIGEST="$(printf '%s' "$MODELS" | jq -r --arg m "$MODEL" '.data[] | select(.id==$m) | .model_identity_digest')"
[ "$GW_DIGEST" = "$DIGEST" ] || die "/v1/models did not list $MODEL with its digest (got: $GW_DIGEST)"

say "E2E PASSED (full mTLS tunnel path)"
echo "model:      $MODEL"
echo "digest:     $GW_DIGEST"
echo "completion: $CONTENT"
