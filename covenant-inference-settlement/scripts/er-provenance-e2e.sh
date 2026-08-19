#!/usr/bin/env bash
# End-to-end proof, on DEVNET, that the MagicBlock ER is the metering + provenance
# spine of the Covenant inference network:
#
#   1. init: open + buy_credits + delegate the throwaway credit account to a pinned
#      devnet ER validator, and start the settlement bridge.
#   2. bring the real inference stack up (llama.cpp <- node <- gateway) with the
#      gateway's --settlement-url pointed at the bridge.
#   3. run the alpha soak (N=30): every served receipt is folded into the on-chain
#      provenance_root by a gasless consume_credits on the ER.
#   4. recompute the fold off-chain from the N receipt hashes and assert it EQUALS
#      the on-chain root; then POST /commit and assert the L1 root reconciles.
#
# The throwaway keypair (.keys/owner.json) is the only signer of the settlement
# lifecycle. No mainnet, no treasury key. Requires: a prior devnet-deploy.mjs (sets
# .keys/deploy.json), plus ollama(qwen3:8b) + llama-server for the real stack.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BRIDGE_DIR="$(cd "$HERE/.." && pwd)"
GW_SCRIPTS="$(cd "$BRIDGE_DIR/../agent-os/crates/covenant-inference-gateway/scripts/alpha" && pwd)"

: "${SOLANA_RPC:?set SOLANA_RPC to a devnet RPC}"
export SOLANA_RPC
export PROGRAM_ID="${PROGRAM_ID:-$(node -e 'console.log(require("./.keys/deploy.json").program)' 2>/dev/null || true)}"
[ -n "${PROGRAM_ID:-}" ] || { echo "no PROGRAM_ID and no .keys/deploy.json — run scripts/devnet-deploy.mjs first" >&2; exit 1; }
export SETTLEMENT_PORT="${SETTLEMENT_PORT:-8799}"
BRIDGE_URL="http://127.0.0.1:$SETTLEMENT_PORT"
N="${N:-30}"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

cd "$BRIDGE_DIR"

say "init: open + buy + delegate the throwaway credit account (program $PROGRAM_ID)"
node cli.mjs init

say "starting settlement bridge on $BRIDGE_URL"
node cli.mjs serve >"$BRIDGE_DIR/.keys/bridge.log" 2>&1 &
BRIDGE_PID=$!
trap 'kill "$BRIDGE_PID" 2>/dev/null || true' EXIT
for i in $(seq 1 20); do curl -fsS "$BRIDGE_URL/health" >/dev/null 2>&1 && break; sleep 0.5; done
curl -fsS "$BRIDGE_URL/health" >/dev/null || die "bridge never became ready ($BRIDGE_DIR/.keys/bridge.log)"

ROOT_BEFORE="$(curl -fsS "$BRIDGE_URL/root" | node -e 'process.stdin.on("data",d=>console.log(JSON.parse(d).er_root_hex))')"
[ -n "$ROOT_BEFORE" ] && [ "$ROOT_BEFORE" != "null" ] || die "credit account is not delegated (no ER root)"
say "root before soak: $ROOT_BEFORE"

say "bringing up the real inference stack with the settlement fold on"
SETTLEMENT_URL="$BRIDGE_URL" bash "$GW_SCRIPTS/alpha-up.sh"

say "running the soak (N=$N) — each served receipt folds into the on-chain provenance_root"
bash "$GW_SCRIPTS/alpha-soak.sh" "$N"

# shellcheck source=/dev/null
. "${COVENANT_ALPHA_STATE:-$HOME/.local/state/covenant-inference-alpha}/alpha.env"

say "waiting for all $N folds to land on the ER"
for i in $(seq 1 60); do
  GATEWAY_URL="$BASE_URL" BRIDGE_URL="$BRIDGE_URL" ROOT_BEFORE="$ROOT_BEFORE" EXPECT_N="$N" \
    node scripts/verify-fold.mjs && break
  rc=$?
  [ "$rc" = "2" ] || exit "$rc"
  sleep 2
done

say "committing the provenance root to L1 (commit_credits permissionless + undelegate)"
COMMIT="$(curl -fsS -X POST "$BRIDGE_URL/commit" --max-time 90)"
echo "$COMMIT" | node -e 'const d=JSON.parse(require("fs").readFileSync(0));console.log("  commit_sig   "+d.commit_sig);console.log("  undelegate   "+d.l1_sig);console.log("  L1 root      "+d.provenance_root_hex);'

# Undelegated now, so /root reports l1_root_hex; verify-fold re-checks the off-chain
# fold against the committed L1 root exactly as it did the ER root.
say "reconciling the committed L1 root against the off-chain fold"
GATEWAY_URL="$BASE_URL" BRIDGE_URL="$BRIDGE_URL" ROOT_BEFORE="$ROOT_BEFORE" EXPECT_N="$N" \
  node scripts/verify-fold.mjs || die "L1 root did not reconcile to the off-chain fold"

say "E2E PASSED"
echo "  program         $PROGRAM_ID (devnet, ephemeral build of the mainnet settlement program)"
echo "  folds           $N gasless consume_credits on the ER"
echo "  on-chain == fold  $EXPECTED"
echo "  L1 committed    $L1_ROOT"
