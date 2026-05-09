# Capability Status

This status matrix separates what exists from what is experimental or planned. Update it when implementation or evidence changes.

| Capability | Status | Evidence | Next hardening step |
|---|---|---|---|
| Local daemon and CLI | Implemented | `agent-os/crates/covenantd`, `agent-os/crates/covenant`, daemon/CLI tests | Broaden live CLI coverage for every privileged verb. |
| IPC and HTTP gateway | Implemented | `covenant-ipc`, `covenantd/src/http.rs`, `http_gateway.rs` tests | Add version negotiation and compatibility tests. |
| Identity and peer auth | Implemented | `covenant-identity`, `covenant-peer-auth`, token rotation tests | Public attestation and key provenance. |
| Signed capabilities | Implemented | `covenant-permissions`, capability enforcement tests | Formalize scope schemas per action namespace. |
| Audit log | Implemented | `covenant-audit`, daemon audit event tests | Add retention policy docs and tamper-evidence design. |
| Memory store | Implemented, hardening | `covenant-memory`, SQLite, embedding tests, read-only drift reports | Explicit repair commands, compaction, stale-context handling. |
| Runtime execution | Implemented, trusted-local | `covenant-runtime`, subprocess timeout tests | gVisor sandbox for untrusted Linux agents. |
| MCP tools | Experimental | `covenant-mcp`, native/external transport tests | More live server compatibility tests. |
| A2A messaging | Implemented, hardening | `covenant-a2a`, leased queue state, restart live tests | Explicit requeue and lease-expiry repair commands. |
| Budget ledger | Implemented | `covenant-budget`, daemon budget tests | Mid-task pause, save, and resume. |
| Local settlement receipts | Implemented | `covenant-settlement`, receipt tests | Stronger reconciliation and drift reports. |
| On-chain settlement | Planned / scaffolded | `agent-os/programs/settlement` | Security review, deployment plan, oracle and mint policy. |
| Autonomous workflow | Experimental | `docs/autonomous-development.md`, `agent-os/autonomy`, validator | Persist task state transitions and review artifacts. |
| Public provenance | Planned | Local signing and audit primitives only | Transparency-log attestation design. |
| Installer and SDK ecosystem | Planned | No stable release path | Define alpha release contract after sandbox and settlement boundaries harden. |

## Validation Signals

- Default Rust gate: `bash agent-os/scripts/validate.sh`
- Autonomy artifact gate: `node agent-os/scripts/validate-autonomy.mjs`
- Next task selector: `node agent-os/scripts/autonomy-next.mjs`
- Live coverage inventory: `bash agent-os/scripts/test-stats.sh`
- Landing docs build: `pnpm --dir landing build`
- Dependency audit: `bash scripts/audit.sh`
