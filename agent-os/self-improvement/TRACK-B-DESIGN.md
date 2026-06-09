# Track B architecture: audit-kernel fuel optimization

Designed by claude-fable-5 (2026-06-09) as architect. Section 5 is a go/no-go to run BEFORE building the optimizer: if a 30-minute hand-optimization gains <15% fuel, abort the target.


**TL;DR verdict first:** covenant has exactly one genuine in-repo compute gradient — the audit chain-fold/verify path — and it is real but finite (expect 5–15 promotions, not unbounded). The on-chain CU target is empirically worse than it sounds: both programs have zero loops and no on-chain hashing, so CU is ~all Anchor framework overhead, the build is currently broken (anchor-lang 0.31.1 vs 1.0.2), and the cheapest CU wins are deleting security constraints — an anti-gaming nightmare. Identity, borsh, and budget have no surface at all (crypto is all ed25519-dalek, serialization is derive-only, budget math is ~30 LOC of O(1) arithmetic bottlenecked on locks/IO). Pick the audit kernel, measure it in **wasmtime fuel** (deterministic instruction-weighted counting — the off-chain twin of compute units), and accept up front that this track also saturates eventually.

---

## 1. Target: extract the audit verify kernel; skip on-chain CU

**Target:** the chain fold + integrity verification in covenant-audit. Today it's `build_chain_entries` / `chain_entry_for_line` / `chain_hash` / `sha256_hex` (lib.rs:511-553) plus the replay loop in `JsonlAuditLog::verify_integrity` (lib.rs:780-840). Refactor once, by hand, before any optimization: extract a **pure sync kernel** into a new tiny crate `covenant-audit-kernel` (deps: `sha2`, `serde_json` only — no tokio, so it compiles to wasm cleanly):

```rust
// covenant-audit-kernel/src/lib.rs
#![forbid(unsafe_code)]   // outside the EVOLVE region; candidates can't remove it
pub fn verify_chain(events_jsonl: &[u8], anchors_jsonl: &[u8]) -> ChainReport
pub fn fold_chain(lines: &[&[u8]]) -> Vec<ChainEntry>
```

`covenant-audit` delegates to it, so improvements are real production wins, not a vestigial bench toy.

**Why this has a gradient a strong model won't one-shot:**

- The current code does ~5–6 heap allocations per event: `serde_json::to_string` per event, a `format!("{prev}\n{evt}")` per link, a `write!`-loop hex encoder, and `String` clones of the running hash. The verify path additionally parses every line into a full `serde_json::Value` just to detect malformedness.
- The optimization ladder is long and instruction-granular: incremental `Sha256::update(prev); update(b"\n"); update(evt)` instead of `format!`+rehash → hex lookup table into a `[u8; 64]` stack buffer → keep the running hash as bytes, not `String` → `serde_json::from_slice::<IgnoredAny>` (or a hand-rolled JSON validity scanner) instead of `Value` → memchr-style line splitting over raw bytes → buffer reuse across iterations. A strong model one-shots the first two or three rungs; the tail (JSON scanner replacement, allocation elimination under an exact-behavior gate) takes iterations because each rung risks failing the differential gate and gets rejected, which is exactly the selection pressure the optimizer needs.
- Fuel counts *every* instruction including allocator internals, so even after allocations are gone there's still branch ordering, bounds-check elimination via iterator shape, etc.

**Why not on-chain CU (the Explore agent's findings make this concrete):** settlement has 17 instructions and stake 11, and *none* contain a loop, hash, or signature check — the heaviest "compute" is `internal_accrue` (stake lib.rs:625-644), five u128 checked ops. Per-instruction CU would be ~95% Anchor entry overhead (account deser, discriminator/constraint checks). The real CU moves there are zero-copy accounts and constraint pruning — i.e., precisely the edits that silently remove owner/signer checks while the lifecycle tests still pass. Your anti-gaming layer can't tell a constraint that was redundant from one that was load-bearing. Add the broken `cargo build-sbf` toolchain (known, no CI gate) and a minutes-long .so rebuild per candidate, and it loses on every axis. Revisit only if you later add real on-chain compute (see §6).

## 2. Metric: wasmtime fuel — deterministic, instruction-weighted, no wall clock

- **Build stage:** `cargo build --release --target wasm32-wasip1 -p covenant-audit-kernel --features bench-bin` producing `kernel_bench.wasm` whose `main` reads the corpus, calls `verify_chain`, and writes a digest of the report to stdout (so the run can't be optimized away as dead code).
- **Measure stage:** a small prebuilt host runner (`agent-os/self-improvement/bench/runners/fuel-runner`, ~40 lines on the `wasmtime` crate): `store.set_fuel(CAP)`, invoke, `consumed = CAP − store.get_fuel()`. It prints `SCALAR <baseline_fuel / consumed_fuel>` given `--baseline N`. Built **once** as engine infrastructure, outside the worktree — zero per-candidate cost. (Nothing is installed yet: `rustup target add wasm32-wasip1` + the runner crate is the entire toolchain setup; no wasmtime CLI needed.)
- **Corpus:** one frozen binary corpus (~50k events: clean chains, tampered events, tampered anchors, malformed JSON, non-UTF8 bytes, length mismatches), generated by a seeded script and committed under the task's held-out `grade/` dir. Same wasm + same corpus → bit-identical fuel, every run. No iterations-vs-noise tuning, ever.
- **Score plumbing:** one new branch in `gradeStages` (run.mjs:105-107): `metric: "scalar"` parses `/^SCALAR ([\d.]+)/m` from stage stdout. `weights: {"scalar": 1}`. Metric is `baseline/candidate` fuel, so 1.0 = parity and your existing `margin 0.02` promotion gate literally means "≥2% fuel reduction". Scores above 1.0 are fine — the optimizer only compares relatively.
- Fuel subsumes an allocation-count metric (dlmalloc instructions are counted), so don't bother with a counting global allocator.

## 3. Correctness gate — four layers, all already idiomatic for your engine

1. **Existing suite (gate):** `cargo test -p covenant-audit -p covenant-audit-kernel --lib` in the worktree. This already pins the NIST sha256 vectors, the `\n` separator composition, `ZERO_CHAIN_HASH`, and tamper detection (lib.rs:1547-1676, 4042-4080) — the chain output format is frozen by tests the candidate may not touch.
2. **Held-out differential test (gate, copied in post-solve like your rustc graders):** `grade/` contains a test crate that vendors a frozen copy of the *original* kernel source as `reference_kernel.rs` and asserts, over ~30 adversarial corpora (empty, 1-event, truncated, duplicated lines, huge lines, invalid UTF-8, every tamper mode), that candidate output **structurally equals** reference output: `(valid, root_hash_hex, per-line pass/fail kinds)` — not failure-message strings, since legitimately replacing the `Value` parse changes serde's error text. Production keeps owning message formatting outside the kernel.
3. **Diff confinement (anti-gaming):** extend `gamingViolations` with a per-task `allowedPaths` allowlist — for this task, exactly `crates/covenant-audit-kernel/src/lib.rs` (the EVOLVE region). Any other changed file = score 0. This also blocks adding dependencies via Cargo.toml, which keeps the wasm build stable. `#![forbid(unsafe_code)]` sits above the EVOLVE markers, so "optimize by skipping UTF-8 checks unsafely" is structurally impossible.
4. **No memorization possible:** the fuel corpus and differential corpora live in held-out `grade/`, which the solver never sees — hardcoding outputs can't work.

## 4. Smallest engine adaptation — Track B reuses ~95% of what exists

The insight: your engine already evolves code — the scaffold solver leaves file changes in a worktree and the grader scores them. Track B only changes *what the prompt asks for* and *what the metric is*. Three small diffs:

1. **lib.mjs / run.mjs (~15 lines):** the `metric: "scalar"` parser, and `allowedPaths` enforcement in `gamingViolations`.
2. **New task** `bench/tasks/audit-kernel-fuel/` — `task.json` (base commit, the 4 grader stages: unit gate → differential gate → wasm-build gate → fuel metric), `prompt.md` ("reduce the fuel cost of the code between `// EVOLVE-BLOCK-START/END` in covenant-audit-kernel/src/lib.rs; do not change the public signature or observable behavior; tests pin exact hashes"), `grade/` (differential crate + corpora + frozen baseline fuel number).
3. **optimize-code.mjs (~60 lines, cloned from optimize.mjs):** same proposer/margin/ledger skeleton, two substitutions. The evolving artifact is the kernel file at `task.base` instead of `coder.md`; on promotion, commit the winning worktree diff to the track branch and **advance `task.base` to that commit** (hill-climbing) while the baseline fuel constant in `grade/` stays frozen at the original naive number so the metric stays monotone across versions. To extract the winner, run the bench with `--keep` and have the JSON report include the worktree path. Proposer prompt feeds the current EVOLVE block + last fuel number + rejection history instead of scaffold text. Archive every kernel version to the ledger exactly as Track A does.

Solver, worktree isolation, held-out copy-in, promotion gate, archive: all unchanged.

## 5. Cheap gradient validation before investing (the Track A `none=0 / unsafe=0.929 / overflow-safe=1.0` analogue)

Half a day, no optimizer runs, no model spend:

1. Build the kernel crate (mechanical extraction of existing code), the fuel-runner, the corpus generator. Freeze baseline fuel = current implementation's consumption.
2. Hand-write three variants and score each via `--solver cmd:"cp variants/X.rs crates/covenant-audit-kernel/src/lib.rs"`:
   - **Pessimized** (extra clones, `format!` everywhere, re-parse per line): expect scalar ≈ 0.5–0.8.
   - **Current** code verbatim: scalar = 1.0 by construction.
   - **30-minute hand-optimized** (incremental hasher, hex LUT, byte-held running hash — only the obvious rungs): expect scalar ≥ 1.2.
3. **Go/no-go:** if the 30-minute hand-opt gains less than ~10–15% fuel, the gradient is too shallow — abort this target. If it gains 20–50%, there's a ladder.
4. **Determinism check:** score the same variant twice; fuel must be bit-identical. (It will be unless the kernel uses HashMap iteration order — don't.)
5. **Depth check (one cheap model run):** one optimizer iteration with the strong model. If its first candidate already matches your hand-opt, remaining depth = whatever lies past it (JSON scanner, etc.); if it lands at hand-opt and the *second* iteration finds more, the loop is live.

## 6. The brutally honest part

- **Confirmed no-surface, don't waste time:** covenant-identity (every crypto op is a thin ed25519-dalek call; the only custom code is key-file permission checks), borsh paths (derive-only, three tiny DTOs), budget accounting (~30 LOC of branch-free O(1) token-bucket math; the real cost is two mutexes and JSONL fsync — design tradeoffs, not an optimizable scalar).
- **On-chain is currently a mirage:** deterministic CU metering is the perfect AlphaEvolve metric *in general*, but covenant's programs deliberately store hashes instead of computing them, so there is almost no user compute to optimize — and the gradient that does exist (Anchor overhead) is adjacent to security-check deletion. Plus the settlement build is broken and ungated in CI.
- **The audit kernel is real but finite.** It's ~50–80 LOC of genuinely allocation-heavy, instruction-optimizable code with a frozen behavioral contract — the best target this repo offers — but expect Track B to saturate after single-digit-to-low-double-digit promotions, because the kernel is small and strong models are good at micro-optimization. That's still categorically better than Track A's zero-step gradient, and it fully exercises the code-evolution machinery (diff confinement, scalar metrics, differential gating) you'll want for any future target.
- **What WOULD have a deep gradient:** (a) *on-chain merkle proof verification for receipt batches* — today `anchor_receipt_batch` stores `merkle_root` unverified; an instruction that actually verifies inclusion proofs on-chain would put a hash loop under a hard 200k-CU budget, where syscall-vs-manual-sha256, proof layout, and account-packing choices create a deep, economically meaningful CU gradient. But that's new launch-sensitive feature work, not optimization of existing code. (b) Off-repo algorithmic kernels (scheduling heuristics, matrix/packing kernels) — the actual AlphaEvolve domain, with effectively unbounded ladders, at the cost of no longer improving covenant itself. If the goal of this loop is the *loop* (self-improvement machinery that demonstrably climbs), audit-kernel-fuel is the right next move; if the goal becomes *unbounded* gradient, plan on (a) after launch or (b) outside the repo.

Suggested build order: kernel extraction + fuel-runner + corpus (½ day) → §5 validation (½ day) → go/no-go → `optimize-code.mjs` + first real iterations only if the hand-opt separation clears 15%.
