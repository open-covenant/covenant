# Project Memory

This file is durable context for humans and agents working on Covenant. Keep it concise. If a fact changes, update this file in the same change that makes it false.

## Stable Thesis

Covenant is an agent-native operating layer for autonomous software systems. It should give agents structured control over execution, tools, memory, permissions, provenance, audit, coordination, and long-running maintenance while preserving human authority over strategy and irreversible decisions.

## Current System Shape

- `agent-os/` is the core operating-layer workspace.
- `covenantd` is the local enforcement boundary.
- `covenant` is the CLI client.
- State lives under `$COVENANT_HOME`; default is `$HOME/.covenant`.
- Identity, permissions, audit, memory, peer auth, budget, MCP, A2A, and local settlement crates exist.
- Identity provenance has a local read-only `identity-provenance` dry-run report that records key-file metadata, public peer subjects, redacted token prefixes, and inferred token rotation history without exporting local paths, display strings, identity seeds, or full peer tokens.
- IPC/HTTP protocol metadata is v1-only; v2 has a staging fixture directory and fail-closed tests requiring versioned fixtures plus migration notes before any supported bump.
- Capability grants validate non-empty scopes for known action namespaces before signing; dispatch-time enforcement interprets exact `tool.call.*` argument allowlists, scoped `audit.purge` cutoffs, and memory read/write/purge/repair/compaction predicates, then otherwise falls back to action predicates.
- Audit logs have local SHA-256 hash-chain sidecars, operator-only integrity reports, unsigned or locally signed `audit-root-attestation.v1` payload generation/verification, and release-target audit-root binding to embedded release subject digests.
- Local memory settlement receipts carry `memory_record_id` for daemon-created memory writes; verifier reconciliation joins exactly when the field exists and falls back to owner/resource counts for legacy receipt rows.
- Settlement receipt migration planning has a read-only JSONL inspector that separates malformed rows, already correlated memory receipts, and legacy uncorrelated memory receipts without exporting local paths, payer display strings, or raw malformed row contents.
- Public provenance envelopes verify committed task evidence from Git object data, without yet claiming release signing or transparency-log publication.
- Release provenance readiness uses `agent-os/scripts/release-provenance-readiness.mjs`; local subject and verifier planning is separate from project key custody, artifact publication, and transparency-log publication.
- Alpha release language must follow `docs/alpha-release-contract.md`: source-built local infrastructure, explicit non-claims, sanitized readiness evidence, and human approval before any tag or artifact publication.
- Source installs use `agent-os/scripts/install-source.mjs`; it builds `covenantd` and `covenant` into an operator-selected local prefix and records a relative-path manifest without claiming signed packages or SDK stability.
- Distribution graduation uses `agent-os/scripts/distribution-readiness.mjs`; source-alpha install readiness is separate from package-manager distribution, signed artifacts, SDK stability, and upgrade safety.
- On-chain settlement deployment readiness uses `agent-os/scripts/settlement-deployment-readiness.mjs`; local scaffold readiness is separate from deployment, security-review acceptance, oracle policy, mint authority custody, and emergency operations.
- Release validation language must follow `docs/release-validation.md`: public claims stay aligned with implementation evidence and validation coverage.
- Solana settlement code is scaffolded, not production.
- Runtime isolation has trusted-local subprocess timeout enforcement, manifest-level sandbox requirements, daemon-selectable Linux gVisor configuration, an initial `runsc` runner, opt-in live Linux gVisor coverage, and a repeatable Linux runner guide.
- Linux gVisor CI promotion uses `agent-os/scripts/gvisor-host-readiness.mjs`; host-local live readiness is separate from required CI, pinned rootfs provenance, and public sandbox-readiness claims.
- The live coverage matrix records Linux gVisor promotion metadata and scoped delegated capability evidence, including prerequisite skips versus real configured-host failures; default validation must not require non-Linux hosts to run gVisor.
- Live tests exist but are opt-in and cover selected real process, socket, restart, HTTP, CLI, and external-service boundaries.
- Live boundary coverage is tracked in `agent-os/autonomy/live-coverage.json` and summarized in `docs/live-coverage.md`.
- Privileged CLI live coverage is tracked at verb level in `agent-os/autonomy/privileged-cli-live-matrix.json`; deferred rows must name the implementation boundary that blocks live coverage.
- Autonomous sprint state can be summarized with `node agent-os/scripts/autonomy-summary.mjs` and locally published or checked with `node agent-os/scripts/autonomy-publish-summary.mjs`.
- Summary output should pass `agent-os/scripts/validate-autonomy-summary.mjs` before it is used as published sprint evidence.

## Invariants

- Privileged state should go through the daemon.
- Capability checks should happen before protected dispatches or mutations.
- Important rejections should produce audit rows.
- Token bytes, private keys, secrets, hostnames, personal usernames, and machine-local paths should not be logged or committed.
- Recent local and upstream commit authors/committers should pass `agent-os/scripts/validate-git-identity.mjs`; pre-push should pass the exact pushed ref range to the same validator.
- The active local Git author and committer should be configured with `agent-os/scripts/configure-git-identity.mjs` and pass `agent-os/scripts/validate-current-git-identity.mjs` before any autonomous commit is created.
- The local Git metadata directory should pass `agent-os/scripts/validate-git-write-access.mjs` before a task is treated as committable; a read-only `.git` directory is a real commit blocker, not a code blocker.
- Commit and push attribution policy should pass `agent-os/scripts/validate-commit-rotation.mjs`; per-clone overrides must stay untracked.
- Local GitHub CLI state should pass `agent-os/scripts/validate-github-cli-account.mjs` before remote write operations; Git metadata can be neutral while web attribution still follows the authenticated account.
- GitHub PushEvent attribution follows the credential that updates the ref; pre-push must pass `agent-os/scripts/validate-github-push-identity.mjs`, and remote writes require a repository-owned deploy key, GitHub App, or approved bot account.
- Autonomous sessions should run `agent-os/scripts/autonomy-preflight.mjs` before starting a fresh slice so commit blockers and push blockers are visible separately.
- If a session cannot commit, `agent-os/scripts/autonomy-dirty-report.mjs --json` should be used as the handoff artifact for dirty paths, active task state, diff stats, and environment blockers.
- If untracked files exist in a commit-blocked session, `agent-os/scripts/autonomy-handoff-bundle.mjs --json` should be used to export the tracked patch plus bounded UTF-8 untracked file contents for reconstruction in a writable checkout.
- Handoff bundles should pass `agent-os/scripts/autonomy-verify-handoff-bundle.mjs --stdin` before another environment restores them.
- Restore sequencing for a handoff bundle should come from `agent-os/scripts/autonomy-plan-handoff-restore.mjs --stdin` so the base commit, untracked files, tracked patch, and validation order are explicit.
- Handoff command changes should pass `agent-os/scripts/validate-autonomy-handoff.mjs` so dirty report, bundle export, verification, restore planning, and tamper rejection stay consistent.
- Alpha readiness includes `agent-os/scripts/validate-autonomy-handoff.mjs`; release work must not proceed from a commit-blocked checkout unless the handoff bundle path can prove the dirty state is recoverable.
- When the autonomy backlog is exhausted, run `agent-os/scripts/autonomy-status-gaps.mjs --json` before inventing templates. It extracts candidate hardening work from `docs/status.md` without writing files.
- `agent-os/scripts/autonomy-review-artifact.mjs <task-id> --json` emits unsigned task review evidence. Do not describe review artifacts as signed until signing policy and key custody are implemented.
- Verify review artifacts with `agent-os/scripts/autonomy-verify-review-artifact.mjs --stdin`; it recomputes source task and event digests from local repository state. Signed artifacts require `--trusted-public-key-spki-base64`.
- Review artifact command changes should pass `agent-os/scripts/validate-autonomy-review-artifacts.mjs` so generation, verification, and tamper rejection stay consistent.
- Alpha readiness includes `agent-os/scripts/validate-autonomy-review-artifacts.mjs`; release preparation must not treat review evidence as signing evidence until signing policy and key custody exist.
- Review artifact verification supports `covenant.autonomy-review-signature.v1` only when an explicit trusted ed25519 SPKI public key is supplied; generated review artifacts remain unsigned by default until project key custody is approved.
- Alpha release evidence embeds sanitized `agent-os/scripts/alpha-release-readiness.mjs --json` state. Accepted bundles should reject blocked readiness unless they are explicitly being validated as draft blocker-review artifacts.
- Alpha release evidence uses schema `covenant.alpha-release-evidence.v1`; bundle validation should fail closed on unversioned or incompatible evidence.
- Alpha release bundles include `manifest.json` using schema `covenant.alpha-release-manifest.v1`; the manifest locally binds every regular bundle file except itself by relative path, byte count, and SHA-256 digest without claiming signing or transparency publication.
- Alpha release validation note metadata (`Status`, `Generated`, `Candidate commit`, `Branch`, `Dirty files`, and `Alpha readiness`) must match `evidence.json`.
- Alpha release bundle validation requires explicit gate outcomes for every evidence command; skipped gates must carry a reason and pending gates are draft-only.
- Alpha release bundles with decision `accepted` require every evidence command to be checked and `result: passed`; failed or skipped gates belong in rejected or superseded evidence.
- Alpha release gate outcome lines only count under `## Required Gates`; copied command lines elsewhere in validation notes must not satisfy evidence validation.
- Alpha release readiness blocker ids only count under `## Alpha Readiness`; copied blocker ids elsewhere in validation notes must not satisfy blocker review.
- `agent-os/scripts/validate-alpha-release-evidence.mjs` should prove both rejection paths and a synthetic clean/ready accepted bundle path without depending on the current checkout state.
- If `origin/main` contains a commit that fails `validate-git-identity.mjs`, do not merge it locally; replace the remote ref only through an approved neutral write credential.
- Public docs must distinguish implemented, experimental, and planned behavior.
- The root README must pass `node agent-os/scripts/validate-readme-copy.mjs` after public copy or status changes.
- Autonomous work is not done until it is reviewed, validated, and resumable.
- When the next autonomous task is already selected and no true blocker exists, continue into the next bounded slice instead of stopping at a status report.
- After each successful commit or push, run `node agent-os/scripts/autonomy-continue.mjs`; if it names an unblocked task, continue immediately. A final status response is allowed only when every candidate is blocked, the user asks to pause, or the execution environment forces a turn boundary.

## Current Gaps

- No production sandbox for untrusted agents.
- No required Linux gVisor CI runner; read-only host readiness gates record runsc, rootfs, runner provisioning, and failure-policy blockers.
- No production on-chain settlement; read-only deployment readiness gates record security-review, oracle, mint authority, and emergency-operation blockers.
- No completed public key custody policy, release publication path, or transparency-log publication for agent-produced artifacts or audit roots.
- No public release provenance or public identity attestation publication; read-only release and identity provenance gates record custody, publication, and transparency blockers.
- No public package-manager distribution, signed release artifacts, automatic upgrades, or stable SDK ecosystem; read-only distribution gates record those blockers.
- Multi-peer operation is experimental.
- Dispatch-time capability scope predicates exist for exact `tool.call.*` argument allowlists, `audit.purge` cutoffs, memory read/write/purge/repair/compaction paths, A2A send/recv/respond/repair paths, peer delegated list/revoke plus purge-retention paths, and chain receipt read/batch/flush paths; live coverage now pins scoped delegated `peers.revoke` denial and allowed mutation evidence.
- Project memory has read-only drift reports, explicit dry-run/apply repair commands, and bounded compaction commands that delete expired working/episodic records while marking long-term stale context instead of deleting it.
- Memory maintenance has read-only `covenant memory plan-compaction --json`, `covenant memory plan-receipt-backfill --json`, and settlement receipt JSONL migration planning surfaces; receipt backfill for legacy uncorrelated rows remains future mutation work.
- Budget pause checkpoints are wired through the daemon for budget-exhausted dispatches, single-use resume claims, and shutdown drains of active budgeted dispatches; hard subprocess preemption remains future runtime work.
- Audit integrity is local tamper evidence only; immutable retention, public key custody, release publication, and transparency-log publication are not implemented.
- A2A has lease-age status filters, manual requeue and force-error repair through IPC/HTTP/CLI, explicit task-kind metadata with legacy `intent_text` fallback for idempotency cache keys, receiver-side idempotency result caching, an explicit disabled-by-default retry gate, and an opt-in daemon scheduler that reuses the same bounded idempotent retry policy with audit-visible scan summaries.
- A2A repair visibility uses `agent-os/scripts/a2a-repair-visibility.mjs`; operator repair visibility is separate from delegated repair, per-peer repair reports, and peer-mismatched denial coverage.

## Human Authority Boundary

Agents may inspect, implement, test, document, and propose repairs. Humans retain authority for:

- credentials and third-party accounts;
- destructive operations;
- production deployments;
- legal, governance, and financial decisions;
- phase completion claims;
- public releases.

## Useful Entry Points

- [README.md](../README.md): public positioning and status.
- [ROADMAP.md](../ROADMAP.md): capability roadmap.
- [docs/status.md](./status.md): implemented, experimental, and planned capability matrix.
- [docs/alpha-release-contract.md](./alpha-release-contract.md): source alpha boundary, blockers, non-claims, and post-alpha research split.
- [docs/autonomous-development.md](./autonomous-development.md): autonomous workflow protocol.
- [docs/repo-map.md](./repo-map.md): repository structure.
- [docs/protocol-versioning.md](./protocol-versioning.md): IPC/HTTP protocol versioning, compatibility windows, and fixture replay policy.
- [docs/protocol-migrations/README.md](./protocol-migrations/README.md): protocol migration note requirements.
- [docs/capabilities.md](./capabilities.md): signed capability scope contract and enforcement boundary.
- [docs/budget-pause-checkpoints.md](./budget-pause-checkpoints.md): budget pause checkpoint format and daemon integration boundary.
- [docs/memory-maintenance.md](./memory-maintenance.md): read-only compaction planning and receipt backfill boundary.
- [docs/settlement-receipt-migration.md](./settlement-receipt-migration.md): settlement receipt JSONL migration dry-run and mutation boundary.
- [docs/source-install.md](./source-install.md): source-built local installer and manifest contract.
- [docs/distribution-readiness.md](./distribution-readiness.md): public distribution, signing, SDK stability, and upgrade gate contract.
- [docs/on-chain-settlement-readiness.md](./on-chain-settlement-readiness.md): on-chain deployment, oracle, mint authority, and emergency-operation gate contract.
- [docs/memory-drift.md](./memory-drift.md): read-only memory drift report contract.
- [docs/audit-integrity.md](./audit-integrity.md): local audit hash-chain and verification boundary.
- [docs/decisions/0004-audit-root-signing-policy.md](./decisions/0004-audit-root-signing-policy.md): planned public audit-root signing policy.
- [docs/provenance/audit-root-release-custody.md](./provenance/audit-root-release-custody.md): audit-root release-subject binding and custody checklist.
- [docs/identity-provenance.md](./identity-provenance.md): local identity key and peer-token provenance dry-run boundary.
- [docs/live-coverage.md](./live-coverage.md): opt-in live test surface matrix.
- [docs/privileged-cli-live-matrix.md](./privileged-cli-live-matrix.md): command-level privileged CLI live coverage contract.
- [docs/runtime-sandbox-security.md](./runtime-sandbox-security.md): runtime isolation security contract.
- [docs/gvisor-host-readiness.md](./gvisor-host-readiness.md): Linux gVisor host readiness and CI promotion gates.
- [docs/provenance/README.md](./provenance/README.md): alpha provenance envelope contract.
- [docs/provenance/release-readiness.md](./provenance/release-readiness.md): release provenance readiness gates.
- [docs/provenance/review-artifact-signing.md](./provenance/review-artifact-signing.md): signed review artifact envelope and custody boundary.
- [agent-os/scripts/validate-alpha-release-evidence.mjs](../agent-os/scripts/validate-alpha-release-evidence.mjs): alpha evidence and readiness gate regression check.
- [agent-os/autonomy/workflow.json](../agent-os/autonomy/workflow.json): lifecycle states, roles, gates, transitions, and definition of done.
- [agent-os/autonomy/backlog.json](../agent-os/autonomy/backlog.json): durable seed queue used when no active task is ready.
- [agent-os/autonomy/tasks](../agent-os/autonomy/tasks): active and completed autonomous maintenance tasks.
- [agent-os/scripts/autonomy-status-gaps.mjs](../agent-os/scripts/autonomy-status-gaps.mjs): read-only backlog-refill candidates from the capability status matrix.
- [agent-os/scripts/autonomy-review-artifact.mjs](../agent-os/scripts/autonomy-review-artifact.mjs): unsigned task review artifact scaffold.
- [agent-os/scripts/autonomy-verify-review-artifact.mjs](../agent-os/scripts/autonomy-verify-review-artifact.mjs): verifier for unsigned review artifacts and signed artifacts with an explicit trusted public key.
- [agent-os/scripts/validate-autonomy-review-artifacts.mjs](../agent-os/scripts/validate-autonomy-review-artifacts.mjs): review artifact toolchain validator.
- [agent-os/scripts/autonomy-summary.mjs](../agent-os/scripts/autonomy-summary.mjs): deterministic sprint and handoff summary generator.
- [agent-os/scripts/autonomy-publish-summary.mjs](../agent-os/scripts/autonomy-publish-summary.mjs): repository-scoped Markdown publication/check wrapper for sprint summaries.
- [agent-os/scripts/validate-autonomy-summary.mjs](../agent-os/scripts/validate-autonomy-summary.mjs): sprint summary output validator.
- [agent-os/scripts/validate-commit-rotation.mjs](../agent-os/scripts/validate-commit-rotation.mjs): commit rotation policy validator.
- [agent-os/README.md](../agent-os/README.md): local daemon workspace.
- [agent-os/00_spec.md](../agent-os/00_spec.md): product spec.
- [docs/a2a-idempotency-policy.md](./a2a-idempotency-policy.md): idempotency policy required before automatic A2A retry.
- [docs/a2a-repair-visibility.md](./a2a-repair-visibility.md): operator repair visibility and delegated repair gate contract.

## Validation

From the repository root:

```bash
bash agent-os/scripts/validate.sh --scripts
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/validate.sh
```

From `agent-os/`, when real-boundary coverage matters:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```
