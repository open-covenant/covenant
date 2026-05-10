# Autonomous Task Backlog

This directory contains machine-readable task records for autonomous maintenance. The records are intentionally small JSON files so agents and humans can diff them, validate them, and update them without a database.

Validate with:

```bash
node agent-os/scripts/validate-autonomy.mjs
```

The validator is also called by `agent-os/scripts/validate.sh`.

Pick the next unblocked task:

```bash
node agent-os/scripts/autonomy-next.mjs
node agent-os/scripts/autonomy-next.mjs --json
node agent-os/scripts/autonomy-next.mjs --seed
```

Use `--seed` when every tracked task is integrated or blocked and the backlog still contains task templates. It creates the next task JSON, appends a durable `events.jsonl` seed event, validates the autonomy records, and then prints the selected task.

Continuation check after a commit or push:

```bash
node agent-os/scripts/autonomy-continue.mjs
```

If this command names a task, keep working on that task instead of ending the session with a status report. Stop only when every candidate is blocked, the user explicitly asks to pause, or the execution environment forces a turn boundary.

Validate summary output before publishing sprint evidence:

```bash
node agent-os/scripts/validate-autonomy-summary.mjs
```

Validate the commit and push attribution policy:

```bash
node agent-os/scripts/validate-commit-rotation.mjs
```

Move a task through an allowed transition:

```bash
node agent-os/scripts/autonomy-transition.mjs memory-drift-repair planned --actor planner --note "ADR drafted"
```

Transitions update the task JSON and append an event to `agent-os/autonomy/events.jsonl`. Commit both when the transition represents durable project state.

Export an unsigned review artifact for a task:

```bash
node agent-os/scripts/autonomy-review-artifact.mjs autonomy-status-gap-report --json
```

The artifact includes the task record, matching transition events, declared gates, verification commands, and content digests. It is not a signature; signing remains a separate hardening step.
Verify it before using it as review evidence:

```bash
node agent-os/scripts/autonomy-review-artifact.mjs autonomy-status-gap-report --json \
  | node agent-os/scripts/autonomy-verify-review-artifact.mjs --stdin
```

Validate the review artifact pipeline after changing artifact commands:

```bash
node agent-os/scripts/validate-autonomy-review-artifacts.mjs
```

Seed the next backlog template directly:

```bash
node agent-os/scripts/autonomy-seed-next.mjs --actor planner --note "Seeded for the next autonomous slice"
```

The actor must be one of the workflow roles. The note is validated with the rest of the autonomy records, so machine-local identifiers and forbidden public framing are rejected before the task is kept.

## Adding Backlog Templates

The seeded tasks in this directory come from templates in `agent-os/autonomy/backlog.json`.

When `node agent-os/scripts/autonomy-seed-next.mjs` reports backlog exhaustion, it means every template already has a corresponding JSON file in `agent-os/autonomy/tasks`. The command prints the scaffold, validation, and reseed flow so a session can refill the queue without relying on chat history.

Use the status gap report to ground the next templates in public project state:

```bash
node agent-os/scripts/autonomy-status-gaps.mjs --json
```

Add a new template with the scaffold CLI, then validate before seeding:

```bash
node agent-os/scripts/autonomy-scaffold-backlog-template.mjs capability-scope-live-check \
  --title "Add live checks for capability scopes" \
  --summary "Capability scope docs need a small live check that proves the documented shape can be parsed before enforcement expands." \
  --next-action "Add a focused validator or test for the documented capability scope shape." \
  --failure "The documented scope shape drifts away from accepted runtime input." \
  --failure "The check accepts local-only paths or secret-bearing examples." \
  --failure "Future enforcement rejects existing grants without a migration signal."

node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/autonomy-next.mjs --seed
```

The scaffold writes a validator-friendly `proposed` template with conservative defaults:

- `priority`: `high`
- `ownerRole`: `implementer`
- `scope`: `["docs"]`
- `gates`: `["docs"]`
- `verification`: `["node agent-os/scripts/validate-autonomy.mjs", "git diff --check"]`
- `humanEscalation`: `[]`

Tighten those defaults before seeding when the task touches code, security, release, CI, or public protocol behavior.

Checklist for new templates:

- Pick a unique `id` that does not exist as `agent-os/autonomy/tasks/<id>.json`.
- Use workflow enums from `agent-os/autonomy/workflow.json` for `priority` and `ownerRole`.
- Keep `verification` commands deterministic and machine-portable (no local-only paths or secrets).
- Prefer templates that can progress without external credentials or key-custody decisions; if not, make the `humanEscalation` explicit and expect the task to move to `blocked`.
- Name at least three concrete `expectedFailureModes` before promoting a task past `proposed`.

## State Rules

- `proposed`: idea exists; expected failure modes may be empty.
- `triaged`: scope, next action, gates, verification, and at least three expected failure modes are named.
- `planned`: implementation path is chosen.
- `in_progress`: files are being changed.
- `self_review`: acting agent is reviewing its own diff.
- `cross_review`: independent review is required or in progress.
- `validation`: checks are running.
- `repair`: review or validation found defects.
- `ready`: validated and ready to integrate.
- `integrated`: accepted into the repository.
- `blocked`: requires human-only input.

Public claims should come from implemented code and validated docs, not from task records.
