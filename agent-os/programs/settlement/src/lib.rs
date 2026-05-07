//! Covenant settlement program — Phase 5 first scaffold.
//!
//! Implements the credit-mint + consumption + buyback shape from
//! `00_spec.md` §8 ("credits + buyback" model for a fixed-supply $covnt
//! launched on pump.fun). v0 emits events instead of moving tokens; the
//! actual SPL token CPIs (burn covnt, mint credits, swap via Raydium) land
//! once Pyth oracle integration + a chosen DEX router are wired in.
//!
//! Three instructions:
//!   - `initialize(args)`             — one-shot setup of the `Config` PDA
//!     under seed `b"settlement-config"`. Records authority, mints, rate.
//!   - `mint_credits(amount_covnt)`   — payer requests credits in exchange
//!     for burned covnt. v0 emits `CreditsRequested`.
//!   - `consume_credits(amount)`      — payer destroys credits at the
//!     point of consumption (memory write, tool call, etc.). v0 emits
//!     `CreditsConsumed`.

// Anchor 0.31.1's `#[program]` macro expands to calls that hit a few
// deprecated `AccountInfo` methods. Suppressing here keeps clippy `-D
// warnings` clean; revisit when bumping anchor-lang.
#![allow(deprecated)]
#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

declare_id!("CovntSettLement1111111111111111111111111111");

#[program]
pub mod settlement {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.authority = ctx.accounts.authority.key();
        cfg.covnt_mint = args.covnt_mint;
        cfg.usdc_mint = args.usdc_mint;
        cfg.credits_per_covnt = args.credits_per_covnt;
        cfg.bump = ctx.bumps.config;
        emit!(SettlementInitialized {
            authority: cfg.authority,
            covnt_mint: cfg.covnt_mint,
            usdc_mint: cfg.usdc_mint,
            credits_per_covnt: cfg.credits_per_covnt,
        });
        Ok(())
    }

    pub fn mint_credits(ctx: Context<MintCredits>, amount_covnt: u64) -> Result<()> {
        require!(amount_covnt > 0, SettlementError::ZeroAmount);
        let cfg = &ctx.accounts.config;
        let credits = amount_covnt
            .checked_mul(cfg.credits_per_covnt)
            .ok_or(SettlementError::Overflow)?;
        // TODO(phase-5+): CPI into spl_token::burn on payer's covnt account
        //   and spl_token::mint_to on the credits mint controlled by this
        //   program's authority PDA. Pyth oracle adjusts the rate.
        emit!(CreditsRequested {
            payer: ctx.accounts.payer.key(),
            amount_covnt,
            credits,
        });
        Ok(())
    }

    pub fn consume_credits(ctx: Context<ConsumeCredits>, amount: u64) -> Result<()> {
        require!(amount > 0, SettlementError::ZeroAmount);
        // TODO(phase-5+): CPI into spl_token::burn on the payer's credits
        //   account; track consumption stats per-payer in a stat PDA so
        //   buyback batches read from on-chain truth rather than off-chain
        //   logs.
        emit!(CreditsConsumed {
            payer: ctx.accounts.payer.key(),
            amount,
        });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [b"settlement-config"],
        bump,
    )]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MintCredits<'info> {
    #[account(
        seeds = [b"settlement-config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
pub struct ConsumeCredits<'info> {
    #[account(
        seeds = [b"settlement-config"],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    pub payer: Signer<'info>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeArgs {
    pub covnt_mint: Pubkey,
    pub usdc_mint: Pubkey,
    /// Initial fixed conversion rate. Phase 5+ replaces this with a Pyth-
    /// driven oracle read so the rate floats with covnt's market price.
    pub credits_per_covnt: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub authority: Pubkey,
    pub covnt_mint: Pubkey,
    pub usdc_mint: Pubkey,
    pub credits_per_covnt: u64,
    pub bump: u8,
}

#[event]
pub struct SettlementInitialized {
    pub authority: Pubkey,
    pub covnt_mint: Pubkey,
    pub usdc_mint: Pubkey,
    pub credits_per_covnt: u64,
}

#[event]
pub struct CreditsRequested {
    pub payer: Pubkey,
    pub amount_covnt: u64,
    pub credits: u64,
}

#[event]
pub struct CreditsConsumed {
    pub payer: Pubkey,
    pub amount: u64,
}

#[error_code]
pub enum SettlementError {
    #[msg("amount must be greater than zero")]
    ZeroAmount,
    #[msg("arithmetic overflow")]
    Overflow,
}
