# Release Validation

Covenant release validation records the operating surfaces, evidence, and verification gates used to ship the local control plane with disciplined engineering traceability.

The profile is evidence-based: every release candidate should identify the commit under review, supported host assumptions, validation results, capability status, live boundary coverage, runtime security boundary, provenance envelope, and audit-root attestations produced for the candidate.

## Operating Surfaces

The validation profile covers:

- the Rust workspace under `agent-os/`, including `covenantd`, the `covenant` CLI, protocol crates, runtime, memory, permissions, audit, identity, peer-auth, MCP, A2A, budget, and local settlement crates;
- local daemon operation over Unix IPC and the local HTTP gateway;
- CLI workflows for intent dispatch, capabilities, peers, memory, audit, A2A, tools, receipts, chain status, and verification;
- trusted-local subprocess execution and fail-closed handling for sandbox-required manifests;
- opt-in Linux gVisor runner validation where host prerequisites are met;
- SQLite-backed memory, ignore rules, repair commands, bounded compaction, and read-only drift verification;
- ed25519 local identity, peer tokens, token rotation, revocation, and signed capability scopes for implemented namespaces;
- append-only audit JSONL, local hash-chain integrity reports, recent audit reads, purge controls, and audit-root attestations;
- durable A2A queue state, lease-age status filters, manual requeue and force-error repair, disabled-by-default retry scans, opt-in scheduler scans, and repair/scheduler audit rows;
- local settlement receipts for resource accounting;
- autonomy task records, transition events, project memory, live coverage matrix, identity guards, and commit-scoped provenance envelopes.

## Evidence

Release evidence should include:

- release commit and tag candidate;
- supported host assumptions: macOS or Linux for trusted-local development, Linux-only for gVisor live validation;
- exact validation commands and pass/fail/skipped outcomes;
- capability status matrix from `docs/status.md`;
- live boundary coverage matrix from `docs/live-coverage.md`;
- runtime security boundary from `docs/runtime-sandbox-security.md`;
- provenance envelope for the release task or release commit;
- audit-root attestation when the release process generates one.

## Alpha Release Evidence Runbook

Create the release evidence bundle from the repository root:

```bash
node agent-os/scripts/alpha-release-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/alpha-release-validate-bundle.mjs v0.1.0-alpha.1
```

The bundle scaffold writes `docs/releases/<release-id>/evidence.json` and `docs/releases/<release-id>/validation.md`. It records the current commit, short commit, branch, dirty-file count, recommended validation commands, and release notes. It does not tag, push, publish, sign, or execute validation gates.

Interpret the output as follows:

- `commit` is the release candidate under review.
- `branch` is the local branch used to generate evidence.
- `dirty_files` must be `0` before evidence is accepted for a release candidate.
- `commands` are the gates the operator must run and record.
- `notes` identify non-claims and live-test prerequisites.

Store release evidence under `docs/releases/<release-id>/`. A candidate bundle should contain the `evidence.json` output, validation notes with pass/fail/skipped outcomes, links to provenance envelopes or audit-root attestations, and the release decision. The scaffold refuses to overwrite an existing non-empty bundle unless `--force` is supplied. The validator rejects accepted release evidence when files are missing, evidence JSON is malformed, dirty-file count is non-zero, gate results remain pending, or the decision is still `draft`.

Signing, tagging, artifact upload, release announcements, and key rotation are human-owned decisions until a project signing and publication policy is implemented.

Minimum validation:

```bash
node agent-os/scripts/alpha-release-evidence.mjs
bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/validate-readme-copy.mjs
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check
```

Full Rust validation:

```bash
bash agent-os/scripts/validate.sh
```

Live tests are evidence when their prerequisites are recorded. Linux gVisor evidence should follow `docs/gvisor-live-runner.md`.

## Review Criteria

A release candidate should not proceed when:

- the local daemon or CLI cannot be built from a clean checkout;
- the quick validation gate fails;
- autonomy task records or transition events fail validation;
- public documentation is not aligned with implemented behavior;
- the current Git identity guard fails;
- the release commit contains personal identity metadata, private host paths, private key names, AI attribution trailers, or local-only operator state;
- security docs do not match the runtime and capability boundary implemented in code.
