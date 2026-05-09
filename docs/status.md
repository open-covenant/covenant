# Capability Status

This status matrix separates what exists from what is experimental or planned. Update it when implementation or evidence changes.

| Capability | Status | Evidence | Next hardening step |
|---|---|---|---|
| Local daemon and CLI | Implemented | `agent-os/crates/covenantd`, `agent-os/crates/covenant`, daemon/CLI tests | Broaden live CLI coverage for every privileged verb. |
| IPC and HTTP gateway | Implemented | `covenant-ipc`, `covenantd/src/http.rs`, `http_gateway.rs` tests | Add version negotiation and compatibility tests. |
| Identity and peer auth | Implemented | `covenant-identity`, `covenant-peer-auth`, token rotation tests | Public attestation and key provenance. |
| Signed capabilities | Implemented | `covenant-permissions`, capability enforcement tests | Formalize scope schemas per action namespace. |
| Audit log | Implemented | `covenant-audit`, daemon audit event tests | Add retention policy docs and tamper-evidence design. |
| Memory store | Implemented, hardening | `covenant-memory`, SQLite, embedding tests, read-only drift reports, repair primitives | Daemon/CLI repair surface, audit rows, compaction, stale-context handling. |
| Runtime execution | Partially implemented | `covenant-runtime`, subprocess timeout tests, manifest sandbox requirement parsing, daemon backend selection, initial `runsc` OCI runner tests, opt-in live Linux gVisor dispatch test | Repeatable Linux CI host requirements and broader sandbox policy enforcement. |
| MCP tools | Experimental | `covenant-mcp`, native/external transport tests | More live server compatibility tests. |
| A2A messaging | Implemented, hardening | `covenant-a2a`, leased queue state, restart live tests, daemon/HTTP/CLI repair commands, repair audit rows | Lease-age filters for stale in-flight work. |
| Budget ledger | Implemented | `covenant-budget`, daemon budget tests | Mid-task pause, save, and resume. |
| Local settlement receipts | Implemented | `covenant-settlement`, receipt tests | Stronger reconciliation and drift reports. |
| On-chain settlement | Planned / scaffolded | `agent-os/programs/settlement` | Security review, deployment plan, oracle and mint policy. |
| Autonomous workflow | Experimental | `docs/autonomous-development.md`, `agent-os/autonomy`, validator, transition event log | Signed review artifacts and stronger sprint summaries. |
| Live boundary coverage | Experimental | `agent-os/autonomy/live-coverage.json`, `validate-live-coverage.mjs`, opt-in `live_` tests, external-service gVisor coverage entry | Add CLI revoke live coverage and a documented Linux runner for gVisor validation. |
| Public provenance | Experimental | `agent-os/scripts/provenance.mjs`, `docs/provenance/attestations` | Signing identity policy, release artifact subjects, transparency-log publication. |
| Installer and SDK ecosystem | Planned | No stable release path | Define alpha release contract after sandbox and settlement boundaries harden. |

## Validation Signals

- Default Rust gate: `bash agent-os/scripts/validate.sh`
- Autonomy artifact gate: `node agent-os/scripts/validate-autonomy.mjs`
- Provenance gate: `node agent-os/scripts/provenance.mjs verify-all`
- Next task selector: `node agent-os/scripts/autonomy-next.mjs`
- Live coverage inventory: `bash agent-os/scripts/test-stats.sh`
- Live coverage matrix gate: `node agent-os/scripts/validate-live-coverage.mjs`
- Landing docs build: `pnpm --dir landing build`
- Dependency audit: `bash scripts/audit.sh`
