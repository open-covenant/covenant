# Plan: Making PAK Substantial Enough For Toly To Engage

Status as of 2026-05-28. Branch `feat/percolator-keeper` is at 12 commits
ahead of `main` with ~5,000 lines of code + tests. That's a credible
v1 sidetrack. To turn it into something Toly actually engages with,
we need to close the gap from "well-built crate" to "deployable,
formally-reasoned, behaviorally-tested artifact that demonstrates
deep engagement with his protocol."

This is multi-day work, not multi-hour. Below is the prioritized
execution plan. Items are ranked by **(impact on Toly noticing) ×
(feasibility this week)**.

---

## What Toly actually values (from the recon)

His commit pattern + writing reveal what he engages with:

1. **Code that reads his code carefully.** His Bounty 5/6 docs spell out
   non-bug surfaces ("known intentional surface" list); engaging with
   THOSE specifically signals you've read.
2. **Formal verification or behavioral testing that closes gaps.**
   The Kani audit explicitly lists "no proof exercises `execute_trade`
   end-to-end". Filling that = direct value.
3. **Reduction of trust surface.** Every commit in the last 30 either
   removes a privileged input (`f01b77f` internal funding), tightens an
   account check, or hardens an invariant. A keeper layer with a
   slashable bond fits this pattern exactly.
4. **Adversarial validation.** His bounty is "drop the insurance fund";
   the win condition is invariant violation. A stress harness that
   exercises edge cases at scale, even without finding a bug, signals
   you understand the model.
5. **Working artifacts on his current binary.** Bounty 6 is `4m3ipBQ…`
   on mainnet right now. Code that runs against it (not a fork, not a
   mock) is unambiguous proof of engagement.

## Where we currently fall short

Honest gap analysis vs Toly's bar:

| What we have | Toly's bar |
| --- | --- |
| Off-chain bond verifier with proptest | A deployable `.so` he can `solana program deploy` |
| Wire-locked instruction builders for his IXs | A `RealPercolator` that decodes his ACTUAL on-chain accounts |
| MockPercolator-only integration tests | Tests that run against the real program binary via solana-program-test / LiteSVM |
| Multi-keeper coordination via deterministic hashing | A liveness simulator that produces quantitative SLA numbers |
| Recovery `KeeperPolicy::decide` that emits ForfeitRecoveryLeg | A formally-specified liquidation policy with proven termination |
| Conceptual paper on permissionless accountable keepers | A polished writeup he'd reference, with numbers + diagrams |
| Bond program as host-tested simulation | A real on-chain program with banks_client integration tests |

We close ~6 of 7 gaps in this plan.

---

## Phase 10 — Deployable on-chain bond program (`.so`)

**Goal:** Turn `covenant-percolator-bond/src/program.rs` from host-tested
simulation into a real SBPF program that builds with `cargo build-sbf`
and runs in `solana-program-test` against a banks_client. Output: a
`.so` artifact + a CI-style integration test that exercises the full
bond lifecycle on-chain.

**Why this moves the needle:** Right now the bond is a Rust library
with a stub `process_instruction`. Toly opens the crate and sees host
tests, not on-chain coverage. A real SBPF program with banks_client
tests turns the bond into something he could deploy and verify himself.

### Concrete deliverables
1. New crate `crates/covenant-percolator-bond-program/`:
   - `Cargo.toml` with `crate-type = ["cdylib", "lib"]`
   - `src/lib.rs` implementing the full `process_instruction` dispatch
     using `solana_program` directly
   - Uses our existing `covenant-percolator-bond` library for state +
     verifier; the program is a thin SBPF wrapper
2. Real PDA derivation in `process_instruction`:
   - Bond PDA `[b"bond", keeper.as_ref()]`
   - Slash-receipt PDA `[b"slash", bond.as_ref(), receipt_id.as_ref()]`
   - Both validated via `Pubkey::create_program_address` (correct bump
     enforcement)
3. CPI to System program for:
   - Account creation on initialize (rent-exempt sizing)
   - Lamport transfers on deposit / withdraw / slash
4. `tests/bank_lifecycle.rs` using `solana-program-test`:
   - Boot a `BanksClient` with our `.so` loaded
   - End-to-end: initialize → deposit → slash → verify recipient holds
     the lamports
   - Including the negative case: foreign-operator pause attempt fails
5. `cargo build-sbf` produces `target/sbpf-solana-solana/release/covenant_percolator_bond.so`
6. Add `make build-program` / `make test-program` to the worktree
   Makefile or scripts/

**Estimate:** ~6-10h of careful work. The SBPF entrypoint dispatch +
account validation is mechanical but the integration tests need to
match real on-chain semantics (rent, sysvars, system-program CPI).

**Risk:** The Solana SDK version in our workspace (`sdk = "2"`) needs
to match what cargo-build-sbf installs. If there's a mismatch we
might need a separate workspace for the SBPF crate.

---

## Phase 11 — `RealPercolator` decoding mainnet accounts

**Goal:** Implement `PercolatorClient` against actual mainnet RPC.
Read his market/portfolio accounts via `getAccountInfo`, decode using
his struct layouts from `v16_program.rs`, populate `MarketState` and
`PortfolioSnapshot`.

**Why this moves the needle:** Currently `MockPercolator` is the only
client. A `RealPercolator` against Bounty 6 proves we've read his
account binary layouts byte-for-byte. It also unblocks running the
keeper against his live program.

### Concrete deliverables
1. Study `v16_program.rs` (11,027 lines) for:
   - The market group account struct layout (he uses zero-copy +
     bytemuck-style POD with explicit padding)
   - Per-asset state slot layout within the market group
   - Portfolio account layout
   - Account discriminators (if any) — check for first-byte tag
     conventions
2. New module `covenant-percolator/src/realclient.rs` gated by
   `--features solana-rpc`:
   - `RealPercolator` implementing `PercolatorClient`
   - Async `RpcClient::get_account_data` for market reads
   - Zero-copy decode into mirrored Rust structs (we own them, locked
     by golden bytes against a snapshot of his mainnet account)
   - `list_portfolios` via `getProgramAccounts` with a memcmp filter
     on the portfolio discriminator
3. A golden-bytes test that pins our decoder against a captured
   snapshot of the live mainnet market `BhkMic5g…` (download once,
   check into the repo, decode and assert expected values)
4. An `--rpc-url` CLI flag on the keeper binary that switches it from
   MockPercolator to RealPercolator

**Estimate:** ~6-8h. Most of it is patient reading of his account
layouts to mirror them correctly. The RPC plumbing is straightforward.

**Risk:** His struct layouts may shift between commits. We pin to his
HEAD as of today (`065a8f0`) and lock our golden bytes against that
specific account state.

---

## Phase 12 — Formal liquidation-sequencing policy

**Goal:** A precise, proven liquidation policy that consumes
`HealthCertV16`s from N portfolios and produces a strict total order
of recovery actions with bounded blast radius. Reference impl + spec
comment block in TLA+-style invariants.

**Why this moves the needle:** Toly's recon item #2: "Multi-asset
liquidation policy" — he hasn't formalized it. His keeper docs
explicitly defer this. A formal policy with proven termination +
bounded-step settlement is something he can review against his §21
preemptible-close language.

### Concrete deliverables
1. New module `covenant-percolator/src/liquidation.rs`:
   - `LiquidationPolicy::sequence(certs: &[HealthCertV16]) -> Vec<KeeperAction>`
   - Strict total order: sort by `(certified_liq_deficit DESC, account_pubkey ASC)`
     so any two keepers compute the same order (matches §21's "strict total order")
   - Per-step `b_delta_budget` bounded by `cert.certified_liq_deficit`
     (we don't ask for more than the engine certified as needed)
   - Termination proof comment: each step reduces aggregate deficit
     monotonically (engine's job, but our policy doesn't add deficit)
2. Properties locked by proptest:
   - **L1** Determinism: same input → same output (already true by
     sort)
   - **L2** Total-deficit-bounded: `sum(b_delta) <= sum(certified_deficits)`
   - **L3** Per-account-bounded: `b_delta[i] <= cert[i].certified_liq_deficit`
   - **L4** Stale-cert exclusion: `cert.valid = false` → no action for
     that account (§16 fail-closed)
   - **L5** Order-independence of inputs: shuffling the input list
     produces the same output sequence (commutative)
3. Spec block in module-level doc engaging §21 ("strict total order,
   no hold-and-wait or equal-priority livelock") quoted verbatim and
   showing our implementation matches each clause

**Estimate:** ~4-6h.

---

## Phase 13 — Adversarial keeper-network stress harness

**Goal:** Take inspiration from `percolator-stress-test`. Build a sim
that spawns N keepers (configurable: 1, 3, 10, 50) with different
latencies, partitions, and one optional hostile actor. Measure
freshness SLA, coordination efficiency, and slash detection rate.

**Why this moves the needle:** Quantitative results — "with 5 honest
keepers + 1 hostile, 99.7% of stale assets are freshened within 2
slots and the hostile keeper's bond is slashed within 3 slots" — is
the kind of empirical claim Toly engages with.

### Concrete deliverables
1. New integration crate or test module
   `covenant-percolator/tests/network_sim.rs`:
   - Async sim with N `KeeperAgent`s running against a shared
     `MockPercolator` (Arc'd)
   - Per-keeper latency sampled from a configurable distribution
   - Adversarial mode: one keeper has a hostile policy that emits
     out-of-scope actions; assert its bond is slashable
   - Partition mode: keepers can be put in "blackout" for N ticks
2. Metrics:
   - Time-to-freshness (slots from staleness → first PushAuthMark)
   - Coordination-deferral rate (how many redundant submissions
     avoided)
   - Slash precision (did the verifier catch every actual violation?)
3. Output: structured JSON results suitable for plotting

**Estimate:** ~4-6h.

---

## Phase 14 — LiteSVM/banks_client integration on his program

**Goal:** Run our `RealPercolator` against an in-process Solana bank
loaded with `percolator-prog`'s actual `.so` (either built from his
HEAD or downloaded from mainnet). Demonstrate that our keeper +
sender can drive his program end-to-end through a real crank cycle.

**Why this moves the needle:** This is the "I built your keeper, here
it is operating your program" demo. Toly clones the repo, runs the
test, and watches our keeper crank his program in-process.

### Concrete deliverables
1. New integration test
   `covenant-percolator/tests/lite_svm_e2e.rs`:
   - Boot LiteSVM (or `solana-program-test`) with `percolator-prog`'s
     .so loaded
   - Initialize a v16 market with our admin keypair
   - Make a stale asset
   - Run our keeper's tick loop with `RealPercolator` pointed at the
     in-process bank
   - Verify the PushAuthMark + CrankAsset actually landed on-chain
     (read `asset.last_mark_slot` post-tick)
2. CI feasibility: ideally runs in <30s

**Estimate:** ~6-8h. Depends on getting his .so to build or finding a
prebuilt artifact.

**Risk:** His program may need specific feature flags or setup we don't
yet understand. Could need to clone his integration test patterns.

---

## Phase 15 — Polished technical writeup

**Goal:** Convert `paper/permissionless-accountable-keepers.md` into a
polished standalone artifact suitable for Toly to read and reference.
Add diagrams (ASCII or simple), concrete numbers from Phase 13's
stress harness, and a side-by-side comparison with his proximity-based
keeper at `9WiMAQtdx8…`.

**Why this moves the needle:** A polished writeup is the artifact he
can share. Without it, the code is buried in a repo.

### Concrete deliverables
1. Restructured paper with:
   - Threat model section (with adversaries enumerated)
   - Architecture diagram (ASCII boxes-and-arrows)
   - Quantitative comparison table populated from Phase 13 numbers
   - Reference to Phase 10's deployable .so
   - Reference to Phase 11's RealPercolator
   - References to specific lines in `v16_program.rs` / `kani_audit.md`
2. README in `~/Projects/covenant-percolator-keeper/` (the worktree)
   with a one-paragraph elevator pitch, quickstart commands, and links
3. Possibly: a `BOUNTY_NOTES.md` documenting how PAK interacts with
   Bounty 6 — what scope shape we'd recommend operators sign

**Estimate:** ~3-4h.

---

## Phase 16 (optional) — Draft proposal to percolator-prog

**Goal:** Open a draft issue or PR on `aeyakovenko/percolator-prog`
proposing a specific protocol-side change that would help the keeper
network. Concrete, narrow, low-risk.

**Candidate:** Expose a public account field `next_due_slot[asset_idx]`
that surfaces deterministically when each asset is next due for crank.
With this, the keeper coordination key includes a "next due hint" and
keepers don't even compute their own staleness — they read it. This
trims keeper compute and gives the network a single source of truth.

Or: A `PermissionlessFinalize` IX that batches `RecoveryFinalize`
across all eligible (asset, side) pairs in a market in one tx, so the
last-leg cleanup doesn't need per-leg keeper coordination.

**Why this moves the needle:** Direct engagement on his repo is the
strongest possible signal. If he merges or even comments, the loop
is closed.

**Estimate:** ~2-3h to research + write the proposal carefully. Risk:
he disagrees with the change.

---

## Execution order

Aiming for the most-impact-per-hour first, with feasibility checks at
each step. If I hit a blocker on one phase, I move to the next.

1. **Phase 10** (SBPF .so + banks_client integration) — biggest leap
   from "library" to "artifact"
2. **Phase 11** (RealPercolator decoding mainnet) — proves we read his
   account layouts; unblocks Phase 14
3. **Phase 12** (Liquidation policy with formal sequencing) — engages
   his §21 language directly
4. **Phase 13** (Adversarial keeper-network stress harness) — produces
   quantitative results
5. **Phase 14** (LiteSVM e2e on his program) — the "watch our keeper
   drive your program" demo
6. **Phase 15** (Polished writeup) — packaging
7. **Phase 16** (Draft PR proposal) — final signal

Each phase commits independently. Tests + clippy gates on every commit.
