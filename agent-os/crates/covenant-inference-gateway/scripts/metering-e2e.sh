#!/usr/bin/env bash
# End-to-end proof of Phase 3 metering on the full path: an OpenAI client hits the
# gateway over a node's outbound mTLS tunnel to a real llama.cpp engine on qwen3,
# with the payment gate armed.
#
#   (a) no X-PAYMENT      -> 402 + accepts[] carrying the price
#   (b) valid dev payment -> real qwen3 completion + an X-Covenant-Receipt header,
#                            and the receipt shows up under GET /v1/receipts
#
# The verifier is the DevPrefundedVerifier: it settles nothing, so this proves the
# 402 -> pay -> serve -> receipt loop without touching a chain or a key. Everything
# runs on loopback with a throwaway CA; every process is torn down on exit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATEWAY_CRATE="$(cd "$SCRIPT_DIR/.." && pwd)"
NODE_CRATE="$(cd "$GATEWAY_CRATE/../covenant-inference-node" && pwd)"

MODEL="qwen3:8b"
LLAMA_PORT=18080
NODE_PORT=18090
GW_HTTP=18091
GW_TUNNEL=17443

PRICE_MICRO=250000
PAY_TO="8xbXHAh15QHqvS8Hh7cGxtVQD1TKtqZgQ2n7bK9K1Zop"
RAIL_NETWORK="solana:devnet"
RAIL_ASSET="4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
DEV_TOKEN="dev-prefunded-$(date +%s)"

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

wait_for() { # url [jq-filter] [expected]
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

say "starting gateway with payment gate armed (price $PRICE_MICRO on $RAIL_NETWORK)"
"$GW_BIN" \
  --http-listen "127.0.0.1:$GW_HTTP" \
  --tunnel-listen "127.0.0.1:$GW_TUNNEL" \
  --tls-cert server.crt --tls-key server.key --client-ca ca.crt \
  --require-payment \
  --price-micro "$PRICE_MICRO" \
  --pay-to "$PAY_TO" \
  --rail-network "$RAIL_NETWORK" \
  --rail-asset "$RAIL_ASSET" \
  --dev-payment-token "$DEV_TOKEN" \
  --receipts-log "$WORK/receipts.log" \
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

REQ='{"model":"'"$MODEL"'","messages":[{"role":"user","content":"say hi in three words /no_think"}],"max_tokens":16,"temperature":0}'

say "(a) unpaid request -> expect 402 + accepts[] with the price"
UNPAID_CODE="$(curl -s -o "$WORK/unpaid.json" -w '%{http_code}' --max-time 30 \
  "http://127.0.0.1:$GW_HTTP/v1/chat/completions" \
  -H 'content-type: application/json' -d "$REQ")"
echo "402 body: $(cat "$WORK/unpaid.json")"
[ "$UNPAID_CODE" = "402" ] || die "expected 402 without payment, got $UNPAID_CODE"
[ "$(jq -r '.x402Version' "$WORK/unpaid.json")" = "1" ] || die "402 body missing x402Version"
ADV_PRICE="$(jq -r '.accepts[0].maxAmountRequired' "$WORK/unpaid.json")"
[ "$ADV_PRICE" = "$PRICE_MICRO" ] || die "402 advertised price $ADV_PRICE, expected $PRICE_MICRO"
[ "$(jq -r '.accepts[0].network' "$WORK/unpaid.json")" = "$RAIL_NETWORK" ] || die "402 network mismatch"
[ "$(jq -r '.accepts[0].payTo' "$WORK/unpaid.json")" = "$PAY_TO" ] || die "402 payTo mismatch"

say "(b) paid request -> expect a real qwen3 completion + a receipt header"
REF="pay-$(date +%s)-$RANDOM"
XPAY="$(printf '{"x402Version":1,"scheme":"exact","network":"%s","payload":{"reference":"%s","credential":"%s","amount":"%s","payTo":"%s","asset":"%s"}}' \
  "$RAIL_NETWORK" "$REF" "$DEV_TOKEN" "$PRICE_MICRO" "$PAY_TO" "$RAIL_ASSET" | base64 | tr -d '\n')"
PAID_CODE="$(curl -s -D "$WORK/paid.headers" -o "$WORK/paid.json" -w '%{http_code}' --max-time 120 \
  "http://127.0.0.1:$GW_HTTP/v1/chat/completions" \
  -H 'content-type: application/json' -H "x-payment: $XPAY" -d "$REQ")"
[ "$PAID_CODE" = "200" ] || die "paid request did not return 200 (got $PAID_CODE): $(cat "$WORK/paid.json")"
echo "raw completion: $(cat "$WORK/paid.json")"
CONTENT="$(jq -r '.choices[0].message.content' "$WORK/paid.json")"
[ -n "$CONTENT" ] && [ "$CONTENT" != "null" ] || die "no completion content routed back"
RECEIPT="$(grep -i '^x-covenant-receipt:' "$WORK/paid.headers" | awk '{print $2}' | tr -d '\r')"
[ -n "$RECEIPT" ] || die "no X-Covenant-Receipt header on the paid response"
echo "receipt hash: $RECEIPT"

say "checking the receipt is queryable and payment-tagged"
RECENT="$(curl -fsS "http://127.0.0.1:$GW_HTTP/v1/receipts")"
echo "GET /v1/receipts: $RECENT"
FOUND="$(printf '%s' "$RECENT" | jq -r --arg h "$RECEIPT" '.data[] | select(.receipt_hash==$h) | .receipt_hash')"
[ "$FOUND" = "$RECEIPT" ] || die "receipt $RECEIPT not present in GET /v1/receipts"
ONE="$(curl -fsS "http://127.0.0.1:$GW_HTTP/v1/receipts/$RECEIPT")"
[ "$(printf '%s' "$ONE" | jq -r '.model_identity_digest')" = "$DIGEST" ] || die "receipt digest mismatch"
[ "$(printf '%s' "$ONE" | jq -r '.amount_micro')" = "$PRICE_MICRO" ] || die "receipt amount mismatch"
[ "$(printf '%s' "$ONE" | jq -r '.payment_reference')" = "$REF" ] || die "receipt payment_reference mismatch"

say "privacy: the receipts log must hold no prompt/output plaintext"
grep -qi 'say hi' "$WORK/receipts.log" && die "receipts log leaked the prompt plaintext"
grep -qiF "$CONTENT" "$WORK/receipts.log" && die "receipts log leaked the completion plaintext"
echo "receipts log line: $(cat "$WORK/receipts.log")"

say "replay: reusing the same payment reference must be refused"
REPLAY_CODE="$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 \
  "http://127.0.0.1:$GW_HTTP/v1/chat/completions" \
  -H 'content-type: application/json' -H "x-payment: $XPAY" -d "$REQ")"
[ "$REPLAY_CODE" = "402" ] || die "replayed payment should be refused with 402, got $REPLAY_CODE"

say "METERING E2E PASSED (402 gate + paid+receipted qwen3 over the mTLS tunnel)"
echo "model:      $MODEL"
echo "digest:     $DIGEST"
echo "price:      $PRICE_MICRO ($RAIL_ASSET on $RAIL_NETWORK)"
echo "receipt:    $RECEIPT"
echo "completion: $CONTENT"
