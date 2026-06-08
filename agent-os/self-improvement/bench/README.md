# Capability benchmark (Phase 2)

The fitness signal for recursive self-improvement. The loop optimizes its scaffold against
this; it must never be able to edit it (immutable core). Tracked on `feat/self-improvement`.

## Why

`autonomy-metrics.mjs` showed the loop passes its own gates ~100% (repair/validation-fail
rates ≈ 0): interface-correct, not production-correct. This is the independent, harder,
machine-graded signal — real build/test/clippy/CU outcomes on a frozen task set, with the
candidate evaluated in isolation so it can't see or game the grader.

## Two task kinds

- **repo-health** — no `base`. Graded in place against the current tree (regression gate:
  "did anything break"). Example: `covenantd-builds-clean`.
- **capability** — has a `base` (a git ref). The runner checks out an isolated ephemeral
  worktree at `base`, runs a **solver** to attempt the task, runs **anti-gaming** checks,
  then runs the **held-out grader**, scores, and tears the worktree down. Measures how good
  the scaffold is at producing correct work.

## Scoring

`score = correctness · Σ(weight_k · metric_k)`. `gate: true` stages are correctness gates —
fail one and the task scores 0, later stages skipped (cheap→expensive ladder cuts broken
candidates early). Metrics: `tests` (passing fraction parsed from cargo), `clippy`/others
(1 if the stage exits 0). The run aggregates per-task scores into `meanScore`.

## Held-out grading + anti-gaming

A capability task's grader files live in `tasks/<id>/grade/` — **outside** the candidate's
tree. They are copied in only *after* the solver runs, so the attempt can't read or weaken
them. Before grading, `gamingViolations` rejects (score 0) any candidate that added
`#[ignore]` or removed/weakened existing tests vs `base`.

## Solvers

    --solver none                 # attempt nothing (baseline; a capability task should score ~0)
    --solver replay               # cherry-pick task.solutionCommit (a known-good fix should score high)
    --solver "cmd:<shell>"        # run a shell command in the worktree (apply a patch, etc.)
    # --solver scaffold           # (planned) invoke `claude -p` with the loop prompt — the real Track A measurement

`none` and `replay` bracket the grader: a correct grader scores `none` low and `replay`
high. That's how you validate a task before trusting it.

## Run

    node agent-os/self-improvement/bench/run.mjs --list
    node agent-os/self-improvement/bench/run.mjs --solver none
    node agent-os/self-improvement/bench/run.mjs --solver replay --task <id> --json
    node agent-os/self-improvement/bench/run.mjs --task smoke-readme-marker --solver "cmd:printf '\nSMOKE-OK\n' >> README.md"

## Authoring a capability task from a real commit

The cleanest source of honest tasks is git history. For a commit `C` that adds a feature
plus its tests:

1. `base` = `C^`. `solutionCommit` = `C` (enables `--solver replay`).
2. Split `C`'s test additions into `tasks/<id>/grade/` (held-out) so the agent must write
   the implementation without seeing the grader; keep impl out of `grade/`.
3. `prompt.md` = the intent (what to build), from `C`'s message.
4. Validate: `--solver none` scores low, `--solver replay` scores high.

## Roadmap

- [x] Harness: ephemeral-worktree isolation, pluggable solver, held-out grading, anti-gaming,
  staged ladder, multi-objective score. Smoke-verified (`smoke-readme-marker`).
- [x] First repo-health task: `covenantd-builds-clean`.
- [ ] ~15–25 frozen capability tasks from real covenantd / covenant / Solana commits.
- [ ] CU/latency metric (litesvm) for hot-path tasks (Track B).
- [ ] Stronger sandbox than worktree isolation: gVisor / no-network / resource caps — treat
  every candidate as hostile.
- [ ] `scaffold` solver (`claude -p`) + public/private split (a private held-out suite the
  agent never sees, beyond the per-task held-out grader).
- [ ] Feed Track A: CI-gated promotion of scaffold versions against `meanScore`.
