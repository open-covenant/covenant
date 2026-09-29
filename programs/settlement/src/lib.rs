// Covenant Solana protocol program.
//
// `$COVNT` is an external SPL mint. The program never mints it; protocol
// utility comes from staking, escrowing, burning, and metering it into
// non-transferable credits.
//
// `allow(deprecated)` / `allow(unexpected_cfgs)` live in `[lints]` (Cargo.toml)
// rather than as crate inner attributes, so this file can be `include!`d verbatim
// by the isolated ER crate (settlement-ephemeral). Inner attributes break include!.
//
// The GPU lease meter is the program's second subsystem and shares none of the
// first one's state. It bills a compute session by the second, escrowed in the
// mint the renter chose (USDC in practice), with the per-second meter running in
// the ER and only the payout arithmetic on L1. It reads `Config.paused` and
// nothing else, so no COVNT account, seed or amount is on its path.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, Burn, CloseAccount, Mint, TokenAccount, TokenInterface, TransferChecked,
};

#[cfg(feature = "ephemeral")]
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral};
#[cfg(feature = "ephemeral")]
use ephemeral_rollups_sdk::cpi::DelegateConfig;
#[cfg(feature = "ephemeral")]
use ephemeral_rollups_sdk::ephem::MagicIntentBundleBuilder;

declare_id!("3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff");

#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "Covenant Settlement",
    project_url: "https://opencovenant.org",
    contacts: "email:security@opencovenant.org",
    policy: "https://github.com/open-covenant/covenant/blob/main/SECURITY.md",
    preferred_languages: "en",
    source_code: "https://github.com/open-covenant/covenant",
    source_release: "v0.1.0-alpha.1",
    auditors: "None"
}

#[cfg_attr(feature = "ephemeral", ephemeral)]
#[program]
pub mod settlement {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        require!(args.credits_per_covnt > 0, CovenantError::ZeroAmount);

        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.slash_authority = args.slash_authority;
        config.covnt_mint = ctx.accounts.covnt_mint.key();
        config.treasury = ctx.accounts.treasury.key();
        config.credits_per_covnt = args.credits_per_covnt;
        config.paused = false;
        config.bump = ctx.bumps.config;
        config.min_stake_lock = args.min_stake_lock;

        emit!(ProtocolInitialized {
            authority: config.authority,
            slash_authority: config.slash_authority,
            covnt_mint: config.covnt_mint,
            treasury: config.treasury,
            credits_per_covnt: config.credits_per_covnt,
        });
        Ok(())
    }

    pub fn set_pause(ctx: Context<SetPause>, paused: bool) -> Result<()> {
        ctx.accounts.config.paused = paused;
        emit!(ProtocolPauseUpdated { paused });
        Ok(())
    }

    pub fn register_agent(ctx: Context<RegisterAgent>, args: RegisterAgentArgs) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);

        let agent = &mut ctx.accounts.agent;
        agent.agent_key = args.agent_key;
        agent.operator = ctx.accounts.operator.key();
        agent.metadata_hash = args.metadata_hash;
        agent.capability_hash = args.capability_hash;
        agent.stake = 0;
        agent.reputation = 0;
        agent.active = true;
        agent.bump = ctx.bumps.agent;

        emit!(AgentRegistered {
            agent_key: agent.agent_key,
            operator: agent.operator,
            metadata_hash: agent.metadata_hash,
            capability_hash: agent.capability_hash,
        });
        Ok(())
    }

    pub fn set_agent_active(ctx: Context<SetAgentActive>, active: bool) -> Result<()> {
        ctx.accounts.agent.active = active;
        emit!(AgentStatusUpdated {
            agent_key: ctx.accounts.agent.agent_key,
            active,
        });
        Ok(())
    }

    pub fn open_credit_account(ctx: Context<OpenCreditAccount>) -> Result<()> {
        let credits = &mut ctx.accounts.credits;
        credits.owner = ctx.accounts.owner.key();
        credits.balance = 0;
        credits.bump = ctx.bumps.credits;
        credits.provenance_root = [0u8; 32];

        emit!(CreditAccountOpened {
            owner: credits.owner,
            credit_account: credits.key(),
        });
        Ok(())
    }

    pub fn buy_credits(ctx: Context<BuyCredits>, amount_covnt: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount_covnt > 0, CovenantError::ZeroAmount);

        token_interface::transfer_checked(
            ctx.accounts.buy_transfer_ctx(),
            amount_covnt,
            ctx.accounts.covnt_mint.decimals,
        )?;

        let credits = amount_covnt
            .checked_mul(ctx.accounts.config.credits_per_covnt)
            .ok_or(CovenantError::Overflow)?;
        ctx.accounts.credits.balance = ctx
            .accounts
            .credits
            .balance
            .checked_add(credits)
            .ok_or(CovenantError::Overflow)?;

        emit!(CreditsPurchased {
            owner: ctx.accounts.owner.key(),
            amount_covnt,
            credits,
        });
        Ok(())
    }

    pub fn consume_credits(
        ctx: Context<ConsumeCredits>,
        amount: u64,
        receipt_hash: [u8; 32],
    ) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount > 0, CovenantError::ZeroAmount);
        require!(
            ctx.accounts.credits.balance >= amount,
            CovenantError::InsufficientCredits
        );

        ctx.accounts.credits.balance -= amount;

        // Fold the receipt into the account's provenance hash-chain. This runs in
        // the ER per consume and commits to L1 with the balance, making the root a
        // real-time, on-chain record of every metered action.
        let provenance_root = anchor_lang::solana_program::hash::hashv(&[
            &ctx.accounts.credits.provenance_root,
            &receipt_hash,
        ])
        .to_bytes();
        ctx.accounts.credits.provenance_root = provenance_root;

        emit!(CreditsConsumed {
            owner: ctx.accounts.owner.key(),
            amount,
            receipt_hash,
            provenance_root,
        });
        Ok(())
    }

    pub fn stake(ctx: Context<Stake>, amount: u64, lock_until: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount > 0, CovenantError::ZeroAmount);
        require!(ctx.accounts.agent.active, CovenantError::AgentInactive);

        let min_lock = ctx.accounts.config.min_stake_lock;
        if min_lock > 0 {
            let now = Clock::get()?.unix_timestamp.max(0) as u64;
            let min_unlock = now.checked_add(min_lock).ok_or(CovenantError::Overflow)?;
            require!(lock_until >= min_unlock, CovenantError::LockTooShort);
        }

        token_interface::transfer_checked(
            ctx.accounts.stake_transfer_ctx(),
            amount,
            ctx.accounts.covnt_mint.decimals,
        )?;

        let position = &mut ctx.accounts.position;
        position.agent_key = ctx.accounts.agent.agent_key;
        position.owner = ctx.accounts.owner.key();
        position.amount = amount;
        position.vault = ctx.accounts.stake_vault.key();
        position.lock_until = lock_until;
        position.active = true;
        position.bump = ctx.bumps.position;

        ctx.accounts.agent.stake = ctx
            .accounts
            .agent
            .stake
            .checked_add(amount)
            .ok_or(CovenantError::Overflow)?;

        emit!(StakeOpened {
            agent_key: position.agent_key,
            owner: position.owner,
            amount,
            lock_until,
            position: position.key(),
        });
        Ok(())
    }

    /// Owner-signed top-up and lock extension of a live position, so an
    /// operator can keep its stake counting past the current lock without
    /// unstaking. Adds `amount` (may be 0) to the vault and moves
    /// `lock_until` forward, never back. A moved lock must clear the same
    /// `min_stake_lock` floor as a fresh `stake`.
    pub fn extend_stake(ctx: Context<ExtendStake>, amount: u64, lock_until: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(ctx.accounts.agent.active, CovenantError::AgentInactive);
        require!(ctx.accounts.position.active, CovenantError::StakeInactive);
        let current_lock = ctx.accounts.position.lock_until;
        require!(lock_until >= current_lock, CovenantError::LockTooShort);
        require!(
            amount > 0 || lock_until > current_lock,
            CovenantError::ZeroAmount
        );

        let min_lock = ctx.accounts.config.min_stake_lock;
        if lock_until > current_lock && min_lock > 0 {
            let now = Clock::get()?.unix_timestamp.max(0) as u64;
            let min_unlock = now.checked_add(min_lock).ok_or(CovenantError::Overflow)?;
            require!(lock_until >= min_unlock, CovenantError::LockTooShort);
        }

        if amount > 0 {
            token_interface::transfer_checked(
                ctx.accounts.extend_transfer_ctx(),
                amount,
                ctx.accounts.covnt_mint.decimals,
            )?;
        }

        let position = &mut ctx.accounts.position;
        position.amount = position
            .amount
            .checked_add(amount)
            .ok_or(CovenantError::Overflow)?;
        position.lock_until = lock_until;
        ctx.accounts.agent.stake = ctx
            .accounts
            .agent
            .stake
            .checked_add(amount)
            .ok_or(CovenantError::Overflow)?;

        emit!(StakeExtended {
            agent_key: position.agent_key,
            owner: position.owner,
            added: amount,
            amount: position.amount,
            lock_until,
        });
        Ok(())
    }

    /// Owner-signed withdrawal of a staked position once `lock_until`
    /// has passed. Transfers the full position balance back to the
    /// owner's COVNT account, decrements `agent.stake`, and closes the
    /// position account (rent returned to the owner). Closing frees the
    /// canonical `[b"stake", agent_key, owner]` PDA so the owner can
    /// re-stake against the same agent afterwards.
    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(ctx.accounts.position.active, CovenantError::StakeInactive);
        require!(ctx.accounts.position.amount > 0, CovenantError::ZeroAmount);
        let now = Clock::get()?.unix_timestamp.max(0) as u64;
        require!(
            now >= ctx.accounts.position.lock_until,
            CovenantError::StakeLocked
        );

        let amount = ctx.accounts.position.amount;
        let agent_key = ctx.accounts.position.agent_key;
        let owner = ctx.accounts.position.owner;
        let signer_seeds: &[&[u8]] = &[
            b"stake",
            agent_key.as_ref(),
            owner.as_ref(),
            &[ctx.accounts.position.bump],
        ];
        token_interface::transfer_checked(
            ctx.accounts
                .unstake_transfer_ctx()
                .with_signer(&[signer_seeds]),
            amount,
            ctx.accounts.covnt_mint.decimals,
        )?;

        ctx.accounts.agent.stake = ctx
            .accounts
            .agent
            .stake
            .checked_sub(amount)
            .ok_or(CovenantError::InsufficientStake)?;

        emit!(StakeWithdrawn {
            agent_key,
            owner,
            amount,
            withdrawn_at: now,
        });
        Ok(())
    }

    pub fn slash_stake(ctx: Context<SlashStake>, amount: u64, reason_hash: [u8; 32]) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount > 0, CovenantError::ZeroAmount);
        require!(ctx.accounts.position.active, CovenantError::StakeInactive);
        require!(
            ctx.accounts.position.amount >= amount,
            CovenantError::InsufficientStake
        );

        let agent_key = ctx.accounts.position.agent_key;
        let owner = ctx.accounts.position.owner;
        let signer_seeds: &[&[u8]] = &[
            b"stake",
            agent_key.as_ref(),
            owner.as_ref(),
            &[ctx.accounts.position.bump],
        ];
        token_interface::transfer_checked(
            ctx.accounts
                .slash_transfer_ctx()
                .with_signer(&[signer_seeds]),
            amount,
            ctx.accounts.covnt_mint.decimals,
        )?;

        ctx.accounts.position.amount -= amount;
        ctx.accounts.agent.stake = ctx
            .accounts
            .agent
            .stake
            .checked_sub(amount)
            .ok_or(CovenantError::InsufficientStake)?;
        if ctx.accounts.position.amount == 0 {
            ctx.accounts.position.active = false;
        }

        emit!(StakeSlashed {
            agent_key,
            owner,
            amount,
            reason_hash,
        });
        Ok(())
    }

    /// Slash an agent's bond citing its on-chain actions. The reason is not
    /// supplied by the caller: it is read from the `provenance_root` of the
    /// operator's canonical credit account (`[b"credits", agent.operator]`), the
    /// hash-chain `consume_credits` folds gaslessly in the ER and commits to L1.
    /// So the reason is the operator's own committed record, not an arbitrary claim.
    ///
    /// Two limits the caller must know. The credit account must be undelegated:
    /// `Account<CreditAccount>` cannot load while owned by the delegation program,
    /// so an operator can defer this slash for up to the delegation timeout. And
    /// the binding is to the operator's canonical credit account, not a per-agent
    /// log, so it assumes the operator meters through that account. For a slash
    /// that depends on neither, use `slash_stake`.
    pub fn slash_for_actions(ctx: Context<SlashForActions>, amount: u64) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount > 0, CovenantError::ZeroAmount);
        require!(ctx.accounts.position.active, CovenantError::StakeInactive);
        require!(
            ctx.accounts.position.amount >= amount,
            CovenantError::InsufficientStake
        );

        // A slash "for actions" requires recorded actions: refuse to cite a
        // genesis (never-folded) provenance root.
        let reason_hash = ctx.accounts.credits.provenance_root;
        require!(reason_hash != [0u8; 32], CovenantError::NoRecordedActions);
        let agent_key = ctx.accounts.position.agent_key;
        let owner = ctx.accounts.position.owner;
        let signer_seeds: &[&[u8]] = &[
            b"stake",
            agent_key.as_ref(),
            owner.as_ref(),
            &[ctx.accounts.position.bump],
        ];
        token_interface::transfer_checked(
            ctx.accounts
                .slash_transfer_ctx()
                .with_signer(&[signer_seeds]),
            amount,
            ctx.accounts.covnt_mint.decimals,
        )?;

        ctx.accounts.position.amount -= amount;
        ctx.accounts.agent.stake = ctx
            .accounts
            .agent
            .stake
            .checked_sub(amount)
            .ok_or(CovenantError::InsufficientStake)?;
        if ctx.accounts.position.amount == 0 {
            ctx.accounts.position.active = false;
        }

        emit!(StakeSlashed {
            agent_key,
            owner,
            amount,
            reason_hash,
        });
        Ok(())
    }

    pub fn create_task(ctx: Context<CreateTask>, args: CreateTaskArgs) -> Result<()> {
        require!(cfg!(feature = "task-escrow"), CovenantError::TasksDisabled);
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(args.amount_covnt > 0, CovenantError::ZeroAmount);
        require!(ctx.accounts.agent.active, CovenantError::AgentInactive);

        token_interface::transfer_checked(
            ctx.accounts.task_fund_ctx(),
            args.amount_covnt,
            ctx.accounts.covnt_mint.decimals,
        )?;

        let task = &mut ctx.accounts.task;
        task.task_id = args.task_id;
        task.client = ctx.accounts.client.key();
        task.agent_key = ctx.accounts.agent.agent_key;
        task.provider = args.provider;
        task.amount_covnt = args.amount_covnt;
        task.task_hash = args.task_hash;
        task.criteria_hash = args.criteria_hash;
        task.deadline = args.deadline;
        task.status = TASK_FUNDED;
        task.bump = ctx.bumps.task;

        emit!(TaskCreated {
            task_id: task.task_id,
            client: task.client,
            agent_key: task.agent_key,
            provider: task.provider,
            amount_covnt: task.amount_covnt,
            task_hash: task.task_hash,
            criteria_hash: task.criteria_hash,
            deadline: task.deadline,
        });
        Ok(())
    }

    pub fn release_task(
        ctx: Context<ReleaseTask>,
        result_hash: [u8; 32],
        receipt_hash: [u8; 32],
    ) -> Result<()> {
        require!(cfg!(feature = "task-escrow"), CovenantError::TasksDisabled);
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(
            ctx.accounts.task.status == TASK_FUNDED,
            CovenantError::WrongTaskStatus
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            now <= ctx.accounts.task.deadline,
            CovenantError::TaskExpired
        );

        let task_id = ctx.accounts.task.task_id;
        let signer_seeds: &[&[u8]] = &[b"task", task_id.as_ref(), &[ctx.accounts.task.bump]];
        token_interface::transfer_checked(
            ctx.accounts.task_release_ctx().with_signer(&[signer_seeds]),
            ctx.accounts.task.amount_covnt,
            ctx.accounts.covnt_mint.decimals,
        )?;

        ctx.accounts.task.status = TASK_RELEASED;
        ctx.accounts.task.result_hash = result_hash;

        emit!(TaskReleased {
            task_id,
            provider: ctx.accounts.task.provider,
            amount_covnt: ctx.accounts.task.amount_covnt,
            result_hash,
            receipt_hash,
        });
        Ok(())
    }

    /// Refund the escrowed COVNT back to the client after the task
    /// deadline has passed. Only the client signs; the provider has no
    /// recourse here. Mirrors the escrow-agent norm where the funder
    /// recovers their funds when the counterparty failed to deliver in
    /// time. Pause check matches `release_task` so a paused protocol
    /// halts all escrow movement uniformly.
    pub fn refund_task(ctx: Context<RefundTask>) -> Result<()> {
        require!(cfg!(feature = "task-escrow"), CovenantError::TasksDisabled);
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(
            ctx.accounts.task.status == TASK_FUNDED,
            CovenantError::WrongTaskStatus
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            now > ctx.accounts.task.deadline,
            CovenantError::TaskNotExpired
        );

        let task_id = ctx.accounts.task.task_id;
        let signer_seeds: &[&[u8]] = &[b"task", task_id.as_ref(), &[ctx.accounts.task.bump]];
        token_interface::transfer_checked(
            ctx.accounts.task_refund_ctx().with_signer(&[signer_seeds]),
            ctx.accounts.task.amount_covnt,
            ctx.accounts.covnt_mint.decimals,
        )?;

        ctx.accounts.task.status = TASK_REFUNDED;

        emit!(TaskRefunded {
            task_id,
            client: ctx.accounts.task.client,
            amount_covnt: ctx.accounts.task.amount_covnt,
            deadline: ctx.accounts.task.deadline,
            refunded_at: now,
        });
        Ok(())
    }

    pub fn burn_covnt(ctx: Context<BurnCovnt>, amount: u64, reason_hash: [u8; 32]) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(amount > 0, CovenantError::ZeroAmount);

        token_interface::burn(ctx.accounts.burn_ctx(), amount)?;

        emit!(CovntBurned {
            owner: ctx.accounts.owner.key(),
            amount,
            reason_hash,
        });
        Ok(())
    }

    pub fn anchor_receipt_batch(
        ctx: Context<AnchorReceiptBatch>,
        args: AnchorReceiptBatchArgs,
    ) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(args.receipt_count > 0, CovenantError::ZeroAmount);

        let batch = &mut ctx.accounts.batch;
        batch.batch_id = args.batch_id;
        batch.authority = ctx.accounts.authority.key();
        batch.merkle_root = args.merkle_root;
        batch.receipt_count = args.receipt_count;
        batch.created_at = Clock::get()?.unix_timestamp;
        batch.bump = ctx.bumps.batch;

        emit!(ReceiptBatchAnchored {
            batch_id: batch.batch_id,
            authority: batch.authority,
            merkle_root: batch.merkle_root,
            receipt_count: batch.receipt_count,
            created_at: batch.created_at,
        });
        Ok(())
    }

    /// Owner-signed reclaim of a spent position account (rent returned to
    /// the owner). A position is spent once it is fully slashed
    /// (`amount == 0`, `active == false`); the normal exit path closes the
    /// position inside `unstake`. This exists so a fully-slashed owner can
    /// reclaim rent and re-stake against the same agent.
    pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
        require!(
            !ctx.accounts.position.active,
            CovenantError::StakeStillActive
        );
        emit!(StakePositionClosed {
            agent_key: ctx.accounts.position.agent_key,
            owner: ctx.accounts.position.owner,
        });
        Ok(())
    }

    pub fn update_authority(ctx: Context<UpdateConfig>, new_authority: Pubkey) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let previous = config.authority;
        config.authority = new_authority;
        emit!(AuthorityUpdated {
            previous,
            new_authority,
        });
        Ok(())
    }

    pub fn update_slash_authority(
        ctx: Context<UpdateConfig>,
        new_slash_authority: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let previous = config.slash_authority;
        config.slash_authority = new_slash_authority;
        emit!(SlashAuthorityUpdated {
            previous,
            new_slash_authority,
        });
        Ok(())
    }

    pub fn update_treasury(ctx: Context<UpdateTreasury>) -> Result<()> {
        let previous = ctx.accounts.config.treasury;
        ctx.accounts.config.treasury = ctx.accounts.treasury.key();
        emit!(TreasuryUpdated {
            previous,
            new_treasury: ctx.accounts.treasury.key(),
        });
        Ok(())
    }

    pub fn set_credits_per_covnt(ctx: Context<UpdateConfig>, credits_per_covnt: u64) -> Result<()> {
        require!(credits_per_covnt > 0, CovenantError::ZeroAmount);
        let config = &mut ctx.accounts.config;
        let previous = config.credits_per_covnt;
        config.credits_per_covnt = credits_per_covnt;
        emit!(CreditsRateUpdated {
            previous,
            credits_per_covnt,
        });
        Ok(())
    }

    pub fn set_min_stake_lock(ctx: Context<UpdateConfig>, min_stake_lock: u64) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let previous = config.min_stake_lock;
        config.min_stake_lock = min_stake_lock;
        emit!(MinStakeLockUpdated {
            previous,
            min_stake_lock,
        });
        Ok(())
    }

    /// One-time migration of a legacy `Config` (predates `min_stake_lock`) to
    /// the current layout: grows the account by 8 bytes and writes the field.
    /// Uses a raw account because the on-chain bytes cannot deserialize into
    /// the new struct until the realloc completes. Authority is checked by
    /// reading the on-chain `authority` field directly. Idempotent: re-running
    /// on a current-layout config just rewrites the value.
    pub fn migrate_config(ctx: Context<MigrateConfig>, min_stake_lock: u64) -> Result<()> {
        let info = ctx.accounts.config.to_account_info();
        {
            let data = info.try_borrow_data()?;
            require!(data.len() >= 40, CovenantError::Unauthorized);
            let onchain_authority =
                Pubkey::try_from(&data[8..40]).map_err(|_| error!(CovenantError::Unauthorized))?;
            require_keys_eq!(
                onchain_authority,
                ctx.accounts.authority.key(),
                CovenantError::Unauthorized
            );
        }

        let new_len = 8 + Config::INIT_SPACE;
        if info.data_len() < new_len {
            let deficit = Rent::get()?
                .minimum_balance(new_len)
                .saturating_sub(info.lamports());
            if deficit > 0 {
                anchor_lang::system_program::transfer(
                    CpiContext::new(
                        ctx.accounts.system_program.to_account_info(),
                        anchor_lang::system_program::Transfer {
                            from: ctx.accounts.authority.to_account_info(),
                            to: info.clone(),
                        },
                    ),
                    deficit,
                )?;
            }
            info.realloc(new_len, false)?;
        }

        let mut data = info.try_borrow_mut_data()?;
        let off = new_len - 8;
        data[off..new_len].copy_from_slice(&min_stake_lock.to_le_bytes());

        emit!(ConfigMigrated { min_stake_lock });
        Ok(())
    }

    /// One-time migration of a legacy `CreditAccount` (predates `provenance_root`)
    /// to the current layout: grows the account by 32 bytes. `realloc` zero-fills
    /// the new bytes, so the provenance root starts at genesis. Owner-gated by the
    /// `[b"credits", owner]` seed binding; idempotent (a no-op realloc on an
    /// already-current account).
    pub fn migrate_credit_account(ctx: Context<MigrateCreditAccount>) -> Result<()> {
        let info = ctx.accounts.credits.to_account_info();
        let new_len = 8 + CreditAccount::INIT_SPACE;
        if info.data_len() < new_len {
            let deficit = Rent::get()?
                .minimum_balance(new_len)
                .saturating_sub(info.lamports());
            if deficit > 0 {
                anchor_lang::system_program::transfer(
                    CpiContext::new(
                        ctx.accounts.system_program.to_account_info(),
                        anchor_lang::system_program::Transfer {
                            from: ctx.accounts.owner.to_account_info(),
                            to: info.clone(),
                        },
                    ),
                    deficit,
                )?;
            }
            info.realloc(new_len, true)?;
        }
        Ok(())
    }

    /// Delegate the caller's credit account `[b"credits", owner]` to the
    /// MagicBlock delegation program so metering (`consume_credits`) can run in
    /// an ephemeral rollup. Only the program-owned accounting PDA moves; no token
    /// custody is involved. Pass an ER validator pubkey as the first remaining
    /// account to pin it (see `DelegateConfig.validator`).
    #[cfg(feature = "ephemeral")]
    pub fn delegate_credits(ctx: Context<DelegateCredits>) -> Result<()> {
        let owner = ctx.accounts.payer.key();
        let (expected, _) = Pubkey::find_program_address(&[b"credits", owner.as_ref()], &crate::ID);
        require_keys_eq!(
            ctx.accounts.pda.key(),
            expected,
            CovenantError::Unauthorized
        );
        let validator = ctx
            .remaining_accounts
            .first()
            .ok_or(CovenantError::ValidatorRequired)?
            .key();
        ctx.accounts.delegate_pda(
            &ctx.accounts.payer,
            &[b"credits".as_ref(), owner.as_ref()],
            DelegateConfig {
                validator: Some(validator),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// Checkpoint the delegated credit account's state (including the
    /// provenance root) back to L1 without releasing it. Permissionless: any
    /// payer can force the record on-chain, since a commit moves no tokens and
    /// releases nothing. This is what lets a verifier or the slash authority
    /// surface an agent's provenance root without the owner's cooperation.
    #[cfg(feature = "ephemeral")]
    pub fn commit_credits(ctx: Context<CommitCreditsPermissionless>) -> Result<()> {
        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit(&[ctx.accounts.credits.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }

    /// Commit the final credit balance and undelegate, returning the account to
    /// L1 writability. Triggered before any L1 op that must move tokens against
    /// this owner (e.g. a top-up via `buy_credits`) or on idle timeout.
    #[cfg(feature = "ephemeral")]
    pub fn undelegate_credits(ctx: Context<CommitCredits>) -> Result<()> {
        MagicIntentBundleBuilder::new(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[ctx.accounts.credits.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }

    /// Open a GPU lease and escrow its whole window in one step.
    ///
    /// The renter signs, and in signing names the coordinator that may
    /// meter them and the rollup validator that may host that meter. Both
    /// are recorded, so a buyer reading the chain can see who was
    /// authorised before a single second was billed, and neither can be
    /// swapped afterwards.
    ///
    /// The renter is part of the lease address. Without that, anyone who
    /// learned a job id (the assigned operator learns it at dispatch)
    /// could occupy the address first with terms of their own, the real
    /// open would fail for good, and the session would quietly fall back
    /// to an off-chain meter.
    ///
    /// The protocol pause applies. This is the one lease instruction that
    /// takes new money, so it is the one a pause has to stop.
    pub fn open_lease(
        ctx: Context<OpenLease>,
        job_id: [u8; 16],
        rate_per_sec: u64,
        max_duration_secs: u64,
    ) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(rate_per_sec > 0, CovenantError::ZeroRate);
        require!(
            max_duration_secs > 0 && max_duration_secs <= MAX_LEASE_DURATION_SECS,
            CovenantError::BadDuration
        );
        let window = rate_per_sec
            .checked_mul(max_duration_secs)
            .ok_or(CovenantError::Overflow)?;

        // Escrow the whole window up front. Metering only ever decides how
        // this is split later; it can never ask for more.
        token_interface::transfer_checked(
            ctx.accounts.lease_fund_ctx(),
            window,
            ctx.accounts.mint.decimals,
        )?;

        // Record what the vault actually received, not what was asked for.
        // A mint that skims a transfer fee delivers less than the window,
        // and a settlement that tried to move the requested figure back out
        // would revert on every attempt, freezing the escrow for the life
        // of the lease.
        ctx.accounts.vault.reload()?;
        let funded_amount = ctx.accounts.vault.amount;
        require!(funded_amount > 0, CovenantError::EmptyDeposit);

        let opened_at = Clock::get()?.unix_timestamp;
        let terms = &mut ctx.accounts.terms;
        terms.job_id = job_id;
        terms.renter = ctx.accounts.renter.key();
        terms.operator = ctx.accounts.operator.key();
        terms.coordinator = ctx.accounts.coordinator.key();
        terms.er_validator = ctx.accounts.er_validator.key();
        terms.mint = ctx.accounts.mint.key();
        terms.rate_per_sec = rate_per_sec;
        terms.max_duration_secs = max_duration_secs;
        terms.funded_amount = funded_amount;
        terms.opened_at = opened_at;
        terms.paid_operator = false;
        terms.paid_renter = false;
        terms.voided = false;
        terms.delegated = false;
        terms.bump = ctx.bumps.terms;
        terms.meter_bump = ctx.bumps.meter;
        terms.vault_bump = ctx.bumps.vault;

        let meter = &mut ctx.accounts.meter;
        meter.terms = ctx.accounts.terms.key();
        meter.coordinator = ctx.accounts.coordinator.key();
        meter.job_id = job_id;
        meter.metered_ms = 0;
        meter.provenance_root = [0u8; 32];
        meter.concluded = false;
        meter.bump = ctx.bumps.meter;

        emit!(LeaseOpened {
            job_id,
            renter: ctx.accounts.renter.key(),
            operator: ctx.accounts.operator.key(),
            coordinator: ctx.accounts.coordinator.key(),
            er_validator: ctx.accounts.er_validator.key(),
            mint: ctx.accounts.mint.key(),
            rate_per_sec,
            max_duration_secs,
            funded_amount,
            opened_at,
        });
        Ok(())
    }

    /// Record the seconds served so far and fold the tick's receipt hash
    /// into the provenance chain. Runs in the ER, so a per-second meter
    /// costs nothing to keep.
    ///
    /// `metered_ms` is cumulative rather than a delta: a lost or duplicated
    /// tick then costs nothing, because settlement is always recomputed
    /// from the total elapsed. A tick that would go backwards is refused;
    /// the meter only ever moves forward.
    ///
    /// No money is computed here. The rollup host can rewrite anything in
    /// this account when it commits, so the rate and the escrow are kept on
    /// L1 and the charge is derived there at settlement. That is also why
    /// there is no pause check: the config PDA is not delegated and so is
    /// not readable where this instruction runs, and a meter tick moves
    /// nothing a pause would need to stop.
    pub fn tick_lease(
        ctx: Context<TickLease>,
        metered_ms: u64,
        receipt_hash: [u8; 32],
    ) -> Result<()> {
        let meter = &mut ctx.accounts.meter;
        require!(!meter.concluded, CovenantError::MeterClosed);
        require!(
            metered_ms >= meter.metered_ms,
            CovenantError::MeterWentBackwards
        );

        meter.metered_ms = metered_ms;
        meter.provenance_root =
            anchor_lang::solana_program::hash::hashv(&[&meter.provenance_root, &receipt_hash])
                .to_bytes();

        emit!(LeaseTicked {
            job_id: meter.job_id,
            metered_ms,
            receipt_hash,
            provenance_root: meter.provenance_root,
        });
        Ok(())
    }

    /// Pay the operator what the meter says and return the rest to the
    /// renter, in one transaction.
    ///
    /// Permissionless, but only once the lease is actually over: the meter
    /// came back from the rollup, the lease was voided, or the window the
    /// renter escrowed has elapsed. An unconditional door here would let a
    /// renter settle at zero in the slot after the open, before the
    /// coordinator's delegate lands, and keep a full session of compute for
    /// nothing.
    ///
    /// Deliberately outside the pause gate, as are the two claim paths and
    /// `void_lease`. Escrow that is already in the vault must stay
    /// recoverable; a pause that could strand it would turn an operational
    /// control into a freeze on user funds.
    pub fn settle_lease(ctx: Context<SettleLease>) -> Result<()> {
        let settlement = lease_settlement(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(
            !(ctx.accounts.terms.paid_operator && ctx.accounts.terms.paid_renter),
            CovenantError::LeaseAlreadySettled
        );

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let signer = &[seeds];
        let decimals = ctx.accounts.mint.decimals;

        let charged = if ctx.accounts.terms.paid_operator {
            0
        } else {
            settlement.charged.min(ctx.accounts.vault.amount)
        };
        if charged > 0 {
            token_interface::transfer_checked(
                ctx.accounts.operator_payout_ctx().with_signer(signer),
                charged,
                decimals,
            )?;
        }

        // Whatever is left is the renter's, including anything a third party
        // sent to the vault after the open. The vault is closed below, so a
        // residue would otherwise be locked behind an authority that will
        // never sign again.
        ctx.accounts.vault.reload()?;
        let refunded = if ctx.accounts.terms.paid_renter {
            0
        } else {
            ctx.accounts.vault.amount
        };
        if refunded > 0 {
            token_interface::transfer_checked(
                ctx.accounts.renter_payout_ctx().with_signer(signer),
                refunded,
                decimals,
            )?;
        }

        ctx.accounts.vault.reload()?;
        if ctx.accounts.vault.amount == 0 {
            token_interface::close_account(CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                CloseAccount {
                    account: ctx.accounts.vault.to_account_info(),
                    destination: ctx.accounts.renter.to_account_info(),
                    authority: ctx.accounts.terms.to_account_info(),
                },
                signer,
            ))?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_operator = true;
        terms.paid_renter = true;
        emit!(LeaseSettled {
            job_id: terms.job_id,
            metered_ms: settlement.metered_ms,
            charged,
            refunded,
            provenance_root: settlement.provenance_root,
            voided: terms.voided,
        });
        Ok(())
    }

    /// Pay the operator's share on its own.
    ///
    /// The two payouts are separable because one blocked destination must
    /// not hold the other side's money. A settlement mint with a live
    /// freeze authority, or a party that simply closed its token account,
    /// would otherwise revert every settlement and strand the whole escrow
    /// rather than just that party's share.
    pub fn claim_operator_share(ctx: Context<ClaimOperatorShare>) -> Result<()> {
        let settlement = lease_settlement(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(
            !ctx.accounts.terms.paid_operator,
            CovenantError::LeaseShareAlreadyPaid
        );

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let amount = settlement.charged.min(ctx.accounts.vault.amount);
        if amount > 0 {
            token_interface::transfer_checked(
                ctx.accounts.operator_payout_ctx().with_signer(&[seeds]),
                amount,
                ctx.accounts.mint.decimals,
            )?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_operator = true;
        emit!(LeaseOperatorPaid {
            job_id: terms.job_id,
            operator: terms.operator,
            metered_ms: settlement.metered_ms,
            amount,
        });
        Ok(())
    }

    /// Return the renter's remainder on its own. The operator's share stays
    /// reserved in the vault until it is claimed, so calling this first
    /// cannot take the money out from under them.
    pub fn claim_renter_refund(ctx: Context<ClaimRenterRefund>) -> Result<()> {
        let settlement = lease_settlement(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(
            !ctx.accounts.terms.paid_renter,
            CovenantError::LeaseShareAlreadyPaid
        );

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let reserved = if ctx.accounts.terms.paid_operator {
            0
        } else {
            settlement.charged
        };
        let amount = ctx.accounts.vault.amount.saturating_sub(reserved);
        if amount > 0 {
            token_interface::transfer_checked(
                ctx.accounts.renter_payout_ctx().with_signer(&[seeds]),
                amount,
                ctx.accounts.mint.decimals,
            )?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_renter = true;
        emit!(LeaseRenterRefunded {
            job_id: terms.job_id,
            renter: terms.renter,
            amount,
        });
        Ok(())
    }

    /// Cancel the charge and open settlement immediately: the renter gets
    /// the whole vault back and the operator gets nothing.
    ///
    /// The marketplace has terminal paths (a deadline that expired, a
    /// receipt that came back failed, an expired session swept) where the
    /// buyer is refunded in full off-chain. The meter is monotonic and
    /// cannot be wound back, so without this a lease that took ticks on one
    /// of those paths would still pay the operator on-chain for work the
    /// marketplace already refused to bill, and the escrow would sit funded
    /// until someone unwound it by hand.
    ///
    /// Coordinator-signed, which grants no power it did not already have:
    /// the party that decides what the meter says can already decide it
    /// says zero.
    pub fn void_lease(ctx: Context<VoidLease>) -> Result<()> {
        let terms = &mut ctx.accounts.terms;
        require!(
            !(terms.paid_operator && terms.paid_renter),
            CovenantError::LeaseAlreadySettled
        );
        require!(!terms.voided, CovenantError::LeaseAlreadyVoided);
        terms.voided = true;
        emit!(LeaseVoided {
            job_id: terms.job_id,
            coordinator: terms.coordinator,
        });
        Ok(())
    }

    /// Hand the meter to the rollup validator the renter pinned at open so
    /// ticks can run there.
    ///
    /// Coordinator-signed and validator-checked. Delegation is the act of
    /// giving an account to a third party that then writes its state back,
    /// so an open door here is an unauthenticated transfer of the session's
    /// meter to a host of the caller's choosing, including one that does
    /// not exist, which would leave the meter unreachable for the rest of
    /// the lease.
    ///
    /// The protocol pause applies. Stopping new opens does not stop leases
    /// opened a minute earlier from being handed to a rollup, so if the
    /// delegation route itself is the thing going wrong, this is the only
    /// lever that closes it. A lease blocked here still settles: the window
    /// expires, the meter never left L1, the charge is zero and the renter
    /// is refunded in full.
    #[cfg(feature = "ephemeral")]
    pub fn delegate_lease(ctx: Context<DelegateLease>) -> Result<()> {
        require!(!ctx.accounts.config.paused, CovenantError::ProtocolPaused);
        require!(!ctx.accounts.terms.voided, CovenantError::LeaseIsVoided);
        require!(
            !(ctx.accounts.terms.paid_operator && ctx.accounts.terms.paid_renter),
            CovenantError::LeaseAlreadySettled
        );
        let terms_key = ctx.accounts.terms.key();
        let validator = ctx.accounts.er_validator.key();
        ctx.accounts.delegate_meter(
            &ctx.accounts.payer,
            &[b"meter".as_ref(), terms_key.as_ref()],
            DelegateConfig {
                validator: Some(validator),
                ..Default::default()
            },
        )?;
        ctx.accounts.terms.delegated = true;
        Ok(())
    }

    /// Close the meter and commit it back to L1, which is what makes
    /// settlement possible.
    ///
    /// The `concluded` flag rides the same commit. Without it the meter is
    /// writable again the moment it lands on L1, and the coordinator could
    /// raise the elapsed after the renter has already reconciled the
    /// committed figure and before anyone settles it.
    ///
    /// Coordinator-signed, for the same reason ticking is: ending the meter
    /// early is worth exactly as much as under-reporting it, so a renter
    /// must not be able to cut a live session's meter two seconds in and
    /// settle for one tick of a window they are still using.
    ///
    /// The meter arrives raw and is typed by hand. `commit_and_undelegate`
    /// hands the account to the delegation program inside this instruction,
    /// so Anchor's automatic write-back would land after the handover and
    /// the rollup rejects it as a write to an account this program no
    /// longer owns. Serializing before the CPI is also what puts
    /// `concluded` into the bytes that get committed.
    #[cfg(feature = "ephemeral")]
    pub fn undelegate_lease(ctx: Context<UndelegateLease>) -> Result<()> {
        let meter_info = ctx.accounts.meter.to_account_info();
        let mut meter = LeaseMeter::try_deserialize(&mut &meter_info.try_borrow_data()?[..])?;
        require_keys_eq!(
            meter.coordinator,
            ctx.accounts.coordinator.key(),
            CovenantError::Unauthorized
        );
        meter.concluded = true;

        let mut buf = Vec::with_capacity(8 + LeaseMeter::INIT_SPACE);
        meter.try_serialize(&mut buf)?;
        meter_info.try_borrow_mut_data()?[..buf.len()].copy_from_slice(&buf);

        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[meter_info])
        .build_and_invoke()?;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [b"config"],
        bump,
    )]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    #[account(
        constraint = treasury.mint == covnt_mint.key() @ CovenantError::WrongMint,
    )]
    pub treasury: InterfaceAccount<'info, TokenAccount>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetPause<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(args: RegisterAgentArgs)]
pub struct RegisterAgent<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = operator,
        space = 8 + Agent::INIT_SPACE,
        seeds = [b"agent", args.agent_key.as_ref()],
        bump,
    )]
    pub agent: Account<'info, Agent>,
    #[account(mut)]
    pub operator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetAgentActive<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"agent", agent.agent_key.as_ref()],
        bump = agent.bump,
    )]
    pub agent: Account<'info, Agent>,
}

#[derive(Accounts)]
pub struct OpenCreditAccount<'info> {
    #[account(
        init,
        payer = owner,
        space = 8 + CreditAccount::INIT_SPACE,
        seeds = [b"credits", owner.key().as_ref()],
        bump,
    )]
    pub credits: Account<'info, CreditAccount>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct BuyCredits<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = treasury,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        has_one = owner,
        seeds = [b"credits", owner.key().as_ref()],
        bump = credits.bump,
    )]
    pub credits: Account<'info, CreditAccount>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        mut,
        constraint = owner_covnt.owner == owner.key() @ CovenantError::Unauthorized,
        constraint = owner_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub owner_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, constraint = treasury.mint == config.covnt_mint @ CovenantError::WrongMint)]
    pub treasury: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> BuyCredits<'info> {
    fn buy_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.owner_covnt.to_account_info(),
                to: self.treasury.to_account_info(),
                authority: self.owner.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ConsumeCredits<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        has_one = owner,
        seeds = [b"credits", owner.key().as_ref()],
        bump = credits.bump,
    )]
    pub credits: Account<'info, CreditAccount>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"agent", agent.agent_key.as_ref()],
        bump = agent.bump,
    )]
    pub agent: Account<'info, Agent>,
    #[account(
        init,
        payer = owner,
        space = 8 + StakePosition::INIT_SPACE,
        seeds = [b"stake", agent.agent_key.as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub position: Account<'info, StakePosition>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        mut,
        constraint = owner_covnt.owner == owner.key() @ CovenantError::Unauthorized,
        constraint = owner_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub owner_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = stake_vault.owner == position.key() @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

impl<'info> Stake<'info> {
    fn stake_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.owner_covnt.to_account_info(),
                to: self.stake_vault.to_account_info(),
                authority: self.owner.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"agent", position.agent_key.as_ref()],
        bump = agent.bump,
        constraint = agent.agent_key == position.agent_key @ CovenantError::AgentMismatch,
    )]
    pub agent: Account<'info, Agent>,
    #[account(
        mut,
        seeds = [b"stake", position.agent_key.as_ref(), position.owner.as_ref()],
        bump = position.bump,
        constraint = position.owner == owner.key() @ CovenantError::Unauthorized,
        close = owner,
    )]
    pub position: Account<'info, StakePosition>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        mut,
        constraint = stake_vault.key() == position.vault @ CovenantError::Unauthorized,
        constraint = stake_vault.owner == position.key() @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = owner_covnt.owner == owner.key() @ CovenantError::Unauthorized,
        constraint = owner_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub owner_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> Unstake<'info> {
    fn unstake_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.stake_vault.to_account_info(),
                to: self.owner_covnt.to_account_info(),
                authority: self.position.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ExtendStake<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"agent", position.agent_key.as_ref()],
        bump = agent.bump,
        constraint = agent.agent_key == position.agent_key @ CovenantError::AgentMismatch,
    )]
    pub agent: Account<'info, Agent>,
    #[account(
        mut,
        seeds = [b"stake", position.agent_key.as_ref(), position.owner.as_ref()],
        bump = position.bump,
        constraint = position.owner == owner.key() @ CovenantError::Unauthorized,
    )]
    pub position: Account<'info, StakePosition>,
    pub owner: Signer<'info>,
    #[account(
        mut,
        constraint = owner_covnt.owner == owner.key() @ CovenantError::Unauthorized,
        constraint = owner_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub owner_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = stake_vault.key() == position.vault @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> ExtendStake<'info> {
    fn extend_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.owner_covnt.to_account_info(),
                to: self.stake_vault.to_account_info(),
                authority: self.owner.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct SlashStake<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = slash_authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    pub slash_authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"agent", agent.agent_key.as_ref()],
        bump = agent.bump,
        constraint = agent.agent_key == position.agent_key @ CovenantError::AgentMismatch,
    )]
    pub agent: Account<'info, Agent>,
    #[account(
        mut,
        seeds = [b"stake", position.agent_key.as_ref(), position.owner.as_ref()],
        bump = position.bump,
    )]
    pub position: Account<'info, StakePosition>,
    #[account(
        mut,
        constraint = stake_vault.key() == position.vault @ CovenantError::Unauthorized,
        constraint = stake_vault.owner == position.key() @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = slash_vault.key() == config.treasury @ CovenantError::Unauthorized,
        constraint = slash_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub slash_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> SlashStake<'info> {
    fn slash_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.stake_vault.to_account_info(),
                to: self.slash_vault.to_account_info(),
                authority: self.position.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct SlashForActions<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = slash_authority @ CovenantError::Unauthorized,
    )]
    pub config: Box<Account<'info, Config>>,
    pub slash_authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"agent", agent.agent_key.as_ref()],
        bump = agent.bump,
        constraint = agent.agent_key == position.agent_key @ CovenantError::AgentMismatch,
    )]
    pub agent: Box<Account<'info, Agent>>,
    #[account(
        mut,
        seeds = [b"stake", position.agent_key.as_ref(), position.owner.as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, StakePosition>>,
    /// The agent's credit account, bound to the agent by its operator. Its
    /// `provenance_root` is read as the slash reason, so the slash can only cite
    /// the agent's own verifiable on-chain record.
    #[account(
        seeds = [b"credits", agent.operator.as_ref()],
        bump = credits.bump,
    )]
    pub credits: Box<Account<'info, CreditAccount>>,
    #[account(
        mut,
        constraint = stake_vault.key() == position.vault @ CovenantError::Unauthorized,
        constraint = stake_vault.owner == position.key() @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = slash_vault.key() == config.treasury @ CovenantError::Unauthorized,
        constraint = slash_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub slash_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> SlashForActions<'info> {
    fn slash_transfer_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.stake_vault.to_account_info(),
                to: self.slash_vault.to_account_info(),
                authority: self.position.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
#[instruction(args: CreateTaskArgs)]
pub struct CreateTask<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        seeds = [b"agent", agent.agent_key.as_ref()],
        bump = agent.bump,
    )]
    pub agent: Account<'info, Agent>,
    #[account(
        init,
        payer = client,
        space = 8 + Task::INIT_SPACE,
        seeds = [b"task", args.task_id.as_ref()],
        bump,
    )]
    pub task: Box<Account<'info, Task>>,
    #[account(mut)]
    pub client: Signer<'info>,
    #[account(
        mut,
        constraint = client_covnt.owner == client.key() @ CovenantError::Unauthorized,
        constraint = client_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub client_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = escrow_vault.owner == task.key() @ CovenantError::Unauthorized,
        constraint = escrow_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub escrow_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateTask<'info> {
    fn task_fund_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.client_covnt.to_account_info(),
                to: self.escrow_vault.to_account_info(),
                authority: self.client.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ReleaseTask<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"task", task.task_id.as_ref()],
        bump = task.bump,
        has_one = client @ CovenantError::Unauthorized,
    )]
    pub task: Box<Account<'info, Task>>,
    pub client: Signer<'info>,
    #[account(
        mut,
        constraint = escrow_vault.owner == task.key() @ CovenantError::Unauthorized,
        constraint = escrow_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub escrow_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = provider_covnt.owner == task.provider @ CovenantError::Unauthorized,
        constraint = provider_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub provider_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> ReleaseTask<'info> {
    fn task_release_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.escrow_vault.to_account_info(),
                to: self.provider_covnt.to_account_info(),
                authority: self.task.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct RefundTask<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"task", task.task_id.as_ref()],
        bump = task.bump,
        has_one = client @ CovenantError::Unauthorized,
    )]
    pub task: Box<Account<'info, Task>>,
    pub client: Signer<'info>,
    #[account(
        mut,
        constraint = escrow_vault.owner == task.key() @ CovenantError::Unauthorized,
        constraint = escrow_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub escrow_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        constraint = client_covnt.owner == client.key() @ CovenantError::Unauthorized,
        constraint = client_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub client_covnt: InterfaceAccount<'info, TokenAccount>,
    #[account(constraint = covnt_mint.key() == config.covnt_mint @ CovenantError::WrongMint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> RefundTask<'info> {
    fn task_refund_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.covnt_mint.to_account_info(),
                from: self.escrow_vault.to_account_info(),
                to: self.client_covnt.to_account_info(),
                authority: self.task.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct BurnCovnt<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, address = config.covnt_mint)]
    pub covnt_mint: InterfaceAccount<'info, Mint>,
    #[account(
        mut,
        constraint = owner_covnt.owner == owner.key() @ CovenantError::Unauthorized,
        constraint = owner_covnt.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub owner_covnt: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> BurnCovnt<'info> {
    fn burn_ctx(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            Burn {
                mint: self.covnt_mint.to_account_info(),
                from: self.owner_covnt.to_account_info(),
                authority: self.owner.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
#[instruction(args: AnchorReceiptBatchArgs)]
pub struct AnchorReceiptBatch<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = authority,
        space = 8 + ReceiptBatch::INIT_SPACE,
        seeds = [b"receipt_batch", args.batch_id.as_ref()],
        bump,
    )]
    pub batch: Account<'info, ReceiptBatch>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(
        mut,
        seeds = [b"stake", position.agent_key.as_ref(), position.owner.as_ref()],
        bump = position.bump,
        constraint = position.owner == owner.key() @ CovenantError::Unauthorized,
        close = owner,
    )]
    pub position: Account<'info, StakePosition>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateTreasury<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ CovenantError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
    #[account(constraint = treasury.mint == config.covnt_mint @ CovenantError::WrongMint)]
    pub treasury: InterfaceAccount<'info, TokenAccount>,
}

#[derive(Accounts)]
pub struct MigrateConfig<'info> {
    /// CHECK: config PDA validated by seeds; deserialized manually because the
    /// legacy on-chain bytes do not fit the current `Config` layout.
    #[account(mut, seeds = [b"config"], bump)]
    pub config: UncheckedAccount<'info>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

/// Migrate a legacy `CreditAccount` to the current layout. Raw account because
/// the legacy bytes do not fit the new struct until realloc; the owner is
/// validated against the on-chain `owner` field in the handler.
#[derive(Accounts)]
pub struct MigrateCreditAccount<'info> {
    /// CHECK: raw bytes (the legacy layout cannot deserialize until realloc); the
    /// `[b"credits", owner]` seeds bind it to the signing owner.
    #[account(mut, seeds = [b"credits", owner.key().as_ref()], bump)]
    pub credits: UncheckedAccount<'info>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

/// Delegate the credit-account PDA to the ER. `#[delegate]` adds the
/// `delegate_pda` helper plus the buffer/record/metadata accounts and the
/// delegation + owner programs. The PDA is passed unchecked because delegation
/// transfers its ownership to the delegation program.
#[cfg(feature = "ephemeral")]
#[delegate]
#[derive(Accounts)]
pub struct DelegateCredits<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the `[b"credits", payer]` PDA, validated by the seeds passed to
    /// `delegate_pda`.
    #[account(mut, del)]
    pub pda: UncheckedAccount<'info>,
}

/// Undelegate the delegated credit account back to L1 writability. `#[commit]`
/// injects `magic_context` and `magic_program`. Owner-gated: returning the
/// account to L1 is the owner's (or the recovery keeper's) call.
#[cfg(feature = "ephemeral")]
#[commit]
#[derive(Accounts)]
pub struct CommitCredits<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        mut,
        has_one = owner @ CovenantError::Unauthorized,
        seeds = [b"credits", owner.key().as_ref()],
        bump = credits.bump,
    )]
    pub credits: Account<'info, CreditAccount>,
}

/// Permissionless commit of the delegated credit account. Any `payer` funds
/// the checkpoint; the credit PDA is validated against its own stored `owner`,
/// so no owner signature is needed. A commit only writes the current ER state
/// to L1 (no token movement, no release), so opening it lets a verifier or
/// keeper force an agent's provenance root on-chain.
#[cfg(feature = "ephemeral")]
#[commit]
#[derive(Accounts)]
pub struct CommitCreditsPermissionless<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"credits", credits.owner.as_ref()],
        bump = credits.bump,
    )]
    pub credits: Account<'info, CreditAccount>,
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 16])]
pub struct OpenLease<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mut)]
    pub renter: Signer<'info>,
    /// CHECK: recorded as the payout destination's owner; never signs.
    pub operator: UncheckedAccount<'info>,
    /// CHECK: recorded as the only key that may meter this lease. The renter
    /// is agreeing here to who observes them, so it is named at open and
    /// fixed for the life of the lease.
    pub coordinator: UncheckedAccount<'info>,
    /// CHECK: recorded as the only rollup identity the meter may be
    /// delegated to.
    pub er_validator: UncheckedAccount<'info>,
    #[account(
        init,
        payer = renter,
        space = 8 + LeaseTerms::INIT_SPACE,
        seeds = [b"lease", renter.key().as_ref(), job_id.as_ref()],
        bump,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    #[account(
        init,
        payer = renter,
        space = 8 + LeaseMeter::INIT_SPACE,
        seeds = [b"meter", terms.key().as_ref()],
        bump,
    )]
    pub meter: Box<Account<'info, LeaseMeter>>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    /// A PDA rather than an associated token account: only this program can
    /// create an account at this address, so the vault cannot be created
    /// ahead of the open to make the open fail.
    #[account(
        init,
        payer = renter,
        seeds = [b"vault", terms.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = terms,
        token::token_program = token_program,
    )]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = renter)]
    pub renter_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

impl<'info> OpenLease<'info> {
    fn lease_fund_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.mint.to_account_info(),
                from: self.renter_tokens.to_account_info(),
                to: self.vault.to_account_info(),
                authority: self.renter.to_account_info(),
            },
        )
    }
}

/// The meter tick, signed by the coordinator the renter named at open.
///
/// This is the whole authority check. The account is reachable by anyone who
/// can send a transaction to the rollup, and its address is derivable from a
/// job id the operator learns at dispatch, so without a signature bound to the
/// lease a stranger could drive the meter to the full escrowed window and
/// settle it.
#[derive(Accounts)]
pub struct TickLease<'info> {
    #[account(mut, has_one = coordinator @ CovenantError::Unauthorized)]
    pub meter: Account<'info, LeaseMeter>,
    pub coordinator: Signer<'info>,
}

#[derive(Accounts)]
pub struct SettleLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint @ CovenantError::WrongMint,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    /// CHECK: pinned to this lease by its seeds. Left raw because a meter that
    /// is still delegated is owned by the delegation program; `lease_settlement`
    /// reads it only when this program owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    /// CHECK: the renter's account, credited the vault's rent when the vault is
    /// closed. They paid it at open.
    #[account(mut, address = terms.renter)]
    pub renter: UncheckedAccount<'info>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = terms.operator)]
    pub operator_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = terms.renter)]
    pub renter_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> SettleLease<'info> {
    fn operator_payout_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.mint.to_account_info(),
                from: self.vault.to_account_info(),
                to: self.operator_tokens.to_account_info(),
                authority: self.terms.to_account_info(),
            },
        )
    }

    fn renter_payout_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.mint.to_account_info(),
                from: self.vault.to_account_info(),
                to: self.renter_tokens.to_account_info(),
                authority: self.terms.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ClaimOperatorShare<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint @ CovenantError::WrongMint,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    /// CHECK: pinned to this lease by its seeds; read only when this program
    /// owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = terms.operator)]
    pub operator_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> ClaimOperatorShare<'info> {
    fn operator_payout_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.mint.to_account_info(),
                from: self.vault.to_account_info(),
                to: self.operator_tokens.to_account_info(),
                authority: self.terms.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ClaimRenterRefund<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint @ CovenantError::WrongMint,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    /// CHECK: pinned to this lease by its seeds; read only when this program
    /// owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = terms.renter)]
    pub renter_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> ClaimRenterRefund<'info> {
    fn renter_payout_ctx(&self) -> CpiContext<'_, '_, '_, 'info, TransferChecked<'info>> {
        CpiContext::new(
            self.token_program.to_account_info(),
            TransferChecked {
                mint: self.mint.to_account_info(),
                from: self.vault.to_account_info(),
                to: self.renter_tokens.to_account_info(),
                authority: self.terms.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct VoidLease<'info> {
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = coordinator @ CovenantError::Unauthorized,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    pub coordinator: Signer<'info>,
}

/// Delegate a lease meter to the ER. Mirrors `DelegateCredits`: `#[delegate]`
/// adds the `delegate_meter` helper plus the buffer/record/metadata accounts
/// and the delegation + owner programs. Only the meter moves; the terms and the
/// escrow vault stay on L1 and stay this program's.
#[cfg(feature = "ephemeral")]
#[delegate]
#[derive(Accounts)]
pub struct DelegateLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = coordinator @ CovenantError::Unauthorized,
        has_one = er_validator @ CovenantError::Unauthorized,
    )]
    pub terms: Box<Account<'info, LeaseTerms>>,
    pub coordinator: Signer<'info>,
    /// CHECK: the rollup identity recorded at open; matched by `has_one`.
    pub er_validator: UncheckedAccount<'info>,
    /// CHECK: the lease's meter, pinned by its seeds. Raw because delegation
    /// zeroes the account and reassigns its owner, which a typed account would
    /// try to write back over.
    #[account(mut, del, seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
}

/// Commit a lease meter back to L1 and release it. `#[commit]` injects
/// `magic_context` and `magic_program`.
#[cfg(feature = "ephemeral")]
#[commit]
#[derive(Accounts)]
pub struct UndelegateLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: owned by this program and typed by hand in the handler, which
    /// also checks the coordinator. It cannot be an `Account<LeaseMeter>`:
    /// Anchor would write it back after the commit has already handed it to the
    /// delegation program.
    #[account(mut, owner = crate::ID)]
    pub meter: UncheckedAccount<'info>,
    pub coordinator: Signer<'info>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeArgs {
    pub slash_authority: Pubkey,
    pub credits_per_covnt: u64,
    pub min_stake_lock: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RegisterAgentArgs {
    pub agent_key: [u8; 32],
    pub metadata_hash: [u8; 32],
    pub capability_hash: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateTaskArgs {
    pub task_id: [u8; 32],
    pub provider: Pubkey,
    pub amount_covnt: u64,
    pub task_hash: [u8; 32],
    pub criteria_hash: [u8; 32],
    pub deadline: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct AnchorReceiptBatchArgs {
    pub batch_id: [u8; 32],
    pub merkle_root: [u8; 32],
    pub receipt_count: u32,
}

pub const TASK_FUNDED: u8 = 1;
pub const TASK_RELEASED: u8 = 2;
pub const TASK_REFUNDED: u8 = 3;

/// A lease may not reserve more than a day. Bounds how much of a renter's
/// balance one signature can lock, and keeps the meter's arithmetic far from
/// overflow.
pub const MAX_LEASE_DURATION_SECS: u64 = 86_400;

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub authority: Pubkey,
    pub slash_authority: Pubkey,
    pub covnt_mint: Pubkey,
    pub treasury: Pubkey,
    pub credits_per_covnt: u64,
    pub paused: bool,
    pub bump: u8,
    /// Minimum seconds a stake must remain locked past the staking instant.
    /// `0` disables the floor (a staker may pick any `lock_until`). Appended
    /// last so legacy 146-byte configs migrate by realloc (see `migrate_config`).
    pub min_stake_lock: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Agent {
    pub agent_key: [u8; 32],
    pub operator: Pubkey,
    pub metadata_hash: [u8; 32],
    pub capability_hash: [u8; 32],
    pub stake: u64,
    pub reputation: u64,
    pub active: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct CreditAccount {
    pub owner: Pubkey,
    pub balance: u64,
    pub bump: u8,
    /// Rolling hash-chain root over every consumed `receipt_hash`:
    /// `root = sha256(root || receipt_hash)`, genesis = 32 zero bytes. Updated
    /// on each `consume_credits` (gaslessly in the ER) and committed to L1 with
    /// the balance, so it is a real-time, on-chain provenance record of the
    /// metered actions. Appended last so legacy accounts migrate by realloc
    /// (see `migrate_credit_account`).
    pub provenance_root: [u8; 32],
}

#[account]
#[derive(InitSpace)]
pub struct StakePosition {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub amount: u64,
    pub lock_until: u64,
    pub vault: Pubkey,
    pub active: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Task {
    pub task_id: [u8; 32],
    pub client: Pubkey,
    pub agent_key: [u8; 32],
    pub provider: Pubkey,
    pub amount_covnt: u64,
    pub task_hash: [u8; 32],
    pub criteria_hash: [u8; 32],
    pub result_hash: [u8; 32],
    pub deadline: i64,
    pub status: u8,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct ReceiptBatch {
    pub batch_id: [u8; 32],
    pub authority: Pubkey,
    pub merkle_root: [u8; 32],
    pub receipt_count: u32,
    pub created_at: i64,
    pub bump: u8,
}

/// Everything about a GPU lease that decides money. Never delegated, so the
/// rollup host that writes the meter cannot name itself the operator, raise the
/// rate, or move the escrow.
///
/// Amounts are in the escrow mint's smallest unit. The mint is the renter's
/// choice and is recorded here rather than on `Config`, so a lease can settle
/// in USDC while everything COVNT-denominated keeps its own pinning.
#[account]
#[derive(InitSpace)]
pub struct LeaseTerms {
    /// The coordinator's job id, as raw uuid bytes: the same identifier the
    /// signed work receipt carries, so a reader can line the two up.
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub operator: Pubkey,
    /// The only key that may meter, delegate, conclude or void this lease.
    /// Named by the renter at open.
    pub coordinator: Pubkey,
    /// The only rollup identity the meter may be delegated to.
    pub er_validator: Pubkey,
    pub mint: Pubkey,
    pub rate_per_sec: u64,
    pub max_duration_secs: u64,
    /// What the vault actually received at open, which is not necessarily what
    /// was asked for.
    pub funded_amount: u64,
    /// Unix seconds at open. The window ends `max_duration_secs` later, and
    /// that is what makes permissionless settlement safe.
    pub opened_at: i64,
    pub paid_operator: bool,
    pub paid_renter: bool,
    /// Set when the marketplace refused to bill the session at all: the charge
    /// becomes zero and the whole vault goes back to the renter.
    pub voided: bool,
    /// Set the first time the meter is handed to the rollup. A meter this
    /// program owns again after that has been through the rollup and come back,
    /// which is the second, independent signal that the lease is over.
    pub delegated: bool,
    pub bump: u8,
    pub meter_bump: u8,
    pub vault_bump: u8,
}

/// The part of a lease that runs in the rollup. Elapsed time and a hash chain,
/// and nothing a host could rewrite into a payout.
#[account]
#[derive(InitSpace)]
pub struct LeaseMeter {
    pub terms: Pubkey,
    /// Carried alongside the terms so a tick can be authenticated inside the
    /// rollup, where the terms account is not present.
    pub coordinator: Pubkey,
    pub job_id: [u8; 16],
    /// Cumulative session time the coordinator has observed.
    pub metered_ms: u64,
    /// `root = sha256(root || receipt_hash)` over every tick, genesis 32 zero
    /// bytes. Committed to L1 with the elapsed, so the settled amount arrives
    /// with a replayable record of how it was reached.
    pub provenance_root: [u8; 32],
    /// Set by the undelegate that commits this meter to L1. A concluded meter
    /// takes no further ticks, so the figure a renter reconciles at commit time
    /// is the figure that settles.
    pub concluded: bool,
    pub bump: u8,
}

#[event]
pub struct ProtocolInitialized {
    pub authority: Pubkey,
    pub slash_authority: Pubkey,
    pub covnt_mint: Pubkey,
    pub treasury: Pubkey,
    pub credits_per_covnt: u64,
}

#[event]
pub struct ProtocolPauseUpdated {
    pub paused: bool,
}

#[event]
pub struct AgentRegistered {
    pub agent_key: [u8; 32],
    pub operator: Pubkey,
    pub metadata_hash: [u8; 32],
    pub capability_hash: [u8; 32],
}

#[event]
pub struct AgentStatusUpdated {
    pub agent_key: [u8; 32],
    pub active: bool,
}

#[event]
pub struct CreditAccountOpened {
    pub owner: Pubkey,
    pub credit_account: Pubkey,
}

#[event]
pub struct CreditsPurchased {
    pub owner: Pubkey,
    pub amount_covnt: u64,
    pub credits: u64,
}

#[event]
pub struct CreditsConsumed {
    pub owner: Pubkey,
    pub amount: u64,
    pub receipt_hash: [u8; 32],
    pub provenance_root: [u8; 32],
}

#[event]
pub struct StakeWithdrawn {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub amount: u64,
    pub withdrawn_at: u64,
}

#[event]
pub struct StakeOpened {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub amount: u64,
    pub lock_until: u64,
    pub position: Pubkey,
}

#[event]
pub struct StakeExtended {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub added: u64,
    /// The position's total after the top-up.
    pub amount: u64,
    pub lock_until: u64,
}

#[event]
pub struct StakeSlashed {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub amount: u64,
    pub reason_hash: [u8; 32],
}

#[event]
pub struct TaskCreated {
    pub task_id: [u8; 32],
    pub client: Pubkey,
    pub agent_key: [u8; 32],
    pub provider: Pubkey,
    pub amount_covnt: u64,
    pub task_hash: [u8; 32],
    pub criteria_hash: [u8; 32],
    pub deadline: i64,
}

#[event]
pub struct TaskReleased {
    pub task_id: [u8; 32],
    pub provider: Pubkey,
    pub amount_covnt: u64,
    pub result_hash: [u8; 32],
    pub receipt_hash: [u8; 32],
}

#[event]
pub struct TaskRefunded {
    pub task_id: [u8; 32],
    pub client: Pubkey,
    pub amount_covnt: u64,
    pub deadline: i64,
    pub refunded_at: i64,
}

#[event]
pub struct CovntBurned {
    pub owner: Pubkey,
    pub amount: u64,
    pub reason_hash: [u8; 32],
}

#[event]
pub struct ReceiptBatchAnchored {
    pub batch_id: [u8; 32],
    pub authority: Pubkey,
    pub merkle_root: [u8; 32],
    pub receipt_count: u32,
    pub created_at: i64,
}

#[event]
pub struct StakePositionClosed {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
}

#[event]
pub struct AuthorityUpdated {
    pub previous: Pubkey,
    pub new_authority: Pubkey,
}

#[event]
pub struct SlashAuthorityUpdated {
    pub previous: Pubkey,
    pub new_slash_authority: Pubkey,
}

#[event]
pub struct TreasuryUpdated {
    pub previous: Pubkey,
    pub new_treasury: Pubkey,
}

#[event]
pub struct CreditsRateUpdated {
    pub previous: u64,
    pub credits_per_covnt: u64,
}

#[event]
pub struct MinStakeLockUpdated {
    pub previous: u64,
    pub min_stake_lock: u64,
}

#[event]
pub struct ConfigMigrated {
    pub min_stake_lock: u64,
}

#[event]
pub struct LeaseOpened {
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub operator: Pubkey,
    pub coordinator: Pubkey,
    pub er_validator: Pubkey,
    pub mint: Pubkey,
    pub rate_per_sec: u64,
    pub max_duration_secs: u64,
    pub funded_amount: u64,
    pub opened_at: i64,
}

#[event]
pub struct LeaseTicked {
    pub job_id: [u8; 16],
    pub metered_ms: u64,
    pub receipt_hash: [u8; 32],
    pub provenance_root: [u8; 32],
}

#[event]
pub struct LeaseSettled {
    pub job_id: [u8; 16],
    pub metered_ms: u64,
    pub charged: u64,
    pub refunded: u64,
    pub provenance_root: [u8; 32],
    pub voided: bool,
}

#[event]
pub struct LeaseOperatorPaid {
    pub job_id: [u8; 16],
    pub operator: Pubkey,
    pub metered_ms: u64,
    pub amount: u64,
}

#[event]
pub struct LeaseRenterRefunded {
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub amount: u64,
}

#[event]
pub struct LeaseVoided {
    pub job_id: [u8; 16],
    pub coordinator: Pubkey,
}

#[error_code]
pub enum CovenantError {
    #[msg("amount must be greater than zero")]
    ZeroAmount,
    #[msg("arithmetic overflow")]
    Overflow,
    #[msg("protocol is paused")]
    ProtocolPaused,
    #[msg("unauthorized")]
    Unauthorized,
    #[msg("wrong COVNT mint")]
    WrongMint,
    #[msg("agent is inactive")]
    AgentInactive,
    #[msg("agent mismatch")]
    AgentMismatch,
    #[msg("insufficient credits")]
    InsufficientCredits,
    #[msg("insufficient stake")]
    InsufficientStake,
    #[msg("stake position is inactive")]
    StakeInactive,
    #[msg("wrong task status")]
    WrongTaskStatus,
    #[msg("task deadline has passed; release no longer allowed (use refund_task)")]
    TaskExpired,
    #[msg("task deadline has not passed; refund not yet available")]
    TaskNotExpired,
    #[msg("stake position is still locked")]
    StakeLocked,
    #[msg("stake position is still active; unstake before closing")]
    StakeStillActive,
    #[msg("lock_until is shorter than the protocol minimum stake lock")]
    LockTooShort,
    #[msg("task escrow is disabled in this build")]
    TasksDisabled,
    #[msg("an explicit ER validator account is required to delegate")]
    ValidatorRequired,
    #[msg("the agent has no recorded actions to slash for (provenance root is genesis)")]
    NoRecordedActions,
    // Lease metering. Appended, never inserted: a variant's position in this
    // enum is the number that reaches the client, so adding one anywhere above
    // would silently renumber every error a deployed client already maps.
    #[msg("a lease rate must be greater than zero")]
    ZeroRate,
    #[msg("a lease window must be between one second and a day")]
    BadDuration,
    #[msg("the lease escrow deposit arrived empty")]
    EmptyDeposit,
    #[msg("this lease is already settled")]
    LeaseAlreadySettled,
    #[msg("this share of the lease is already paid")]
    LeaseShareAlreadyPaid,
    #[msg("this lease is already voided")]
    LeaseAlreadyVoided,
    #[msg("this lease is voided")]
    LeaseIsVoided,
    #[msg("a meter may only move forward")]
    MeterWentBackwards,
    #[msg("this meter is closed")]
    MeterClosed,
    #[msg("this lease cannot be settled until its meter is concluded or its window has elapsed")]
    LeaseStillRunning,
}

struct LeaseSettlement {
    metered_ms: u64,
    charged: u64,
    provenance_root: [u8; 32],
}

/// What a lease owes, computed on L1, and whether it may be paid yet.
///
/// The meter arrives as a raw account because a lease that is still delegated
/// is owned by the delegation program and cannot be deserialized at all. That
/// case is not an error: it is a meter whose host never committed, which
/// carries no evidence of time served, so the charge is zero and the escrow
/// goes home. Only the window expiring or a void opens that door, and only the
/// coordinator the renter named can put a lease there in the first place.
fn lease_settlement(terms: &LeaseTerms, meter: &UncheckedAccount) -> Result<LeaseSettlement> {
    let committed = if meter.owner == &crate::ID {
        let data = meter.try_borrow_data()?;
        Some(LeaseMeter::try_deserialize(&mut &data[..])?)
    } else {
        None
    };

    let window_over = Clock::get()?.unix_timestamp
        >= terms
            .opened_at
            .saturating_add(terms.max_duration_secs as i64);
    // A meter this program owns again after a delegation has been through the
    // rollup and come back. Its own closed flag is not the test: that flag rides
    // a commit this program does not author, and a finished lease should not
    // have to wait out its whole window because one byte went missing on the way
    // home.
    let returned_from_rollup = terms.delegated && committed.is_some();
    require!(
        terms.voided || returned_from_rollup || window_over,
        CovenantError::LeaseStillRunning
    );

    let metered_ms = committed.as_ref().map_or(0, |m| m.metered_ms);
    let provenance_root = committed.as_ref().map_or([0u8; 32], |m| m.provenance_root);

    Ok(LeaseSettlement {
        metered_ms,
        charged: lease_charge(terms, metered_ms),
        provenance_root,
    })
}

/// Pro-rata to the millisecond, rounded up: a started second is a served
/// second.
///
/// Clamped at both the window the renter signed for and the balance the vault
/// actually holds. The first clamp is what bounds a rollup host that commits an
/// inflated elapsed; the second is what keeps a mint that skimmed the deposit
/// from making every settlement ask the vault for more than it has and revert
/// forever.
fn lease_charge(terms: &LeaseTerms, metered_ms: u64) -> u64 {
    if terms.voided {
        return 0;
    }
    let rate = u128::from(terms.rate_per_sec);
    let window = rate * u128::from(terms.max_duration_secs);
    ((rate * u128::from(metered_ms)).div_ceil(1_000))
        .min(window)
        .min(u128::from(terms.funded_amount)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::Discriminator;

    fn terms(rate_per_sec: u64, max_duration_secs: u64, funded_amount: u64) -> LeaseTerms {
        LeaseTerms {
            job_id: [0u8; 16],
            renter: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            coordinator: Pubkey::new_unique(),
            er_validator: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            rate_per_sec,
            max_duration_secs,
            funded_amount,
            opened_at: 1_700_000_000,
            paid_operator: false,
            paid_renter: false,
            voided: false,
            delegated: false,
            bump: 255,
            meter_bump: 254,
            vault_bump: 253,
        }
    }

    #[test]
    fn a_started_second_is_a_served_second() {
        let t = terms(100, 600, 60_000);
        assert_eq!(lease_charge(&t, 0), 0);
        assert_eq!(lease_charge(&t, 1), 1);
        assert_eq!(lease_charge(&t, 1_000), 100);
        assert_eq!(lease_charge(&t, 1_001), 101);
        assert_eq!(lease_charge(&t, 12_500), 1_250);
    }

    #[test]
    fn an_inflated_elapsed_cannot_charge_past_the_window() {
        // The rollup host authors the committed elapsed, so the escrow has to
        // be the ceiling rather than the meter's word.
        let t = terms(100, 600, 60_000);
        assert_eq!(lease_charge(&t, 600_000), 60_000);
        assert_eq!(lease_charge(&t, 600_001), 60_000);
        assert_eq!(lease_charge(&t, u64::MAX), 60_000);
    }

    #[test]
    fn a_short_deposit_caps_the_charge_at_what_the_vault_holds() {
        // A transfer-fee mint delivers less than the window. Settlement must
        // still be payable, so the charge follows the balance.
        let t = terms(100, 600, 59_000);
        assert_eq!(lease_charge(&t, 600_000), 59_000);
        assert_eq!(lease_charge(&t, u64::MAX), 59_000);
    }

    #[test]
    fn a_voided_lease_charges_nothing_however_long_it_ran() {
        let mut t = terms(100, 600, 60_000);
        t.voided = true;
        assert_eq!(lease_charge(&t, 600_000), 0);
        assert_eq!(lease_charge(&t, u64::MAX), 0);
    }

    #[test]
    fn the_widest_legal_lease_does_not_overflow() {
        let rate = u64::MAX / MAX_LEASE_DURATION_SECS;
        let window = rate * MAX_LEASE_DURATION_SECS;
        let t = terms(rate, MAX_LEASE_DURATION_SECS, window);
        assert_eq!(lease_charge(&t, u64::MAX), window);
        assert_eq!(lease_charge(&t, MAX_LEASE_DURATION_SECS * 1_000), window);
    }

    #[test]
    fn two_renters_cannot_collide_on_one_job_id() {
        // The renter is in the seeds so that learning a job id, which the
        // assigned operator does at dispatch, is not enough to occupy the
        // address the honest open derives.
        let job_id = [7u8; 16];
        let renter = Pubkey::new_unique();
        let squatter = Pubkey::new_unique();
        let (mine, _) =
            Pubkey::find_program_address(&[b"lease", renter.as_ref(), job_id.as_ref()], &crate::ID);
        let (theirs, _) = Pubkey::find_program_address(
            &[b"lease", squatter.as_ref(), job_id.as_ref()],
            &crate::ID,
        );
        assert_ne!(mine, theirs);
    }

    #[test]
    fn the_delegated_account_carries_no_money_fields() {
        // The rollup host can rewrite every byte of the meter when it commits.
        // Nothing that decides a payout may live there.
        assert_eq!(
            LeaseMeter::INIT_SPACE,
            32 + 32 + 16 + 8 + 32 + 1 + 1,
            "a field was added to the delegated account; check it cannot decide money"
        );
        assert_eq!(LeaseTerms::INIT_SPACE, 16 + 32 * 5 + 8 * 4 + 7);
    }

    // The three guards below are the upgrade-safety net for a program that is
    // already live on mainnet holding real positions. Each one fails loudly if
    // an edit stops being additive.

    #[test]
    fn live_account_layouts_are_unchanged() {
        // Sizes the deployed program allocates today. A changed number here
        // means an existing account's field list moved, which would make the
        // upgraded program read live bytes at the wrong offsets.
        assert_eq!(Config::INIT_SPACE, 146);
        assert_eq!(Agent::INIT_SPACE, 146);
        assert_eq!(CreditAccount::INIT_SPACE, 73);
        assert_eq!(StakePosition::INIT_SPACE, 114);
        assert_eq!(Task::INIT_SPACE, 242);
        assert_eq!(ReceiptBatch::INIT_SPACE, 109);
    }

    #[test]
    fn live_account_discriminators_are_unchanged_and_the_new_ones_are_distinct() {
        // Anchor derives these from the type name, so a rename is a silent
        // account-type swap. The six are the values in the deployed binary.
        assert_eq!(
            Config::DISCRIMINATOR,
            [0x9b, 0x0c, 0xaa, 0xe0, 0x1e, 0xfa, 0xcc, 0x82]
        );
        assert_eq!(
            Agent::DISCRIMINATOR,
            [0x2f, 0xa6, 0x70, 0x93, 0x9b, 0xc5, 0x56, 0x07]
        );
        assert_eq!(
            CreditAccount::DISCRIMINATOR,
            [0xc4, 0xab, 0xea, 0x84, 0xef, 0xff, 0x15, 0x60]
        );
        assert_eq!(
            StakePosition::DISCRIMINATOR,
            [0x4e, 0xa5, 0x1e, 0x6f, 0xab, 0x7d, 0x0b, 0xdc]
        );
        assert_eq!(
            Task::DISCRIMINATOR,
            [0x4f, 0x22, 0xe5, 0x37, 0x58, 0x5a, 0x37, 0x54]
        );
        assert_eq!(
            ReceiptBatch::DISCRIMINATOR,
            [0xea, 0xfa, 0x30, 0x3b, 0xf2, 0x94, 0x37, 0x4c]
        );

        let all = [
            Config::DISCRIMINATOR,
            Agent::DISCRIMINATOR,
            CreditAccount::DISCRIMINATOR,
            StakePosition::DISCRIMINATOR,
            Task::DISCRIMINATOR,
            ReceiptBatch::DISCRIMINATOR,
            LeaseTerms::DISCRIMINATOR,
            LeaseMeter::DISCRIMINATOR,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two account types share a discriminator");
            }
        }
    }

    #[test]
    fn lease_seeds_cannot_reach_an_existing_account() {
        // Seeds are concatenated before hashing, so the real question is
        // whether any lease seed vector can produce the same byte string as an
        // existing one. Distinct first labels of the same length settle it, and
        // pinning the numbers here means a future seed rename has to look at
        // this test.
        let live: [&[u8]; 6] = [
            b"config",
            b"agent",
            b"credits",
            b"stake",
            b"task",
            b"receipt_batch",
        ];
        let leased: [&[u8]; 3] = [b"lease", b"meter", b"vault"];
        for l in leased {
            for e in live {
                assert_ne!(l, e);
                assert!(
                    !l.starts_with(e) && !e.starts_with(l),
                    "one seed label is a prefix of another, so a longer tail could collide"
                );
            }
        }

        let owner = Pubkey::new_unique();
        let job_id = [3u8; 16];
        let key32 = [3u8; 32];
        let (lease, _) =
            Pubkey::find_program_address(&[b"lease", owner.as_ref(), job_id.as_ref()], &crate::ID);
        let (meter, _) = Pubkey::find_program_address(&[b"meter", lease.as_ref()], &crate::ID);
        let (vault, _) = Pubkey::find_program_address(&[b"vault", lease.as_ref()], &crate::ID);
        let derived = [
            Pubkey::find_program_address(&[b"config"], &crate::ID).0,
            Pubkey::find_program_address(&[b"agent", &key32], &crate::ID).0,
            Pubkey::find_program_address(&[b"credits", owner.as_ref()], &crate::ID).0,
            Pubkey::find_program_address(&[b"stake", &key32, owner.as_ref()], &crate::ID).0,
            Pubkey::find_program_address(&[b"task", &key32], &crate::ID).0,
            Pubkey::find_program_address(&[b"receipt_batch", &key32], &crate::ID).0,
            lease,
            meter,
            vault,
        ];
        for (i, a) in derived.iter().enumerate() {
            for b in derived.iter().skip(i + 1) {
                assert_ne!(a, b, "two PDA families derived the same address");
            }
        }
    }

    #[test]
    fn live_error_numbers_are_unchanged() {
        // An `#[error_code]` variant's position is its wire number. The lease
        // errors had to be appended, and this pins the seam.
        assert_eq!(u32::from(CovenantError::ZeroAmount), 6000);
        assert_eq!(u32::from(CovenantError::NoRecordedActions), 6018);
        assert_eq!(u32::from(CovenantError::ZeroRate), 6019);
        assert_eq!(u32::from(CovenantError::LeaseStillRunning), 6028);
    }
}
