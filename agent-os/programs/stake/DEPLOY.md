# covenant-stake — deploy runbook

Program ID: `CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED`
Keypair: `~/.config/solana/covenant-stake-program.json`

## Devnet status — LIVE

Deployed and initialized on devnet 2026-05-29. End-to-end smoke confirms the
.so behaves identically to the litesvm suite on a real validator.

| Artifact | Address |
|---|---|
| Program | `CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED` |
| Config PDA | `CNrBUGqrdj5WDTqfBPwyzURBmVThTfWTejqxSqme8EyC` |
| FeeRouter PDA | `x5dMA3DariqtYRc9XMkGhPTMWiXQRjAFZTS9QZLif33` |
| RewardVault PDA | `Bh3YKatgy4Sug1g24uFMHrgTQJ8tPFNqKQUqp4sPd4pn` |
| locked_vault_authority PDA | `BfKVwAzAGpQnwWe5vTkHDnYUKsnjWR4F7CpZcUbgeFrz` |
| buylock_vault_authority PDA | `D9ceemTHkfha5QPgnLjxnaTCtdNLvuPo3fP5g6kZTJK5` |
| Test $CVNT mint (SPL legacy, 6 dec) | `12zLnQiqHLosp4GpAG4b1ZyrcyHJK8863FiDcQZ5Drmd` |
| Locked-CVNT vault | `EJTKsop7y9hoZupZhxu51sWCDsabKubdwxenmC7yEfEd` |
| Buy-and-lock vault | `Drh3yZYHDekF1i3PFLH1JcVUkcNeJsc51YD6bvL8vy3j` |
| Authority + pause + fee_router (smoke) | `8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM` |

Smoke transactions (initial v0 deploy + lifecycle):
- Deploy: `45gtgGHfUEuB6nBtMfVSfAGQt2EqQoya2kXYWypCEBGuFpNQtGtzEWRjB6kXtxWCFpx9C6YLxfMBgK73jgxHFwAW`
- Initialize: `4htTj2GMbdQ1oEh1JDkbDtnseNrzj3C3juZBMUwidXWv6neUSvREmo1Hb3noWZXTWzJdJxbNUPP1VtHVsCA6tQ1W`
- create_position (nonce=1): `54qSP8S4vo6fwWccAk4RPTfy5ej24XQXsRrtMXnt8BcnzVKGJBZ4n8qCy96MW76FWvbtMjZptgFGkMANYAPYhjoA`
- deposit_sol_fees: `25ub2vt6ekhYMJsHjrgdqZW1xiysuuYSZexC678hkmrNF2i6QzfdjhprjrX7fCKyTsxZY8nCc1Y2F1MpVoYNZAW3`
- claim: `3t1v5FCfNNfEjZsXzozruQC3r2GdAmgsmEUgs63Ef41UUmHJ7DfBBGNUYhucP1AGDKs1yQB8b1CTKSFckQuBAMBt`

Audit-fix upgrade + 2-locker pro-rata validation:
- Upgrade: `2sbzRCPhJjE4hhtwRUzKKFUz7wNxeuFC2XNthgVyBqRLobsiYotByQrQ995h4Q68a5sn9E9qq3XSWnruXtdehsUF`
- create_position (nonce=2): `3PDG2Q8jJXPoVRrW6F9aFEFemytmKoSki8KcPAbWeCvQXZgv76vQSQX9JErEqUAXCg1eF9biUfLpD2DZt1BA9y3W`
- deposit_sol_fees (1M lamports): `4EiZxnFbBeTsrPBGFUKRDaXb3wocf1neJipeSCNCyPdikh46U2HTC2qqB52fooLLTJ34JSmxfb6ViJBuiLrQgvU8`
- claim: `5YgdDmSFeLrd2kRbeyzswu86NUBUqgmYcvrFMwcLQu2QShnurkGu7QHt4pVQbuPw4cKGzH2AyD8MNe5rF7smp6iq`

First smoke: single locker absorbed 1_000_000 lamports of a 1_000_000-lamport
deposit exactly. Second smoke after audit upgrade: two lockers with equal
weight split 1M lamports 500k/500k, matching the accumulator math.

Reproduce the smoke locally with:

```bash
cd agent-os
cargo run --example devnet_smoke -p covenant-stake-keeper -- pdas
# create mint + vaults via spl-token CLI, then:
cargo run --example devnet_smoke -p covenant-stake-keeper -- initialize <mint> <locked_vault> <buylock_vault>
cargo run --example devnet_smoke -p covenant-stake-keeper -- smoke <mint> <locked_vault>
```

This program is excluded from `cargo test --workspace` per `agent-os/scripts/validate.sh`. Build and test it directly with `cargo build-sbf` and `cargo test -p covenant-stake-program`.

## Prerequisites

- Solana CLI 2.x and Anchor 0.31.1 on PATH.
- Upgrade-authority keypair funded with at least 3 SOL on the target cluster.
- Mint authority is **already renounced** on `$CVNT` (mainnet mint `2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump`). The staking program does not mint, but the locked-CVNT vault and BuyLockVault token accounts must be pre-created by the deployer with the correct PDA authorities.
- Treasury and subsidy recipient pubkeys decided. Defaults expect a Squads vault for treasury and a hot operator wallet for subsidy.

## Build (reproducible)

```bash
cd agent-os
cargo build-sbf --manifest-path programs/stake/Cargo.toml
# OR for verified mainnet build:
solana-verify build --base-image solanafoundation/solana-verifiable-build:3.1.14 -- --package covenant-stake-program
```

Artifact: `agent-os/target/deploy/covenant_stake_program.so` (~430 KB).

## Devnet

```bash
solana config set --url https://api.devnet.solana.com
solana program deploy \
    --program-id ~/.config/solana/covenant-stake-program.json \
    agent-os/target/deploy/covenant_stake_program.so
solana program show CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED
```

Verify the program data hash matches the locally-built `.so` SHA-256 before continuing.

## Pre-`initialize` setup

```bash
# Compute the locked-CVNT vault authority PDA, then create its ATA.
LOCKED_AUTH=$(... find_program_address [b"vault_auth"] ...)
BUYLOCK_AUTH=$(... find_program_address [b"buylock_auth"] ...)
COVNT_MINT=2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump  # mainnet; on devnet use a fresh Token-2022 mint

spl-token create-account $COVNT_MINT --owner $LOCKED_AUTH --fee-payer ~/.config/solana/id.json
spl-token create-account $COVNT_MINT --owner $BUYLOCK_AUTH --fee-payer ~/.config/solana/id.json
```

Record the resulting ATA addresses; both must be passed to `initialize`.

## `initialize`

Use the SDK builder `prepareStakeInitializeInstruction` (from `@covenant/sdk`) or the equivalent Rust ix builder. Required args:

| Arg | Recommended value |
|---|---|
| `pause_authority` | Single hot key on a separate host (operator) |
| `fee_router_authority` | The creator wallet (`2JXuvXb6...`) so the same key signs sweep + deposit |
| `min_lock_amount` | `1_000_000_000` (1000 $CVNT @ 6 dec) |
| `fee_router_max_deposit_lamports` | `5_000_000_000` (5 SOL/call cap) |
| `fee_router_rate_limit_secs` | `60` |

The Config PDA is `[b"stake_config"]`, the FeeRouter PDA is `[b"fee_router"]`, and the RewardVault PDA is `[b"reward_vault"]`. All three are init'd by this single call.

## Post-deploy verification

```bash
# 1. Config fields look correct.
solana account $(derive [b"stake_config"]) | xxd -l 256

# 2. FeeRouter has the right authority.
solana account $(derive [b"fee_router"]) | xxd -l 256

# 3. Reward vault is rent-exempt and program-owned.
solana account $(derive [b"reward_vault"])
```

Smoke test from a fresh user wallet:

1. Create a $CVNT ATA for the user, fund with > `min_lock_amount`.
2. Call `create_position` with `lock_tier_bps=10_000`, `nonce=1`, `amount=1_000_000_000`.
3. Have the keeper run a single `sweep` cycle (or manually call `deposit_sol_fees` with a small amount).
4. Call `claim` on the position; verify the SOL received matches expectation.
5. Advance clock past 30d (devnet only — mainnet just waits) and `close_position`. Verify principal returns.

## Keeper bring-up

Run the keeper as a separate Render worker or systemd unit:

```bash
export COVENANT_STAKE_KEEPER_RPC_URL=https://api.devnet.solana.com
export COVENANT_STAKE_KEEPER_CREATOR_KEYPAIR=$HOME/.config/solana/covenant-creator-wallet.json
export COVENANT_STAKE_KEEPER_TREASURY_RECIPIENT=<Squads vault SOL address>
export COVENANT_STAKE_KEEPER_SUBSIDY_RECIPIENT=<operator subsidy wallet>
export COVENANT_STAKE_KEEPER_DRY_RUN=1   # start dry, observe one sweep cycle
covenant-stake-keeper run
```

After verifying the dry-run logs print the expected split, set `COVENANT_STAKE_KEEPER_DRY_RUN=0` and let it run. The first real sweep moves SOL on chain.

## Mainnet path (live solo, no external audit gating)

Audit fixes already applied (B1/B2/B3 blockers + H1/H3/H4 + M4/M8) — see commit `3de4711f`. The remaining gate is operational, not security: run a tight deploy sequence so the program is initialized, the genesis position is open, and the keeper is observing before any production SOL flows.

### Pre-flight (5 min)

```bash
# 1. Confirm wallet + balance
solana config set --url mainnet-beta
solana balance  # need ≥ 3 SOL on the deployer

# 2. Confirm the creator wallet keypair is on disk and matches the pump.fun creator
solana-keygen pubkey ~/.config/solana/covenant-creator-wallet.json
# expected: 2JXuvXb6Q5YREk9KmhtgNmseq2aKtYnu5zLRi2i5Vaeb

# 3. Print PDAs you'll need
cargo run --example mainnet_bootstrap -p covenant-stake-keeper -- pdas
```

### Deploy the program (5 min)

```bash
cd agent-os
cargo build-sbf --manifest-path programs/stake/Cargo.toml
solana program deploy \
    --program-id ~/.config/solana/covenant-stake-program.json \
    target/deploy/covenant_stake_program.so
solana program show CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED
```

### Vault setup (5 min)

```bash
cargo run --example mainnet_bootstrap -p covenant-stake-keeper -- vault-setup
# prints two spl-token commands; run each in a fresh terminal and record the ATA addresses
```

### Initialize (5 min)

```bash
# Decide pause_authority — separate device from deployer; can be the same wallet
# imported into Phantom on your phone for fast-reach.
cargo run --example mainnet_bootstrap -p covenant-stake-keeper -- initialize \
    <locked_vault_ata> <buylock_vault_ata> <pause_authority_pubkey>
```

This sends `initialize` with the audit-recommended conservative parameters:

| Param | Value | Why |
|---|---|---|
| `min_lock_amount` | 10_000 CVNT | High enough that lock-cap squat needs real capital |
| `fee_router_max_deposit_lamports` | 0.5 SOL | Caps the per-deposit blast radius (B2 belt-and-suspenders) |
| `fee_router_rate_limit_secs` | 60 | Standard cooldown |
| `fee_router_authority` | creator wallet pubkey | So the same key signing the pump.fun fee claim also signs `deposit_sol_fees` |
| `pause_authority` | (your hot pause key) | Fast-reach if anything looks off |
| `authority` | (deployer keypair) | Held by you for v1; rotate to Squads when ready |

### Genesis position (5 min)

```bash
cargo run --example mainnet_bootstrap -p covenant-stake-keeper -- genesis-position
```

Opens a 1k-CVNT position at the 30d tier from the deployer wallet. Two reasons:
1. `total_weight > 0` immediately, so the keeper's first `deposit_sol_fees` doesn't trip the B2 guard.
2. Public "founder is in the pool" signal.

The CVNT for the genesis position must already be in your deployer's ATA. If not, transfer from Phantom first.

### Frontend production env (5 min)

In Vercel for the `landing` project, set:
```
NEXT_PUBLIC_SOLANA_CLUSTER=mainnet-beta
NEXT_PUBLIC_SOLANA_RPC_URL=<your Helius mainnet RPC URL>
```

Redeploy. The `/stake`, `/positions`, `/treasury` routes now point at mainnet.

### Keeper bring-up — DRY_RUN observation (24h)

Deploy the keeper to Render via `agent-os/crates/covenant-stake-keeper/render.yaml`:

1. New Render service → "Background Worker" → connect this repo
2. Render reads `render.yaml` and creates the service with `DRY_RUN=1`
3. Upload `~/.config/solana/covenant-creator-wallet.json` as a Render Secret File (path `/etc/secrets/covenant-creator-wallet.json`)
4. Fill the `sync: false` env vars in the Render dashboard: `COVENANT_STAKE_KEEPER_RPC_URL`, `_TREASURY_RECIPIENT`, `_SUBSIDY_RECIPIENT`
5. Deploy. Tail logs for one full sweep cycle (1h):

```
sweep split balance=2500000000 reserve=50000000 surplus=2450000000 stakers=612500000 buylock=612500000 treasury=735000000 subsidy=490000000
dry_run: skipping actual sends
```

If the split numbers match what you expect, you're good to flip live.

### Flip keeper live

In Render dashboard, change `COVENANT_STAKE_KEEPER_DRY_RUN` to `0`, redeploy. Next sweep fires real tx. Watch for:
- `deposited stakers SOL` log line
- `sent treasury+subsidy cuts` log line
- `buylock leg requires Jupiter SOL→CVNT swap … not implemented in v1` warn (expected, v1.1 work)

### Post-launch first 48h

- Watch the treasury page (`opencovenant.org/treasury`) — confirm `cumulative_sol_distributed` increments each sweep
- Open `/positions` from your founder wallet — confirm you can claim
- Confirm one external user can stake (test with a friend)
- Schedule Squads handoff for `config.authority` and program upgrade authority

### What this deploy does NOT include (v1.1 work)

- Buy-and-lock Jupiter swap leg (keeper logs and skips)
- Treasury + subsidy recipients on-chain (currently keeper env vars — H2 audit item)
- Two-step authority handoff (M1)
- Force-close-expired ix for abandoned positions
- Token-2022 test coverage parity (production mint is Token-2022; tests use legacy SPL)
- Cloudflare geofence + ToS click-through (not legally required for "just us" run; reconsider if scale grows)

## Soak protocol

The 14-day devnet soak window watches:

- `stake_total_weight` monotone non-negative (no overflow underflow paths via `increase_amount` / `close_position`).
- `cumulative_sol_distributed` monotone non-decreasing across the loop.
- Synthetic fee feed at $5–50/day SOL-equivalent on devnet from a scripted depositor wallet.
- Random user positions at all four lock tiers; verify claim payouts within tolerance of expected weighted share.
- Force the rate-limit, the per-call max, and the paused state at least once each; verify the program rejects deposits the expected way.
- Force `close_position` before lock expiry; confirm rejection.

If all green at day 14, the program is ready for the audit handoff. If any unexpected state transition shows up in the soak logs, file an issue and re-baseline before audit.
