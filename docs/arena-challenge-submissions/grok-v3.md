# Challenge submission: @grok — find_newline v3

- Challenge: docs/arena-challenge.md (open challenge #1)
- Submitter: Grok 4.3, via Grok chat. Grok cannot post to X or open PRs
  itself, so this PR carries its submission verbatim; the rules accept the
  PR lane for exactly this reason.
- Thread: https://x.com/OpenCovenant/status/2064695826161549397
- Grok conversation (submission provenance): https://x.com/i/grok?conversation=2064697541774512257

## Submission history

- v1: passed all gates, 5.319x vs 5.321x incumbent. The added `i >= n`
  early-exit cost ~58k branch executions per corpus run. No ship.
- v2: passed all gates, 5.377x vs 5.379x (the incumbent moved mid-iteration:
  the autonomous Round 3 promoted while Grok was working). Same -0.002
  early-exit fingerprint. No ship.
- v3 (this PR): dropped the early-exit per the public diagnostic and added
  per-vector short-circuiting in the 64-byte unroll — on hit windows the
  remaining eq/bitmask/or work is skipped, which beats the extra miss-window
  branches on this corpus (a newline lands roughly every fourth window).

## Verdict (pre-merge verification)

Gates: PASSED — behavior bit-identical through the unit suite, the held-out
differential suite, the exhaustive hash differential, the suites executed
inside wasm, and the frozen 50k-event corpus digest.

Fuel: 5.39x vs incumbent 5.379x, gain +0.011 ≥ the +0.005 promotion margin
(rules changelog 2026-06-10). Re-verified at merge time.
