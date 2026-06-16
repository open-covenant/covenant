# settlement-ephemeral — MagicBlock ER build of the Covenant settlement program

This crate is the **Ephemeral Rollup build** of the settlement program. It compiles
`../settlement/src/lib.rs` verbatim (single source of truth) with the `ephemeral`
feature on, which adds the credit-account delegation lifecycle:

- `delegate_credits` — delegate `[b"credits", owner]` to the MagicBlock delegation
  program. Pass an ER validator pubkey as the first remaining account to pin it.
- `commit_credits` — checkpoint the delegated balance to L1 without releasing.
- `undelegate_credits` — commit final balance and return the account to L1.
- `#[ephemeral]` injects the L1 `process_undelegation` callback.

Everything else (token custody, staking, slashing, governance) is unchanged and
stays on L1. Only the program-owned `u64` credit balance moves to the ER.

## Why a separate crate

`ephemeral-rollups-sdk` (via `magicblock-delegation-program-api`) forces the solana
runtime crates to 2.2.20, which is incompatible with the workspace's `litesvm 0.6`
test deps (pinned at 2.2.1, pre the `solana-feature-set` → `agave-feature-set`
rename). Keeping this crate **out of the agent-os workspace** (`[workspace].exclude`)
gives it its own `Cargo.lock` so the ER graph never perturbs the L1 program's tests.

Two consequences of the isolated lock, both handled here:
- `ephemeral-rollups-sdk` must keep **default features** (the `solana-system-interface`
  crate is referenced unconditionally in its `utils.rs`) plus `anchor-compat`.
- `anchor-compat` lets `anchor-lang` resolve to 0.32.1 for the SDK while our program
  wants 0.31.1 → two versions → trait mismatch. Fixed by pinning anchor-lang to a
  single 0.31.1 in this crate's lock (`cargo update -p anchor-lang --precise 0.31.1`).

## Build (verified)

```bash
# from this directory
cargo update -p anchor-lang --precise 0.31.1   # one-time, collapses anchor-lang to 0.31.1
cargo build-sbf                                 # -> target/deploy/covenant_settlement_program_er.so
```

Verified locally with anchor 0.31.1 + agave (cargo-build-sbf), solana-program 2.2.1,
ephemeral-rollups-sdk 0.15.5. Artifact ~693 KB (vs ~513 KB for the L1-only build).

The L1-only program is unchanged: build it and run its tests from `../settlement`
(`cargo build-sbf`, `cargo test -p covenant-settlement-program`). Both stay green.

## Endpoints (MagicBlock devnet, perpetual public infra)

| | URL |
|---|---|
| Magic Router (auto-route ER vs L1) | `https://devnet-router.magicblock.app` (+`wss://`) |
| L1 | `https://api.devnet.solana.com` |

Pin an ER validator by passing its pubkey as `delegate_credits`' first remaining
account (EU is closest): `MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e`.
Others: US `MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd`, Asia
`MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57`, TEE
`MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo`.

Fee schedule: 0.0001 SOL/commit to L1, 0.0003 SOL/ER session (charged at undelegate).

## Live spike (the remaining manual step — needs a funded devnet keypair)

This is the one thing not yet run: it requires deploying the ER artifact to devnet
and a funded payer. The flow, which `spike/run-spike.mjs` drives, is:

1. **L1 setup**: `initialize` (if needed), `open_credit_account`, `buy_credits` so the
   owner has a known starting balance `B`.
2. **Delegate**: send `delegate_credits` on **L1** (Magic Router routes it), pinning
   the EU validator. The `[b"credits", owner]` PDA is now ER-owned.
3. **Meter in the ER**: send N × `consume_credits(amount, receipt_hash)` through the
   **Magic Router** — these land in the ER, gasless, ~ms latency.
4. **Commit**: `commit_credits` (optional mid-run checkpoint).
5. **Undelegate**: `undelegate_credits` → commits the final balance and returns the
   account to L1.
6. **Reconcile**: read the credit account on **L1** and assert
   `balance == B - N*amount`. Exact reconciliation is the success criterion.

Deploy + run:

```bash
solana program deploy target/deploy/covenant_settlement_program_er.so \
  --program-id <er-program-keypair.json> --url https://api.devnet.solana.com
cd spike && npm i
PROGRAM_ID=<deployed id> PAYER=<keypair.json> COVNT_MINT=<mint> \
  ROUTER=https://devnet-router.magicblock.app L1=https://api.devnet.solana.com \
  N=1000 AMOUNT=1 node run-spike.mjs
```

The client uses the program IDL (run `anchor build` once to emit it — the
`#[delegate]`/`#[commit]` macros add their accounts to the IDL) and the MagicBlock
JS SDK for the delegation PDAs + `GetCommitmentSignature`. See `spike/run-spike.mjs`.

> Status: program + artifact verified; the live ER reconciliation is unrun pending a
> funded devnet deploy. Do not report the latency/cost numbers until this runs.
