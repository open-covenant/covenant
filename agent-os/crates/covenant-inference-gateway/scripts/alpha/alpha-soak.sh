#!/usr/bin/env bash
# Drive real metered traffic through the alpha gateway and prove the loop holds.
#
#   1. one unpaid request must come back 402 with an accepts[] carrying the price
#   2. N varied paid requests (unique payment reference each, so no replay) must each
#      return a real assistant completion and an X-Covenant-Receipt header
#   3. GET /v1/receipts must have grown by exactly the number served
#   4. the receipts log must contain no prompt plaintext (privacy)
#
# Usage: alpha-soak.sh [N]   (default 30). Requires alpha-up.sh to have run first.
set -euo pipefail

STATE="${COVENANT_ALPHA_STATE:-$HOME/.local/state/covenant-inference-alpha}"
ENV_FILE="$STATE/alpha.env"
[ -f "$ENV_FILE" ] || { echo "no $ENV_FILE — run alpha-up.sh first" >&2; exit 1; }
# shellcheck source=/dev/null
. "$ENV_FILE"

N="${1:-30}"
[[ "$N" =~ ^[0-9]+$ ]] && [ "$N" -ge 1 ] || { echo "N must be a positive integer" >&2; exit 1; }

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die()  { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

online="$(curl -fsS --max-time 5 "$BASE_URL/health" 2>/dev/null | jq -r '.online_nodes' 2>/dev/null || echo 0)"
[ "${online:-0}" -ge 1 ] 2>/dev/null || die "no node online at $BASE_URL — is the stack up?"
say "gateway healthy, $online node(s) online at $BASE_URL"

# A single-use payment envelope for the dev prefunded verifier.
xpayment() { # reference
  jq -cn --arg net "$RAIL_NETWORK" --arg ref "$1" --arg cred "$DEV_TOKEN" \
         --arg amt "$PRICE_MICRO" --arg payto "$PAY_TO" --arg asset "$RAIL_ASSET" \
    '{x402Version:1,scheme:"exact",network:$net,payload:{reference:$ref,credential:$cred,amount:$amt,payTo:$payto,asset:$asset}}' \
    | base64 | tr -d '\n'
}

chat_body() { # content
  jq -cn --arg m "$MODEL" --arg c "$1" \
    '{model:$m,messages:[{role:"user",content:$c}],max_tokens:64,temperature:0}'
}

PROMPTS=(
  "In one sentence, what is a merkle tree?"
  "Name three primary colors."
  "What is the boiling point of water at sea level in Celsius?"
  "Give a haiku about the ocean."
  "Translate 'good morning' into Spanish."
  "What is 17 multiplied by 23?"
  "List two benefits of unit tests."
  "Who wrote the play Hamlet?"
  "Explain what a hash function does in one line."
  "What gas do plants absorb during photosynthesis?"
  "Convert 100 kilometers to miles, roughly."
  "Give one tip for writing readable code."
  "What is the capital of Japan?"
  "Summarize what TCP does in a sentence."
  "Name a programming language known for memory safety."
  "What is the freezing point of water in Fahrenheit?"
  "Give an antonym for 'transparent'."
  "What does the acronym API stand for?"
  "Write a one-line motivational note for a builder."
  "What is the square root of 144?"
  "Name the largest planet in the solar system."
  "Explain idempotency in one sentence."
  "What year did the first moon landing happen?"
  "Give a synonym for 'robust'."
)

CANARY="ALPHA-CANARY-$(openssl rand -hex 4)"
CANARY_IDX=1

say "(guard) unpaid request must be refused with 402 + accepts[]"
UNPAID_CODE="$(curl -s -o "$WORK/unpaid.json" -w '%{http_code}' --max-time 30 \
  "$BASE_URL/v1/chat/completions" -H 'content-type: application/json' \
  -d "$(chat_body 'this should never be served without payment /no_think')")"
[ "$UNPAID_CODE" = "402" ] || die "unpaid request returned $UNPAID_CODE, expected 402"
[ "$(jq -r '.x402Version' "$WORK/unpaid.json")" = "1" ] || die "402 body missing x402Version"
[ "$(jq -r '.accepts | length' "$WORK/unpaid.json")" -ge 1 ] || die "402 body has no accepts[]"
[ "$(jq -r '.accepts[0].maxAmountRequired' "$WORK/unpaid.json")" = "$PRICE_MICRO" ] \
  || die "402 advertised the wrong price"
[ "$(jq -r '.accepts[0].payTo' "$WORK/unpaid.json")" = "$PAY_TO" ] || die "402 payTo mismatch"
echo "   402 held: $(jq -c '{x402Version,accepts:[.accepts[0]|{network,maxAmountRequired,payTo,asset}]}' "$WORK/unpaid.json")"

BEFORE="$(curl -fsS "$BASE_URL/v1/receipts?limit=1000" | jq '.data | length')"
say "receipts in ring before soak: $BEFORE"

say "sending $N paid requests through POST /v1/chat/completions"
SERVED=0
FAILED=0
RETRIES=0
MAX_TRIES=8
: > "$WORK/latencies"
SAMPLE_RECEIPT=""
for i in $(seq 1 "$N"); do
  base="${PROMPTS[$(( (i - 1) % ${#PROMPTS[@]} ))]}"
  if [ "$i" -eq "$CANARY_IDX" ]; then
    content="Remember this codeword and repeat nothing else about it: $CANARY. $base /no_think"
  else
    content="$base /no_think"
  fi

  # A 503 is a transient no-capacity (the node is mid-redial): the gateway refunds
  # the reference, so retry with a fresh one — exactly what a real payer's client does.
  code=""; secs=""; tries=0
  while [ "$tries" -lt "$MAX_TRIES" ]; do
    tries=$((tries + 1))
    ref="alpha-$(date +%s)-$i-$tries-$RANDOM"
    meas="$(curl -s -D "$WORK/h" -o "$WORK/b" -w '%{http_code} %{time_total}' --max-time 120 \
      "$BASE_URL/v1/chat/completions" \
      -H 'content-type: application/json' -H "x-payment: $(xpayment "$ref")" \
      -d "$(chat_body "$content")")"
    code="${meas%% *}"; secs="${meas##* }"
    [ "$code" = "503" ] || break
    RETRIES=$((RETRIES + 1))
    sleep 0.5
  done
  retry_note=""; [ "$tries" -gt 1 ] && retry_note=" (${tries} tries)"

  if [ "$code" != "200" ]; then
    FAILED=$((FAILED + 1))
    printf '   [%2d/%d] FAIL http=%s%s: %s\n' "$i" "$N" "$code" "$retry_note" "$(head -c 160 "$WORK/b")"
    continue
  fi
  reply="$(python3 -c 'import json,sys
m=json.load(open(sys.argv[1]))["choices"][0]["message"]
print((m.get("content") or m.get("reasoning_content") or "").strip())' "$WORK/b" 2>/dev/null || true)"
  receipt="$(grep -i '^x-covenant-receipt:' "$WORK/h" | awk '{print $2}' | tr -d '\r')"
  if [ -z "$reply" ]; then
    FAILED=$((FAILED + 1)); printf '   [%2d/%d] FAIL empty completion%s\n' "$i" "$N" "$retry_note"; continue
  fi
  if [ -z "$receipt" ]; then
    FAILED=$((FAILED + 1)); printf '   [%2d/%d] FAIL no receipt header%s\n' "$i" "$N" "$retry_note"; continue
  fi
  SERVED=$((SERVED + 1))
  echo "$secs" >> "$WORK/latencies"
  [ -z "$SAMPLE_RECEIPT" ] && SAMPLE_RECEIPT="$receipt"
  printf '   [%2d/%d] ok  %5.2fs  receipt %s…%s  %s\n' "$i" "$N" "$secs" "${receipt:0:12}" "$retry_note" "$(echo "$reply" | tr '\n' ' ' | cut -c1-44)"
done

AFTER="$(curl -fsS "$BASE_URL/v1/receipts?limit=1000" | jq '.data | length')"
GROWTH=$((AFTER - BEFORE))

say "asserting receipt count grew by the number served"
[ "$GROWTH" -eq "$SERVED" ] || die "receipts grew by $GROWTH but $SERVED were served"
echo "   receipts: $BEFORE -> $AFTER (+$GROWTH), served $SERVED"

say "privacy: the receipts log must hold no prompt plaintext"
[ -f "$RECEIPTS_LOG" ] || die "receipts log missing at $RECEIPTS_LOG"
grep -qiF "$CANARY" "$RECEIPTS_LOG" && die "receipts log leaked the canary codeword ($CANARY)"
grep -qi 'photosynthesis' "$RECEIPTS_LOG" && die "receipts log leaked a prompt word"
grep -qi 'merkle' "$RECEIPTS_LOG" && die "receipts log leaked a prompt word"
echo "   canary '$CANARY' and sampled prompt words are absent from $RECEIPTS_LOG"

read -r LMIN LAVG LMAX < <(awk '
  NR==1{min=max=$1}
  {sum+=$1; if($1<min)min=$1; if($1>max)max=$1}
  END{ if(NR>0) printf "%.2f %.2f %.2f\n", min, sum/NR, max; else print "0 0 0" }' "$WORK/latencies") || true

say "SOAK SUMMARY"
printf '   requested       : %d\n' "$N"
printf '   served          : %d\n' "$SERVED"
printf '   failed          : %d\n' "$FAILED"
printf '   503 retries     : %d (transient no-capacity, refunded + re-sent)\n' "$RETRIES"
printf '   receipts (ring) : %d -> %d (+%d)\n' "$BEFORE" "$AFTER" "$GROWTH"
printf '   latency (s)     : min %s  avg %s  max %s\n' "$LMIN" "$LAVG" "$LMAX"
printf '   402 gate        : held (unpaid -> 402 + accepts[])\n'
printf '   privacy         : receipts log carries hashes only\n'
if [ -n "$SAMPLE_RECEIPT" ]; then
  echo "   sample receipt  :"
  curl -fsS "$BASE_URL/v1/receipts/$SAMPLE_RECEIPT" | jq . | sed 's/^/     /'
fi

[ "$SERVED" -eq "$N" ] || die "only $SERVED/$N requests served — see failures above"
say "SOAK PASSED ($SERVED/$N served, receipts +$GROWTH, 402 held, no plaintext leaked)"
