# Alpha Release Contract

This contract defines what Covenant may truthfully call an alpha release. It is a release boundary, not a claim that the system is production-safe or complete.

The alpha is source-built local infrastructure for engineers and researchers who can inspect the code, run the validation gates, and operate the daemon with explicit trust boundaries. It is not an installer-backed consumer product, a hosted service, a production sandbox, or a live settlement network.

Human approval is required before creating or publishing any alpha tag, release artifact, package, or announcement.

## Supported Alpha Scope

An alpha release may include:

- the Rust workspace under `agent-os/`, including `covenantd`, the `covenant` CLI, protocol crates, runtime, memory, permissions, audit, identity, peer-auth, MCP, A2A, budget, and local settlement crates;
- local daemon operation over Unix IPC and the local HTTP gateway;
- source-built CLI workflows for intent dispatch, capabilities, peers, memory, audit, A2A, tools, receipts, chain status, and verification;
- trusted-local subprocess execution for agents that do not require sandbox isolation;
- fail-closed handling for manifests that require a sandbox when the selected runtime cannot satisfy it;
- opt-in Linux gVisor runner validation where host prerequisites are met;
- SQLite-backed memory, ignore rules, explicit repair commands, bounded compaction, and read-only drift verification;
- ed25519 local identity, peer tokens, token rotation, revocation, and signed capability scopes for implemented namespaces;
- append-only audit JSONL, local hash-chain integrity reports, recent audit reads, purge controls, and unsigned or locally signed audit-root attestations;
- durable A2A queue state, lease-age status filters, manual requeue and force-error repair, and repair audit rows;
- local settlement receipts for resource accounting;
- autonomy task records, transition events, project memory, live coverage matrix, identity guards, and commit-scoped provenance envelopes.

The alpha release must be described as local-first, source-built, and experimental.

## Required Release Evidence

Before an alpha tag is allowed, the release operator must record:

- the release commit and tag candidate;
- the supported host assumptions: macOS or Linux for trusted-local development, Linux-only for gVisor live validation;
- exact validation commands and pass/fail/skipped outcomes;
- the current capability status matrix from `docs/status.md`;
- the live boundary coverage matrix from `docs/live-coverage.md`;
- the security boundary from `docs/runtime-sandbox-security.md`;
- the provenance envelope for the release task or release commit;
- an audit-root attestation if the release process generates one for the candidate.

Minimum validation for a source alpha:

```bash
bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check
```

The full Rust gate should also pass before a public release unless a skipped external-service live path is explicitly recorded:

```bash
bash agent-os/scripts/validate.sh
```

Opt-in live tests are release evidence only when their prerequisites are named. Linux gVisor evidence must follow `docs/gvisor-live-runner.md`.

## Explicit Non-Claims

An alpha release must not claim:

- production sandbox-grade execution by default;
- Firecracker isolation;
- browser, desktop, or compositor control;
- on-chain settlement on mainnet;
- a deployed, audited settlement program;
- immutable public audit retention;
- public key custody for release signing;
- transparency-log publication;
- package-manager installers;
- stable SDKs;
- a marketplace or registry;
- multi-host production operation;
- autonomous self-improvement without human authority;
- safety for untrusted third-party agents.

If public language needs one of those claims, the implementation and evidence must land first or the language must mark it as planned research.

## Alpha Blockers

These block an alpha tag:

- the local daemon or CLI cannot be built from a clean checkout;
- the quick validation gate fails;
- autonomy task records or transition events fail validation;
- the public docs make unsupported sandbox, settlement, installer, SDK, provenance, or autonomy claims;
- the current Git identity guard fails;
- the release commit contains personal identity metadata, private host paths, private key names, AI attribution trailers, or local-only operator state;
- security docs do not match the runtime and capability boundary implemented in code;
- a human release operator has not approved the tag and publication text.

These do not block a source alpha if they are documented as unsupported:

- no package installer;
- no production gVisor CI host;
- no Firecracker backend;
- no Solana deployment;
- no public release signing key custody;
- no transparency log;
- no SDK publication;
- no automatic A2A retry.

## Post-Alpha Research

Post-alpha work is tracked separately from release blockers:

- signed release artifacts and key custody policy;
- transparency-log publication for audit roots and release attestations;
- production Linux sandbox CI with pinned rootfs fixtures;
- Firecracker runtime backend;
- on-chain settlement deployment, audit, mint policy, oracle policy, and treasury controls;
- installer and upgrade path;
- SDK publication and compatibility policy;
- multi-host identity and A2A operation;
- automatic retry semantics after task classes can declare idempotency;
- native OS integration and compositor research.

## Public Wording Rules

Use:

- "alpha release target" before the release exists;
- "source-built alpha" after a tag is published without installers;
- "local-first control plane" for the current daemon and CLI;
- "experimental" for provenance, autonomous workflow, MCP, A2A hardening, and gVisor paths;
- "planned" or "research" for settlement network, public signing, installer, SDK, marketplace, and compositor claims.

Do not use:

- "production-ready";
- "mainnet settlement";
- "secure sandbox by default";
- "fully autonomous";
- "self-improving without oversight";
- "installer-ready";
- "SDK stable";
- "live marketplace".
