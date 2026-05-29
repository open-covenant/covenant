# covenant-stake — deploy runbook

Program ID: `CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED`
Keypair: `~/.config/solana/covenant-stake-program.json`

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

## Mainnet path (gated)

Mainnet locks-open MUST wait for:

1. External audit pass on the `covenant-stake-program` `.so` and matching commit SHA.
2. Squads 3-of-5 multisig holding the program upgrade authority and the `Config.authority` field (call `update_authority` to hand off after `initialize`).
3. Treasury recipient = a Squads vault, not a hot wallet.
4. Legal opinion signed (Howey/MiCA framing per spec §5).
5. Cloudflare geofence + ToS click-through wired on the frontend per spec §3.7.
6. Real-yield inflow observable: pump.fun creator-fee inflow OR sandbox metered-tier USDC swap path producing > $50/week into the creator wallet (otherwise locks-open ships against zero distribution).

Once those gates clear, the mainnet deploy is the same `solana program deploy` against `https://api.mainnet-beta.solana.com` with the Squads multisig as upgrade authority. The first mainnet `initialize` MUST be queued through Squads — the program's `Config.authority` field is set to the Squads vault from the first call.

## Soak protocol

The 14-day devnet soak window watches:

- `stake_total_weight` monotone non-negative (no overflow underflow paths via `increase_amount` / `close_position`).
- `cumulative_sol_distributed` monotone non-decreasing across the loop.
- Synthetic fee feed at $5–50/day SOL-equivalent on devnet from a scripted depositor wallet.
- Random user positions at all four lock tiers; verify claim payouts within tolerance of expected weighted share.
- Force the rate-limit, the per-call max, and the paused state at least once each; verify the program rejects deposits the expected way.
- Force `close_position` before lock expiry; confirm rejection.

If all green at day 14, the program is ready for the audit handoff. If any unexpected state transition shows up in the soak logs, file an issue and re-baseline before audit.
