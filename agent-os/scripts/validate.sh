#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
usage: scripts/validate.sh [--scripts] [--quick] [--live]

  --scripts  run public repo guardrails without Rust tooling
  --quick    run format, repo guards, cargo check, and cargo test
  --live     run ignored live tests instead of the default mock suite
EOF
}

mode="full"
set_mode() {
  local next="$1"
  if [ "$mode" != "full" ] && [ "$mode" != "$next" ]; then
    printf 'error: choose exactly one mode flag (--scripts, --quick, --live)\n' >&2
    usage >&2
    exit 2
  fi
  mode="$next"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --scripts) set_mode "scripts" ;;
    --quick) set_mode "quick" ;;
    --live) set_mode "live" ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

run() {
  printf '>> %s\n' "$*"
  "$@"
}

if [ "$mode" != "scripts" ]; then
  run cargo fmt --check
fi

run node ./scripts/provenance.mjs verify-all
run node ./scripts/validate-cli-envelope-docs.mjs
run node ./scripts/validate-chain-cli-envelope-fields.mjs
run node ./scripts/validate-chain-tx-test-line-refs.mjs

case "$mode" in
  scripts)
    ;;
  quick)
    run cargo check --workspace --exclude covenant-settlement-program --locked
    run cargo test --workspace --exclude covenant-settlement-program --locked
    ;;
  full)
    run cargo build --workspace --exclude covenant-settlement-program --locked
    run cargo clippy --workspace --all-targets --exclude covenant-settlement-program --locked -- -D warnings
    run cargo test --workspace --exclude covenant-settlement-program --locked
    ;;
  live)
    run cargo test --workspace --exclude covenant-settlement-program --locked -- --ignored live_
    ;;
esac
