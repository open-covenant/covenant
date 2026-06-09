# Track A findings (2026-06-09)

The Track A keystone is built and was tested empirically. Result: scaffold optimization
saturates on clean coding tasks, even with a weak executor.

## What was built and proven

- `bench/run.mjs` gained a `scaffold` solver: `claude -p` with a candidate scaffold
  (`scaffold/coder.md`) as the scored executor, in the isolated worktree, held-out graded.
- `optimize.mjs`: the optimizer. Scores the incumbent on the frozen bench, runs a separate
  `claude -p` proposer for one minimal scaffold edit, re-scores the candidate, promotes only on
  `gain >= margin` with no held-out regression, archives every version. Executor != optimizer.

The full cycle (score -> propose -> score -> gate -> promote/reject -> archive) is proven end to
end.

## What the live runs showed

Three real, validated capability tasks, graded by held-out `rustc --test`:

| task | none | reference | scaffold (incumbent) |
|---|---|---|---|
| smoke-readme-marker | 0 | n/a | 1.0 |
| rust-duration-parse | 0 | safe 1.0 / unsafe 0.929 | 1.0 (opus and haiku) |
| rust-expr-eval (spec trap) | 0 | left-to-right 1.0 | 1.0 (haiku) |

Every optimizer cycle REJECTED: the incumbent scaffold already scores 1.0, even with haiku as
the executor, even on a spec-adherence trap (left-to-right arithmetic, no precedence).

## Conclusion

On self-contained, clearly specified coding tasks, current models are interface-correct about
100% with a basic scaffold. Scaffold optimization measured by correctness therefore has
essentially no gradient. This is the same interface-correct smell `autonomy-metrics` first
surfaced, now confirmed at the capability-benchmark level.

## Implications

- Do NOT wire Track A into the loop as-is. It would spend budget only to reject.
- Real signal needs either a continuous metric (Track B: litesvm-CU on a hot-path, where correct
  code still has room to get cheaper) or genuinely hard, multi-file, or underspecified tasks
  (SWE-bench-shaped), not clean self-contained ones.
- The engine is sound and reusable the moment a high-signal benchmark exists. Bank it; revisit via
  Track B if there is appetite for a speculative optimization experiment.
