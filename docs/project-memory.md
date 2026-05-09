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
- Capability grants validate non-empty scopes for known action namespaces before signing; dispatch-time enforcement interprets exact `tool.call.*` argument allowlists and scoped `audit.purge` cutoffs, then otherwise falls back to action predicates.
- Audit logs have local SHA-256 hash-chain sidecars, operator-only integrity reports, and unsigned or locally signed `audit-root-attestation.v1` payload generation/verification.
- Public provenance envelopes verify committed task evidence from Git object data, without yet claiming release signing or transparency-log publication.
- Solana settlement code is scaffolded, not production.
- Runtime isolation has trusted-local subprocess timeout enforcement, manifest-level sandbox requirements, daemon-selectable Linux gVisor configuration, an initial `runsc` runner, opt-in live Linux gVisor coverage, and a repeatable Linux runner guide.
- Live tests exist but are opt-in and cover selected real process, socket, restart, HTTP, CLI, and external-service boundaries.
- Live boundary coverage is tracked in `agent-os/autonomy/live-coverage.json` and summarized in `docs/live-coverage.md`.
- Autonomous sprint state can be summarized with `node agent-os/scripts/autonomy-summary.mjs`.

## Invariants

- Privileged state should go through the daemon.
- Capability checks should happen before protected dispatches or mutations.
- Important rejections should produce audit rows.
- Token bytes, private keys, secrets, hostnames, personal usernames, and machine-local paths should not be logged or committed.
- Recent local and upstream commit authors/committers should pass `agent-os/scripts/validate-git-identity.mjs` before autonomous work is pushed.
- Public docs must distinguish implemented, experimental, and planned behavior.
- Autonomous work is not done until it is reviewed, validated, and resumable.
- When the next autonomous task is already selected and no true blocker exists, continue into the next bounded slice instead of stopping at a status report.
- After each successful commit or push, run `node agent-os/scripts/autonomy-continue.mjs`; if it names an unblocked task, continue immediately. A final status response is allowed only when every candidate is blocked, the user asks to pause, or the execution environment forces a turn boundary.

## Current Gaps

- No production sandbox for untrusted agents.
- No production on-chain settlement.
- No completed public key custody policy, release publication path, or transparency-log publication for agent-produced artifacts or audit roots.
- No installer or stable SDK ecosystem.
- Multi-peer operation is experimental.
- Dispatch-time capability scope predicates exist for exact `tool.call.*` argument allowlists and `audit.purge` cutoffs; memory, peer, A2A, and settlement predicates still need enforcement.
- Project memory has read-only drift reports, explicit dry-run/apply repair commands, and bounded compaction commands that delete expired working/episodic records while marking long-term stale context instead of deleting it.
- Audit integrity is local tamper evidence only; immutable retention, public key custody, release publication, and transparency-log publication are not implemented.
- A2A has lease-age status filters plus manual requeue and force-error repair through IPC, HTTP, and CLI; automatic retry remains disabled until task classes can declare idempotency safely.

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
- [docs/autonomous-development.md](./autonomous-development.md): autonomous workflow protocol.
- [docs/repo-map.md](./repo-map.md): repository structure.
- [docs/capabilities.md](./capabilities.md): signed capability scope contract and enforcement boundary.
- [docs/memory-drift.md](./memory-drift.md): read-only memory drift report contract.
- [docs/audit-integrity.md](./audit-integrity.md): local audit hash-chain and verification boundary.
- [docs/decisions/0004-audit-root-signing-policy.md](./decisions/0004-audit-root-signing-policy.md): planned public audit-root signing policy.
- [docs/live-coverage.md](./live-coverage.md): opt-in live test surface matrix.
- [docs/runtime-sandbox-security.md](./runtime-sandbox-security.md): runtime isolation security contract.
- [docs/provenance/README.md](./provenance/README.md): alpha provenance envelope contract.
- [agent-os/autonomy/workflow.json](../agent-os/autonomy/workflow.json): lifecycle states, roles, gates, transitions, and definition of done.
- [agent-os/autonomy/backlog.json](../agent-os/autonomy/backlog.json): durable seed queue used when no active task is ready.
- [agent-os/autonomy/tasks](../agent-os/autonomy/tasks): active and completed autonomous maintenance tasks.
- [agent-os/scripts/autonomy-summary.mjs](../agent-os/scripts/autonomy-summary.mjs): deterministic sprint and handoff summary generator.
- [agent-os/README.md](../agent-os/README.md): local daemon workspace.
- [agent-os/00_spec.md](../agent-os/00_spec.md): product spec.

## Validation

From the repository root:

```bash
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/validate.sh
```

From `agent-os/`, when real-boundary coverage matters:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```
