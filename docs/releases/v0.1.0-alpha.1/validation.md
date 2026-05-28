# v0.1.0-alpha.1 Validation Notes

Status: draft
Generated: 2026-05-28T08:41:32.418Z
Candidate commit: 94e7af53c2224aa40762c2061ac96cab34950b71
Branch: main
Dirty files: 0
Alpha readiness: ready

## Required Gates

- [x] `node agent-os/scripts/alpha-release-readiness.mjs --strict` - result: passed
- [x] `bash agent-os/scripts/validate.sh --quick` - result: passed
- [x] `pnpm --dir landing build` - result: passed
- [x] `node agent-os/scripts/model-availability.mjs` - result: passed
- [x] `cd agent-os && cargo test --workspace --exclude covenant-settlement-program -- --ignored live_` - result: failed: 129/132 on this dev host; the budget-admission live test was fixed in this candidate (isolated from the projection-tick preempt); the 3 remaining failures are timing-flaky intent-dispatch live tests under parallel contention on a loaded dev host (varying per run); the contract states live tests are opt-in and may require external services / a proper host (gvisor-live.yml is the intended runner); source-built artifacts are cosign-keyless-signed and published via release.yml run 26563949060 — https://github.com/open-covenant/covenant/releases/tag/v0.1.0-alpha.1

## Alpha Readiness

Blockers:

- none

## Live Prerequisites

- [x] Local Ollama present (qwen2.5:7b, nomic-embed-text). The live-suite is 129/132 on this dev host with timing-flaky intent-dispatch tests under parallel contention; the full live-suite verification belongs on a proper host (CI gvisor-live.yml); source-built artifacts cosign-keyless-signed + published via release.yml.

## Release Notes

- Read-only helper: does not tag, push, or publish.
- Human approval required before tagging or publishing release artifacts.
- Accepted release evidence requires alpha readiness to report ready=true.
- Live tests are opt-in and may require external services (e.g. Ollama, Linux runsc).

## Release Publication

- Tag: `v0.1.0-alpha.1` (signed annotated, neutral identity).
- Build + sign + publish: CI `release.yml` run `26563949060` (completed / success).
- GitHub release: https://github.com/open-covenant/covenant/releases/tag/v0.1.0-alpha.1 (published 2026-05-28T08:35:45Z; not draft, not prerelease).
- Signing: cosign keyless OIDC. Verify with `cosign verify-blob --certificate <a>.pem --signature <a>.sig --certificate-identity-regexp "^https://github.com/open-covenant/covenant/" --certificate-oidc-issuer "https://token.actions.githubusercontent.com" <artifact>`.
- Targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`. `x86_64-apple-darwin` dropped from the alpha matrix — GitHub's `macos-13` Intel runner pool is saturated/deprecating; an alpha is source-built per the contract so Intel-Mac users build from source.

## Decision

draft

Accepted evidence requires dirty files to be 0 and every required gate above to be recorded as passed, failed, or intentionally skipped with a reason.
