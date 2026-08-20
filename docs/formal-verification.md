# Formal verification (Kani)

Covenant uses the [Kani](https://model-checking.github.io/kani/) bit-precise
model checker to prove arithmetic-safety and invariant properties over the
*entire* input domain, not just the examples a unit test enumerates. Where a
`#[test]` pins named cases, a `#[kani::proof]` harness proves the same property
holds for every value the type system admits.

## Running locally

Kani is a separate toolchain, not part of the default `cargo` install:

```sh
cargo install --locked kani-verifier
cargo kani setup
```

Then, from the `agent-os/` workspace:

```sh
cargo kani --package covenant-budget
```

Proof harnesses live in a `#[cfg(kani)]` module inside each crate, next to the
unit tests, so they only compile under the Kani compiler and never affect the
normal `cargo build` / `cargo test` graph.

## What is proven

### `covenant-budget` token bucket

The lazy-refill token bucket backs `Settlement.budget_credits_per_hour`. Its
refill math mixes `u64` clocks with `u128` intermediates, saturating adds, and
division by a runtime capacity. That is exactly the surface where an off-by-one
or a dropped guard silently mints or strands credits. The harnesses prove, for
all `(capacity, tokens, last_refill_ms, now)`:

- **`refill_never_panics`**: the `now - last_refill_ms` subtraction never
  underflows, the `u128` multiply never overflows, and the saturating clock
  advance never wraps. It holds for any bucket, not only invariant-respecting
  ones, so a corruption elsewhere still can't make refill panic.
- **`refill_keeps_tokens_within_capacity`**: refill preserves the
  `tokens <= capacity` class invariant (assumed on entry, since it is
  maintained by `set_capacity`, `try_debit`, and bucket creation), so an idle
  agent cannot bank refills past its ceiling.
- **`refill_clock_never_rewinds`**: `last_refill_ms` only ever moves forward,
  never backward (monotonic non-decrease). The tighter bound, that it lands at
  exactly `now` minus the unspent sub-token remainder, holds by construction;
  see the deferred-property note below.
- **`refill_eta_never_schedules_in_the_past`**: the projected refill instant is
  always `>= now`, regardless of shortfall or rate.
- **`project_overshoot_matches_spec`**: proves the projection's decision
  *contract*, not just panic-freedom (the body has no panic-capable op, so that
  would prove nothing). `NoExtrapolation` flags exactly when already over
  budget; below either threshold it never flags; above the thresholds an
  already-over debit must flag, a zero debit never flags, and the decision is
  monotonic in observed debit. The exact 2x projection magnitude is pinned by
  the unit tests, so the harness states the contract independently rather than
  restating the formula against itself.

The dual to `refill_clock_never_rewinds`, that the clock never advances *past*
`now`, is left to the unit tests (`refill_full_bucket_resets_clock_to_now`,
`refill_partial_elapsed_accumulates_in_clock`) on purpose. Proving it requires
reasoning through the nested division `add = elapsed*cap/H` then
`consumed = add*H/cap`; symbolic 128-bit division bit-blasts to a SAT instance
no solver closes in bounded time, and value-range assumptions don't shrink the
divider circuit. It is a known limitation of bounded model checking on
nonlinear integer arithmetic, not a gap in coverage of the linear properties.

## Adversarial LLM review

A passing Kani run says every harness held. It does **not** say the harnesses
are worth holding. An over-tight `kani::assume` can make a harness vacuously
true: Kani reports success, but nothing was proven. `scripts/verify-proofs.mjs`
closes that gap. It runs Kani, mechanically confirms `0 failures`, then puts
the proofs in front of a panel of **independent adversarial reviewers**. One
agent rubber-stamping its own read is the weak spot it exists to avoid.

Each reviewer is given the functions under test, the harness source, and the
Kani output, and is told to *refute* the proofs from a different angle:

- **vacuity / reachability**: do the assumptions exclude the inputs that would
  expose a bug? do any assertions report `UNREACHABLE`?
- **encoding strength**: does each assertion express the invariant it claims, or
  something weaker / tautological?
- **soundness / scope**: is the proven domain the real domain? is panic-freedom
  asserting away a condition that should be a failure?

A harness is rejected only when a **majority** of reviewers refute it, so one
over-aggressive reviewer can't sink a sound proof, while any single genuine
flaw is still surfaced in the output. The harness set is parsed from the source
(not from whatever the reviewers happen to name), so a misspelled or invented
name can't split the vote or hide a harness, and the run fails if any harness
wasn't covered by a majority. The reviewers run through `claude -p` (headless,
the local Claude subscription): no API key, no extra billing.

```sh
node scripts/verify-proofs.mjs            # Kani + adversarial panel
node scripts/verify-proofs.mjs --no-llm   # mechanical only (no reviewers)
node scripts/verify-proofs.mjs --selftest # check the parsing/aggregation helpers
```

## CI

`.github/workflows/kani.yml` runs the budget proofs on every change to that
crate or to the workflow itself. CI runs Kani alone (the LLM review needs the
local subscription); the adversarial review is the local / autonomy-loop gate.
