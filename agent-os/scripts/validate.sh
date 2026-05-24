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
run node ./scripts/validate-covenant-ipc-chain-status-field-list-line-refs.mjs
run node ./scripts/validate-covenant-ipc-verify-check-field-list-line-refs.mjs
run node ./scripts/validate-covenant-ipc-verify-drift-field-list-line-refs.mjs
run node ./scripts/validate-covenant-ipc-receipt-batch-summary-field-list-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-struct-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-peer-summary-field-list-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-ambiguous-variant-range-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-enum-annotation-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-ambiguous-truncated-attribute-line-refs.mjs
run node ./scripts/validate-covenant-peer-auth-revoke-outcome-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-struct-line-refs.mjs
run node ./scripts/validate-covenant-a2a-field-attribute-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-enum-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-struct-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-default-impl-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-result-error-fn-block-range-line-refs.mjs
run node ./scripts/validate-covenant-a2a-a2a-idempotency-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-queue-entry-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-result-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-auto-retry-report-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-auto-retry-policy-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-auto-retry-requeued-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-auto-retry-skipped-field-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-status-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-task-queue-state-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-a2a-auto-retry-skip-reason-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-mcp-struct-line-refs.mjs
run node ./scripts/validate-covenant-mcp-tool-spec-annotation-line-refs.mjs
run node ./scripts/validate-covenant-mcp-tool-spec-field-list-line-refs.mjs
run node ./scripts/validate-covenant-mcp-content-enum-annotation-line-refs.mjs
run node ./scripts/validate-covenant-mcp-content-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-audit-struct-line-refs.mjs
run node ./scripts/validate-covenant-audit-kind-annotation-line-refs.mjs
run node ./scripts/validate-covenant-audit-event-field-list-line-refs.mjs
run node ./scripts/validate-covenant-audit-integrity-report-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-struct-line-refs.mjs
run node ./scripts/validate-covenant-types-capability-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-memory-record-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-settlement-receipt-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-agent-id-serialize-impl-line-refs.mjs
run node ./scripts/validate-covenant-types-enum-serde-rename-annotation-line-refs.mjs
run node ./scripts/validate-covenant-types-field-attribute-range-line-refs.mjs
run node ./scripts/validate-covenant-types-enum-block-range-line-refs.mjs
run node ./scripts/validate-covenant-types-resource-kind-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-types-memory-tier-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-types-memory-repair-mode-variant-list-line-refs.mjs
run node ./scripts/validate-covenant-types-memory-compaction-outcome-field-list-line-refs.mjs
run node ./scripts/validate-covenant-types-tx-sig-doc-comment-range-line-refs.mjs
run node ./scripts/validate-covenant-permissions-struct-line-refs.mjs
run node ./scripts/validate-covenant-permissions-sig-b58-module-range-line-refs.mjs
run node ./scripts/validate-covenant-permissions-signed-capability-field-list-line-refs.mjs
run node ./scripts/validate-covenantd-recent-capabilities-fn-block-range-line-refs.mjs
run node ./scripts/validate-receipt-list-type-level-pin-line-refs.mjs
run node ./scripts/validate-capability-list-type-level-pin-line-refs.mjs
run node ./scripts/validate-capability-grant-type-level-pin-line-refs.mjs
run node ./scripts/validate-peer-list-type-level-pin-line-refs.mjs
run node ./scripts/validate-intent-result-type-level-pin-line-refs.mjs
run node ./scripts/validate-intents-resume-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-read-type-level-pin-line-refs.mjs
run node ./scripts/validate-a2a-status-type-level-pin-line-refs.mjs
run node ./scripts/validate-tool-result-type-level-pin-line-refs.mjs
run node ./scripts/validate-ignore-report-type-level-pin-line-refs.mjs
run node ./scripts/validate-audit-recent-type-level-pin-line-refs.mjs
run node ./scripts/validate-audit-purge-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-purge-type-level-pin-line-refs.mjs
run node ./scripts/validate-capabilities-purge-type-level-pin-line-refs.mjs
run node ./scripts/validate-peers-purge-type-level-pin-line-refs.mjs
run node ./scripts/validate-peers-rotate-type-level-pin-line-refs.mjs
run node ./scripts/validate-peer-revoke-type-level-pin-line-refs.mjs
run node ./scripts/validate-audit-verify-type-level-pin-line-refs.mjs
run node ./scripts/validate-chain-status-type-level-pin-line-refs.mjs
run node ./scripts/validate-capability-revoke-type-level-pin-line-refs.mjs
run node ./scripts/validate-verify-report-type-level-pin-line-refs.mjs
run node ./scripts/validate-tool-list-type-level-pin-line-refs.mjs
run node ./scripts/validate-receipt-batch-flushed-type-level-pin-line-refs.mjs
run node ./scripts/validate-receipt-batch-list-type-level-pin-line-refs.mjs
run node ./scripts/validate-bootstrap-result-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-type-level-pin-line-refs.mjs
run node ./scripts/validate-a2a-retry-type-level-pin-line-refs.mjs
run node ./scripts/validate-a2a-compact-type-level-pin-line-refs.mjs
run node ./scripts/validate-settlement-backfill-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-backfill-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-plan-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-plan-outcome-type-level-pin-line-refs.mjs
run node ./scripts/validate-ping-type-level-pin-line-refs.mjs
run node ./scripts/validate-audit-verify-root-hash-hex-length-pin-line-refs.mjs
run node ./scripts/validate-audit-verify-report-failures-empty-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-plan-records-empty-renders-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-outcome-deleted-empty-renders-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-outcome-parents-detached-empty-renders-pin-line-refs.mjs
run node ./scripts/validate-intents-resume-error-ok-false-pin-line-refs.mjs
run node ./scripts/validate-bootstrap-result-granted-entries-object-pin-line-refs.mjs
run node ./scripts/validate-bootstrap-result-already-granted-entries-string-pin-line-refs.mjs
run node ./scripts/validate-bootstrap-result-empty-granted-no-new-grants-pin-line-refs.mjs
run node ./scripts/validate-settlement-backfill-schema-value-pin-line-refs.mjs
run node ./scripts/validate-memory-backfill-schema-value-pin-line-refs.mjs
run node ./scripts/validate-memory-backfill-savepoint-name-non-empty-pin-line-refs.mjs
run node ./scripts/validate-bootstrap-result-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-daemon-ping-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-peer-list-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-peer-revoke-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-intent-result-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-receipt-list-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-capability-list-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-capability-granted-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-capability-revoked-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-capabilities-purged-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-peers-purged-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-peer-token-rotated-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-audit-recent-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-audit-purged-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-audit-integrity-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-memory-purged-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-memory-read-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-memory-compacted-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-memory-compaction-plan-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-ignore-report-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-chain-status-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-verify-report-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-tool-list-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-tool-result-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-receipt-batch-flushed-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-receipt-batch-list-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-a2a-status-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-a2a-auto-retry-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-a2a-compacted-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-intents-resume-kind-literal-value-pin-line-refs.mjs
run node ./scripts/validate-intents-resume-ok-text-type-level-pin-line-refs.mjs
run node ./scripts/validate-intents-resume-mode-type-level-pin-line-refs.mjs
run node ./scripts/validate-intent-result-text-type-level-pin-line-refs.mjs
run node ./scripts/validate-a2a-status-tasks-type-level-pin-line-refs.mjs
run node ./scripts/validate-peer-list-peers-type-level-pin-line-refs.mjs
run node ./scripts/validate-verify-report-drift-type-level-pin-line-refs.mjs
run node ./scripts/validate-tool-result-name-type-level-pin-line-refs.mjs
run node ./scripts/validate-memory-read-mode-type-level-pin-line-refs.mjs
run node ./scripts/validate-daemon-ping-status-literal-value-pin-line-refs.mjs
run node ./scripts/validate-tool-list-no-tools-print-line-refs.mjs
run node ./scripts/validate-capability-revoke-no-live-print-line-refs.mjs
run node ./scripts/validate-a2a-status-queue-empty-print-line-refs.mjs
run node ./scripts/validate-tool-list-tool-format-print-line-refs.mjs
run node ./scripts/validate-capability-list-limit-default-line-refs.mjs
run node ./scripts/validate-peer-list-limit-default-line-refs.mjs
run node ./scripts/validate-a2a-status-limit-default-line-refs.mjs
run node ./scripts/validate-capability-list-print-format-line-refs.mjs
run node ./scripts/validate-intent-result-prefer-stream-line-refs.mjs
run node ./scripts/validate-settlement-backfill-section-preamble-line-refs.mjs

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
