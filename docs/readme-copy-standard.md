# README Copy Standard

The root README is the public first impression of Covenant. It should read like infrastructure from a serious AI systems lab: precise, confident, evidence-backed, and externally legible.

## Voice

- Lead with what Covenant is, not with release caveats or internal process.
- Use systems language: agent- and blockchain-native operating layer, control plane, governed execution, provenance, capability-scoped authority, durable memory, runtime dispatch, settlement.
- Present "built by agents for agents" as an operational fact: governed agent workflows, recorded validation evidence, and provenance for human review. Do not turn it into a slogan.
- Keep claims tied to implemented surfaces or clearly named research direction.
- Prefer compact declarative sentences over narrative project history.
- Keep validation and status references factual, but do not turn the README into a release checklist.

## Forbidden Public Framing

Do not use the README for:

- alpha launch contracts;
- disclaimers framed as lists of things the project does not claim;
- internal approval process;
- hobby, demo, or toy-project language;
- apology-style copy;
- contributor-only backlog notes.

## Status Discipline

`docs/status.md` is the source of truth for implementation status. When that file changes, review the README in the same change and update the hidden status marker with:

```bash
node agent-os/scripts/validate-readme-copy.mjs --update
```

The marker is not a substitute for judgment. It is a forcing function that prevents status changes from bypassing the public positioning review.

## Required Shape

The README should keep these sections unless there is a deliberate product-level rewrite:

- `Why Covenant`
- `Architecture`
- `Capabilities`
- `Validation`
- `Research Direction`
- `Contributing`
- `Security`

The README should link to the implementation map, status matrix, audit integrity docs, release validation profile, and `agent-os/README.md`.
