---
name: covenant-compute-security-audit
description: Use Cloudflare's pinned security-audit workflow with Covenant Compute for requested code security reviews, vulnerability investigations, or full audits. Provides local orchestration, independent review requirements, and structured audit artifacts; loading it does not dispatch paid work.
---

# Covenant Compute security audit

Use the bundled Cloudflare workflow to review a codebase and distinguish confirmed
boundary failures, blocked validation, and refuted candidates. Covenant supplies
the pinned skill through its buyer tools; the calling agent owns the audit.

## Load the workflow

Read [upstream/SKILL.md](upstream/SKILL.md) first. Its operating modes and safety
requirements apply. For upstream instructions, **skill directory** means the
absolute `upstream/` directory inside this bundle, so its relative references and
validators resolve correctly. [UPSTREAM.json](UPSTREAM.json) records the source
commit and SHA-256 of each unchanged upstream file; [upstream/LICENSE](upstream/LICENSE)
contains Cloudflare's MIT notice.

Use guidance mode for a focused question or investigation. Use all six phases only
for a requested codebase audit, comprehensive review, pen test, or report artifacts.
Installing or reading this skill does not initiate an audit or authorize remote
source disclosure, paid purchases, fixes, publication, or deployed testing.

## Available Covenant surfaces

- `compute.audit` reads bundled documents without spending or submitting a job.
  Call `{}` or `{"document":"SKILL.md"}` for this entrypoint, then request assets
  such as `{"document":"upstream/RECONNAISSANCE.md"}` as needed. This is a document
  loader, not an audit executor or remote security-audit job kind.
- `covenant-compute audit-skill --list` lists assets; `--read <asset>` prints one;
  `--export <new-directory>` installs the whole bundle offline. The destination
  must not already exist. Export before running validators or when the host needs
  local skill files.
- Use the host's subagent tools for local orchestration. Assign fresh agents and
  separate contexts as the workflow requires; a role label alone is not independent
  review. If the host cannot provide required delegation or independence, disclose
  that limitation and keep the run incomplete rather than simulating the passes.
- Existing `compute.infer` may assist source-only roles when sending that source
  to a remote operator and the associated purchase are authorized. Send only the
  assigned source and necessary context, within the approved scope and spend cap.
  Its receipt binds a job's returned output; it does not prove source correctness,
  a reproduced boundary failure, or reviewer independence. Preserve the workflow's
  context separation and evidence checks regardless of the inference provider.
- `compute.agent` and `compute.fix` are for separately authorized, scoped fixes
  after review. Their acceptance checks and checker receipts do not establish a
  confirmed vulnerability or substitute for this audit's independent verification.

## Full audit orchestration

Follow the upstream workflow in order, reading each phase document when needed:

1. [Reconnaissance](upstream/RECONNAISSANCE.md): map principals, trust boundaries,
   entry surfaces, available sandbox controls, prior evidence, and deterministic
   coverage units. Reserve the budget before launching the four baseline roles.
2. [Coverage-led hunting](upstream/HUNTING.md): assign ledger units to isolated
   hunters and run coverage critics. Select applicable blocks from
   [ATTACK-CLASSES.md](upstream/ATTACK-CLASSES.md) and its linked companions based on
   actual boundaries; preserve the exact prompt blocks and structured results.
3. [Candidate validation](upstream/VALIDATION-AND-REPORTING.md): deduplicate root
   causes and give each candidate to a fresh verifier that did not discover it.
4. [Structured output](upstream/VALIDATION-AND-REPORTING.md): write schema-valid
   `confirmed`, `needs_validation`, and `rejected` records and validate both the
   findings and ledger with the bundled validators.
5. [Independent record verification](upstream/VALIDATION-AND-REPORTING.md): fresh
   agents review the final confirmed and blocked records. Material replacements
   need another independent verifier. `quick` merges this with Phase 3 only as
   upstream permits; no profile removes independent candidate review.
6. [Reporting](upstream/VALIDATION-AND-REPORTING.md): derive `REPORT.md`,
   `FINDINGS-DETAIL.md`, and `NEEDS-VALIDATION.md` from the verified records and
   coverage ledger. Disclose every incomplete phase, unresolved candidate, and gap.

Record target/source revision and dirty state, profile, scope paths, prior runs,
invocation budget, and execution policy in `run-metadata.json`. Use `standard` by
default; `quick` is exactly one hunter wave and one final critic with partial
coverage, while `deep` adds granularity and review. Scope exclusions stay visible.
Reserve mandatory reconnaissance, critics, and verification before hunting. Respect
any user-set invocation and monetary caps; insufficient budget means deferred work
or an incomplete run, never an unreviewed confirmation.

The parent alone writes shared metadata, architecture, ledger, findings, and reports.
Each child writes only its unique scratch directory. Use an external output
directory unless the user selected a wholly version-control-ignored directory
inside the target. Keep stable fingerprints and recheck relevant current source
before carrying prior evidence; prior deferred or blocked work is not coverage.

## Execution and evidence gates

Inspect source read-only. Execute target-controlled builds, tests, fixtures, or
processes only when the OS sandbox enforces every upstream control: no external
network, an empty allowlisted environment, read-only target/toolchain, scratch-only
writes, and explicit CPU, memory, process, file-size, disk, and time limits. A
prompt restriction, remote machine, or receipt is not that sandbox. Do not fetch
dependencies, expose credentials, probe deployments, or consume paid APIs from a
test. If a control is missing, do not execute and retain the exact validation blocker.

Retain local evidence only through the trusted parent-side, descriptor-based
promotion procedure in [upstream/SKILL.md](upstream/SKILL.md). Children and target
code cannot write retained artifacts. A failed promotion cannot supply evidence.

Only `confirmed` records receive severity and require a complete source trace plus
a bounded observed result. `needs_validation` requires a specific decisive blocker
and safe resolution plan, without severity; `rejected` records refuted claims.
Unvalidated candidates remain in the ledger and make the run incomplete.

Run both validators at the upstream phase boundaries. A successful validator checks
document structure and semantics; it does not inspect source claims, establish
that referenced artifacts exist, prove safe promotion, or establish independent
review. The parent must check evidence ownership, candidate-to-ledger links, source
revision, and verifier separation before reporting a completed audit. Keep any
Covenant receipts as separately identified provenance; never convert a checker
pass or signed model response into a security verdict.
