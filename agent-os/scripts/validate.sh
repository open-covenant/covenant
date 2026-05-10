#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
usage: scripts/validate.sh [--scripts] [--quick] [--live]

  --scripts  run repo guardrails without Rust tooling
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

run node ./scripts/validate-autonomy.mjs
run node ./scripts/validate-autonomy-next-cli.mjs
run node ./scripts/validate-autonomy-summary.mjs
run node ./scripts/validate-commit-rotation.mjs
run node ./scripts/validate-git-identity.mjs --ref HEAD --ref origin/main..HEAD
run node ./scripts/validate-github-cli-account.mjs
run node ./scripts/validate-readme-copy.mjs
run node ./scripts/validate-status-evidence.mjs
run node ./scripts/validate-live-coverage.mjs
run node ./scripts/validate-privileged-cli-live-matrix.mjs
run node ./scripts/validate-source-installer.mjs
run node ./scripts/validate-source-install-upgrade-plan.mjs
run node ./scripts/validate-source-install-rollback.mjs
run node ./scripts/validate-sdk-compatibility.mjs
run node ./scripts/validate-package-manager-readiness.mjs
run node ./scripts/validate-distribution-readiness.mjs
run node ./scripts/validate-settlement-oracle-policy.mjs
run node ./scripts/validate-settlement-deployment-readiness.mjs
run node ./scripts/validate-settlement-receipt-migration.mjs
run node ./scripts/validate-gvisor-host-readiness.mjs
run node ./scripts/validate-release-artifact-subject.mjs
run node ./scripts/validate-release-provenance-readiness.mjs
run node ./scripts/validate-identity-provenance.mjs
run node ./scripts/validate-mcp-live-compatibility.mjs
run node ./scripts/validate-a2a-repair-authorization.mjs
run node ./scripts/validate-a2a-peer-repair-report.mjs
run node ./scripts/validate-a2a-repair-visibility.mjs
run node ./scripts/provenance.mjs verify-all
run node ./scripts/provenance-self-test.mjs
run ./scripts/check-no-display-form-a2a.sh

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
