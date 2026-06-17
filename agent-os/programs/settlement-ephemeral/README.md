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

## Live spike — RUN AND VERIFIED on devnet (2026-06-16)

The ER build is deployed to `cov9UDyp…` on devnet (an upgrade; authority is the
local `id.json`) and `spike/run-spike.mjs` ran the full flow end to end:

- delegate on L1 → N×consume in the EU ER validator → undelegate → reconcile on L1.
- **N=100, amount=2: reconciled exactly** (4,999,999,995 → 4,999,999,795). Earlier
  N=5 run also exact.
- **75.5 ms/op** confirmed for the ER consumes (vs ~400–800 ms L1 confirmed).
- **Whole session cost: ~0.000305 SOL** — the delegate + commit + session overhead.
  The 100 consumes themselves are gasless, so the cost is ~constant in N: thousands
  of metered consumes per session cost the same fixed ~0.0003 SOL. That is the
  unit-economics result we wanted.

The harness uses hand-encoded instructions (anchor global discriminators + the exact
`#[delegate]`/`#[commit]` account order) and the MagicBlock JS SDK only for the
delegation PDAs, so it needs no generated IDL. `check.mjs <owner>` dumps current
config/credit state. Re-run: `N=100 AMOUNT=2 node run-spike.mjs`.

Original spec of the flow that `spike/run-spike.mjs` drives:

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

## x402-over-ER demo (Phase 2) — RUN AND VERIFIED on devnet (2026-06-16)

`spike/x402-demo.mjs` is the flagship: a paid HTTP endpoint where each call is
settled by a gasless `consume_credits` in the ER instead of an on-chain SPL
transfer. It runs a real HTTP 402 boundary plus a verifying facilitator, end to end
against the live devnet ER.

- facilitator returns **402 + {amount, nonce}** until a valid `x-payment` header is
  presented, then **200 + content**.
- the agent settles on 402 by running `consume_credits(amount, sha256(nonce))` in the
  ER; the `receipt_hash` binds the payment to that specific request.
- the facilitator verifies the signature **on the ER**: right program, right credit
  account, `amount >= price`, `receipt_hash == sha256(nonce)`, and signature-unused
  (anti-replay). No SPL transfer, no per-call L1 fee.

Session model: delegate once → serve K gasless paid calls → undelegate → reconcile.
Verified run (K=5): `402 → ER pay → 200` at **~71 ms/call steady-state**, ER balance
moved by exactly K, **exact L1 reconciliation** after undelegate. This is
agent-to-agent pay-per-call at a price mainnet can't touch, demonstrated live.

Run: `K=5 PRICE=1 node x402-demo.mjs`.

Production path — BUILT and live-verified. `EphemeralSigner` in the `covenant-x402`
crate implements the `Signer` trait by metering: `build_payment()` submits a
`consume_credits` to the pinned ER validator and returns the signature envelope, so
the daemon's `pay_and_record()` path works unmodified. Gated by the crate's `solana`
feature, no on-chain ER SDK dep. Unit + wiremock tested, and live-verified against
devnet-eu via `cargo run -p covenant-x402 --features solana --example ephemeral_live`
(delegate with `spike/er-session.mjs delegate` first): it metered 3 credits in the
ER, balance dropped by exactly 3, reconciled to L1 on undelegate.

The verifier counterpart is the `covenant-x402-facilitator` crate, and the full
loop is wired into the daemon:
- **Sidecar:** `covenant-x402-er-signer` (in `covenant-x402`, `solana` feature) is
  the binary the daemon spawns — stdin `PaymentRequirements` → `consume_credits` in
  the ER via `EphemeralSigner` → stdout `x-payment` header.
- **Daemon selection:** `X402Config::signer_for(network)` routes `solana-er:*`
  networks to the ER sidecar (env `COVENANT_X402_ER_SIGNER_BINARY` +
  `COVENANT_X402_ER_{KEYPAIR,PROGRAM,RPC}`), everything else to the default SPL
  signer. The funding key stays in the sidecar's address space, never the daemon's.
- **Provider registration:** an operator registers ER-settled providers via
  `COVENANT_X402_ER_PROVIDERS`, a JSON array of
  `{slug, endpoint, per_call_cap, [method, network, asset, credits, description]}`
  (only slug/endpoint/per_call_cap required; defaults to a GET on `solana-er:devnet`
  in `credits`). Each registered provider is advertised to agents as an `er.<slug>`
  tool. An agent calls it with `CallTool { name: "er.<slug>" }` — gated by the
  `tool.call.er.<slug>` capability — which routes through the same x402 pay path and
  records a settlement receipt. Example:
  `COVENANT_X402_ER_PROVIDERS='[{"slug":"quote","endpoint":"https://host/paid","per_call_cap":1}]'`.

Both daemon paths are live-verified on devnet (`#[ignore]` tests in `covenantd`):
`pay_x402_settles_through_the_er_signer_live` (the raw op) and
`er_provider_tool_settles_in_the_er_live` (an agent calling a registered `er.*` tool).

> Status: program + artifact verified; the live ER reconciliation ran (exact, 75.5
> ms/op, ~0.0003 SOL/session); and the x402-over-ER pay-per-call demo ran end to end
> (exact reconciliation, ~71 ms/call, gasless). All numbers are from real devnet runs.
