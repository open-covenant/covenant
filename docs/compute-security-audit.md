# Security audits with Covenant Compute

Available in `covenant-compute` and `covenant-compute-mcp` version **0.4.1** or
later. Check `covenant-compute --version` after upgrading; Homebrew and the website
installer pin their releases separately. Restart an existing MCP server after
upgrading its binary so clients can discover `compute.audit`.

Covenant Compute bundles Cloudflare's security-audit skill as a local workflow for
the calling agent. It provides source-guided reconnaissance, coverage-led hunting,
independent candidate review, machine-readable findings, and reports. The buyer
CLI can export the package offline, and the MCP `compute.audit` tool can read its
documents without buying compute.

This integration does not add a paid security-audit job kind or automatically send
a repository to an operator. The host agent coordinates the audit and supplies
delegation, filesystem access, and any required sandbox. The bundle does not itself
implement that sandbox or execute the six phases.

## Read or install the skill

Inspect the package with the buyer CLI:

```sh
covenant-compute audit-skill --list
covenant-compute audit-skill --read SKILL.md
covenant-compute audit-skill --read UPSTREAM.json
covenant-compute audit-skill --read upstream/report-schema.json
```

From the repository where an agent should discover the skill, export it into the
agent's skill directory. The final destination must not already exist; export
refuses to replace an installed copy.

```sh
mkdir -p .agents/skills
covenant-compute audit-skill --export .agents/skills/covenant-compute-security-audit
```

The destination contains a Covenant `SKILL.md`, `UPSTREAM.json`, and the unchanged
Cloudflare documents, schema, validators, tests, and license in `upstream/`. Hosts
with a different skill search directory can export to that directory instead.
Loading the skill provides guidance; a request such as “Audit this repository
using covenant-compute-security-audit, standard profile, output outside the
repository” selects the full workflow.

MCP clients can load the same instructions progressively:

```json
{}
```

Pass that object to `compute.audit` to read the entrypoint. To read a phase or
schema, call `compute.audit` with an asset name:

```json
{"document":"upstream/RECONNAISSANCE.md"}
```

`compute.audit` reads packaged assets. It does not accept a target repository,
launch agents, run validators, submit a network job, or spend funds. Use CLI export
when a host needs local files, including the zero-dependency Node validators.

## Run the workflow

Start with the exported `SKILL.md` and `upstream/SKILL.md`. When upstream refers to
its skill directory, use the exported `upstream/` directory. Its six phases are:

1. Reconnaissance: map architecture, principals, boundaries, inputs, local execution
   limits, and prior evidence; create a deterministic coverage ledger.
2. Coverage-led hunting: assign units to isolated hunters and use critics to find
   missing paths or unsupported coverage claims.
3. Candidate validation: assign each unique candidate to a fresh verifier that
   tries to disprove it from current source and bounded local evidence.
4. Structured output: write the findings and ledger and validate both documents.
5. Independent record verification: fresh reviewers check final confirmed and
   blocked records; material replacements receive another independent review.
6. Reporting: derive the summary, confirmed finding details, and validation
   handoff from the final records and ledger.

The default profile is `standard`. `quick` performs one hunter wave followed by one
final critic and combines candidate/final-record review into one fresh verifier
per candidate. It explicitly reports partial coverage. `deep` uses finer coverage
units, additional independent passes, and separate validation stages. Scoped runs
retain excluded areas as `out_of_scope`, never `covered`.

Use the host's local subagent tools with fresh contexts and separate scratch roots.
The parent owns shared files and enforces identity, scope, budget, and evidence
boundaries. Record the source revision and dirty state, scope, profile, prior runs,
and budget before delegation. Reserve all four baseline reconnaissance calls,
mandatory critics, and candidate verification before assigning hunters. If a strict
budget cannot support the required phases, disclose an incomplete run and its
remaining units. Do not silently remove verification to fit the budget.

The retained output is:

| Artifact | Purpose |
| --- | --- |
| `run-metadata.json` | Source, scope, profile, execution policy, budget, and run status |
| `architecture.md` | Principals, trust boundaries, source paths, and companion selection |
| `coverage-ledger.json` | Stable coverage units, assignments, evidence, candidates, and gaps |
| `findings.json` | Independently decided confirmed, blocked, and rejected records |
| `REPORT.md` | Results and explicit coverage/validation limits |
| `FINDINGS-DETAIL.md` | Reproduction and remediation for confirmed medium-or-higher findings |
| `NEEDS-VALIDATION.md` | Verified source traces, decisive blockers, and safe next checks |

Use a new output directory outside the audited target by default. An in-target
directory requires the user's explicit selection and must be ignored by version
control. The workflow describes source fixes; it does not edit the audited code.

For example, after exporting the skill as above and completing the corresponding
phase, validate the records with Node.js. Replace the output path with the actual
external run directory:

```sh
audit_skill_dir="$PWD/.agents/skills/covenant-compute-security-audit/upstream"
audit_output_dir="/absolute/path/outside-the-target/audit-run"
node "$audit_skill_dir/validate-findings.cjs" "$audit_output_dir/findings.json"
node "$audit_skill_dir/validate-coverage-ledger.cjs" "$audit_output_dir/coverage-ledger.json"
```

Run ledger validation after creating or updating the ledger, and both validators
at the prescribed final-output and replacement boundaries. Valid output can contain
zero confirmed findings. Validator success checks document structure and semantics;
it does not independently inspect source, verify actual artifact files, or establish
that a vulnerability was reproduced.

## Keep execution and claims bounded

Target-controlled execution requires an OS-enforced sandbox with no external
network, a sanitized allowlisted environment, read-only source and tools,
scratch-only writes, and explicit CPU, memory, process, file-size, disk, and time
limits. The bundle does not make a host or remote worker compliant merely by
loading these instructions. Missing any control blocks execution: retain a
source-grounded candidate as `needs_validation` with the exact missing capability.
Use dummy fixtures and installed local dependencies; do not probe deployments,
fetch dependencies, expose real credentials, or test against paid/shared services.

The parent must retain evidence through upstream's race-safe descriptor-based
promotion procedure after all target processes terminate. Raw scratch files,
symlinks, recursive copies, and unverified artifact paths are not retained evidence.
The upstream ledger validator checks artifact ownership syntax, so the parent must
also establish that the referenced evidence was actually promoted safely.

Only `confirmed` records receive severity, supported by a complete source trace
and a bounded observed result. `needs_validation` records have precise decisive
blockers and no severity. `rejected` records retain disproved claims. Candidates
that never received independent validation remain in the ledger, outside
`findings.json`, and make the run incomplete.

Where explicitly authorized, existing `compute.infer` purchases can assist
source-only roles. Remote operators receive the submitted context: send only
authorized source and necessary evidence within the agreed scope and spend caps.
Fresh contexts, source checks, and review independence remain the parent's
responsibility. A signed inference receipt establishes the identity and hash of
returned job output, not correctness or independence of a security review.

Use `compute.agent` or `compute.fix` only for separately authorized, scoped
remediation. Their checker acceptance and receipts establish the checks that ran
on that work; they do not by themselves confirm a security finding. Preserve each
receipt as execution provenance and keep the audit's source/evidence/verifier chain
separate from it.

## Upstream provenance

The bundle preserves
[Cloudflare security-audit-skill at c1c8a8c1471069fb0e188eeaff69b8e8db6564a8](https://github.com/cloudflare/security-audit-skill/tree/c1c8a8c1471069fb0e188eeaff69b8e8db6564a8)
under the MIT license, copyright Cloudflare 2025–2026.
[UPSTREAM.json](../agent-os/crates/covenant-compute-buyer/skills/security-audit/UPSTREAM.json)
records SHA-256 hashes for every vendored upstream file, including its license.
The Covenant wrapper is maintained separately so upstream text remains attributable
and updates can be reviewed against a pinned commit.
