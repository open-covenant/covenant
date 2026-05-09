# Repository Map

Covenant is a monorepo. The agent-native operating layer lives primarily in `agent-os/`; adjacent apps, contracts, circuits, and services support public documentation, settlement experiments, and protocol surfaces.

## Top-level Layout

| Path | Role |
|---|---|
| `agent-os/` | Rust workspace for the daemon, CLI, operating primitives, local web console, and Solana settlement scaffold. |
| `landing/` | Public site and documentation app. |
| `docs/` | Repository-level documentation that should remain readable without running the docs site. |
| `docs/decisions/` | Architecture decision records. |
| `docs/provenance/` | Alpha provenance contract and committed attestation envelopes. |
| `apps/portal/` | Protocol portal and user-facing application experiments. |
| `apps/docs/` | Secondary docs app retained for the broader workspace. |
| `contracts/` | EVM contracts and scripts for protocol surfaces outside the local daemon. |
| `circuits/` | Circom proof circuits and catalog metadata. |
| `packages/` | Shared TypeScript SDK, UI, and configuration packages. |
| `services/` | Supporting services: discovery, MCP bridge, proof generation, indexer, compute broker, gateway, and bots. |
| `.github/workflows/` | CI, security scans, CodeQL, and workflow security checks. |
| `scripts/` | Root-level workspace scripts for artifacts, contract metadata, and audits. |

## Agent OS Workspace

| Path | Role |
|---|---|
| `agent-os/crates/covenantd` | Main daemon library and binary. Owns request handling, auth, routing, audit, memory, settlement, peers, and HTTP. |
| `agent-os/crates/covenant` | CLI client. |
| `agent-os/crates/covenant-types` | Shared wire types: intents, agents, capabilities, memory records, receipts. |
| `agent-os/crates/covenant-identity` | Local ed25519 identity. |
| `agent-os/crates/covenant-permissions` | Signed capabilities, verification, expiry, and revocation store. |
| `agent-os/crates/covenant-audit` | Append-only audit log. |
| `agent-os/crates/covenant-memory` | Memory records, SQLite store, embeddings, ignore rules. |
| `agent-os/crates/covenant-runtime` | Agent subprocess runner and timeout handling. |
| `agent-os/crates/covenant-router` | Agent-card matching and intent routing. |
| `agent-os/crates/covenant-ipc` | Local socket protocol. |
| `agent-os/crates/covenant-mcp` | Native and external MCP tool integration. |
| `agent-os/crates/covenant-a2a` | Agent-to-agent task and mailbox primitives. |
| `agent-os/crates/covenant-peer-auth` | Peer registry, tokens, revocation, and list filters. |
| `agent-os/crates/covenant-budget` | Credit budget ledger and exhaustion behavior. |
| `agent-os/crates/covenant-settlement` | Local settlement receipt ledger. |
| `agent-os/agents/research` | Reference research agent. |
| `agent-os/programs/settlement` | Experimental Solana settlement program. |
| `agent-os/covenant-web` | Local operator web console. |
| `agent-os/autonomy` | Machine-readable autonomous workflow and task backlog. |
| `agent-os/scripts` | Validation, provenance verification, test inventory, handoff, and regression guards. |

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
