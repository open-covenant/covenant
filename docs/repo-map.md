# Repository Map

Covenant is a monorepo. The agent-native operating layer lives primarily in `agent-os/`; adjacent apps, circuits, packages, and services support public documentation, Solana settlement experiments, and protocol surfaces.

## Top-level Layout

| Path | Role |
|---|---|
| `agent-os/` | Rust workspace for the daemon, CLI, operating primitives, local web console, and Solana protocol program. |
| `landing/` | Public site and documentation app. |
| `docs/` | Repository-level documentation that should remain readable without running the docs site. |
| `docs/decisions/` | Architecture decision records. |
| `docs/alpha-release-contract.md` | Source alpha boundary, non-claims, evidence bundle expectations, and human-owned release decisions. |
| `docs/distribution-readiness.md` | Public distribution, signing, SDK stability, and upgrade graduation gates. |
| `docs/release-validation.md` | Release evidence profile and validation requirements. |
| `docs/protocol-versioning.md` | IPC/HTTP protocol versioning, compatibility windows, and fixture replay policy. |
| `docs/memory-maintenance.md` | Memory compaction planning and receipt backfill boundary. |
| `docs/budget-pause-checkpoints.md` | Budget pause checkpoint format and daemon integration boundary. |
| `docs/on-chain-settlement-readiness.md` | On-chain settlement deployment, review, oracle, mint authority, and emergency-operation gates. |
| `docs/gvisor-host-readiness.md` | Linux gVisor host readiness and CI promotion gates. |
| `docs/releases/` | Release evidence bundle records for alpha candidates, scaffolded by `agent-os/scripts/alpha-release-bundle.mjs`. |
| `docs/provenance/` | Provenance contract and committed attestation envelopes. |
| `circuits/` | Circom proof circuits and catalog metadata. |
| `packages/` | Shared TypeScript SDK, UI, and configuration packages. |
| `services/` | Supporting services: MCP bridges, proof generation, indexer, compute broker, and bots. |
| `.github/workflows/` | CI, security scans, CodeQL, and workflow security checks. |
| `scripts/` | Root-level workspace scripts for bootstrap, validation, and audits. |

## Agent OS Workspace

| Path | Role |
|---|---|
| `agent-os/crates/covenantd` | Main daemon library and binary. Owns request handling, auth, routing, audit, memory, settlement, peers, and HTTP. |
| `agent-os/crates/covenant` | CLI client. |
| `agent-os/crates/covenant-types` | Shared wire types: intents, agents, capabilities, memory records, memory repair/compaction requests, receipts. |
| `agent-os/crates/covenant-identity` | Local ed25519 identity. |
| `agent-os/crates/covenant-permissions` | Signed capabilities, scope validation, verification, expiry, and revocation store. |
| `agent-os/crates/covenant-audit` | Append-only audit log, local hash-chain sidecar, and integrity reports. |
| `agent-os/crates/covenant-memory` | Memory records, SQLite store, embeddings, ignore rules, repair planning, and compaction planning. |
| `agent-os/crates/covenant-runtime` | Agent subprocess runner and timeout handling. |
| `agent-os/crates/covenant-router` | Agent-card matching and intent routing. |
| `agent-os/crates/covenant-ipc` | Local socket protocol and versioned fixture replay harness. |
| `agent-os/crates/covenant-mcp` | Native and external MCP tool integration. |
| `agent-os/crates/covenant-a2a` | Agent-to-agent task and mailbox primitives. |
| `agent-os/crates/covenant-peer-auth` | Peer registry, tokens, revocation, and list filters. |
| `agent-os/crates/covenant-budget` | Credit budget ledger, exhaustion behavior, and pause checkpoint storage. |
| `agent-os/crates/covenant-settlement` | Local settlement receipt ledger. |
| `agent-os/agents/research` | Reference research agent. |
| `agent-os/programs/settlement` | Solana protocol program for agent registration, stake, credits, task escrow, and receipt anchors. |
| `agent-os/covenant-web` | Local operator web console. |
| `agent-os/autonomy` | Machine-readable autonomous workflow, live coverage matrix, and task backlog. |
| `agent-os/scripts` | Validation, live coverage checks, autonomy summaries, provenance verification, test inventory, handoff, and regression guards. |

## MCP Bridges

| Path | Role |
|---|---|
| `services/mcp-bridge` | Covenant MCP server for Solana instruction prep plus authenticated `covenantd` HTTP tools. |
| `services/hermes-mcp-bridge` | Hermes API Server wrapper that exposes generic Hermes agent runs over MCP. |

## Public vs Internal State

Tracked files should explain durable architecture, commands, status, and contribution rules. Local handoff notes, session locks, and private operator state should remain untracked.

The public docs should not require access to private chat history, local agent sessions, untracked handoff files, API keys, hostnames, or machine-specific paths.

## Validation Entry Points

Use these from the repository root unless noted:

```bash
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/validate.sh
pnpm --dir landing build
```

From `agent-os/`, opt into live tests when a change touches real process or backend behavior:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```
