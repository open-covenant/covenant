#!/usr/bin/env bash
# Covenant Compute, end to end, in under a minute.
#
# One command tells the whole story: a real GPU rented on a live market and
# paid for in USDC on Solana mainnet, the same lease arithmetic running as an
# onchain meter one tick per second inside a MagicBlock Ephemeral Rollup, and
# the meter refusing eight attempts to drive it from the wrong key.
#
#   ./demo/run-demo.sh --dry-run     replay the recorded run; no keys, no spend
#   ./demo/run-demo.sh               run what this machine can, replay the rest
#   ./demo/run-demo.sh --live        insist on live; fail loudly if it cannot
#
# Nothing here prints key material and nothing pretends. Every line that comes
# from a recording is marked "replay", every live leg says which network it is
# on, and a leg that needs a funded key says which key and skips.

set -uo pipefail

# ------------------------------------------------------------------ locations

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${DEMO_DIR}/.." && pwd)"
RECORDING="${COVENANT_DEMO_RECORDING:-${DEMO_DIR}/recorded-run.json}"
KEYDIR="${REPO}/scratchpad/compute-mainnet"
ER_CLIENT="${REPO}/programs/compute-lease/client/lease-er.mjs"

MAINNET_RPC="${COVENANT_DEMO_MAINNET_RPC:-https://api.mainnet-beta.solana.com}"
DEVNET_RPC="${COVENANT_DEMO_DEVNET_RPC:-https://api.devnet.solana.com}"

# --------------------------------------------------------------------- flags

MODE="auto"        # auto | replay | live
WANT_GPU=1
WANT_METER=1
WANT_ATTACK=1
FAST=0
OFFLINE=0
SPEND=0
COLOR=auto
TICKS=60

usage() {
  cat <<'USAGE'
Covenant Compute demo.

  ./demo/run-demo.sh [options]

Modes
  --dry-run        Replay the recorded run. No keys, no network spend. ~45s.
  --live           Run both legs against live networks. Fails if it cannot.
  (default)        Run each leg live if this machine can, replay it otherwise.

Options
  --gpu-only       Only the GPU rental and the mainnet payout.
  --meter-only     Only the onchain meter and the refusals it landed.
  --no-attack      Drop the refusal replay.
  --spend          Allow the GPU leg to spend real money (rents one machine).
  --ticks N        Replay the first N of 60 recorded ticks.
  --fast           Drop the pauses. Useful in CI.
  --offline        Skip the closing check against public RPC endpoints.
  --no-color       Plain text.
  -h, --help       This.

Environment for the live GPU leg
  COVENANT_VAST_API_KEY               funded market account, required
  COVENANT_COMPUTE_PAYOUT_SIGNER      path to a built covenant-x402-signer
  COVENANT_COMPUTE_FUNDING_KEYPAIR    custody keypair that funds the payout
  COVENANT_COMPUTE_RPC_URL            mainnet RPC
  COVENANT_COMPUTE_PAYOUT_ADDRESS     operator wallet
Without the last four the rental and the meter still run and the payout is
recorded rather than pushed.
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run|--replay) MODE="replay" ;;
    --live) MODE="live" ;;
    --gpu-only) WANT_METER=0; WANT_ATTACK=0 ;;
    --meter-only) WANT_GPU=0 ;;
    --no-attack) WANT_ATTACK=0 ;;
    --spend) SPEND=1 ;;
    --ticks) TICKS="${2:-60}"; shift ;;
    --fast) FAST=1 ;;
    --offline) OFFLINE=1 ;;
    --no-color) COLOR=off ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

[ "${COVENANT_DEMO_SPEND:-0}" = "1" ] && SPEND=1

# --------------------------------------------------------------------- paint

if [ "$COLOR" = "auto" ] && [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then COLOR=on; fi
if [ "$COLOR" = "on" ]; then
  B=$'\033[1m'; D=$'\033[2m'; G=$'\033[32m'; Y=$'\033[33m'; C=$'\033[36m'; R=$'\033[31m'; Z=$'\033[0m'
else
  B=""; D=""; G=""; Y=""; C=""; R=""; Z=""
fi

TICK_PACE=0.30
BEAT=0.9
if [ "$FAST" = "1" ]; then TICK_PACE=0; BEAT=0; fi

beat() { [ "$BEAT" = "0" ] || sleep "${1:-$BEAT}"; }
say()  { printf '%s\n' "$*"; }
dim()  { printf '%s%s%s\n' "$D" "$*" "$Z"; }
hd()   { printf '\n%s%s%s\n' "$B" "$*" "$Z"; }
kv()   { printf '  %-30s %s\n' "$1" "$2"; }
lnk()  { printf '  %s%s%s\n' "$C" "$1" "$Z"; }
warn() { printf '  %s%s%s\n' "$Y" "$*" "$Z"; }
bad()  { printf '  %s%s%s\n' "$R" "$*" "$Z"; }
good() { printf '  %s%s%s\n' "$G" "$*" "$Z"; }
rule() { printf '%s%s%s\n' "$D" "----------------------------------------------------------------------" "$Z"; }
rp()   { printf '  %sreplay%s %s\n' "$D" "$Z" "$*"; }

START_TS=$(date +%s)

# ------------------------------------------------------------------ preflight

need() { command -v "$1" >/dev/null 2>&1; }

if ! need python3; then
  echo "python3 is required to read the recording at demo/recorded-run.json" >&2
  exit 1
fi
if [ ! -f "$RECORDING" ]; then
  echo "missing recording: ${RECORDING}" >&2
  exit 1
fi

# One python call per read; JSON stays the single source of truth.
j() { python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."):
    d=d[int(k)] if k.isdigit() else d[k]
print(d)
' "$RECORDING" "$1"; }

usdc() { python3 -c 'import sys;print("%.6f"%(int(sys.argv[1])/1e6))' "$1"; }

# Can the onchain meter leg run live here?
meter_live_ready() {
  need node || { METER_BLOCK="node is not installed"; return 1; }
  [ -f "$ER_CLIENT" ] || { METER_BLOCK="missing ${ER_CLIENT#"$REPO"/}"; return 1; }
  [ -d "$(dirname "$ER_CLIENT")/node_modules" ] || { METER_BLOCK="client deps are not installed (npm install in programs/compute-lease/client)"; return 1; }
  [ -f "${KEYDIR}/devnet-deployer.json" ] || { METER_BLOCK="no funded devnet keypair at scratchpad/compute-mainnet/devnet-deployer.json, and the faucet is usually rate limited"; return 1; }
  return 0
}

# Can the GPU leg run live here?
gpu_live_ready() {
  need cargo || { GPU_BLOCK="cargo is not installed"; return 1; }
  [ -n "${COVENANT_VAST_API_KEY:-}" ] || { GPU_BLOCK="COVENANT_VAST_API_KEY is not set"; return 1; }
  [ "$SPEND" = "1" ] || { GPU_BLOCK="this leg rents a real machine with real credit; pass --spend to allow it"; return 1; }
  return 0
}

METER_BLOCK=""; GPU_BLOCK=""
METER_PLAN="replay"; GPU_PLAN="replay"
case "$MODE" in
  replay) METER_PLAN="replay"; GPU_PLAN="replay" ;;
  live)
    meter_live_ready && METER_PLAN="live" || METER_PLAN="skip"
    gpu_live_ready   && GPU_PLAN="live"   || GPU_PLAN="skip"
    ;;
  auto)
    meter_live_ready && METER_PLAN="live" || METER_PLAN="replay"
    gpu_live_ready   && GPU_PLAN="live"   || GPU_PLAN="replay"
    ;;
esac
[ "$WANT_METER" = "1" ] || METER_PLAN="off"
[ "$WANT_GPU" = "1" ]   || GPU_PLAN="off"

# ------------------------------------------------------------------- opening

RECORDED_ON="$(j recordedOn)"

hd "Covenant Compute"
say "  GPU rentals billed by the second. Escrow and settlement on Solana, the"
say "  meter on a MagicBlock Ephemeral Rollup."
say ""
plan_word() {
  case "$1" in
    live)   printf '%slive%s' "$G" "$Z" ;;
    replay) printf '%sreplay of the %s run%s' "$D" "$RECORDED_ON" "$Z" ;;
    skip)   printf '%sskipped%s' "$Y" "$Z" ;;
    off)    printf '%snot requested%s' "$D" "$Z" ;;
  esac
}
printf '  %-34s %b\n' "1. GPU rental, mainnet payout" "$(plan_word "$GPU_PLAN")"
printf '  %-34s %b\n' "2. onchain meter, devnet rollup" "$(plan_word "$METER_PLAN")"
if [ "$WANT_METER" = "1" ] && [ "$WANT_ATTACK" = "1" ]; then
  printf '  %-34s %b\n' "3. the meter refuses the wrong key" "$(plan_word replay)"
fi
[ "$GPU_PLAN" = "skip" ] && warn "leg 1 blocked: ${GPU_BLOCK}"
[ "$METER_PLAN" = "skip" ] && warn "leg 2 blocked: ${METER_BLOCK}"
if [ "$MODE" = "auto" ]; then
  [ -n "$GPU_BLOCK" ] && [ "$GPU_PLAN" = "replay" ] && dim "  leg 1 is a replay because ${GPU_BLOCK}"
  [ -n "$METER_BLOCK" ] && [ "$METER_PLAN" = "replay" ] && dim "  leg 2 is a replay because ${METER_BLOCK}"
fi
beat 1.6

# ================================================== leg 1: the machine, the money

gpu_replay() {
  local p=gpuLeg
  hd "1. A real GPU, rented and paid for on Solana mainnet"
  dim "  recorded ${RECORDED_ON}. Live network: $(j $p.network). Every figure below is USDC."
  beat
  rp "buyer signs a lease: $(j $p.rateMicroUsdcPerSec) micro-USDC per second over a $(j $p.windowSecs)s window"
  rp "coordinator holds $(usdc "$(j $p.ledgerHoldMicroUsdc)") USDC against it in its own ledger"
  beat
  rp "broker walks the market and rents one machine"
  kv "machine" "$(j $p.gpu), $(j $p.vramMiB) MiB VRAM"
  kv "handed to the buyer" "$(j $p.endpoint)"
  kv "reachable after" "$(python3 -c 'import sys;print("%.2f s"%(int(sys.argv[1])/1000))' "$(j $p.reachableAfterMs)")"
  beat 1.4
  rp "buyer holds the box, then closes the lease with a signed close request"
  kv "session ran" "$(j $p.sessionRanMs) ms"
  beat
  hd "   The split"
  kv "ledger hold" "$(usdc "$(j $p.ledgerHoldMicroUsdc)") USDC"
  kv "real USDC in custody" "$(usdc "$(j $p.custodyFundedMicroUsdc)") USDC"
  kv "charged for the seconds served" "$(usdc "$(j $p.chargedMicroUsdc)") USDC"
  kv "swept back from custody" "$(usdc "$(j $p.sweptBackMicroUsdc)") USDC"
  dim "  A lease closed early costs what it ran for. That is the product."
  warn "the hold is bookkeeping, not collateral. Custody held less than the"
  warn "hold, so a session that ran the full $(j $p.windowSecs)s window could not have"
  warn "settled from it. Leg 2 is what an escrow that cannot do that looks like."
  beat 1.4
  hd "   Settlement on Solana mainnet"
  kv "operator paid" "$(j $p.settlement.operatorAfter) USDC ($(j $p.settlement.mintName), mint $(j $p.settlement.mint))"
  kv "custody" "$(j $p.settlement.custodyBefore) to $(j $p.settlement.custodyAfter)"
  kv "slot / time" "$(j $p.settlement.slot) / $(j $p.settlement.blockTime)"
  kv "network fee" "$(j $p.settlement.feeLamports) lamports"
  say "  memo, binding the payment to the operator's signed work receipt:"
  printf '  %s%s%s\n' "$D" "$(j $p.settlement.memo)" "$Z"
  lnk "https://explorer.solana.com/tx/$(j $p.settlement.signature)"
  beat 1.6
  hd "   Teardown"
  kv "market contract" "$(j $p.teardown.marketContractId)"
  kv "rented / destroyed" "$(j $p.teardown.rentedAt) / $(j $p.teardown.destroyedAt)"
  kv "instances left running" "$(j $p.teardown.instancesLeftRunning)"
  dim "  Those two timestamps come from the market's own audit log, not from"
  dim "  this client. 43 seconds apart, against a metered $(j $p.sessionRanMs) ms."
  beat
}

gpu_live() {
  hd "1. A real GPU, rented and paid for on Solana mainnet"
  warn "live run. This rents one machine with real credit and destroys it."
  if [ -n "${COVENANT_COMPUTE_PAYOUT_SIGNER:-}" ] && [ -n "${COVENANT_COMPUTE_FUNDING_KEYPAIR:-}" ] &&
     [ -n "${COVENANT_COMPUTE_RPC_URL:-}" ] && [ -n "${COVENANT_COMPUTE_PAYOUT_ADDRESS:-}" ]; then
    good "settlement is real: mainnet USDC to $(printf '%s' "${COVENANT_COMPUTE_PAYOUT_ADDRESS}")"
  else
    warn "payout will be recorded rather than pushed: set COVENANT_COMPUTE_PAYOUT_SIGNER,"
    warn "COVENANT_COMPUTE_FUNDING_KEYPAIR, COVENANT_COMPUTE_RPC_URL and"
    warn "COVENANT_COMPUTE_PAYOUT_ADDRESS for a mainnet settlement."
  fi
  say ""
  ( cd "${REPO}/agent-os" && cargo run --quiet -p covenant-compute-node --example live_lease_canary ) 2>&1 |
    sed 's/^/  /'
  local rc=${PIPESTATUS[0]}
  if [ "$rc" != "0" ]; then
    bad "the GPU leg exited ${rc}. Nothing above is claimed as a success."
    return 1
  fi
  good "GPU leg finished, instance destroyed"
}

# ============================================== leg 2: the meter, tick by tick

meter_replay() {
  local p=meterLeg
  hd "2. The same meter, onchain, one tick per second"
  dim "  recorded ${RECORDED_ON}. Network: $(j $p.network), rollup $(j $p.erRpc)."
  warn "devnet. The escrow token is a throwaway 6 decimal mint, not USDC:"
  warn "devnet USDC is not freely mintable. Figures below are that stand-in unit."
  beat 1.4
  kv "program" "$(j $p.program)"
  kv "lease account" "$(j $p.lease)"
  kv "meter account" "$(j $p.meterAccount)"
  kv "rollup validator" "$(j $p.validator)"
  say ""
  rp "open_lease   funded $(j $p.meter.fundedMicro) micro at $(j $p.meter.rateMicroPerSec)/s over $(j $p.meter.maxDurationSecs)s"
  rp "             the renter names one coordinator, $(j $p.coordinator)"
  dim "  That name is written into the meter account at open and never changes."
  dim "  Every tick below has to be signed by it. Leg 3 tries eight ways around"
  dim "  that and the chain refuses all eight."
  lnk "https://explorer.solana.com/tx/$(j $p.signatures.openLease)?cluster=devnet"
  beat
  rp "delegate_lease   the account moves to the rollup, picked up in $(j $p.erPickupMs) ms"
  lnk "https://explorer.solana.com/tx/$(j $p.signatures.delegateLease)?cluster=devnet"
  beat 1.2
  say ""
  say "  ${B}Escrow on Solana. Meter on the rollup. Watch the root change every tick.${Z}"
  say ""

  # Fold the hash chain here, from the recorded receipts, and show it moving.
  python3 - "$RECORDING" "$TICKS" <<'PY' > /tmp/covenant-demo-ticks.$$
import json, hashlib, sys
d = json.load(open(sys.argv[1]))["meterLeg"]
n = max(1, min(int(sys.argv[2]), len(d["receipts"])))
rate = int(d["meter"]["rateMicroPerSec"])
root = bytes(32)
for r in d["receipts"][:n]:
    root = hashlib.sha256(root + bytes.fromhex(r["receiptHash"])).digest()
    ms = int(r["meteredMs"])          # cumulative metered time at this tick
    owed = rate * ms // 1000
    h = root.hex()
    print("|".join([
        "t+%02d:%02d" % (ms // 60000, ms // 1000 % 60),
        str(ms),
        "%.6f" % (owed / 1e6),
        h[:8] + "…" + h[-4:],
        str(r["fee"]),
        str(r["slot"]),
    ]))
PY
  local lat
  lat="$(j $p.latencyMs.p50)"
  local i=0
  while IFS='|' read -r t ms owed rootp fee slot; do
    i=$((i + 1))
    printf '  %sreplay%s %s  metered %6s ms  owed %s  root %s  rollup fee %s  slot %s\n' \
      "$D" "$Z" "$t" "$ms" "$owed" "$rootp" "$fee" "$slot"
    [ "$TICK_PACE" = "0" ] || sleep "$TICK_PACE"
  done < /tmp/covenant-demo-ticks.$$
  rm -f /tmp/covenant-demo-ticks.$$
  say ""
  kv "ticks replayed" "$i of $(j $p.meter.ticks)"
  kv "tick latency" "min $(j $p.latencyMs.min) ms, p50 ${lat} ms, p95 $(j $p.latencyMs.p95) ms, max $(j $p.latencyMs.max) ms"
  kv "rollup fees on ticks" "$(j $p.lamports.erFeesOnTicks) lamports"
  kv "L1 spend on ticks" "$(j $p.lamports.l1SpentOnTicks) lamports"
  dim "  Free to the renter, not free. delegate, commit and settle are ordinary"
  dim "  L1 transactions, and the rollup validator paid $(j $p.lamports.erValidatorCommitLamports) lamports on L1 to"
  dim "  commit the meter back."
  beat 1.4

  hd "   Commit and settle"
  rp "undelegate_lease   final state commits to L1 in $(j $p.commitLandedMs) ms"
  rp "settle_lease   charged $(j $p.meter.chargedMicro), refunded $(j $p.meter.refundedMicro) of $(j $p.meter.fundedMicro)"
  lnk "https://explorer.solana.com/tx/$(j $p.signatures.settleLease)?cluster=devnet"
  lnk "https://explorer.solana.com/address/$(j $p.meterAccount)?cluster=devnet"
  say ""
  kv "provenance root" "$(j $p.meter.provenanceRoot)"

  # Recompute the full chain from all 60 receipts and check it against the
  # root the program committed. This is real work, not a printed constant.
  if python3 - "$RECORDING" <<'PY'
import json, hashlib, sys
d = json.load(open(sys.argv[1]))["meterLeg"]
root = bytes(32)
for r in d["receipts"]:
    root = hashlib.sha256(root + bytes.fromhex(r["receiptHash"])).digest()
sys.exit(0 if root.hex() == d["meter"]["provenanceRoot"] else 1)
PY
  then
    good "recomputed from all $(j $p.meter.ticks) tick receipts: matches the committed root"
  else
    bad "recomputed chain does not match the committed root"
    return 1
  fi
  beat
}

meter_live() {
  hd "2. The same meter, onchain, one tick per second"
  warn "live devnet run. Escrow is a throwaway stand-in mint, not USDC."
  say ""
  ( cd "$(dirname "$ER_CLIENT")" && N="$TICKS" node "$ER_CLIENT" ) 2>&1 | sed 's/^/  /'
  local rc=${PIPESTATUS[0]}
  if [ "$rc" != "0" ]; then
    bad "the meter leg exited ${rc}. Nothing above is claimed as a success."
    return 1
  fi
  good "meter leg finished and reconciled"
}

# ================================ leg 3: what the meter refuses, and from whom

attack_replay() {
  local p=attackLeg
  hd "3. Only the coordinator the renter named can move the meter"
  dim "  recorded ${RECORDED_ON}. $(j $p.network)."
  say ""
  say "  The renter names one coordinator when the lease opens. That key is"
  say "  written into the meter account and it is the only key allowed to tick,"
  say "  undelegate or settle. Nothing else can add a second to the bill."
  say ""
  kv "escrow at risk" "$(j $p.escrowAtRiskMicro) micro"
  kv "attacker asked for" "$(j $p.askedFor.ceilingMs) ms, then $(j $p.askedFor.absurdMs) ms"
  dim "  Every attempt below was sent with preflight off, so the chain itself"
  dim "  rejected it and the rejection is a transaction you can look up."
  say ""
  python3 - "$RECORDING" <<'PY' > /tmp/covenant-demo-refusals.$$
import json, sys
for r in json.load(open(sys.argv[1]))["attackLeg"]["refusals"]:
    print("|".join([r["cluster"], str(r["slot"]), str(r["code"]), r["label"]]))
PY
  while IFS='|' read -r cluster slot code label; do
    printf '  %sreplay%s %-10s slot %-10s %srefused %s%s  %s\n' \
      "$D" "$Z" "$cluster" "$slot" "$G" "$code" "$Z" "$label"
    [ "$TICK_PACE" = "0" ] || sleep 0.25
  done < /tmp/covenant-demo-refusals.$$
  rm -f /tmp/covenant-demo-refusals.$$
  say ""
  good "8 of 8 refused, on both the L1 and the rollup"
  python3 - "$RECORDING" <<'PY' > /tmp/covenant-demo-codes.$$
import json, sys
seen = {}
for r in json.load(open(sys.argv[1]))["attackLeg"]["refusals"]:
    seen.setdefault(r["code"], r["codeMeans"].split(": ", 1)[-1])
for code in sorted(seen):
    print("%d|%s" % (code, seen[code]))
PY
  while IFS='|' read -r code means; do
    printf '  %s%-6s%s %s\n' "$D" "$code" "$Z" "$means"
  done < /tmp/covenant-demo-codes.$$
  rm -f /tmp/covenant-demo-codes.$$
  beat 1.2
  say ""
  say "  Then the same lease settled honestly, for what it actually ran:"
  kv "metered" "$(j $p.honestSettlement.meteredMs) ms"
  kv "operator paid" "$(j $p.honestSettlement.operatorPaidMicro) micro"
  kv "returned to the renter" "$(j $p.honestSettlement.refundedMicro) micro"
  lnk "https://explorer.solana.com/tx/$(j $p.honestSettlement.settleSignature)?cluster=devnet"
  dim "  The attacker asked for 600 seconds and then for the largest number a"
  dim "  u64 holds. The books recorded five seconds."
  say ""
  dim "  Regression: $(j $p.source)"
  beat
}

# ----------------------------------------------------------------- run the legs

FAILED=0
case "$GPU_PLAN" in
  live)   gpu_live   || FAILED=1 ;;
  replay) gpu_replay ;;
  skip)   hd "1. GPU rental, mainnet payout"; warn "skipped: ${GPU_BLOCK}" ;;
esac

if [ "$GPU_PLAN" != "off" ] && [ "$METER_PLAN" != "off" ]; then
  say ""
  rule
  say "  Two artifacts, joined by one signed work receipt. Leg 1 settles on"
  say "  mainnet with an SPL transfer and a memo. It does not call the lease"
  say "  program and it does not touch the rollup, and the hold behind it is"
  say "  the coordinator's own bookkeeping. Leg 2 is that same arithmetic with"
  say "  the escrow in a vault the coordinator cannot overdraw, and it runs on"
  say "  devnet today."
  rule
  beat 1.6
fi

case "$METER_PLAN" in
  live)   meter_live   || FAILED=1 ;;
  replay) meter_replay || FAILED=1 ;;
  skip)   hd "2. Onchain meter, devnet rollup"; warn "skipped: ${METER_BLOCK}" ;;
esac

# The refusals are always a replay: they are landed transactions from a
# regression run, and re-sending them would open a fresh lease to attack.
[ "$WANT_METER" = "1" ] && [ "$WANT_ATTACK" = "1" ] && attack_replay

# ------------------------------------------------- check it against public RPC

VERIFY_TX_PY=$(cat <<'PY'
import json, sys
body, op, expect = sys.argv[1], sys.argv[2], sys.argv[3]
r = json.load(open(body)).get("result")
if not r:
    print("  the endpoint returned no transaction (it may not serve slots this old)")
    raise SystemExit(1)
err = r["meta"]["err"]
post = {b["owner"]: b["uiTokenAmount"]["uiAmountString"] for b in r["meta"]["postTokenBalances"]}
got = post.get(op)
memo = any("compute-payout:v1" in line for line in r["meta"].get("logMessages", []))
print("  confirmed   slot %s, error %s, fee %s lamports"
      % (r["slot"], "none" if err is None else err, r["meta"]["fee"]))
print("  confirmed   operator %s… holds %s USDC after this transaction" % (op[:8], got))
print("  confirmed   the memo carries the job id and the signed receipt" if memo
      else "  the memo was not found in the logs")
ok = err is None and got is not None and abs(float(got) - float(expect)) < 1e-9 and memo
raise SystemExit(0 if ok else 1)
PY
)

VERIFY_ACCT_PY=$(cat <<'PY'
import base64, json, sys
body, program, root, coordinator = sys.argv[1:5]
v = json.load(open(body)).get("result", {}).get("value")
if not v:
    print("  the endpoint returned no account")
    raise SystemExit(1)
raw = base64.b64decode(v["data"][0])
owned = v["owner"] == program
found = bytes.fromhex(root) in raw

# base58 decode without a dependency; the coordinator is stored as raw 32 bytes
A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
n = 0
for ch in coordinator:
    n = n * 58 + A.index(ch)
key = n.to_bytes(32, "big")
pinned = key in raw

print("  confirmed   the meter account is owned by %s" % v["owner"] if owned
      else "  the meter account is owned by %s, which is not the program" % v["owner"])
print("  confirmed   committed account data carries provenance root %s…" % root[:16] if found
      else "  the provenance root is not present in the account data")
print("  confirmed   the meter still names coordinator %s…, pinned at open" % coordinator[:8]
      if pinned else "  the coordinator is not present in the account data")
raise SystemExit(0 if owned and found and pinned else 1)
PY
)

VERIFY_REFUSAL_PY=$(cat <<'PY'
import json, sys
body, code, label = sys.argv[1], int(sys.argv[2]), sys.argv[3]
r = json.load(open(body)).get("result")
if not r:
    print("  the endpoint returned no transaction (it may not serve slots this old)")
    raise SystemExit(1)
err = r["meta"]["err"]
got = None
if isinstance(err, dict) and "InstructionError" in err:
    detail = err["InstructionError"][1]
    if isinstance(detail, dict):
        got = detail.get("Custom")
print("  confirmed   slot %s landed and failed with custom error %s" % (r["slot"], got))
print("  confirmed   %s, refused on chain" % label)
raise SystemExit(0 if got == code else 1)
PY
)

verify_public() {
  need curl || { dim "  curl is not installed, skipping the public check"; return 0; }
  hd "Check the recorded run yourself"
  dim "  The ${RECORDED_ON} artifacts are public and permanent. This asks the"
  dim "  networks for them directly, whatever ran above."
  local tmp sig op expect meter root
  tmp="$(mktemp -t covenant-demo-rpc)"

  sig="$(j gpuLeg.settlement.signature)"
  op="$(j gpuLeg.settlement.operator)"
  expect="$(j gpuLeg.settlement.operatorAfter)"
  say "  Asking ${MAINNET_RPC} for the settlement transaction."
  if curl -s --max-time 25 -X POST "$MAINNET_RPC" -H 'content-type: application/json' \
      -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"${sig}\",{\"encoding\":\"jsonParsed\",\"maxSupportedTransactionVersion\":0}]}" \
      -o "$tmp" 2>/dev/null && [ -s "$tmp" ]; then
    python3 -c "$VERIFY_TX_PY" "$tmp" "$op" "$expect" ||
      warn "could not confirm the mainnet transaction from this endpoint"
  else
    warn "no answer from the mainnet RPC. Network is unreachable or rate limited."
  fi
  beat

  meter="$(j meterLeg.meterAccount)"
  root="$(j meterLeg.meter.provenanceRoot)"
  say "  Asking ${DEVNET_RPC} for the meter account."
  if curl -s --max-time 25 -X POST "$DEVNET_RPC" -H 'content-type: application/json' \
      -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"${meter}\",{\"encoding\":\"base64\"}]}" \
      -o "$tmp" 2>/dev/null && [ -s "$tmp" ]; then
    python3 -c "$VERIFY_ACCT_PY" "$tmp" "$(j meterLeg.program)" "$root" "$(j meterLeg.coordinator)" ||
      warn "could not confirm the devnet meter account from this endpoint"
  else
    warn "no answer from the devnet RPC. Network is unreachable or rate limited."
  fi
  beat

  if [ "$WANT_ATTACK" = "1" ]; then
    sig="$(j attackLeg.refusals.1.signature)"
    say "  Asking ${DEVNET_RPC} for one of the refusals."
    if curl -s --max-time 25 -X POST "$DEVNET_RPC" -H 'content-type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"${sig}\",{\"encoding\":\"json\",\"maxSupportedTransactionVersion\":0}]}" \
        -o "$tmp" 2>/dev/null && [ -s "$tmp" ]; then
      python3 -c "$VERIFY_REFUSAL_PY" "$tmp" "$(j attackLeg.refusals.1.code)" "$(j attackLeg.refusals.1.label)" ||
        warn "could not confirm the refusal from this endpoint"
    else
      warn "no answer from the devnet RPC. Network is unreachable or rate limited."
    fi
  fi
  rm -f "$tmp"
}

if [ "$OFFLINE" = "0" ] && [ "$MODE" != "live" ]; then
  verify_public
fi

# --------------------------------------------------------------------- close

hd "What is real here"
say "  Mainnet, real value: the GPU rental and the operator payout. In the"
say "  ${RECORDED_ON} run an $(j gpuLeg.gpu) was rented on a live market and"
say "  $(j gpuLeg.settlement.operatorAfter) USDC moved on Solana mainnet, memo bound to the signed"
say "  receipt for the seconds served. The machine was rented and held. No job"
say "  ran on it, so this is a payment for access time priced by a clock."
say ""
say "  Devnet, stand-in value: the whole onchain meter, including anything it"
say "  printed above. Its escrow token is a throwaway mint. The program treats"
say "  any 6 decimal mint the same way, so the arithmetic is the arithmetic"
say "  that would run against USDC."
say ""
say "  Still open: the buyer's deposit. Money out is onchain. Money in is the"
say "  coordinator's custodial ledger, and on the mainnet leg that ledger held"
say "  more than the wallet behind it. The devnet leg is what closing that gap"
say "  looks like."
say ""
say "  What the refusals do and do not cover: they bind every key except the"
say "  coordinator. The coordinator can still meter any value up to the window"
say "  ceiling, and the vault balance is the only other limit on it."
say ""
say "  The rate here is a demo rate. It is not a price."

ELAPSED=$(( $(date +%s) - START_TS ))
hd "Done in ${ELAPSED}s"
if [ "$FAILED" != "0" ]; then
  bad "one or more legs failed. Read the output above rather than this line."
  exit 1
fi
exit 0
