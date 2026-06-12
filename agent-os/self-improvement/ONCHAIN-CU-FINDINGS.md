# On-chain merkle-verify CU target — go/no-go: NO-GO (2026-06-12)

Built and measured the on-chain merkle-verification target the Track B design
(§6) flagged as "what WOULD have a deep gradient." It does not — at the level
the arena optimizes (a behavior-preserving function under a frozen contract).
Reporting the data because the go/no-go did its job: cheaply de-risked a
multi-day build before any optimizer spend.

## What was built (Phase 0, all working)

- `crates/covenant-merkle` — off-chain domain-separated merkle (leaf =
  sha256(0x00‖receipt), node = sha256(0x01‖l‖r), power-of-two padded). The
  reference. 4 tests green.
- `programs/merkle-verify` — Anchor-free Solana program; `verify_inclusion`
  kernel between EVOLVE markers, hashing via the `sol_sha256` syscall
  (`hashv`). Compiles cleanly with `cargo build-sbf` (the Anchor-free design
  sidesteps the broken anchor-lang 0.31.1 build). Native tests confirm it
  matches the reference bit-for-bit and rejects every tampered proof.
- `bench/runners/cu-runner` — litesvm CU meter (the on-chain twin of the
  wasmtime fuel-runner). Captures `compute_units_consumed`. **Deterministic:
  same program + batch → identical CU.**

## The measurement (16384-leaf tree, depth 14)

| | CU (16 proofs) | vs current |
|---|---|---|
| pessimized (heap alloc per hash) | fails / ~2.8× at 4 proofs | far worse |
| current (idiomatic) | 45,703 | 1.000 |
| hand-optimized (stack buffer, one slice per syscall) | 45,079 | **1.014** |

Decomposition (CU at 1/2/16/64 proofs = 3131 / 5968 / 45703 / 181991):
- per-proof ≈ **2,838 CU**, flat → 15 `sol_sha256` calls × **~189 CU each**.
- fixed program overhead ≈ **293 CU** (entrypoint + deserialization).
- the kernel is **~99% irreducible sha256 syscalls**.

## Why the gradient is shallow

The CU cost is dominated by the `sol_sha256` syscall, which is **fixed-cost
per call** (~189 CU for a 65-byte hash) and **fixed-count** for a given merkle
scheme (depth+1 hashes per proof). A behavior-preserving function optimizer
can only touch the ~1% that isn't the syscall itself: memory-region count per
syscall, loop overhead, allocation. The one large lever — **don't allocate on
the heap** (the 32 KB SBF bump heap makes per-hash `Vec`s ~2.8× worse) — is
already taken by any non-naive implementation. Past that, the floor is hit.

This is the opposite of the audit kernel, which had genuine algorithmic fat
(per-line allocations, full `Value` re-parsing, scalar→SIMD headroom) that
compounded across 18 rounds to 6.5×. The merkle verifier has none: it is
already minimal — hash up a tree, compare.

## Where a real gradient WOULD live (and why the arena can't reach it)

Not in the kernel — in the **scheme design**:
- **Tree arity** (16-ary instead of binary: 4 hashes × 512 B vs 14 hashes ×
  65 B — fewer syscall base costs).
- **Batch subtree dedup** (16 proofs in one tree share upper nodes; verify
  the shared structure once).
- **Layer caching** (verify against a cached intermediate layer).

Each of these changes the behavioral contract / proof format / corpus — they
are **redesigns, not behavior-preserving optimizations**. The arena optimizes
a fixed function against a frozen reference; it cannot legally change the
scheme. So this gradient is human/operator design work, not loop work.

## Recommendation

1. **Do not make merkle-verify an arena target.** 1.4% < the 15% bar.
2. **The audit kernel remains the one real in-repo gradient** (still climbing,
   18 rounds, 6.5×). Keep the arena there.
3. **The verification feature is independently worth shipping** — today
   `anchor_receipt_batch` (settlement lib.rs:411) stores `merkle_root`
   *unverified*. Wiring real on-chain inclusion verification is a genuine
   security improvement, just an operator-gated, launch-sensitive protocol
   change, not an optimization experiment. The Phase 0 crates are the
   substrate if/when that ships.
4. **Keep the infra** (merkle crate, SBF verifier, CU-runner) committed and
   labeled — it's reusable if a future on-chain target with real heavy compute
   ever appears, and it's the proof this was measured, not assumed.
