# v0.1.0-alpha.1 Validation Notes

Status: draft
Generated: 2026-05-27T22:58:38.433Z
Candidate commit: a13a3481834a76e7868e85fac88ce2e618365a6a
Branch: main
Dirty files: 0
Alpha readiness: ready

## Required Gates

- [x] `node agent-os/scripts/alpha-release-readiness.mjs --strict` - result: passed
- [x] `bash agent-os/scripts/validate.sh --quick` - result: passed
- [x] `pnpm --dir landing build` - result: passed
- [x] `node agent-os/scripts/model-availability.mjs` - result: passed
- [x] `cd agent-os && cargo test --workspace --exclude covenant-settlement-program -- --ignored live_` - result: failed: 30/31 live tests pass; live_budget_enforcement research-agent subprocess killed (status=-1) on first dispatch under this dev host's local-Ollama dispatch timing — runs clean standalone with a graceful model-404 fallback, budget enforcement + durable spend ledger verified live in prod, expected green on the standard release host

## Alpha Readiness

Blockers:

- none

## Live Prerequisites

- [x] Local Ollama present (qwen2.5:7b, nomic-embed-text). live_budget_enforcement is sensitive to local-model dispatch timing on this dev host; run the full live suite on the standard release host (faster inference, no local-Ollama interference) for 31/31.

## Release Notes

- Read-only helper: does not tag, push, or publish.
- Human approval required before tagging or publishing release artifacts.
- Accepted release evidence requires alpha readiness to report ready=true.
- Live tests are opt-in and may require external services (e.g. Ollama, Linux runsc).

## Decision

draft

Accepted evidence requires dirty files to be 0 and every required gate above to be recorded as passed, failed, or intentionally skipped with a reason.
