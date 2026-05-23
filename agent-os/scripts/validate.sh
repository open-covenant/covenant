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
run node ./scripts/validate-receipt-list-line-refs.mjs
run node ./scripts/validate-memory-backfill-line-refs.mjs
run node ./scripts/validate-settlement-backfill-line-refs.mjs
run node ./scripts/validate-chain-status-line-refs.mjs
run node ./scripts/validate-flush-receipts-line-refs.mjs
run node ./scripts/validate-receipt-batch-list-line-refs.mjs
run node ./scripts/validate-capabilities-purge-line-refs.mjs
run node ./scripts/validate-peers-rotate-line-refs.mjs
run node ./scripts/validate-ping-line-refs.mjs
run node ./scripts/validate-tool-list-line-refs.mjs
run node ./scripts/validate-tool-result-line-refs.mjs
run node ./scripts/validate-capability-list-line-refs.mjs
run node ./scripts/validate-capability-revoke-line-refs.mjs
run node ./scripts/validate-intent-result-line-refs.mjs
run node ./scripts/validate-peers-purge-line-refs.mjs
run node ./scripts/validate-audit-recent-line-refs.mjs
run node ./scripts/validate-audit-purge-line-refs.mjs
run node ./scripts/validate-audit-verify-line-refs.mjs
run node ./scripts/validate-memory-read-line-refs.mjs
run node ./scripts/validate-memory-compaction-line-refs.mjs
run node ./scripts/validate-capability-grant-line-refs.mjs
run node ./scripts/validate-peer-revoke-line-refs.mjs
run node ./scripts/validate-memory-purge-line-refs.mjs
run node ./scripts/validate-a2a-compact-line-refs.mjs
run node ./scripts/validate-a2a-retry-line-refs.mjs
run node ./scripts/validate-ignore-report-line-refs.mjs
run node ./scripts/validate-bootstrap-result-line-refs.mjs
run node ./scripts/validate-memory-compaction-plan-line-refs.mjs
run node ./scripts/validate-a2a-status-line-refs.mjs
run node ./scripts/validate-peer-list-line-refs.mjs
run node ./scripts/validate-verify-report-line-refs.mjs
run node ./scripts/validate-intents-resume-line-refs.mjs
run node ./scripts/validate-covenant-ipc-struct-line-refs.mjs
run node ./scripts/validate-covenant-ipc-field-attribute-range-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-struct-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-ambiguous-variant-range-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-enum-annotation-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-ambiguous-truncated-attribute-line-refs.mjs
run node ./scripts/validate-covenant-a2a-struct-line-refs.mjs
run node ./scripts/validate-covenant-a2a-field-attribute-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-enum-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-struct-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-default-impl-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-result-error-fn-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-a2a-idempotency-field-list-line-refs.mjs
run node ./scripts/validate-covenant-mcp-struct-line-refs.mjs
run node ./scripts/validate-covenant-mcp-tool-spec-annotation-line-refs.mjs
run node ./scripts/validate-covenant-mcp-content-enum-annotation-line-refs.mjs
run node ./scripts/validate-covenant-audit-struct-line-refs.mjs
run node ./scripts/validate-covenant-audit-kind-annotation-line-refs.mjs
run node ./scripts/validate-covenant-types-struct-line-refs.mjs
run node ./scripts/validate-covenant-types-capability-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-agent-id-serialize-impl-line-refs.mjs
run node ./scripts/validate-covenant-types-enum-serde-rename-annotation-line-refs.mjs
run node ./scripts/validate-covenant-types-field-attribute-range-line-refs.mjs
run node ./scripts/validate-covenant-types-enum-block-range-line-refs.mjs
run node ./scripts/validate-covenant-types-tx-sig-doc-comment-range-line-refs.mjs
run node ./scripts/validate-covenant-permissions-struct-line-refs.mjs
run node ./scripts/validate-covenant-permissions-sig-b58-module-range-line-refs.mjs

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
