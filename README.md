# Covenant

[![CI](https://github.com/open-covenant/covenant/actions/workflows/ci.yml/badge.svg)](https://github.com/open-covenant/covenant/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

> Operating-layer infrastructure for governed autonomous software engineering.

Covenant is an open control plane for AI agents that need to operate against real codebases, tools, and execution environments with durable memory, scoped authority, auditability, and verifiable provenance.

It is designed for research teams and engineering organizations building long-running autonomous systems where every privileged action must be bounded, inspectable, resumable, and attributable. Covenant sits below agent applications and above the host operating system, giving agents a disciplined substrate for planning, execution, review, repair, and handoff.

- **Web:** [opencovenant.org](https://opencovenant.org)
- **Docs:** [docs.opencovenant.org](https://docs.opencovenant.org)

## Why Covenant

Advanced software agents are moving beyond chat sessions and isolated tool calls. They need to maintain context over time, coordinate with other agents, enforce policy before touching sensitive surfaces, preserve evidence for what changed, and recover cleanly after interruptions.

Conventional developer environments were built around a human operator sitting at the terminal. Covenant adds the missing operating layer for autonomous engineering systems:

- **Control:** dispatch work through explicit intents, routes, manifests, and review gates.
- **Authority:** grant only the capabilities required for a task, with expiry, revocation, and scoped enforcement.
- **Memory:** persist project context in tiered records with drift checks and repair workflows.
- **Provenance:** bind agent-produced changes to task state, validation, audit events, and Git object data.
- **Continuity:** keep work resumable across process failures, context changes, and multi-step handoffs.
- **Interoperability:** expose native tooling, MCP integration, A2A messaging, and local gateway APIs.

## Platform Model

Covenant is not a single-agent framework and not a hosted wrapper around an LLM. The system center is `covenantd`, a Rust daemon that owns local state and mediates privileged operations through IPC, an HTTP gateway, signed capabilities, audit logs, memory stores, and runtime dispatch.

The platform is organized around seven operating primitives:

| Primitive | Role |
|---|---|
| Intent | Normalized request shapes for CLI, IPC, HTTP, routing, and daemon dispatch. |
| Runtime | Agent execution with timeout enforcement, manifest contracts, trusted-local subprocesses, and opt-in Linux gVisor runner support. |
| Identity | Local ed25519 identity, peer registry, operator tokens, token rotation, and peer revocation. |
| Permissions | Signed capabilities with known-scope validation, dispatch-time enforcement, expiry, and revocation tombstones. |
| Memory | SQLite-backed working, episodic, and long-term records with embedding hooks, ignore rules, drift reports, repair, and bounded compaction. |
| Audit | Append-only JSONL events, local hash-chain integrity reports, retention controls, signed actions, and audit-root attestations. |
| Settlement | Local resource receipts today; Solana settlement program scaffolding for future on-chain coordination. |

The primary implementation lives in `agent-os/`, the Rust workspace containing the daemon, CLI, protocol crates, runtime, memory, permissions, peer authentication, audit, MCP and A2A adapters, budget ledger, and settlement scaffold. The rest of the monorepo contains public documentation, web surfaces, circuits, SDK packages, and supporting services.

Start with [docs/repo-map.md](./docs/repo-map.md), [docs/status.md](./docs/status.md), [docs/audit-integrity.md](./docs/audit-integrity.md), and [agent-os/README.md](./agent-os/README.md) for the build map and capability status.

## Capability Surface

Covenant includes:

- Rust daemon and CLI for local agent orchestration.
- IPC and local HTTP gateway surfaces.
- Signed capability lifecycle for implemented namespaces, including grant-time validation, expiry, revocation, and dispatch-time scope enforcement.
- Peer authentication, operator token rotation, peer revocation, and peer-scoped A2A checks.
- Append-only audit log with structured event types, bounded reads, retention purge, and local hash-chain verification.
- SQLite-backed project memory across working, episodic, and long-term tiers.
- MCP adapter, native tool integration, and A2A mailbox primitives.
- Local settlement receipts for resource accounting.
- Commit-scoped provenance envelopes that bind task records, changed Git blobs, transition events, and validation evidence.
- Unsigned or locally signed audit-root attestations for local integrity reports.
- Opt-in live tests for daemon, CLI, runtime, and selected backend boundaries.
- CI coverage for Rust, documentation, workflow linting, live coverage matrix validation, provenance verification, dependency audits, and CodeQL.

## Status

| Area | Status | Boundary |
|---|---|---|
| Local daemon and CLI | Implemented | Source-built Rust workspace under `agent-os/`. |
| IPC and HTTP gateway | Implemented | Local operation with protocol metadata and daemon tests. |
| Identity, peer auth, and permissions | Implemented, hardening | Signed ed25519 capability model with scoped enforcement for implemented namespaces. |
| Audit and provenance | Implemented, hardening | Local hash-chain verification and provenance envelopes are included; public key custody and transparency publication are outside the release boundary. |
| Memory | Implemented, hardening | SQLite records, drift reports, repair commands, and bounded compaction are included; automatic schedules are on the roadmap. |
| MCP and A2A | Implemented, hardening | Adapter tests, durable queue state, lease inspection, and manual repair are included; multi-peer production operation is outside the release boundary. |
| Runtime sandboxing | Partially implemented | Trusted-local execution is available; sandbox-required manifests fail closed when unsupported; Linux gVisor support is opt-in and still hardening. |
| Autonomous workflow | Experimental | Task protocol, validation gates, session locking, review gates, continuation, and sprint summaries are included; benchmarked self-improvement is outside the release boundary. |
| On-chain settlement | Scaffolded | Local receipts are included; the Solana program is not production deployed. |
| Installer and SDK ecosystem | Roadmap | The alpha is source-built; package installers and stable SDK commitments are outside the release boundary. |

The authoritative status matrix is maintained in [docs/status.md](./docs/status.md).

## Release Boundary

Covenant alpha is source-built, local-first infrastructure for engineers and researchers who can inspect the code, run the validation gates, and operate the daemon with explicit trust boundaries.

The alpha does not include production deployment guarantees, default sandbox isolation, live network settlement, installer-backed distribution, stable SDK commitments, or safety guarantees for untrusted third-party agents. Those capabilities sit outside this release boundary.

The release contract, non-goals, and evidence requirements are tracked in [docs/alpha-release-contract.md](./docs/alpha-release-contract.md).

## Validation

From the repository root, run the fast local gate:

```bash
bash agent-os/scripts/validate.sh --quick
```

Run the full Rust validation gate used by CI:

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

Run opt-in live boundary tests when host prerequisites are available:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

Inspect the live coverage inventory:

```bash
bash agent-os/scripts/test-stats.sh
```

## Research Agenda

Covenant is advancing open infrastructure for:

- governed autonomous software maintenance;
- verifiable agent actions and commit-scoped provenance;
- capability-scoped delegation across local and remote agents;
- durable project memory for long-running work;
- resumable task ownership across interruptions;
- policy-aware tool use and sandboxed execution;
- audit-root attestations, public provenance, and future settlement coordination.

Roadmap items remain roadmap items until they have implementation evidence and validation coverage.

## Contributing

Covenant is early infrastructure with security-sensitive boundaries. Contributions are expected to include a validation plan, tests for changed behavior, and a clear statement of any remaining production risks.

Start with [CONTRIBUTING.md](./CONTRIBUTING.md), [docs/autonomous-development.md](./docs/autonomous-development.md), and [ROADMAP.md](./ROADMAP.md). Changes touching identity, permissions, audit, runtime isolation, settlement, provenance, release automation, or CI should receive especially close review.

## Security

Follow [SECURITY.md](./SECURITY.md) for responsible disclosure. The runtime isolation boundary is tracked in [docs/runtime-sandbox-security.md](./docs/runtime-sandbox-security.md), and the opt-in Linux gVisor runner setup is tracked in [docs/gvisor-live-runner.md](./docs/gvisor-live-runner.md). Do not open public issues for vulnerabilities.

## License

Apache-2.0. See [LICENSE](./LICENSE).
