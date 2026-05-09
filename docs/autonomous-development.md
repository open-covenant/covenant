# Autonomous Development Protocol

Covenant treats autonomous software maintenance as an operating-layer problem. Agents should be able to plan, execute, review, repair, document, and resume work without turning the repository into an opaque pile of generated output.

This protocol is the public, tool-neutral version of the engineering loop. Local deployments may implement it with different agent clients, shells, signing policies, and review tools.

The machine-readable workflow lives at [agent-os/autonomy/workflow.json](../agent-os/autonomy/workflow.json). Active autonomous work lives in [agent-os/autonomy/tasks](../agent-os/autonomy/tasks). Durable seed templates live in [agent-os/autonomy/backlog.json](../agent-os/autonomy/backlog.json). Both are validated by `node agent-os/scripts/validate-autonomy.mjs`.

Operational helpers:

```bash
node agent-os/scripts/autonomy-next.mjs
node agent-os/scripts/autonomy-continue.mjs
node agent-os/scripts/autonomy-seed-next.mjs --dry-run
node agent-os/scripts/autonomy-summary.mjs --since 2026-05-09
node agent-os/scripts/autonomy-transition.mjs <task-id> <state> --actor <role> --note "<why>"
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/install-git-hooks.mjs --dry-run
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/provenance.mjs verify-all
```

`autonomy-transition.mjs` enforces allowed transitions from `workflow.json`, updates the task record, appends to `agent-os/autonomy/events.jsonl`, and re-runs the autonomy validator.

`autonomy-continue.mjs` selects the highest-priority unblocked task. If every task is already integrated or blocked, it seeds the next template from `autonomy/backlog.json` through `autonomy-seed-next.mjs` and then returns that new proposed task. This keeps the loop moving without depending on chat history for the next work item.

If `autonomy-continue.mjs` reports that the backlog is exhausted, do not treat that as a blocker by itself. Refill `agent-os/autonomy/backlog.json` from durable project evidence: `docs/status.md`, `docs/live-coverage.md`, `ROADMAP.md`, open task `nextAction` fields, and validated implementation gaps. Add concrete templates with scoped files, failure modes, gates, and verification commands, validate the autonomy records, then rerun the continuation command.

`autonomy-summary.mjs` renders a deterministic sprint summary from task records and the transition log. Use Markdown for handoff notes and `--format json` for machine-readable monitors:

```bash
node agent-os/scripts/autonomy-summary.mjs
node agent-os/scripts/autonomy-summary.mjs --since 2026-05-09 --format json
```

`provenance.mjs` verifies committed provenance envelopes that bind a Git commit to task state, transition events, changed file blobs, and recorded validation.

`install-git-hooks.mjs` installs a local pre-push hook that runs the git identity guard. It refuses to overwrite an unmanaged hook unless `--force` is supplied.

## Objectives

- Make autonomous work inspectable and repeatable.
- Keep human strategic authority distinct from agent execution.
- Preserve enough context for a fresh session to resume safely.
- Require verification before work is treated as complete.
- Escalate only for true blockers: credentials, destructive operations, legal or business decisions, production deploy authority, and unclear human intent.
- Record gaps instead of inflating claims.

## Task Lifecycle

Every task moves through explicit states:

| State | Meaning | Exit condition |
|---|---|---|
| Proposed | Work is identified but not scoped. | A concrete task and owner are selected. |
| Triaged | The task is understood well enough to prioritize. | Risks, touched surfaces, and expected output are named. |
| Planned | Implementation path is chosen. | Plan gate passes or the task is split. |
| In progress | Files are being changed. | The implementation is ready for self-review. |
| Self-review | The acting agent reviews its own diff. | Obvious defects, scope creep, and style issues are fixed. |
| Cross-review | A separate reviewer checks high-risk work. | Findings are fixed or explicitly carried forward. |
| Validation | Tests, formatters, linters, and guards run. | Checks pass or failures move the task to repair. |
| Repair | Validation or review found defects. | Defects are fixed and validation repeats. |
| Ready | The work is coherent and validated. | Docs/status are updated and the change is integrated. |
| Integrated | The change is merged or otherwise accepted. | Handoff context is written. |
| Blocked | Progress depends on a human-only input. | The blocker is recorded and unblocked work continues. |

Tasks should not skip from "in progress" to "integrated". The value of the loop is the review and validation pressure in between.

## Agent Roles

Roles are functions, not personalities:

| Role | Responsibility |
|---|---|
| Strategist | Converts human direction into bounded task candidates and roadmap updates. |
| Planner | Compares viable implementation paths before code is written. |
| Implementer | Makes scoped changes and keeps diffs reviewable. |
| Reviewer | Looks for regressions, missing tests, unclear contracts, and overclaiming. |
| Security reviewer | Reviews changes to identity, permissions, audit, secrets, settlement, sandboxing, and external execution. |
| Verifier | Runs local and CI-equivalent checks, records failures, and confirms repairs. |
| Release operator | Owns credentials, production deploys, public releases, and final milestone claims. |

One agent may perform multiple roles on low-risk work, but security-sensitive or broad changes need independent review.

## Required Gates

### Plan Gate

Trigger when more than one credible implementation path exists.

The plan must record:

- the chosen path;
- rejected alternatives;
- expected production failure modes;
- how the change will be verified.

### Security Gate

Trigger when a diff touches:

- capability signing, verification, expiry, or revocation;
- identity keys, peer auth, token rotation, or registry semantics;
- audit log integrity or event audience;
- settlement accounting or on-chain program code;
- sandboxing, subprocess execution, filesystem access, or secret loading;
- CI workflows, release automation, or dependency policy.

Security review must happen before integration. Findings are fixed in the same task unless the gap is explicitly accepted and tracked.

### Fan-out Gate

Trigger when work spans more than three crates, apps, or service boundaries.

The task should either be split or assigned as disjoint write scopes. Broad serial edits are allowed only when they are mechanical and validation is strong.

### Test-expansion Gate

Trigger when new public behavior has only happy-path coverage.

The task must add failure-mode coverage or record the missing cases in a status document. "No tests because it was simple" is not a sufficient reason for protocol, security, persistence, or CLI behavior.

### Docs Gate

Trigger when a public command, type, protocol, architecture claim, setup command, or status boundary changes.

Docs must be updated in the same change. Public docs should separate implemented behavior from experimental and planned work.

### Escalation Gate

Trigger only for:

- missing credentials or accounts;
- destructive operations;
- production deploys;
- legal, governance, or financial decisions;
- contradictory human instructions;
- ambiguous requirements where a reasonable assumption could cause harm.

When escalation fires, record the blocker and continue with unrelated unblocked work.

## Verification Levels

Use the narrowest sufficient gate during development, then the full gate before integration.

| Level | Command | Use |
|---|---|---|
| Fast Rust gate | `bash agent-os/scripts/validate.sh --quick` | Early local iteration. |
| Full Rust gate | `bash agent-os/scripts/validate.sh` | Before integration. |
| Git identity guard | `node agent-os/scripts/validate-git-identity.mjs` | Scans recent local and upstream commit authors/committers by default; pre-push passes the exact pushed ref ranges. |
| Current identity guard | `node agent-os/scripts/validate-current-git-identity.mjs` | Refuses commits and pushes unless the active local Git author and committer resolve to the neutral automation identity. |
| Autonomy summary | `node agent-os/scripts/autonomy-summary.mjs --since YYYY-MM-DD` | Repeatable handoff and sprint evidence from task JSON plus event history. |
| Live coverage matrix | `node agent-os/scripts/validate-live-coverage.mjs` | Ensures opt-in live coverage inventory matches real test files. |
| Provenance gate | `node agent-os/scripts/provenance.mjs verify-all` | Public task and commit evidence. |
| Live tests | `cargo test --workspace --exclude covenant-settlement-program -- --ignored live_` from `agent-os/` | Real daemon, subprocess, model, or network paths. |
| Landing docs | `pnpm --dir landing build` | Public docs and website changes. |
| Solana program | `anchor build` from `agent-os/` | Protocol state, staking, credits, escrow, and receipt anchors. |

Live tests are not a substitute for unit tests. They are the signal that a path survives real process and tool boundaries.

When running a targeted live CLI test directly, build the CLI first with `cargo build -p covenant --locked`; those tests execute the workspace `target/debug/covenant` binary.

Before autonomous commits, run `node agent-os/scripts/configure-git-identity.mjs` in the repository. The history validator still accepts older project pseudonyms for audit continuity, but the current-identity guard only accepts the neutral automation identity for new local work.

## Project Memory

Tracked memory should be durable, concise, and useful to future contributors:

- [README.md](../README.md): public thesis and status.
- [ROADMAP.md](../ROADMAP.md): capability roadmap.
- [docs/project-memory.md](./project-memory.md): durable project context and invariants.
- [docs/repo-map.md](./repo-map.md): repository structure.
- [docs/live-coverage.md](./live-coverage.md): live boundary coverage matrix.
- [agent-os/autonomy/workflow.json](../agent-os/autonomy/workflow.json): machine-readable lifecycle, roles, gates, and definition of done.
- [agent-os/autonomy/backlog.json](../agent-os/autonomy/backlog.json): durable seed queue for future autonomous tasks.
- [agent-os/autonomy/tasks](../agent-os/autonomy/tasks): active and completed autonomous maintenance tasks.
- [agent-os/autonomy/events.jsonl](../agent-os/autonomy/events.jsonl): append-only task transition log.
- `node agent-os/scripts/autonomy-summary.mjs`: deterministic sprint and handoff summaries from the public task state.
- [docs/provenance/README.md](./provenance/README.md): alpha provenance envelope contract.
- [agent-os/00_spec.md](../agent-os/00_spec.md): operating-layer product spec.
- [BUILT.md](../BUILT.md): recursive engineering model and honesty boundaries.

Local handoff files may exist for a particular deployment, but public claims should not depend on private state.

## Handoff Contract

A handoff is valid when a fresh session can answer:

- What changed?
- What remains unmerged or unvalidated?
- Which checks passed?
- Which checks failed and why?
- Which blockers require human action?
- What is the next safest task?

If the answer lives only in chat history, the handoff is incomplete.

## Definition of Done

A task is done when:

- the behavior exists in code or the claim is removed;
- tests or explicit validation cover the changed surface;
- docs reflect new public behavior;
- security-sensitive changes passed review;
- known gaps are tracked as experimental or planned;
- the repository can be resumed by a new agent or human without private context.
