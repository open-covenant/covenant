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
- Public provenance envelopes verify committed task evidence from Git object data, without yet claiming release signing or transparency-log publication.
- Solana settlement code is scaffolded, not production.
- Runtime isolation is trusted-local subprocess timeout enforcement with manifest-level sandbox requirements; gVisor execution is planned, not implemented.
- Live tests exist but are opt-in and cover only selected real boundaries.
- Live boundary coverage is tracked in `agent-os/autonomy/live-coverage.json` and summarized in `docs/live-coverage.md`.

## Invariants

- Privileged state should go through the daemon.
- Capability checks should happen before protected dispatches or mutations.
- Important rejections should produce audit rows.
- Token bytes, private keys, secrets, hostnames, personal usernames, and machine-local paths should not be logged or committed.
- Public docs must distinguish implemented, experimental, and planned behavior.
- Autonomous work is not done until it is reviewed, validated, and resumable.
- When the next autonomous task is already selected and no true blocker exists, continue into the next bounded slice instead of stopping at a status report.

## Current Gaps

- No production sandbox for untrusted agents.
- No production on-chain settlement.
- No public signing identity policy or transparency-log publication for agent-produced artifacts.
- No installer or stable SDK ecosystem.
- Multi-peer operation is experimental.
- Project memory has read-only drift reports; explicit repair commands, compaction, and long-horizon stale-context handling need more work.

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
- [docs/memory-drift.md](./memory-drift.md): read-only memory drift report contract.
- [docs/live-coverage.md](./live-coverage.md): opt-in live test surface matrix.
- [docs/runtime-sandbox-security.md](./runtime-sandbox-security.md): runtime isolation security contract.
- [docs/provenance/README.md](./provenance/README.md): alpha provenance envelope contract.
- [agent-os/autonomy/workflow.json](../agent-os/autonomy/workflow.json): lifecycle states, roles, gates, transitions, and definition of done.
- [agent-os/autonomy/tasks](../agent-os/autonomy/tasks): validated autonomous maintenance backlog.
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
