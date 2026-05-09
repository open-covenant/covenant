# Covenant Agent OS

`agent-os/` contains the local operating-layer runtime: the daemon, CLI, shared protocol crates, agent runtime, memory, identity, permissions, audit, peer auth, MCP/A2A adapters, local operator console, and settlement scaffold.

## Build

From this directory:

```bash
cargo build --workspace --exclude covenant-settlement-program
```

The two primary binaries are:

- `target/debug/covenantd`: local daemon
- `target/debug/covenant`: CLI client

## Validate

Fast local gate:

```bash
./scripts/validate.sh --quick
```

Full Rust gate:

```bash
./scripts/validate.sh
```

Live tests are opt-in:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

## Autonomy Artifacts

The autonomous workflow has a machine-readable control surface:

- `autonomy/workflow.json`: lifecycle states, roles, gates, transitions, and definition of done.
- `autonomy/tasks/*.json`: task backlog records with scope, gates, expected failure modes, verification, next action, and human escalation needs.
- `autonomy/events.jsonl`: append-only task transition log.
- `scripts/validate-autonomy.mjs`: dependency-free validator for the workflow and task records.
- `scripts/autonomy-next.mjs`: selects the next unblocked task by priority and state.
- `scripts/autonomy-transition.mjs`: applies an allowed task state transition and records an event.

The normal validation script runs the autonomy validator before the Rust gates.

## Runtime State

The daemon stores local state under `$COVENANT_HOME`. If unset, it uses `$HOME/.covenant`.

Common paths:

| Path | Purpose |
|---|---|
| `sock` | Unix socket for local clients. |
| `identity/local.key` | Local ed25519 seed. |
| `peers/operator.token` | Local operator token. |
| `peers/registry.jsonl` | Peer registry and tombstones. |
| `capabilities/granted.jsonl` | Signed capability grants. |
| `capabilities/revoked.jsonl` | Capability revocation tombstones. |
| `audit/events.jsonl` | Append-only audit log. |
| `audit/events.chain.jsonl` | Local hash-chain sidecar for audit integrity verification. |
| `memory.db` | SQLite memory store. |
| `receipts/working.jsonl` | Local settlement receipts. |
| `runtime/gvisor` | Default scratch root when `COVENANT_RUNTIME_BACKEND=linux-gvisor`. |

## Runtime Backend

`covenantd` defaults to trusted-local subprocess execution:

```bash
COVENANT_RUNTIME_BACKEND=trusted-local
```

To opt into the initial Linux gVisor runner:

```bash
COVENANT_RUNTIME_BACKEND=linux-gvisor
COVENANT_GVISOR_ROOTFS=/path/to/rootfs
```

Optional settings:

```bash
COVENANT_RUNSC=runsc
COVENANT_GVISOR_SCRATCH=$COVENANT_HOME/runtime/gvisor
```

The daemon fails startup when `linux-gvisor` is selected without a rootfs. Trusted-local execution refuses manifests that declare `sandbox.required = true`; it does not silently downgrade sandbox-required agents.

The real `runsc` dispatch path has ignored live coverage. It is skipped unless a Linux host rootfs is provided:

```bash
COVENANT_LIVE_GVISOR_ROOTFS=/path/to/rootfs \
  cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc
```

See [`../docs/gvisor-live-runner.md`](../docs/gvisor-live-runner.md) for the Linux host, `runsc`, rootfs, and CI adoption contract.

## Crate Groups

| Group | Crates |
|---|---|
| Core protocol | `covenant-types`, `covenant-ipc`, `covenant-manifest` |
| Control plane | `covenantd`, `covenant`, `covenant-router`, `covenant-runtime` |
| Trust and policy | `covenant-identity`, `covenant-permissions`, `covenant-peer-auth`, `covenant-audit`, `covenant-budget` |
| State and tools | `covenant-memory`, `covenant-tools`, `covenant-llm`, `covenant-mcp`, `covenant-a2a` |
| Settlement | `covenant-settlement`, `programs/settlement` |

## Operating Model

The daemon is the enforcement boundary. Agents, web clients, and CLI calls should not bypass it for privileged state. New behavior should preserve these invariants:

- authenticate before serving privileged requests;
- check capabilities before dispatching or mutating protected state;
- record audit rows for important state transitions and rejections;
- keep the audit hash-chain sidecar consistent when audit retention rewrites retained rows;
- keep token bytes, private keys, and secrets out of logs and responses;
- add tests for both success and failure paths when changing protocol behavior.
