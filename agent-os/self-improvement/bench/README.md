# Capability benchmark (Phase 2)

The fitness signal for recursive self-improvement. The loop optimizes its scaffold against
this; it must never be able to edit it (immutable core). Tracked on `feat/self-improvement`.

## Why

`autonomy-metrics.mjs` showed the loop passes its own gates ~100% (repair/validation-fail
rates ≈ 0). That's interface-correct, not production-correct. This benchmark is the
independent, harder signal: real build/test/clippy/CU outcomes on a frozen task set.

## Model

- A **task** is `tasks/<id>/task.json`: an ordered list of `stages` (a cheap→expensive
  ladder) plus metric `weights`.
- A **stage** runs a command. `gate: true` stages are correctness gates — if one fails the
  task scores 0 (the AlphaEvolve "correctness gates quality" rule) and later stages are
  skipped (cheap stages first, so a broken candidate is cut early and cheaply).
- A **metric** stage contributes to quality: `tests` = passing fraction parsed from cargo
  output, `clippy` = 1 if `-D warnings` is clean else 0.
- **score** = `correctness * Σ(weight_k · metric_k)`. Multi-objective, hard gate.
- The run aggregates per-task scores into a `meanScore`.

## Run

    node agent-os/self-improvement/bench/run.mjs --list      # discover tasks
    node agent-os/self-improvement/bench/run.mjs             # grade all, markdown
    node agent-os/self-improvement/bench/run.mjs --json      # machine-readable
    node agent-os/self-improvement/bench/run.mjs --task <id> # one task

## Adding a task

Create `tasks/<id>/task.json`. Keep stages ordered cheap→expensive
(`cargo check` → unit tests → clippy → integration/litesvm → CU). Put any required golden
files beside it. Tasks must be **deterministic** and **machine-graded** — no LLM-as-judge in
the gate (it gets reward-hacked). An optional held-out *private* suite (not shown to the
agent) catches gaming; keep it out of `tasks/` and grade it separately.

## Roadmap

- [x] Harness: staged ladder, correctness gate, multi-objective score.
- [x] First task: `covenantd-builds-clean`.
- [ ] ~15–25 frozen tasks: real Rust/Solana tickets + scaffold/skill tasks.
- [ ] CU/latency metric (litesvm) for hot-path tasks (Track B).
- [ ] Sandbox each candidate (gVisor / ephemeral worktree); never trust generated code.
- [ ] public/private split + anti-gaming checks (reject edits to bench tests / `#[ignore]`).
- [ ] Feed Track A: CI-gated promotion of scaffold versions against `meanScore`.
