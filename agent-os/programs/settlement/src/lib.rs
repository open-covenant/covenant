//! Covenant Solana protocol program.
//!
//! `$COVNT` is an external SPL mint. The program never mints it; protocol
//! utility comes from staking, escrowing, burning, and metering it into
//! non-transferable credits.

#![allow(deprecated)]
#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, Burn, Mint, TokenAccount, TokenInterface, TransferChecked,
};

declare_id!("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");

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

        emit!(CreditsConsumed {
            owner: ctx.accounts.owner.key(),
            amount,
            receipt_hash,
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

        ctx.accounts.agent.stake = ctx.accounts.agent.stake.saturating_sub(amount);

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
        ctx.accounts.agent.stake = ctx.accounts.agent.stake.saturating_sub(amount);
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
        constraint = stake_vault.owner == position.key() @ CovenantError::Unauthorized,
        constraint = stake_vault.mint == config.covnt_mint @ CovenantError::WrongMint,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, constraint = slash_vault.mint == config.covnt_mint @ CovenantError::WrongMint)]
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
}

#[account]
#[derive(InitSpace)]
pub struct StakePosition {
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub amount: u64,
    pub lock_until: u64,
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
}
