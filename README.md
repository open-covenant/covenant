# Covenant

[![CI](https://github.com/open-covenant/covenant/actions/workflows/ci.yml/badge.svg)](https://github.com/open-covenant/covenant/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

> Open, agent-native operating layer for long-running autonomous software systems.

Covenant is a local-first control plane for software agents. It sits above macOS or Linux and gives autonomous systems structured access to codebases, tools, execution environments, project memory, capability-scoped permissions, audit trails, and review loops.

The project is intentionally recursive: Covenant is being developed through the same operating model it exposes. Agents plan work, implement changes, review diffs, run verification, repair failures, update memory, and escalate only when human judgment, credentials, or external deployment authority are required.

- **Web:** [opencovenant.org](https://opencovenant.org)
- **Docs:** [docs.opencovenant.org](https://docs.opencovenant.org)

## Thesis

Modern computers are still human-operated environments. Filesystems, terminals, process managers, permission prompts, package managers, and issue trackers assume a person is present to decide what matters, what may run, what state should persist, and when work is complete.

Frontier agents need a different layer: one that can decompose work, delegate execution, preserve context, enforce policy, manage tools, record provenance, review outputs, repair failures, and continue across interruptions. Covenant explores that layer as open infrastructure for autonomous software maintenance.

## System Overview

Covenant is not a chat wrapper or a single agent framework. The core is a Rust daemon, `covenantd`, with a CLI, local HTTP gateway, IPC protocol, agent manifests, and storage primitives. The daemon owns local state and mediates all privileged actions.

At a high level, Covenant provides:

- an **agent control plane** for dispatching intents, routing work, and coordinating peers;
- an **execution substrate** for spawning agents and bounding their runtime behavior;
- a **policy layer** based on signed capabilities, expiry, revocation, and peer authentication;
- **persistent project memory** backed by tiered records, embeddings, ignore rules, and read-only drift reports;
- **audit and provenance** through append-only JSONL logs, signed actions, CI gates, review artifacts, and verifiable commit provenance envelopes;
- **tool orchestration** through native tools, MCP integration, A2A messaging, and local gateway APIs;
- a path toward **economic settlement** through local receipts today and a Solana program scaffold for future on-chain settlement.

## Architecture

The repository is organized around a small set of operating-layer primitives:

| Primitive | Current implementation |
|---|---|
| Intent | CLI/IPC/HTTP request shapes, router, daemon dispatch path |
| Runtime | Trusted-local subprocess runner, timeout enforcement, manifest sandbox contract, opt-in Linux gVisor runner selection |
| Memory | SQLite-backed tiered records, embedding hooks, ignore rules |
| Identity | Local ed25519 identity, peer registry, token rotation |
| Permissions | Signed capabilities, expiry, revocation tombstones, enforcement |
| Comms | IPC socket, HTTP gateway, MCP adapter, A2A mailbox |
| Audit | Append-only JSONL events for dispatch, auth, capabilities, peers |
| Settlement | Local receipt ledger; Solana program scaffold is experimental |

The architectural center is `agent-os/`: the Rust workspace containing the daemon, CLI, protocol crates, runtime, memory, permissions, peer auth, audit, MCP/A2A adapters, and settlement scaffold. The surrounding monorepo contains public docs, web surfaces, contracts, circuits, and services that support or experiment with adjacent protocol layers.

See [docs/repo-map.md](./docs/repo-map.md), [docs/status.md](./docs/status.md), and [agent-os/README.md](./agent-os/README.md) for the build map and capability status.

## Autonomous Development Loop

Covenant's engineering loop is treated as part of the system design, not as repository trivia. The loop follows a task lifecycle:

`intake -> plan -> implement -> self-review -> cross-review -> validate -> repair -> document -> integrate -> handoff`

The loop uses explicit gates for architectural choices, security-sensitive diffs, broad cross-crate edits, insufficient tests, docs drift, and human-only blockers. The goal is not to pretend agents need no oversight. The goal is to make autonomous work inspectable, repeatable, resumable, and hard to overclaim.

The tracked protocol is in [docs/autonomous-development.md](./docs/autonomous-development.md). The machine-readable lifecycle and autonomous backlog live under [agent-os/autonomy](./agent-os/autonomy). Durable context lives in [docs/project-memory.md](./docs/project-memory.md). The implementation history and current limits are summarized in [BUILT.md](./BUILT.md).

## Current Capabilities

Implemented and tested in the repository:

- Rust workspace with `covenantd`, `covenant` CLI, IPC, HTTP, router, runtime, memory, identity, permissions, audit, MCP, A2A, peer-auth, budget, and settlement crates.
- Signed capability lifecycle: grant, verify, expire, revoke, list, and enforce.
- Peer authentication, operator token rotation, peer revocation, and peer-scoped A2A capability checks.
- Append-only audit log with structured event types and bounded recent reads.
- SQLite-backed memory records with working, episodic, and long-term tiers.
- Local settlement receipts for resource accounting.
- Commit-scoped provenance envelopes that bind agent-produced changes to autonomy tasks, changed Git blobs, transition events, and recorded validation.
- Live opt-in tests for real daemon/CLI boundaries and selected real backends.
- Machine-readable live coverage matrix for protocol, CLI, runtime, and model boundaries.
- CI for Rust, landing docs, workflow linting, live coverage matrix validation, provenance verification, dependency audits, and CodeQL.

## Status

| Area | Status | Notes |
|---|---|---|
| Local daemon and CLI | Implemented | Active Rust workspace under `agent-os/`. |
| Identity, permissions, audit | Implemented | Signed ed25519 capability model with revocation and audit rows. |
| Memory | Implemented, still hardening | SQLite, embeddings, and read-only drift reports are present; explicit repair and compaction need more work. |
| MCP and A2A | Implemented, hardening | MCP adapter tests exist; A2A has durable leased delivery and queue-state inspection. Multi-peer production operation is not claimed. |
| Autonomous development loop | Experimental | Protocol, session locking, validation, and review gates exist; full benchmarked self-improvement is not claimed. |
| Public provenance | Experimental | Alpha JSON envelopes verify committed task evidence from Git object data; public signing and transparency-log publication are not claimed. |
| Runtime sandboxing | Partially implemented | Manifest sandbox requirements are parsed; trusted-local execution fails closed for sandbox-required agents; the runtime crate has an initial `runsc` runner; the daemon can select `trusted-local` or `linux-gvisor` at startup; opt-in live Linux gVisor coverage exists. Repeatable CI coverage and Firecracker isolation are future work. |
| On-chain settlement | Planned / scaffolded | Local receipts exist; Solana program wiring is not production. |
| Installer and SDK ecosystem | Planned | Not a release-ready developer platform yet. |

## Local Validation

From the repository root:

```bash
bash agent-os/scripts/validate.sh --quick
```

For the full Rust gate used by CI:

```bash
bash agent-os/scripts/validate.sh
```

To inspect public provenance envelopes directly:

```bash
node agent-os/scripts/provenance.mjs verify-all
```

The landing docs build separately:

```bash
pnpm --dir landing install --frozen-lockfile --ignore-workspace
pnpm --dir landing build
```

Live tests are opt-in because they may spawn real binaries or require local services:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

Coverage inventory:

```bash
bash agent-os/scripts/test-stats.sh
```

## Research Direction

Covenant is exploring:

- durable project memory for long-running autonomous maintenance;
- verifiable agent actions and public provenance;
- capability-scoped delegation across local and remote agents;
- resumable task ownership after process, context, or machine interruption;
- policy-aware tool use and sandboxed execution;
- continuous repair and regression hardening;
- human-directed autonomous engineering without hiding the human authority boundary.

Claims that are not implemented are kept in the roadmap rather than marketed as shipped behavior.

## Contributing

Covenant is early infrastructure. Serious contributions are welcome from systems engineers, AI researchers, security reviewers, protocol designers, and open-source maintainers.

Start with [CONTRIBUTING.md](./CONTRIBUTING.md), [docs/autonomous-development.md](./docs/autonomous-development.md), and [ROADMAP.md](./ROADMAP.md). Pull requests should include a validation plan, tests for changed behavior, and a clear statement of any remaining production risks.

## Security

Follow [SECURITY.md](./SECURITY.md) for responsible disclosure. The runtime isolation contract is tracked in [docs/runtime-sandbox-security.md](./docs/runtime-sandbox-security.md). Do not open public issues for vulnerabilities.

## License

Apache-2.0. See [LICENSE](./LICENSE).
