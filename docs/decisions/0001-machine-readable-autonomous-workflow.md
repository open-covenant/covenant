# 0001: Machine-readable autonomous workflow

## Status

Accepted.

## Context

Covenant's public thesis depends on autonomous engineering being inspectable, resumable, and hard to overclaim. Prose workflow documents are useful, but they do not give agents or CI anything concrete to validate. The project needs a minimal state layer that can represent task lifecycle, gates, expected failure modes, verification commands, and human escalation without introducing a database or heavyweight orchestration service.

## Decision

Add a small machine-readable workflow under `agent-os/autonomy`:

- `workflow.json` defines states, transitions, roles, priorities, gates, and definition of done.
- `tasks/*.json` stores task records with state, scope, gates, expected failure modes, verification, next action, and escalation needs.
- `scripts/validate-autonomy.mjs` validates those artifacts without third-party dependencies.
- `scripts/validate.sh` runs the autonomy validator before the Rust gates.

This makes autonomous process state part of the validated repository instead of private chat state.

## Consequences

- Agents can update backlog state in a format that is easy to diff and validate.
- CI can reject malformed task records and forbidden public identifiers.
- Public docs can reference task state without depending on untracked handoff files.
- The model remains intentionally simple; it does not yet persist every transition event or review artifact.

## Follow-up

- Add transition history when tasks start moving through states.
- Link task records to tests, commits, and review artifacts.
- Add generated status output only after the JSON records prove useful by hand.
