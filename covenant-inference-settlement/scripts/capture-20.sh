#!/usr/bin/env bash
# Drive N real paid inference turns through the alpha gateway with settlement OFF and
# capture each turn's receipt_hash (the X-Covenant-Receipt header) in serve order.
# These are the exact 32-byte hashes that later fold into the on-chain provenance root.
set -euo pipefail

STATE="${COVENANT_ALPHA_STATE:?set COVENANT_ALPHA_STATE}"
ENV_FILE="$STATE/alpha.env"
[ -f "$ENV_FILE" ] || { echo "no $ENV_FILE - run alpha-up first" >&2; exit 1; }
. "$ENV_FILE"

N="${1:-20}"
OUT="${2:-$STATE/receipt-hashes.txt}"
: > "$OUT"

online="$(curl -fsS --max-time 5 "$BASE_URL/health" | jq -r '.online_nodes')"
[ "${online:-0}" -ge 1 ] || { echo "no node online at $BASE_URL" >&2; exit 1; }
echo "gateway healthy, $online node(s) online"

xpayment() {
  jq -cn --arg net "$RAIL_NETWORK" --arg ref "$1" --arg cred "$DEV_TOKEN" \
         --arg amt "$PRICE_MICRO" --arg payto "$PAY_TO" --arg asset "$RAIL_ASSET" \
    '{x402Version:1,scheme:"exact",network:$net,payload:{reference:$ref,credential:$cred,amount:$amt,payTo:$payto,asset:$asset}}' \
    | base64 | tr -d '\n'
}
chat_body() {
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
)

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
served=0
for i in $(seq 1 "$N"); do
  content="${PROMPTS[$(((i-1) % ${#PROMPTS[@]}))]} /no_think"
  code=""; tries=0
  while [ "$tries" -lt 8 ]; do
    tries=$((tries+1))
    ref="fold-$(date +%s)-$i-$tries-$RANDOM"
    code="$(curl -s -D "$WORK/h" -o "$WORK/b" -w '%{http_code}' --max-time 120 \
      "$BASE_URL/v1/chat/completions" -H 'content-type: application/json' \
      -H "x-payment: $(xpayment "$ref")" -d "$(chat_body "$content")")"
    [ "$code" = "503" ] || break
    sleep 0.5
  done
  [ "$code" = "200" ] || { echo "[$i] FAIL http=$code: $(head -c 160 "$WORK/b")" >&2; exit 1; }
  receipt="$(grep -i '^x-covenant-receipt:' "$WORK/h" | awk '{print $2}' | tr -d '\r')"
  [ -n "$receipt" ] || { echo "[$i] no receipt header" >&2; exit 1; }
  [ "${#receipt}" -eq 64 ] || { echo "[$i] receipt not 32 bytes: $receipt" >&2; exit 1; }
  echo "$receipt" >> "$OUT"
  served=$((served+1))
  printf '[%2d/%d] receipt %s\n' "$i" "$N" "$receipt"
done
echo "captured $served receipt hashes -> $OUT"
