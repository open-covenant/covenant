#!/usr/bin/env bash
# Bring up the closed-alpha inference stack on this machine and leave it running:
#
#   llama.cpp (qwen3)  <-  covenant-inferd serve  <-  [outbound mTLS tunnel]  <-  gateway
#
# The gateway runs with the payment gate armed against the dev prefunded verifier, so
# the whole 402 -> pay -> serve -> receipt loop is live without touching a chain or a
# key. Real-USDC settlement is the gated cutover and is deliberately not wired here.
#
# All runtime state (throwaway certs, pids, logs, the node identity, the receipts log)
# lives under $STATE, never in the repo. Idempotent: if a node is already online at the
# gateway this prints the client details and exits without touching anything.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATEWAY_CRATE="$(cd "$SCRIPT_DIR/../.." && pwd)"
NODE_CRATE="$(cd "$GATEWAY_CRATE/../covenant-inference-node" && pwd)"

STATE="${COVENANT_ALPHA_STATE:-$HOME/.local/state/covenant-inference-alpha}"
CERTS="$STATE/certs"
PIDS="$STATE/pids"
LOGS="$STATE/logs"
ENV_FILE="$STATE/alpha.env"
DEVICE="$STATE/device.json"
RECEIPTS_LOG="$STATE/receipts.jsonl"

MODEL="qwen3:8b"
LLAMA_PORT=28080
NODE_PORT=28090
GW_HTTP=28091
GW_TUNNEL=27443
BASE_URL="http://127.0.0.1:$GW_HTTP"

# Optional on-chain provenance fold. Set SETTLEMENT_URL to the settlement bridge base
# URL (e.g. http://127.0.0.1:8799) to fold each receipt into the on-chain
# provenance_root via a gasless consume_credits on a MagicBlock ER. Off by default —
# the stack, its e2e, and the soak are unchanged when it is unset.
SETTLEMENT_URL="${SETTLEMENT_URL:-}"
SETTLEMENT_AMOUNT="${SETTLEMENT_AMOUNT:-1}"

PRICE_MICRO=250000
RAIL_NETWORK="solana:devnet"
RAIL_ASSET="4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"   # devnet USDC mint
PAY_TO="CovAgentDevnetPayTo1111111111111111111111111"        # devnet placeholder payee
OPERATOR_WALLET="CovAgentOperatorDevnet11111111111111111111111"
PAYOUT_WALLET="CovAgentPayoutDevnet111111111111111111111111"

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '   %s\n' "$*"; }
die()  { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

pidfile() { echo "$PIDS/$1.pid"; }

alive() { # name -> 0 if the recorded pid is running
  local f; f="$(pidfile "$1")"
  [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null
}

start() { # name -- command...
  local name="$1"; shift
  "$@" >"$LOGS/$name.log" 2>&1 &
  echo $! >"$(pidfile "$name")"
  info "$name pid $! (log: $LOGS/$name.log)"
}

wait_for() { # url [jq-filter] [expected]
  local url="$1" filter="${2:-}" want="${3:-}" i body got
  for i in $(seq 1 300); do
    if body="$(curl -fsS --max-time 5 "$url" 2>/dev/null)"; then
      [ -z "$filter" ] && return 0
      got="$(printf '%s' "$body" | jq -r "$filter" 2>/dev/null || true)"
      if [ -n "$want" ]; then
        [ "$got" = "$want" ] && return 0
      elif [ -n "$got" ] && [ "$got" != "null" ] && [ "$got" != "0" ]; then
        return 0
      fi
    fi
    sleep 1
  done
  return 1
}

mkdir -p "$CERTS" "$PIDS" "$LOGS"

if node_count="$(curl -fsS --max-time 5 "$BASE_URL/health" 2>/dev/null | jq -r '.online_nodes' 2>/dev/null)" \
   && [ "${node_count:-0}" -ge 1 ] 2>/dev/null; then
  say "alpha stack already up ($node_count node(s) online) — nothing to do"
  [ -f "$ENV_FILE" ] && . "$ENV_FILE"
  info "client base url : $BASE_URL"
  info "dev token       : ${DEV_TOKEN:-<see $ENV_FILE>}"
  info "receipts log    : $RECEIPTS_LOG"
  exit 0
fi

say "clearing any stale processes from a previous run"
for name in tunnel heartbeat gateway node-serve llama; do
  if alive "$name"; then
    pid="$(cat "$(pidfile "$name")")"
    kill "$pid" 2>/dev/null || true
    info "killed stale $name (pid $pid)"
  fi
  rm -f "$(pidfile "$name")"
done
sleep 1

say "building gateway + node (release)"
cargo build --release --quiet --manifest-path "$GATEWAY_CRATE/Cargo.toml"
cargo build --release --quiet --manifest-path "$NODE_CRATE/Cargo.toml"
GW_BIN="$GATEWAY_CRATE/target/release/covenant-inference-gateway"
NODE_BIN="$NODE_CRATE/target/release/covenant-inferd"
[ -x "$GW_BIN" ] || die "gateway binary missing at $GW_BIN"
[ -x "$NODE_BIN" ] || die "node binary missing at $NODE_BIN"

say "resolving $MODEL weights via ollama"
GGUF="$(ollama show "$MODEL" --modelfile | awk '/^FROM /{print $2; exit}')"
[ -f "$GGUF" ] || die "could not resolve gguf for $MODEL (got: $GGUF)"
info "gguf: $GGUF"

if [ ! -f "$CERTS/ca.crt" ] || [ ! -f "$CERTS/server.crt" ] || [ ! -f "$CERTS/client.crt" ]; then
  say "minting throwaway mTLS trust (CA + gateway server cert + node client cert)"
  (
    cd "$CERTS"
    cat > ca.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = covenant-inference-alpha-ca
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
    openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 30 -config ca.cnf >/dev/null 2>&1
    openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -config server.cnf >/dev/null 2>&1
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 30 \
      -extfile server.cnf -extensions v3_req >/dev/null 2>&1
    openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -config client.cnf >/dev/null 2>&1
    openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 30 \
      -extfile client.cnf -extensions v3_req >/dev/null 2>&1
  )
  chmod 600 "$CERTS"/*.key
else
  info "reusing existing certs under $CERTS"
fi

say "hashing weights for the enrollment identity (sha-256 over the gguf)"
WEIGHTS_HASH="$(shasum -a 256 "$GGUF" | awk '{print $1}')"
RV="alpha-$(llama-server --version 2>&1 | grep -oE '[0-9]{3,}' | head -1 || echo 0)"
info "weights hash: $WEIGHTS_HASH"
info "runtime version: $RV"

say "starting llama.cpp on the gguf (port $LLAMA_PORT)"
start llama llama-server -m "$GGUF" --host 127.0.0.1 --port "$LLAMA_PORT" -c 4096 --parallel 2
wait_for "http://127.0.0.1:$LLAMA_PORT/health" || die "llama-server never became ready ($LOGS/llama.log)"

say "starting node serve surface (port $NODE_PORT)"
start node-serve "$NODE_BIN" serve \
  --listen "127.0.0.1:$NODE_PORT" \
  --engine-url "http://127.0.0.1:$LLAMA_PORT" \
  --model "$MODEL" --weights "$GGUF" \
  --quantization q4_k_m --runtime llama.cpp --runtime-version "$RV" \
  --temperature 0 --top-p 1 --seed 0 --sampling-max-tokens 512
wait_for "http://127.0.0.1:$NODE_PORT/health" '.engine_ready' 'true' \
  || die "node serve never became ready ($LOGS/node-serve.log)"
DIGEST="$(curl -fsS "http://127.0.0.1:$NODE_PORT/v1/models" | jq -r '.data[0].model_identity_digest')"
[ -n "$DIGEST" ] && [ "$DIGEST" != "null" ] || die "node serve did not report a model identity digest"
info "served model identity digest: $DIGEST"

DEV_TOKEN="alpha-dev-$(openssl rand -hex 12)"
SETTLE_ARGS=()
if [ -n "$SETTLEMENT_URL" ]; then
  SETTLE_ARGS=(--settlement-url "$SETTLEMENT_URL" --settlement-amount "$SETTLEMENT_AMOUNT")
  info "settlement fold on: receipts fold into the on-chain provenance_root via $SETTLEMENT_URL"
fi
say "starting gateway with payment gate armed (price $PRICE_MICRO on $RAIL_NETWORK)"
start gateway "$GW_BIN" \
  --http-listen "127.0.0.1:$GW_HTTP" \
  --tunnel-listen "127.0.0.1:$GW_TUNNEL" \
  --tls-cert "$CERTS/server.crt" --tls-key "$CERTS/server.key" --client-ca "$CERTS/ca.crt" \
  --require-payment \
  --price-micro "$PRICE_MICRO" \
  --pay-to "$PAY_TO" \
  --rail-network "$RAIL_NETWORK" \
  --rail-asset "$RAIL_ASSET" \
  --dev-payment-token "$DEV_TOKEN" \
  --receipts-log "$RECEIPTS_LOG" \
  ${SETTLE_ARGS[@]+"${SETTLE_ARGS[@]}"}
wait_for "$BASE_URL/health" || die "gateway never became ready ($LOGS/gateway.log)"

say "creating node identity + enrolling with the gateway"
rm -f "$DEVICE"
NODE_ID="$("$NODE_BIN" create-identity --path "$DEVICE")"
info "node id: $NODE_ID"
"$NODE_BIN" enroll \
  --identity "$DEVICE" \
  --control-plane "$BASE_URL" \
  --operator-wallet "$OPERATOR_WALLET" \
  --payout-wallet "$PAYOUT_WALLET" \
  --weights-hash "$WEIGHTS_HASH" \
  --quantization q4_k_m --runtime llama.cpp --runtime-version "$RV" \
  --temperature 0 --top-p 1 --seed 0 --sampling-max-tokens 512 \
  --max-tokens-per-second 200 --rate-per-second 1000

say "opening the outbound mTLS tunnel + starting the heartbeat"
# Slots are parked reverse connections the gateway pops one-per-request. A node
# redials to refill, and its reconnect backoff escalates on any session shorter than
# 15s — so a thin pool starves under a burst. 32 keeps ample headroom parked for the
# soak; the client still retries a transient no-capacity, as a real payer would.
start tunnel "$NODE_BIN" tunnel \
  --identity "$DEVICE" \
  --gateway "127.0.0.1:$GW_TUNNEL" --server-name localhost \
  --ca-certificate "$CERTS/ca.crt" --client-certificate "$CERTS/client.crt" --client-key "$CERTS/client.key" \
  --connection-id alpha --target "127.0.0.1:$NODE_PORT" --slots 32
start heartbeat "$NODE_BIN" heartbeat \
  --identity "$DEVICE" \
  --control-plane "$BASE_URL" \
  --tunnel-connected --served-model-digest "$DIGEST" \
  --interval-seconds 5

wait_for "$BASE_URL/health" '.online_nodes' \
  || die "node never showed online at the gateway (heartbeat: $LOGS/heartbeat.log, tunnel: $LOGS/tunnel.log)"
sleep 3   # let the tunnel park its slots before the first client request

cat > "$ENV_FILE" <<EOF
# written by alpha-up.sh — sourced by alpha-soak.sh and alpha-down.sh
STATE="$STATE"
PIDS="$PIDS"
LOGS="$LOGS"
BASE_URL="$BASE_URL"
MODEL="$MODEL"
DEV_TOKEN="$DEV_TOKEN"
PRICE_MICRO="$PRICE_MICRO"
PAY_TO="$PAY_TO"
RAIL_NETWORK="$RAIL_NETWORK"
RAIL_ASSET="$RAIL_ASSET"
RECEIPTS_LOG="$RECEIPTS_LOG"
DIGEST="$DIGEST"
NODE_ID="$NODE_ID"
EOF

say "alpha stack up"
info "client base url : $BASE_URL"
info "model           : $MODEL"
info "dev payment token: $DEV_TOKEN"
info "receipts log    : $RECEIPTS_LOG"
info "gateway health  : $(curl -fsS "$BASE_URL/health")"
echo
info "next: scripts/alpha/alpha-soak.sh 30   (then scripts/alpha/alpha-down.sh)"
