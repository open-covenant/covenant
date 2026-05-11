#!/usr/bin/env bash
# scripts/test-stats.sh — count mock vs live tests across the workspace.
#
# Convention (see AGENTS.md "Mock vs live tests"): a test whose function name
# starts with `live_` exercises a real backend (real LLM, real network, real
# Solana RPC, real subprocess). Anything else is mock/fixture-driven.
#
# This script does NOT run the suite. It greps test functions out of source.
# Run `cargo test -- --ignored live_` to actually execute the live ones.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

node ./scripts/validate-live-coverage.mjs
printf '\n'

# State-machine over Rust source: when we see `#[test]` or `#[tokio::test]`
# on a line, the next `fn <name>(...)` we see is a test. Works under BSD awk
# (no gawk-only `match($0, re, arr)` array capture).
for d in crates agents programs; do
  [ -d "$d" ] || { echo "test-stats.sh: expected $ROOT/$d to exist" >&2; exit 1; }
done

ALL_TESTS=$(
  find crates agents programs -name '*.rs' -not -path '*/target/*' \
    -print0 2>/dev/null \
    | xargs -0 awk '
        /#\[(tokio::)?test\]/ { is_test = 1; next }
        is_test {
          if (match($0, /fn[ \t]+[A-Za-z_][A-Za-z0-9_]*/)) {
            chunk = substr($0, RSTART, RLENGTH)
            sub(/^fn[ \t]+/, "", chunk)
            print chunk
            is_test = 0
          } else if ($0 !~ /^[ \t]*#/) {
            # Non-attribute, non-fn line breaks the streak.
            is_test = 0
          }
        }
      ' \
    | sort -u
)

TOTAL=$(printf '%s\n' "$ALL_TESTS" | grep -c . || true)
LIVE=$(printf '%s\n' "$ALL_TESTS" | grep -c '^live_' || true)
MOCK=$((TOTAL - LIVE))

if [ "$TOTAL" -eq 0 ]; then
  RATIO="n/a"
else
  RATIO=$(awk -v l="$LIVE" -v t="$TOTAL" 'BEGIN { printf "%.1f%%", (l / t) * 100 }')
fi

printf 'tests   total: %d\n' "$TOTAL"
printf '         mock: %d\n' "$MOCK"
printf '         live: %d  (%s of total)\n' "$LIVE" "$RATIO"
printf '\n'

if [ "$LIVE" -gt 0 ]; then
  printf 'live tests:\n'
  printf '%s\n' "$ALL_TESTS" | grep '^live_' | sed 's/^/  /'
else
  printf 'no live tests yet — every claim of "shipped" / "complete" rests on mocks.\n'
fi
