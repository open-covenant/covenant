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
division by a runtime capacity — exactly the surface where an off-by-one or a
dropped guard silently mints or strands credits. The harnesses prove, for all
`(capacity, tokens, last_refill_ms, now)`:

- **`refill_never_panics`** — the `now - last_refill_ms` subtraction never
  underflows, the `u128` multiply never overflows, and the saturating clock
  advance never wraps. This is the unsigned-underflow hazard the time-rewind
  guard exists to prevent.
- **`refill_keeps_tokens_within_capacity`** — tokens never exceed capacity, so
  an idle agent cannot bank refills past its ceiling.
- **`refill_clock_never_rewinds`** — `last_refill_ms` only ever moves forward,
  so an already-paid time window can never be re-credited.
- **`refill_eta_never_schedules_in_the_past`** — the projected refill instant
  is always `>= now`, regardless of shortfall or rate.
- **`project_overshoot_never_panics`** — the saturating rate model is total
  under both projection policies.

The dual to `refill_clock_never_rewinds` — that the clock never advances *past*
`now` — is left to the unit tests (`refill_full_bucket_resets_clock_to_now`,
`refill_partial_elapsed_accumulates_in_clock`) on purpose. Proving it requires
reasoning through the nested division `add = elapsed*cap/H` then
`consumed = add*H/cap`; symbolic 128-bit division bit-blasts to a SAT instance
no solver closes in bounded time, and value-range assumptions don't shrink the
divider circuit. It's a known limitation of bounded model checking on
nonlinear integer arithmetic, not a gap in coverage of the linear properties.

## Adversarial LLM review

A passing Kani run says every harness held — it does **not** say the harnesses
are worth holding. An over-tight `kani::assume` can make a harness vacuously
true: Kani reports success, but nothing was proven. `scripts/verify-proofs.mjs`
closes that gap. It runs Kani, mechanically confirms `0 failures`, then puts
the proofs in front of a panel of **independent adversarial reviewers** — one
agent rubber-stamping its own read is the weak spot it exists to avoid.

Each reviewer is given the functions under test, the harness source, and the
Kani output, and is told to *refute* the proofs from a different angle:

- **vacuity / reachability** — do the assumptions exclude the inputs that would
  expose a bug? do any assertions report `UNREACHABLE`?
- **encoding strength** — does each assertion express the invariant it claims,
  or something weaker / tautological?
- **soundness / scope** — is the proven domain the real domain? is
  panic-freedom asserting away a condition that should be a failure?

A harness is rejected only when a **majority** of reviewers refute it, so one
over-aggressive reviewer can't sink a sound proof, while any single genuine
flaw is still surfaced in the output. The reviewers run through `claude -p`
(headless, the local Claude subscription) — no API key, no extra billing.

```sh
node scripts/verify-proofs.mjs            # Kani + adversarial panel
node scripts/verify-proofs.mjs --no-llm   # mechanical only (no reviewers)
```

## CI

`.github/workflows/kani.yml` runs the budget proofs on every change to that
crate or to the workflow itself. CI runs Kani alone (the LLM review needs the
local subscription); the semantic review is the local / autonomy-loop gate.
