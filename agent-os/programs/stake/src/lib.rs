//! Covenant `$CVNT` staking program.
//!
//! Lock-tiered fee-share: users lock `$CVNT` for 30/90/180/365 days at
//! 1.0x/1.5x/2.0x/3.0x weight multipliers, and earn pro-rata SOL fees
//! collected from protocol revenue (pump.fun creator fees, sandbox metered
//! tier, Hyre markup, SAP capabilities) routed in via a permissioned
//! FeeRouter PDA.
//!
//! The program is denomination-isolated from the settlement program
//! (`cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y`). It reads no settlement
//! state and exposes no governance surface.
//!
//! A separate `BuyLockVault` PDA-owned CVNT account holds buyback inflows.
//! The vault has no withdraw instruction in v1 — buybacks are effectively
//! locked until a future upgrade adds a timelocked release path.

#![allow(deprecated)]
#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::system_program;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};

declare_id!("CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED");

#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "Covenant Stake",
    project_url: "https://opencovenant.org",
    contacts: "email:security@opencovenant.org",
    policy: "https://github.com/open-covenant/covenant/blob/main/SECURITY.md",
    preferred_languages: "en",
    source_code: "https://github.com/open-covenant/covenant",
    source_release: "",
    auditors: "None"
}


/// Accumulator scaling factor for `acc_sol_per_weight`. 1e12 keeps u128 math
/// well clear of overflow: with `max_active_locks = 500`, per-position weight
/// bounded by `1e9 * 6dec * 3 = 3e15`, total_weight bounded by `1.5e18`, and
/// per-deposit pending bounded by `max_deposit_lamports * 1e12 ≈ 5e21`,
/// `acc_sol_per_weight` per step ≤ `5e21 / 1` < 2^75. Cumulative growth is
/// bounded by total distributed lifetime SOL × 1e12 / smallest-ever total_weight,
/// which audit must re-check against expected distribution volume.
pub const ACC_SCALE: u128 = 1_000_000_000_000;

/// v1 hard cap on concurrent active positions. Bounds CU cost of
/// `internal_accrue` and is re-evaluated at v1.1 with real TVL.
pub const MAX_ACTIVE_LOCKS: u32 = 500;

/// Default minimum lock principal (1000 CVNT @ 6 decimals).
pub const DEFAULT_MIN_LOCK_AMOUNT: u64 = 1_000_000_000;

const TIER_30D_BPS: u16 = 10_000;
const TIER_90D_BPS: u16 = 15_000;
const TIER_180D_BPS: u16 = 20_000;
const TIER_365D_BPS: u16 = 30_000;

const SECONDS_PER_DAY: i64 = 86_400;
const TIER_30D_SECS: i64 = 30 * SECONDS_PER_DAY;
const TIER_90D_SECS: i64 = 90 * SECONDS_PER_DAY;
const TIER_180D_SECS: i64 = 180 * SECONDS_PER_DAY;
const TIER_365D_SECS: i64 = 365 * SECONDS_PER_DAY;

/// Default FeeRouter rate limit (60s between deposits).
pub const DEFAULT_FEE_ROUTER_RATE_LIMIT_SECS: i64 = 60;

/// Default FeeRouter max per-deposit cap (5 SOL).
pub const DEFAULT_FEE_ROUTER_MAX_DEPOSIT_LAMPORTS: u64 = 5_000_000_000;

/// $CVNT decimals — enforced at initialize so DEFAULT_MIN_LOCK_AMOUNT and
/// downstream unit math always reference the same base.
pub const EXPECTED_MINT_DECIMALS: u8 = 6;

/// Layout offsets into a BPF UpgradeableLoaderState::ProgramData account.
const PD_OPT_TAG_OFFSET: usize = 12;
const PD_AUTHORITY_OFFSET: usize = 13;
const PD_AUTHORITY_END: usize = PD_AUTHORITY_OFFSET + 32;

fn verify_upgrade_authority(program_data: &AccountInfo, signer: &Pubkey) -> Result<()> {
    require_keys_eq!(
        *program_data.owner,
        bpf_loader_upgradeable::ID,
        CovenantStakeError::UnauthorizedAuthority
    );
    let (expected_pd, _) =
        Pubkey::find_program_address(&[crate::ID.as_ref()], &bpf_loader_upgradeable::ID);
    require_keys_eq!(
        program_data.key(),
        expected_pd,
        CovenantStakeError::UnauthorizedAuthority
    );
    let data = program_data.try_borrow_data()?;
    require!(
        data.len() >= PD_AUTHORITY_END,
        CovenantStakeError::UnauthorizedAuthority
    );
    require!(
        data[PD_OPT_TAG_OFFSET] == 1,
        CovenantStakeError::UnauthorizedAuthority
    );
    let raw: [u8; 32] = data[PD_AUTHORITY_OFFSET..PD_AUTHORITY_END]
        .try_into()
        .map_err(|_| error!(CovenantStakeError::UnauthorizedAuthority))?;
    require_keys_eq!(
        Pubkey::new_from_array(raw),
        *signer,
        CovenantStakeError::UnauthorizedAuthority
    );
    Ok(())
}

fn tier_lock_secs(bps: u16) -> Result<i64> {
    match bps {
        TIER_30D_BPS => Ok(TIER_30D_SECS),
        TIER_90D_BPS => Ok(TIER_90D_SECS),
        TIER_180D_BPS => Ok(TIER_180D_SECS),
        TIER_365D_BPS => Ok(TIER_365D_SECS),
        _ => err!(CovenantStakeError::InvalidLockTier),
    }
}

fn weight_for(amount: u64, multiplier_bps: u16) -> Result<u128> {
    (amount as u128)
        .checked_mul(multiplier_bps as u128)
        .and_then(|v| v.checked_div(10_000))
        .ok_or_else(|| error!(CovenantStakeError::MathOverflow))
}


#[program]
pub mod stake {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        require!(
            ctx.accounts.covnt_mint.decimals == EXPECTED_MINT_DECIMALS,
            CovenantStakeError::InvalidMintDecimals
        );
        verify_upgrade_authority(&ctx.accounts.program_data, &ctx.accounts.authority.key())?;

        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.pause_authority = args.pause_authority;
        config.paused = false;
        config.covnt_mint = ctx.accounts.covnt_mint.key();
        config.min_lock_amount = if args.min_lock_amount == 0 {
            DEFAULT_MIN_LOCK_AMOUNT
        } else {
            args.min_lock_amount
        };
        config.max_active_locks = MAX_ACTIVE_LOCKS;
        config.active_lock_count = 0;
        config.acc_sol_per_weight = 0;
        config.total_weight = 0;
        config.pending_sol_lamports = 0;
        config.last_accrual_ts = Clock::get()?.unix_timestamp;
        config.cumulative_sol_distributed = 0;
        config.cumulative_buylock_cvnt = 0;
        config.initialized_ts = Clock::get()?.unix_timestamp;
        config.bump = ctx.bumps.config;
        config.locked_vault_authority_bump = ctx.bumps.locked_vault_authority;
        config.reward_vault_bump = ctx.bumps.reward_vault;
        config.buylock_vault_authority_bump = ctx.bumps.buylock_vault_authority;

        let fee_router = &mut ctx.accounts.fee_router;
        fee_router.authority = args.fee_router_authority;
        fee_router.max_deposit_lamports = if args.fee_router_max_deposit_lamports == 0 {
            DEFAULT_FEE_ROUTER_MAX_DEPOSIT_LAMPORTS
        } else {
            args.fee_router_max_deposit_lamports
        };
        fee_router.rate_limit_secs = if args.fee_router_rate_limit_secs == 0 {
            DEFAULT_FEE_ROUTER_RATE_LIMIT_SECS
        } else {
            args.fee_router_rate_limit_secs
        };
        fee_router.last_deposit_ts = i64::MIN;
        fee_router.bump = ctx.bumps.fee_router;

        emit!(ProgramInitialized {
            authority: config.authority,
            pause_authority: config.pause_authority,
            fee_router_authority: fee_router.authority,
            covnt_mint: config.covnt_mint,
            min_lock_amount: config.min_lock_amount,
            max_active_locks: config.max_active_locks,
        });
        Ok(())
    }

    pub fn pause(ctx: Context<Pause>) -> Result<()> {
        ctx.accounts.config.paused = true;
        emit!(ProtocolPauseUpdated {
            paused: true,
            by: ctx.accounts.signer.key(),
        });
        Ok(())
    }

    pub fn unpause(ctx: Context<Unpause>) -> Result<()> {
        ctx.accounts.config.paused = false;
        emit!(ProtocolPauseUpdated {
            paused: false,
            by: ctx.accounts.authority.key(),
        });
        Ok(())
    }

    pub fn update_min_lock_amount(
        ctx: Context<UpdateConfigParam>,
        new_min: u64,
    ) -> Result<()> {
        require!(new_min > 0, CovenantStakeError::InvalidParameter);
        let old = ctx.accounts.config.min_lock_amount;
        ctx.accounts.config.min_lock_amount = new_min;
        emit!(MinLockAmountUpdated { old, new: new_min });
        Ok(())
    }

    pub fn update_max_active_locks(
        ctx: Context<UpdateConfigParam>,
        new_max: u32,
    ) -> Result<()> {
        require!(new_max > 0, CovenantStakeError::InvalidParameter);
        require!(
            new_max >= ctx.accounts.config.active_lock_count,
            CovenantStakeError::InvalidParameter
        );
        let old = ctx.accounts.config.max_active_locks;
        ctx.accounts.config.max_active_locks = new_max;
        emit!(MaxActiveLocksUpdated { old, new: new_max });
        Ok(())
    }

    pub fn update_authority(
        ctx: Context<UpdateAuthority>,
        new_authority: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let old = config.authority;
        config.authority = new_authority;
        emit!(AuthorityRotated {
            old,
            new: new_authority,
        });
        Ok(())
    }

    pub fn rotate_fee_router(
        ctx: Context<RotateFeeRouter>,
        args: RotateFeeRouterArgs,
    ) -> Result<()> {
        let fee_router = &mut ctx.accounts.fee_router;
        let old_authority = fee_router.authority;
        if let Some(new_authority) = args.new_authority {
            fee_router.authority = new_authority;
        }
        if let Some(max_deposit) = args.new_max_deposit_lamports {
            require!(max_deposit > 0, CovenantStakeError::InvalidParameter);
            fee_router.max_deposit_lamports = max_deposit;
        }
        if let Some(rate_limit) = args.new_rate_limit_secs {
            require!(rate_limit > 0, CovenantStakeError::InvalidParameter);
            fee_router.rate_limit_secs = rate_limit;
        }
        emit!(FeeRouterRotated {
            old_authority,
            new_authority: fee_router.authority,
            max_deposit_lamports: fee_router.max_deposit_lamports,
            rate_limit_secs: fee_router.rate_limit_secs,
        });
        Ok(())
    }

    pub fn create_position(
        ctx: Context<CreatePosition>,
        args: CreatePositionArgs,
    ) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantStakeError::ProtocolPaused);
        let lock_secs = tier_lock_secs(args.lock_tier_bps)?;
        require!(
            args.amount >= ctx.accounts.config.min_lock_amount,
            CovenantStakeError::LockAmountTooSmall
        );
        require!(
            ctx.accounts.config.active_lock_count < ctx.accounts.config.max_active_locks,
            CovenantStakeError::LockCapExceeded
        );

        // Fold any pending SOL into the accumulator before adding new weight,
        // so the new position does not retroactively earn against pre-existing fees.
        internal_accrue(&mut ctx.accounts.config, Clock::get()?.unix_timestamp)?;

        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.owner_cvnt_ata.to_account_info(),
                mint: ctx.accounts.covnt_mint.to_account_info(),
                to: ctx.accounts.locked_cvnt_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        );
        token_interface::transfer_checked(cpi, args.amount, ctx.accounts.covnt_mint.decimals)?;

        let weight = weight_for(args.amount, args.lock_tier_bps)?;
        let now = Clock::get()?.unix_timestamp;

        let position = &mut ctx.accounts.position;
        position.owner = ctx.accounts.owner.key();
        position.nonce = args.nonce;
        position.amount = args.amount;
        position.weight = weight;
        position.multiplier_bps = args.lock_tier_bps;
        position.lock_start = now;
        position.lock_end = now
            .checked_add(lock_secs)
            .ok_or(CovenantStakeError::MathOverflow)?;
        position.reward_debt = weight
            .checked_mul(ctx.accounts.config.acc_sol_per_weight)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .ok_or(CovenantStakeError::MathOverflow)?;
        position.unclaimed_lamports = 0;
        position.created_at = now;
        position.bump = ctx.bumps.position;

        let config = &mut ctx.accounts.config;
        config.total_weight = config
            .total_weight
            .checked_add(weight)
            .ok_or(CovenantStakeError::MathOverflow)?;
        config.active_lock_count = config
            .active_lock_count
            .checked_add(1)
            .ok_or(CovenantStakeError::MathOverflow)?;

        emit!(PositionCreated {
            owner: position.owner,
            nonce: position.nonce,
            amount: position.amount,
            weight: position.weight,
            multiplier_bps: position.multiplier_bps,
            lock_end: position.lock_end,
        });
        Ok(())
    }

    pub fn increase_amount(ctx: Context<IncreaseAmount>, extra: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantStakeError::ProtocolPaused);
        require!(extra > 0, CovenantStakeError::ZeroAmount);
        let now = Clock::get()?.unix_timestamp;
        require!(
            ctx.accounts.position.lock_end > now,
            CovenantStakeError::LockExpired
        );

        internal_accrue(&mut ctx.accounts.config, now)?;

        // Settle pending against the OLD weight before reseating reward_debt.
        let position = &mut ctx.accounts.position;
        let pending = position
            .weight
            .checked_mul(ctx.accounts.config.acc_sol_per_weight)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .and_then(|v| v.checked_sub(position.reward_debt))
            .ok_or(CovenantStakeError::MathOverflow)?;
        let pending_u64 = u64::try_from(pending).map_err(|_| CovenantStakeError::MathOverflow)?;
        position.unclaimed_lamports = position
            .unclaimed_lamports
            .checked_add(pending_u64)
            .ok_or(CovenantStakeError::MathOverflow)?;

        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.owner_cvnt_ata.to_account_info(),
                mint: ctx.accounts.covnt_mint.to_account_info(),
                to: ctx.accounts.locked_cvnt_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        );
        token_interface::transfer_checked(cpi, extra, ctx.accounts.covnt_mint.decimals)?;

        let old_weight = position.weight;
        position.amount = position
            .amount
            .checked_add(extra)
            .ok_or(CovenantStakeError::MathOverflow)?;
        let new_weight = weight_for(position.amount, position.multiplier_bps)?;
        position.weight = new_weight;
        position.reward_debt = new_weight
            .checked_mul(ctx.accounts.config.acc_sol_per_weight)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .ok_or(CovenantStakeError::MathOverflow)?;

        let config = &mut ctx.accounts.config;
        config.total_weight = config
            .total_weight
            .checked_sub(old_weight)
            .and_then(|v| v.checked_add(new_weight))
            .ok_or(CovenantStakeError::MathOverflow)?;

        emit!(PositionIncreased {
            owner: position.owner,
            nonce: position.nonce,
            extra,
            new_amount: position.amount,
            new_weight: position.weight,
            settled_pending_lamports: pending_u64,
        });
        Ok(())
    }

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantStakeError::ProtocolPaused);
        let now = Clock::get()?.unix_timestamp;
        internal_accrue(&mut ctx.accounts.config, now)?;

        let position = &mut ctx.accounts.position;
        let acc = ctx.accounts.config.acc_sol_per_weight;
        let pending = position
            .weight
            .checked_mul(acc)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .and_then(|v| v.checked_sub(position.reward_debt))
            .ok_or(CovenantStakeError::MathOverflow)?;
        let pending_u64 = u64::try_from(pending).map_err(|_| CovenantStakeError::MathOverflow)?;
        let total = position
            .unclaimed_lamports
            .checked_add(pending_u64)
            .ok_or(CovenantStakeError::MathOverflow)?;
        require!(total > 0, CovenantStakeError::NothingToClaim);

        position.reward_debt = position
            .weight
            .checked_mul(acc)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .ok_or(CovenantStakeError::MathOverflow)?;
        position.unclaimed_lamports = 0;

        transfer_from_reward_vault(
            &ctx.accounts.reward_vault,
            &ctx.accounts.owner,
            ctx.accounts.config.reward_vault_bump,
            total,
        )?;

        emit!(RewardsClaimed {
            owner: position.owner,
            nonce: position.nonce,
            lamports: total,
        });
        Ok(())
    }

    pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
        // No pause gate: principal is the user's right after lock_end.
        let now = Clock::get()?.unix_timestamp;
        require!(
            ctx.accounts.position.lock_end <= now,
            CovenantStakeError::LockNotExpired
        );
        internal_accrue(&mut ctx.accounts.config, now)?;

        let position = &mut ctx.accounts.position;
        let acc = ctx.accounts.config.acc_sol_per_weight;
        let pending = position
            .weight
            .checked_mul(acc)
            .and_then(|v| v.checked_div(ACC_SCALE))
            .and_then(|v| v.checked_sub(position.reward_debt))
            .ok_or(CovenantStakeError::MathOverflow)?;
        let pending_u64 = u64::try_from(pending).map_err(|_| CovenantStakeError::MathOverflow)?;
        let total_lamports = position
            .unclaimed_lamports
            .checked_add(pending_u64)
            .ok_or(CovenantStakeError::MathOverflow)?;

        let principal = position.amount;
        let weight = position.weight;
        let owner_key = position.owner;
        let nonce = position.nonce;

        // Return principal CVNT
        let locked_authority_bump = ctx.accounts.config.locked_vault_authority_bump;
        let seeds: &[&[u8]] = &[b"vault_auth", &[locked_authority_bump]];
        let signer: &[&[&[u8]]] = &[seeds];
        let cpi = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.locked_cvnt_vault.to_account_info(),
                mint: ctx.accounts.covnt_mint.to_account_info(),
                to: ctx.accounts.owner_cvnt_ata.to_account_info(),
                authority: ctx.accounts.locked_vault_authority.to_account_info(),
            },
            signer,
        );
        token_interface::transfer_checked(cpi, principal, ctx.accounts.covnt_mint.decimals)?;

        // Pay out any unclaimed SOL
        if total_lamports > 0 {
            transfer_from_reward_vault(
                &ctx.accounts.reward_vault,
                &ctx.accounts.owner,
                ctx.accounts.config.reward_vault_bump,
                total_lamports,
            )?;
        }

        // Update config aggregates
        let config = &mut ctx.accounts.config;
        config.total_weight = config
            .total_weight
            .checked_sub(weight)
            .ok_or(CovenantStakeError::MathOverflow)?;
        config.active_lock_count = config
            .active_lock_count
            .checked_sub(1)
            .ok_or(CovenantStakeError::MathOverflow)?;

        emit!(PositionClosed {
            owner: owner_key,
            nonce,
            returned_amount: principal,
            paid_lamports: total_lamports,
        });
        Ok(())
    }

    pub fn deposit_sol_fees(ctx: Context<DepositSolFees>, amount: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantStakeError::ProtocolPaused);
        require!(amount > 0, CovenantStakeError::ZeroAmount);
        require!(
            ctx.accounts.config.total_weight > 0,
            CovenantStakeError::NoActiveStakers
        );
        let now = Clock::get()?.unix_timestamp;

        let fee_router = &mut ctx.accounts.fee_router;
        require!(
            amount <= fee_router.max_deposit_lamports,
            CovenantStakeError::DepositAmountExceeded
        );
        require!(
            now.saturating_sub(fee_router.last_deposit_ts) >= fee_router.rate_limit_secs,
            CovenantStakeError::RateLimited
        );

        let cpi = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.depositor.to_account_info(),
                to: ctx.accounts.reward_vault.to_account_info(),
            },
        );
        system_program::transfer(cpi, amount)?;

        let config = &mut ctx.accounts.config;
        config.pending_sol_lamports = config
            .pending_sol_lamports
            .checked_add(amount)
            .ok_or(CovenantStakeError::MathOverflow)?;
        fee_router.last_deposit_ts = now;

        // Immediate fold if there is weight to distribute against.
        internal_accrue(config, now)?;

        emit!(SolFeesDeposited {
            depositor: ctx.accounts.depositor.key(),
            amount,
            pending_after: config.pending_sol_lamports,
            acc_sol_per_weight: config.acc_sol_per_weight,
            total_weight: config.total_weight,
        });
        Ok(())
    }

    pub fn accrue(ctx: Context<Accrue>) -> Result<()> {
        // accrue is a pure observability poke — no pause gate.
        let now = Clock::get()?.unix_timestamp;
        let config = &mut ctx.accounts.config;
        let folded = config.pending_sol_lamports;
        internal_accrue(config, now)?;
        emit!(AccrualTick {
            folded_lamports: folded,
            acc_sol_per_weight: config.acc_sol_per_weight,
            total_weight: config.total_weight,
        });
        Ok(())
    }

    pub fn deposit_buylock_cvnt(ctx: Context<DepositBuyLockCvnt>, amount: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantStakeError::ProtocolPaused);
        require!(amount > 0, CovenantStakeError::ZeroAmount);

        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.depositor_cvnt_ata.to_account_info(),
                mint: ctx.accounts.covnt_mint.to_account_info(),
                to: ctx.accounts.buylock_cvnt_vault.to_account_info(),
                authority: ctx.accounts.depositor.to_account_info(),
            },
        );
        token_interface::transfer_checked(cpi, amount, ctx.accounts.covnt_mint.decimals)?;

        let config = &mut ctx.accounts.config;
        config.cumulative_buylock_cvnt = config
            .cumulative_buylock_cvnt
            .checked_add(amount)
            .ok_or(CovenantStakeError::MathOverflow)?;

        emit!(BuyLockDeposited {
            depositor: ctx.accounts.depositor.key(),
            amount,
            cumulative_buylock_cvnt: config.cumulative_buylock_cvnt,
        });
        Ok(())
    }
}


fn internal_accrue(config: &mut Config, now: i64) -> Result<()> {
    if config.total_weight == 0 || config.pending_sol_lamports == 0 {
        return Ok(());
    }
    let delta = (config.pending_sol_lamports as u128)
        .checked_mul(ACC_SCALE)
        .and_then(|v| v.checked_div(config.total_weight))
        .ok_or(CovenantStakeError::MathOverflow)?;
    config.acc_sol_per_weight = config
        .acc_sol_per_weight
        .checked_add(delta)
        .ok_or(CovenantStakeError::MathOverflow)?;
    config.cumulative_sol_distributed = config
        .cumulative_sol_distributed
        .checked_add(config.pending_sol_lamports)
        .ok_or(CovenantStakeError::MathOverflow)?;
    config.pending_sol_lamports = 0;
    config.last_accrual_ts = now;
    Ok(())
}

fn transfer_from_reward_vault<'info>(
    reward_vault: &Account<'info, RewardVault>,
    recipient: &AccountInfo<'info>,
    _bump: u8,
    amount: u64,
) -> Result<()> {
    // Manual lamport mutation — reward_vault is system-owned PDA but our
    // program is the only path that decrements it because all entry points
    // are gated and `init` set the rent-exempt floor.
    //
    // SystemAccount's underlying AccountInfo is mutable as long as we are
    // currently being invoked with it marked writable. We require this in the
    // Anchor account constraint (#[account(mut)]).
    let vault_info = reward_vault.to_account_info();
    let rent_min = Rent::get()?.minimum_balance(8 + RewardVault::INIT_SPACE);
    let mut vault_lamports = vault_info.try_borrow_mut_lamports()?;
    let mut recipient_lamports = recipient.try_borrow_mut_lamports()?;
    let after_vault = vault_lamports
        .checked_sub(amount)
        .ok_or(CovenantStakeError::InsufficientRewardVault)?;
    require!(after_vault >= rent_min, CovenantStakeError::InsufficientRewardVault);
    **vault_lamports = after_vault;
    **recipient_lamports = recipient_lamports
        .checked_add(amount)
        .ok_or(CovenantStakeError::MathOverflow)?;
    Ok(())
}


#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializeArgs {
    pub pause_authority: Pubkey,
    pub fee_router_authority: Pubkey,
    pub min_lock_amount: u64,
    pub fee_router_max_deposit_lamports: u64,
    pub fee_router_rate_limit_secs: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct RotateFeeRouterArgs {
    pub new_authority: Option<Pubkey>,
    pub new_max_deposit_lamports: Option<u64>,
    pub new_rate_limit_secs: Option<i64>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CreatePositionArgs {
    pub nonce: u64,
    pub amount: u64,
    pub lock_tier_bps: u16,
}


#[account]
#[derive(InitSpace)]
pub struct Config {
    pub authority: Pubkey,
    pub pause_authority: Pubkey,
    pub paused: bool,
    pub covnt_mint: Pubkey,
    pub min_lock_amount: u64,
    pub max_active_locks: u32,
    pub active_lock_count: u32,
    pub acc_sol_per_weight: u128,
    pub total_weight: u128,
    pub pending_sol_lamports: u64,
    pub last_accrual_ts: i64,
    pub cumulative_sol_distributed: u64,
    pub cumulative_buylock_cvnt: u64,
    pub initialized_ts: i64,
    pub bump: u8,
    pub locked_vault_authority_bump: u8,
    pub reward_vault_bump: u8,
    pub buylock_vault_authority_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct StakePosition {
    pub owner: Pubkey,
    pub nonce: u64,
    pub amount: u64,
    pub weight: u128,
    pub multiplier_bps: u16,
    pub lock_start: i64,
    pub lock_end: i64,
    pub reward_debt: u128,
    pub unclaimed_lamports: u64,
    pub created_at: i64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct RewardVault {}

#[account]
#[derive(InitSpace)]
pub struct FeeRouter {
    pub authority: Pubkey,
    pub max_deposit_lamports: u64,
    pub rate_limit_secs: i64,
    pub last_deposit_ts: i64,
    pub bump: u8,
}


#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [b"stake_config"],
        bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = authority,
        space = 8 + FeeRouter::INIT_SPACE,
        seeds = [b"fee_router"],
        bump,
    )]
    pub fee_router: Account<'info, FeeRouter>,
    /// PDA signer for the locked-CVNT vault ATA. No data, derived only.
    /// CHECK: seeds-validated PDA, used as authority for the vault ATA.
    #[account(seeds = [b"vault_auth"], bump)]
    pub locked_vault_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = authority,
        space = 8 + RewardVault::INIT_SPACE,
        seeds = [b"reward_vault"],
        bump,
    )]
    pub reward_vault: Account<'info, RewardVault>,
    /// PDA signer for the BuyLock CVNT vault ATA. No data, derived only.
    /// CHECK: seeds-validated PDA, used as authority for the vault ATA.
    #[account(seeds = [b"buylock_auth"], bump)]
    pub buylock_vault_authority: UncheckedAccount<'info>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    /// Locked-CVNT vault: ATA owned by `locked_vault_authority`. Must be created
    /// by the caller (use Associated Token Program create_idempotent first).
    #[account(
        token::mint = covnt_mint,
        token::authority = locked_vault_authority,
    )]
    pub locked_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    /// BuyLock CVNT vault: ATA owned by `buylock_vault_authority`. Must be
    /// created by the caller.
    #[account(
        token::mint = covnt_mint,
        token::authority = buylock_vault_authority,
    )]
    pub buylock_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: ProgramData PDA for the bpf upgradeable loader. Validated by
    /// `verify_upgrade_authority` against the bpf_loader_upgradeable program
    /// owner + derived PDA + the upgrade_authority field in the layout.
    pub program_data: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Pause<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        constraint = signer.key() == config.authority
            || signer.key() == config.pause_authority
            @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub config: Account<'info, Config>,
    pub signer: Signer<'info>,
}

#[derive(Accounts)]
pub struct Unpause<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        has_one = authority @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateConfigParam<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        has_one = authority @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateAuthority<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        has_one = authority @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct RotateFeeRouter<'info> {
    #[account(
        seeds = [b"stake_config"],
        bump = config.bump,
        has_one = authority @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"fee_router"],
        bump = fee_router.bump,
    )]
    pub fee_router: Account<'info, FeeRouter>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(args: CreatePositionArgs)]
pub struct CreatePosition<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        constraint = config.covnt_mint == covnt_mint.key() @ CovenantStakeError::InvalidMint,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = owner,
        space = 8 + StakePosition::INIT_SPACE,
        seeds = [b"stake_v2", owner.key().as_ref(), &args.nonce.to_le_bytes()],
        bump,
    )]
    pub position: Account<'info, StakePosition>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    /// CHECK: seeds-validated PDA
    #[account(seeds = [b"vault_auth"], bump = config.locked_vault_authority_bump)]
    pub locked_vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = locked_vault_authority,
    )]
    pub locked_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = owner,
    )]
    pub owner_cvnt_ata: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct IncreaseAmount<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        constraint = config.covnt_mint == covnt_mint.key() @ CovenantStakeError::InvalidMint,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"stake_v2", owner.key().as_ref(), &position.nonce.to_le_bytes()],
        bump = position.bump,
        has_one = owner @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub position: Account<'info, StakePosition>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    /// CHECK: seeds-validated PDA
    #[account(seeds = [b"vault_auth"], bump = config.locked_vault_authority_bump)]
    pub locked_vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = locked_vault_authority,
    )]
    pub locked_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = owner,
    )]
    pub owner_cvnt_ata: InterfaceAccount<'info, TokenAccount>,
    pub owner: Signer<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"stake_v2", owner.key().as_ref(), &position.nonce.to_le_bytes()],
        bump = position.bump,
        has_one = owner @ CovenantStakeError::UnauthorizedAuthority,
    )]
    pub position: Account<'info, StakePosition>,
    #[account(
        mut,
        seeds = [b"reward_vault"],
        bump = config.reward_vault_bump,
    )]
    pub reward_vault: Account<'info, RewardVault>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        constraint = config.covnt_mint == covnt_mint.key() @ CovenantStakeError::InvalidMint,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"stake_v2", owner.key().as_ref(), &position.nonce.to_le_bytes()],
        bump = position.bump,
        has_one = owner @ CovenantStakeError::UnauthorizedAuthority,
        close = owner,
    )]
    pub position: Account<'info, StakePosition>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    /// CHECK: seeds-validated PDA
    #[account(seeds = [b"vault_auth"], bump = config.locked_vault_authority_bump)]
    pub locked_vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = locked_vault_authority,
    )]
    pub locked_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = owner,
    )]
    pub owner_cvnt_ata: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        seeds = [b"reward_vault"],
        bump = config.reward_vault_bump,
    )]
    pub reward_vault: Account<'info, RewardVault>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct DepositSolFees<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"fee_router"],
        bump = fee_router.bump,
        constraint = depositor.key() == fee_router.authority
            @ CovenantStakeError::UnauthorizedFeeRouter,
    )]
    pub fee_router: Account<'info, FeeRouter>,
    #[account(
        mut,
        seeds = [b"reward_vault"],
        bump = config.reward_vault_bump,
    )]
    pub reward_vault: Account<'info, RewardVault>,
    #[account(mut)]
    pub depositor: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Accrue<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
pub struct DepositBuyLockCvnt<'info> {
    #[account(
        mut,
        seeds = [b"stake_config"],
        bump = config.bump,
        constraint = config.covnt_mint == covnt_mint.key() @ CovenantStakeError::InvalidMint,
    )]
    pub config: Account<'info, Config>,
    #[account(
        seeds = [b"fee_router"],
        bump = fee_router.bump,
        constraint = depositor.key() == fee_router.authority
            @ CovenantStakeError::UnauthorizedFeeRouter,
    )]
    pub fee_router: Account<'info, FeeRouter>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    /// CHECK: seeds-validated PDA
    #[account(seeds = [b"buylock_auth"], bump = config.buylock_vault_authority_bump)]
    pub buylock_vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = buylock_vault_authority,
    )]
    pub buylock_cvnt_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        token::mint = covnt_mint,
        token::authority = depositor,
    )]
    pub depositor_cvnt_ata: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub depositor: Signer<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}


#[event]
pub struct ProgramInitialized {
    pub authority: Pubkey,
    pub pause_authority: Pubkey,
    pub fee_router_authority: Pubkey,
    pub covnt_mint: Pubkey,
    pub min_lock_amount: u64,
    pub max_active_locks: u32,
}

#[event]
pub struct ProtocolPauseUpdated {
    pub paused: bool,
    pub by: Pubkey,
}

#[event]
pub struct AuthorityRotated {
    pub old: Pubkey,
    pub new: Pubkey,
}

#[event]
pub struct FeeRouterRotated {
    pub old_authority: Pubkey,
    pub new_authority: Pubkey,
    pub max_deposit_lamports: u64,
    pub rate_limit_secs: i64,
}

#[event]
pub struct PositionCreated {
    pub owner: Pubkey,
    pub nonce: u64,
    pub amount: u64,
    pub weight: u128,
    pub multiplier_bps: u16,
    pub lock_end: i64,
}

#[event]
pub struct PositionIncreased {
    pub owner: Pubkey,
    pub nonce: u64,
    pub extra: u64,
    pub new_amount: u64,
    pub new_weight: u128,
    pub settled_pending_lamports: u64,
}

#[event]
pub struct RewardsClaimed {
    pub owner: Pubkey,
    pub nonce: u64,
    pub lamports: u64,
}

#[event]
pub struct PositionClosed {
    pub owner: Pubkey,
    pub nonce: u64,
    pub returned_amount: u64,
    pub paid_lamports: u64,
}

#[event]
pub struct SolFeesDeposited {
    pub depositor: Pubkey,
    pub amount: u64,
    pub pending_after: u64,
    pub acc_sol_per_weight: u128,
    pub total_weight: u128,
}

#[event]
pub struct AccrualTick {
    pub folded_lamports: u64,
    pub acc_sol_per_weight: u128,
    pub total_weight: u128,
}

#[event]
pub struct BuyLockDeposited {
    pub depositor: Pubkey,
    pub amount: u64,
    pub cumulative_buylock_cvnt: u64,
}

#[event]
pub struct MinLockAmountUpdated {
    pub old: u64,
    pub new: u64,
}

#[event]
pub struct MaxActiveLocksUpdated {
    pub old: u32,
    pub new: u32,
}

#[error_code]
pub enum CovenantStakeError {
    #[msg("protocol is paused")]
    ProtocolPaused,
    #[msg("lock period has not yet expired")]
    LockNotExpired,
    #[msg("lock period has already expired")]
    LockExpired,
    #[msg("lock tier must be 10000, 15000, 20000, or 30000 bps")]
    InvalidLockTier,
    #[msg("lock principal below configured minimum")]
    LockAmountTooSmall,
    #[msg("max active lock cap exceeded")]
    LockCapExceeded,
    #[msg("signer is not the configured authority")]
    UnauthorizedAuthority,
    #[msg("signer is not the configured fee router authority")]
    UnauthorizedFeeRouter,
    #[msg("rate-limited: too soon after last deposit")]
    RateLimited,
    #[msg("deposit exceeds per-call max")]
    DepositAmountExceeded,
    #[msg("reward math overflow")]
    MathOverflow,
    #[msg("mint mismatch with config")]
    InvalidMint,
    #[msg("reward vault has insufficient lamports above rent floor")]
    InsufficientRewardVault,
    #[msg("nothing to claim")]
    NothingToClaim,
    #[msg("zero amount")]
    ZeroAmount,
    #[msg("invalid parameter")]
    InvalidParameter,
    #[msg("no active stakers — fees would orphan")]
    NoActiveStakers,
    #[msg("covnt mint decimals must equal 6")]
    InvalidMintDecimals,
}
