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
```

Move a task through an allowed transition:

```bash
node agent-os/scripts/autonomy-transition.mjs memory-drift-repair planned --actor planner --note "ADR drafted"
```

Transitions update the task JSON and append an event to `agent-os/autonomy/events.jsonl`. Commit both when the transition represents durable project state.

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
