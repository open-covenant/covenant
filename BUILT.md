# Recursive Engineering Model

Covenant is developed with an autonomous engineering loop that mirrors the operating-layer primitives in the codebase. The loop is not presented as full self-improvement, full autonomy, or a benchmarked replacement for maintainers. It is an inspectable process for letting agents perform bounded engineering work while preserving review, provenance, and human authority boundaries.

The detailed protocol lives in [docs/autonomous-development.md](./docs/autonomous-development.md). This file records how the public project relates to that loop.

## What Is Real Today

The repository already contains concrete substrate for agentic operation:

| Product primitive | Repository implementation | Workflow use |
|---|---|---|
| Identity | `agent-os/crates/covenant-identity` and peer auth | Local sessions and commits can be tied to scoped identities. |
| Permissions | `agent-os/crates/covenant-permissions` | Security-sensitive operations are expected to pass capability and review gates. |
| Audit | `agent-os/crates/covenant-audit` | Engineering cycles record validation, review, and handoff artifacts. |
| Runtime | `agent-os/crates/covenant-runtime` | Agents run as bounded trusted-local subprocesses by default; an opt-in Linux gVisor runner is selectable and has ignored live coverage gated on `runsc` and an explicit rootfs. |
| Memory | `agent-os/crates/covenant-memory` | Project state is persisted through tracked docs and local handoff files. |
| Comms | IPC, HTTP, MCP, and A2A crates | Tool and peer boundaries are explicit, not implicit process sharing. |
| Settlement | Local receipts plus Solana scaffold | Resource accounting exists locally; on-chain settlement is not production. |

The loop also has practical enforcement:

- a tracked pre-commit hook for one-session-per-checkout protection;
- a validation script used by local development and CI;
- guard scripts for known regression classes;
- a test convention that separates mock tests from opt-in `live_` tests;
- explicit human escalation rules for credentials, production deployment, legal decisions, and destructive operations.

## Engineering Cycle

Each autonomous cycle should be small enough to review and large enough to move a real system boundary. The cycle is:

1. Inspect repository state and prior handoff notes.
2. Define the next bounded task and its expected production failure modes.
3. Plan the implementation before editing.
4. Implement the change.
5. Run self-review against correctness, scope, style, and security impact.
6. Trigger cross-review or security review when the gates require it.
7. Run validation and repair failures.
8. Update docs, status, and project memory.
9. Integrate the change with a human-readable commit history.
10. Hand off enough context for a fresh agent session to resume.

The lifecycle is intentionally conservative. A change is not complete because code was generated. It is complete when the behavior is implemented, validated, reviewed at the appropriate level, and documented where the public contract changed.

## Review Gates

The tracked workflow defines six gates:

| Gate | Trigger | Required outcome |
|---|---|---|
| Plan gate | More than one credible architecture path | Record the chosen design and rejected alternatives. |
| Security gate | Identity, permissions, audit, settlement, secrets, or sandbox boundaries changed | Security review before integration. |
| Fan-out gate | Broad cross-crate or cross-app changes | Split ownership by module or reduce scope. |
| Test-expansion gate | New public behavior with shallow tests | Add failure-mode coverage or record the gap. |
| Docs gate | Public terminology, commands, or architecture changed | Update public docs in the same change. |
| Escalation gate | Credentials, destructive operations, legal/business calls, production deploys | Stop that path and continue only with unblocked work. |

These gates are useful only when they create artifacts a future maintainer can inspect. Silent reasoning does not count.

## Provenance

The public model is scoped provenance, not personality. Work should be attributable by domain, task, reviewer, validation command, and commit, without relying on named agent personas or private operator details.

Accepted provenance signals:

- signed commits or signed release artifacts where available;
- CI run links and validation command output;
- structured audit rows for daemon actions;
- review notes for security-sensitive changes;
- status docs that distinguish implemented, experimental, and planned work.

The foremost missing piece is public keyless attestation. A future implementation should connect local signing identities to a transparency log so third parties can verify which automation produced which artifact.

## Honesty Boundaries

Covenant does not currently claim:

- full autonomous software engineering without human authority;
- production sandbox-grade isolation for arbitrary untrusted agents;
- production multi-peer operation across untrusted hosts;
- on-chain settlement in production;
- public benchmarked self-improvement;
- release-ready installer or SDK ecosystem.

Those are roadmap items. The current system is a working local substrate with strong primitives in some areas and deliberately marked gaps in others.

## Verification

Useful local checks:

```bash
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/test-stats.sh
```

The full Rust validation gate is:

```bash
bash agent-os/scripts/validate.sh
```

Live tests are opt-in:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```
