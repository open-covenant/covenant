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
- durable A2A queue state, lease-age status filters, manual requeue and force-error repair, and repair audit rows;
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

Minimum validation:

```bash
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
