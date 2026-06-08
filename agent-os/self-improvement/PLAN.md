# Recursive self-improvement for the covenant loop

Status: Phase 0 complete + Phase 1 metrics landed 2026-06-08; loop building on `main`. Local-only doc (under gitignored `agent-os/autonomy/`).

## Goal

Take the autonomous loop from "builds covenant" to "builds and improves itself." The
achievable, safe meaning of that — the one every serious system below converges on:

> The loop optimizes its own **scaffold** (prompt, skills, selection heuristics, tooling)
> and a few **measurable code hot-spots**, against a **frozen benchmark it cannot edit**,
> promoting a change only when it provably beats the incumbent.

Not in scope: the loop rewriting Claude, or evolving open-ended features unsupervised.
The keystone we lack today: a **feedback edge** and a **quality gate**. The loop is a clean
closed cycle (select → prompt → execute → validate → commit → log) but nothing reads its
own performance to change future behavior, and no check scores whether the work was *good*.

## What the references teach

| System | Steal this | Covenant seam |
|---|---|---|
| SICA | CI-gated promotion (beat incumbent's lower confidence bound, not argmax) + multi-objective utility (score/cost/time) + per-trace post-mortems | `autonomy-summary.mjs` → `autonomy-metrics.mjs`; promotion gate |
| Hyperagents / DGM | Archive + stochastic parent selection (∝ score, ∝ 1/children) + **fitness fn & controller outside the editable surface** + train/test split | immutable-core manifest; git-worktree archive |
| OpenEvolve / AlphaEvolve | Evolution needs a machine-gradeable scalar; `rust_adaptive_sort` template (cargo build/run + correctness-gates-speed) | Track B CU/perf; litesvm-CU grader |
| ShinkaEvolve | Novelty rejection *before* the expensive eval + fitness/novelty parent sampling + public/private metric split | pre-eval diff dedup; cheap→expensive ladder |
| EvoAgentX | Benchmark/Evaluator interface + TextGrad textual-gradient prompt edits + executor≠optimizer LLM + rollback | optimizer-role `claude -p`; bench graders |
| Hermes self-evolution | "Benchmarks are GATES, not fitness" + skills as self-verifying procedural memory (`## Verification`) + human PR-gated, ~$2–10/run | `agent-os/scripts/` skill hygiene; `autonomy-review-artifact.mjs` → PR |

Seven invariants all of them share (the actual spec):

1. A fitness harness is the keystone — automated, scalar, fast enough to run often.
2. Archive of versions with scores; never delete, route around regressions.
3. Statistically-gated promotion — don't chase noise (our signals are noisy: tests, CU, flaky live).
4. Fitness fn + selection controller live **outside** the self-editable surface (safety + anti-Goodhart).
5. Train/test split; the held-out set is a **gate, not a fitness target**.
6. Staged cheap→expensive eval (`cargo check` → clippy → unit → anchor/litesvm → CU).
7. Sandbox every candidate — we already have gVisor + sandbox-gated fs/terminal.

Honest cost note (SICA): ~$7k / 15 iters on noisy 50-task subsets, gains lumpy not monotone.
Our evals are slower (compile + validator), so invariants 3/5/6 and novelty-rejection are
mandatory, not optional.

## Covenant baseline (seams)

- Measure: extend `autonomy-summary.mjs` (`buildSummary()` already parses `events.jsonl`).
- Archive: git worktrees (the `covenant-skills` worktree pattern already exists).
- Gate: `validate-autonomy.mjs` (mandatory pre-action) + `validate.sh --scripts` + the
  `autonomy-review-artifact.mjs` review-evidence pipeline → PR.
- Self-mod targets: the loop prompt (now `docs/internal/loop-prompt.md`), `agent-os/scripts/`
  skills, the selection heuristic (`autonomy-continue.mjs`), backlog authoring.
- Backlog/refill: `backlog.json` + `autonomy-seed-next.mjs` + `autonomy-status-gaps.mjs`.

## Plan

Two tracks on a shared substrate. Track A is the heart of "improves itself." Track B is a
capability the loop gains. Everything reuses existing infra — no new orchestrator.

### Phase 0 — clear landmines + immutable core  (precondition)
- [x] Activate repo hooks for the autonomous session (leak scan / session-lock / identity).
- [x] Reconcile autonomous identity aw → Covenant.
- [x] Move the loop prompt out of the untracked watchdog heredoc into the repo.
- [x] **Immutable core** — `validate-immutable-core.mjs` + `immutable-core.json` (sha256
  baseline of governance files + glob block for keypairs/deploy/bench), enforced in
  `hooks/pre-commit` for autonomous sessions only (operator-exempt). Protects identity/
  selection/gates/prompt/metrics. Verified: integrity tamper blocked (exit 2), operator
  exempt, glob blocks staged keypair. Residual: manifest holds its own baseline — move
  out-of-band before Track A.

### Phase 1 — feedback edge: measure
- [x] `autonomy-metrics.mjs` (sibling to `autonomy-summary.mjs`): repair rate, validation
  pass/fail rate, time-in-state (median/p90), throughput/day, transitions-by-role,
  most-repaired tasks. `--format json|markdown`. First read (8020 events): repair rate ≈ 0,
  validation failure rate ≈ 0 — the loop passes its own gates ~100%, the
  interface-vs-production smell Phase 2's real benchmark must expose.
- [ ] Per-slice trace post-mortem appended to the archive (SICA) — next.
- [ ] mock/live ratio wired into metrics (from metrics.mjs / live-coverage.json) — next.

### Phase 2 — capability benchmark (keystone)
- `agent-os/autonomy/bench/` — ~15–25 frozen held-out tasks (real Rust/Solana tickets +
  scaffold/skill tasks), each with a deterministic grader returning a metric dict.
- Multi-objective scalar defined up front; hard correctness gate (correctness<1 ⇒ 0).
- public/private split (held-out private tests catch reward-hacking).
- Runs in the existing gVisor sandbox / ephemeral worktree. In the immutable core.

### Phase 3 — Track A: recursive scaffold optimization
- Archive = git worktrees; version = {prompt, skills, selection heuristic} + scores.
- Optimizer role = a second `claude -p` reading metrics + worst-K post-mortems → one minimal
  scaffold edit (TextGrad-style) → re-score on the benchmark. Executor ≠ optimizer.
- Promotion gate: beats incumbent lower confidence bound AND regresses no held-out gate; else rollback.
- Parent selection stochastic ∝ score × 1/(1+children). Novelty rejection before each eval.
- Every promotion → PR via `autonomy-review-artifact.mjs`, never auto-merge.

### Phase 4 — Track B: bounded code evolution
- Tagged hot-spots only (CU-critical handler, Borsh serializer, curve/Merkle op): EVOLVE-BLOCK
  + correctness-gated CU/latency fitness (rust_adaptive_sort pattern, cargo run → litesvm CU).
- Exposed as a skill invoked on `optimize`-tagged backlog tasks. Human review + checkmark
  discipline gate anything mainnet-bound. Never point at open-ended/consensus-critical code.

### Phase 5 — guardrails
- Async semantic overseer: extend the watchdog past mtime-stuck to cancel a thrashing optimizer.
- Hard caps: $/token budget per run, iteration cap, existing idle-backoff.
- Anti-Goodhart: reject candidates that edit benchmark tests or add `#[ignore]`; held-out private suite.
- Respect existing no-upstream gate, docs-stay-local, banned files.

## Risks & cost
- Scaffold optimization is the expensive part (each eval = compile + test). Budget caps +
  novelty-rejection + cheap→expensive ladder are load-bearing.
- Gains are lumpy; design for discrete stats-gated wins, not a smooth curve.
- Real danger is blast radius (self-edit touching keypairs/deploy/commit logic) — contained by
  the immutable core + now-live hooks + human PR gate. Phase 0's immutable-core item is non-negotiable
  before Track A goes live.

## Open issues
- **Branch drift — RESOLVED 2026-06-08.** Loop switched to `main` (ff to origin/main); the
  watchdog now switches back to main before each launch (clean tree only) and the prompt forbids
  feature branches. `feat/said-bridge` (22-commit SAID-protocol crate, today's work) is preserved
  on `origin/feat/said-bridge` — awaiting a review/merge decision (not abandoned, not force-merged).
- **Next:** Phase 2 (capability benchmark, the keystone), then Track A. Finish Phase 1's two
  remaining items (trace post-mortems, mock/live ratio). Before Track A, move the immutable-core
  baseline out-of-band (it currently self-hosts its own hashes).
