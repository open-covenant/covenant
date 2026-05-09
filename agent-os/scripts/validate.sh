#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
usage: scripts/validate.sh [--quick] [--live]

  --quick  run format, repo guards, cargo check, and cargo test
  --live   run ignored live tests instead of the default mock suite
EOF
}

mode="full"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --quick) mode="quick" ;;
    --live) mode="live" ;;
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

run cargo fmt --check
run node ./scripts/validate-autonomy.mjs
run node ./scripts/validate-git-identity.mjs --ref HEAD
run node ./scripts/validate-github-cli-account.mjs
run node ./scripts/validate-readme-copy.mjs
run node ./scripts/validate-live-coverage.mjs
run node ./scripts/provenance.mjs verify-all
run node ./scripts/provenance-self-test.mjs
run ./scripts/check-no-display-form-a2a.sh

case "$mode" in
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
