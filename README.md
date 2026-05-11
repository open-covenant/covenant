# Covenant

[![CI](https://github.com/open-covenant/covenant/actions/workflows/ci.yml/badge.svg)](https://github.com/open-covenant/covenant/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

> Agent- and blockchain-native operating layer for governed autonomous systems.

Covenant is an agent- and blockchain-native operating layer for autonomous software engineering systems. It coordinates agents, tools, memory, execution, authorization, audit, provenance, and settlement through a local control plane designed for research teams and engineering organizations building agents that operate across real codebases.

Covenant sits below agent applications and above the host operating system. It gives autonomous systems a disciplined substrate for planning, execution, review, repair, and handoff while keeping privileged actions scoped, inspectable, resumable, and attributable.

Covenant is developed through the same governed agent workflows it exposes: agents plan work, produce changes, record validation evidence, and leave provenance for human review. The result is infrastructure shaped around the operational requirements of agents rather than a human-only developer environment retrofitted with automation.

- **Web:** [opencovenant.org](https://opencovenant.org)
- **Docs:** [docs.opencovenant.org](https://docs.opencovenant.org)

## Why Covenant

Software agents are moving from interactive assistance toward long-running engineering work. That shift changes the infrastructure problem. Agents need durable context, explicit authority, reliable tool access, recovery after interruption, and a record of what happened.

Conventional developer environments assume a human operator is present at every step. Blockchain systems assume verifiable state transitions, explicit authority, and durable coordination across independent actors. Covenant brings those assumptions into agent infrastructure:

- **Governance:** intents, manifests, scoped permissions, review gates, and policy-aware dispatch.
- **Continuity:** persistent memory, resumable task state, repair workflows, and structured handoff.
- **Accountability:** append-only audit logs, integrity reports, signed actions, and commit-scoped provenance.
- **Interoperability:** native tools, MCP integration, A2A messaging, local gateway APIs, and protocol adapters.
- **Execution:** daemon-mediated runtime dispatch with timeout enforcement and sandbox-aware agent manifests.
- **Settlement:** local receipts and protocol scaffolding for accountable resource use and agent coordination economics.

## Architecture

The system center is `covenantd`, a Rust daemon that owns local state and mediates privileged operations through IPC, an HTTP gateway, signed capabilities, audit logs, memory stores, and runtime dispatch.

| # | Primitive | Role |
|---|---|---|
| 1 | Intent | Normalized request shapes for CLI, IPC, HTTP, routing, and daemon dispatch. |
| 2 | Runtime | Agent execution with timeout enforcement, manifest contracts, trusted-local subprocesses, and opt-in Linux gVisor runner support. |
| 3 | Memory | SQLite-backed working, episodic, and long-term records with embedding hooks, ignore rules, drift reports, repair, and bounded compaction. |
| 4 | Identity | Local ed25519 identity, peer registry, operator tokens, token rotation, and peer revocation. |
| 5 | Permissions | Signed capabilities with known-scope validation, dispatch-time enforcement, expiry, and revocation tombstones. |
| 6 | Comms | IPC frames, local HTTP gateway, MCP adapter, and A2A mailbox primitives. |
| 7 | Compositor | Next.js web console (`agent-os/covenant-web`) and public landing/docs surface; richer TUI deferred to Phase 4. |
| 8 | Settlement | Local resource receipts and protocol scaffolding for agent coordination economics. |

Audit underlies Identity, Permissions, and Settlement — append-only JSONL events, local hash-chain integrity reports, retention controls, signed actions, and audit-root attestations. The primary implementation lives in `agent-os/`, the Rust workspace containing the daemon, CLI, protocol crates, runtime, memory, permissions, peer authentication, audit, MCP and A2A adapters, budget ledger, and settlement components. The surrounding monorepo contains public documentation, web surfaces, circuits, SDK packages, and supporting services.

See [docs/audit-integrity.md](./docs/audit-integrity.md), [docs/capabilities.md](./docs/capabilities.md), and [agent-os/README.md](./agent-os/README.md) for implementation details and validation evidence.

## Capabilities

Covenant includes:

- Rust daemon and CLI for local agent orchestration.
- IPC and local HTTP gateway surfaces.
- Signed capability lifecycle for implemented namespaces, including grant-time validation, expiry, revocation, and dispatch-time scope enforcement.
- Peer authentication, operator token rotation, peer revocation, and peer-scoped A2A checks.
- Append-only audit log with structured event types, bounded reads, retention purge, and local hash-chain verification.
- SQLite-backed project memory across working, episodic, and long-term tiers.
- MCP adapter, native tool integration, and A2A mailbox primitives.
- Budget ledger primitives with daemon-backed pause checkpoint storage for budget exhaustion, shutdown drains, and single-use resume handoff.
- Local settlement receipts for resource accounting.
- Commit-scoped provenance envelopes that bind task records, changed Git blobs, transition events, and validation evidence.
- Unsigned or locally signed audit-root attestations for local integrity reports.
- Opt-in live tests for daemon, CLI, runtime, and selected backend boundaries.
- Source-built local installer for the daemon and CLI with a relative-path install manifest.
- CI coverage for Rust, documentation, workflow linting, live coverage matrix validation, provenance verification, dependency audits, and CodeQL.

## Validation

Run the scripts-only gate when the change does not need Rust tooling:

```bash
bash agent-os/scripts/validate.sh --scripts
```

Run the fast local gate from the repository root:

```bash
bash agent-os/scripts/validate.sh --quick
```

Run the full Rust validation gate:

```bash
bash agent-os/scripts/validate.sh
```

Verify committed provenance envelopes:

```bash
node agent-os/scripts/provenance.mjs verify-all
```

Build the public documentation surface:

```bash
pnpm --dir landing install --frozen-lockfile --ignore-workspace
pnpm --dir landing build
```

Run live boundary tests when host prerequisites are available:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

Inspect the live coverage inventory:

```bash
bash agent-os/scripts/test-stats.sh
```

## Research Direction

Covenant advances open infrastructure for:

- governed autonomous software maintenance;
- verifiable agent actions and commit-scoped provenance;
- capability-scoped delegation across local and remote agents;
- durable project memory for long-running work;
- resumable task ownership across interruptions;
- policy-aware tool use and sandboxed execution;
- audit-root attestations, public provenance, and agent coordination economics.

## Contributing

Covenant is systems infrastructure with security-sensitive boundaries. Contributions should include a validation plan, tests for changed behavior, and a clear statement of operational impact.

Start with [CONTRIBUTING.md](./CONTRIBUTING.md) and [ROADMAP.md](./ROADMAP.md). Changes touching identity, permissions, audit, runtime isolation, settlement, provenance, release automation, or CI should receive especially close review.

## Security

Follow [SECURITY.md](./SECURITY.md) for responsible disclosure. The runtime isolation boundary is tracked in [docs/runtime-sandbox-security.md](./docs/runtime-sandbox-security.md). Do not open public issues for vulnerabilities.

## License

Apache-2.0. See [LICENSE](./LICENSE).
