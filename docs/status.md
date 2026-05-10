# Capability Status

This status matrix separates what exists from what is experimental or planned. Update it when implementation or evidence changes.

| Capability | Status | Evidence | Next hardening step |
|---|---|---|---|
| Local daemon and CLI | Implemented | `agent-os/crates/covenantd`, `agent-os/crates/covenant`, daemon/CLI tests, CLI protocol metadata probe | Broaden live CLI coverage for every privileged verb. |
| IPC and HTTP gateway | Implemented | `covenant-ipc`, unauthenticated `protocol_info`, HTTP `/version`, protected HTTP mutation live coverage, daemon and gateway tests | Define the protocol v2 bump + migration harness before the first breaking schema change lands. |
| Identity and peer auth | Implemented | `covenant-identity`, `covenant-peer-auth`, token rotation tests | Public attestation and key provenance. |
| Signed capabilities | Implemented, hardening | `covenant-permissions`, grant-time scope validation for known namespaces, dispatch-time `tool.call.*` argument allowlists, dispatch-time `audit.purge` cutoff enforcement with live CLI rejection coverage, dispatch-time memory read/write/purge/repair/compaction scope enforcement, dispatch-time A2A send/recv/respond/repair scope enforcement, dispatch-time peer list/revoke/purge scope enforcement, dispatch-time chain receipt read/batch/flush scope enforcement, capability enforcement tests, `docs/capabilities.md` scope contract | Broaden live coverage for scoped delegated paths. |
| Audit log | Implemented, hardening | `covenant-audit`, daemon audit event tests, local hash-chain sidecar, CLI/HTTP/IPC integrity report, stable CLI integrity JSON envelope, unsigned and locally signed `audit-root-attestation.v1` generator/verifier, ADR 0004 signing policy | Add project key custody and release publication, then later transparency-log publication. |
| Memory store | Implemented, hardening | `covenant-memory`, SQLite, embedding tests, read-only drift reports with exact receipt correlation, scoped read/write/purge/repair/compaction dispatch checks, daemon/HTTP/CLI repair commands, bounded daemon/HTTP/CLI compaction commands with stable JSON envelope, live compaction coverage, repair and compaction audit rows | Automatic compaction schedules and receipt backfill tooling for legacy rows. |
| Runtime execution | Partially implemented | `covenant-runtime`, subprocess timeout and malformed-stdout tests, manifest sandbox requirement parsing, daemon backend selection, initial `runsc` OCI runner tests, opt-in live Linux gVisor dispatch test, documented Linux runner setup | Automate Linux CI host provisioning and broaden sandbox policy enforcement. |
| MCP tools | Experimental | `covenant-mcp`, native/external transport tests | More live server compatibility tests. |
| A2A messaging | Implemented, hardening | `covenant-a2a`, leased queue state, explicit task-kind metadata with legacy `intent_text` fallback, task idempotency metadata, receiver-side idempotency result cache, lease-age status filters, restart and CLI repair live tests, daemon/HTTP/CLI repair commands, disabled-by-default retry gate, opt-in background retry scheduler, repair and scheduler audit rows | Add per-peer repair visibility coverage if delegated repair moves beyond operator-owned tasks. |
| Budget ledger | Implemented | `covenant-budget`, daemon budget tests, `covenant intents resume latest --json`, live CLI resume test | Mid-task pause, save, and resume. |
| Local settlement receipts | Implemented | `covenant-settlement`, receipt tests, memory-record correlation field | Backfill and migration tooling for legacy uncorrelated receipts. |
| On-chain settlement | Planned / scaffolded | `agent-os/programs/settlement` | Security review, deployment plan, oracle and mint policy. |
| Autonomous workflow | Experimental | `docs/autonomous-development.md`, `agent-os/autonomy`, validator, transition event log, sprint summary generator/publication/check validator, status gap report, unsigned review artifact scaffold/verifier/toolchain validator, autonomy environment preflight, dirty handoff report, handoff bundle export/verifier/restore plan, handoff toolchain validator, git identity validator, Git metadata write-access preflight, local pre-push guard | Signed review artifacts backed by key custody policy. |
| Live boundary coverage | Experimental | `agent-os/autonomy/live-coverage.json`, `validate-live-coverage.mjs`, opt-in `live_` tests, verifier drift/repair coverage, external-service gVisor coverage entry, `docs/gvisor-live-runner.md` | Promote the documented Linux gVisor runner into CI once host provisioning is stable, and keep adding mutation-edge live tests where policies are stable. |
| Public provenance | Experimental | `agent-os/scripts/provenance.mjs`, `docs/provenance/attestations`, audit-root attestation signing/verification support, ADR 0004 audit-root signing policy | Implement key custody, release artifact subjects, and transparency publication. |
| Distribution and SDK ecosystem | Planned, alpha contract defined | `docs/alpha-release-contract.md` defines a source-built alpha boundary, `docs/releases/README.md` defines evidence bundles, `agent-os/scripts/alpha-release-evidence.mjs` emits `covenant.alpha-release-evidence.v1` with sanitized alpha readiness state, `agent-os/scripts/alpha-release-bundle.mjs` scaffolds local evidence bundles with `covenant.alpha-release-manifest.v1` digest manifests, `agent-os/scripts/alpha-release-validate-bundle.mjs` validates bundle acceptance gates including schema, evidence metadata binding, complete manifest file inventory, manifest digests, section-scoped gate outcomes, skipped-gate reasons, accepted-decision gate consistency, and section-scoped readiness blockers, `agent-os/scripts/validate-alpha-release-evidence.mjs` checks draft override and synthetic accepted-bundle paths, and `agent-os/scripts/alpha-release-readiness.mjs` reports repository-level alpha blockers including handoff and unsigned review-artifact toolchain readiness | Package installers, stable SDKs, signed release artifacts, and upgrade policy. |

## Validation Signals

- Default Rust gate: `bash agent-os/scripts/validate.sh`
- Scripts-only gate: `bash agent-os/scripts/validate.sh --scripts`
- Autonomy artifact gate: `node agent-os/scripts/validate-autonomy.mjs`
- Git identity guard: `node agent-os/scripts/validate-git-identity.mjs`
- Provenance gate: `node agent-os/scripts/provenance.mjs verify-all`
- Next task selector: `node agent-os/scripts/autonomy-next.mjs`
- Live coverage inventory: `bash agent-os/scripts/test-stats.sh`
- Live coverage matrix gate: `node agent-os/scripts/validate-live-coverage.mjs`
- Landing docs build: `pnpm --dir landing build`
- Dependency audit: `bash scripts/audit.sh`
- Alpha evidence gate: `node agent-os/scripts/validate-alpha-release-evidence.mjs`

Alpha release boundary: `docs/alpha-release-contract.md`
