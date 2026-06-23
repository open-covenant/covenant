//! Covenant Oracle — runtime gating for MPL Core agent assets.
//!
//! MPL Core's Oracle external plugin loads an external account during an
//! asset's lifecycle events (create / transfer / burn / update) and reads an
//! [`OracleValidation`] from it to approve, reject, or pass the event. This
//! program owns one oracle account per subject agent and exposes a single
//! authority-gated knob: flip the agent's `transfer` validation between
//! `Pass` (audit valid — don't block) and `Rejected` (audit invalid / out of
//! policy — veto the transfer).
//!
//! Covenant pushes the live verdict here (`set_validation`), so an agent
//! asset configured with this account as its Oracle can only move while its
//! audit chain is valid and in policy. The on-chain rule is enforced by MPL
//! Core itself — Covenant only supplies the signal.
//!
//! The account is an Anchor account, so the asset's Oracle plugin uses
//! `ValidationResultsOffset::Anchor` (offset 8) and `validation` is the first
//! field — MPL Core reads exactly the [`OracleValidation`] bytes there. The
//! enum layouts below are byte-compatible with `mpl-core` 0.12.

use anchor_lang::prelude::*;

declare_id!("2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD");

#[program]
pub mod covenant_oracle_program {
    use super::*;

    /// Create the oracle account for `subject` (the agent's Core asset).
    /// The signer becomes the authority allowed to flip the verdict.
    /// Defaults to valid (transfer passes).
    pub fn init_oracle(ctx: Context<InitOracle>, subject: Pubkey) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        oracle.validation = OracleValidation::valid();
        oracle.authority = ctx.accounts.authority.key();
        oracle.subject = subject;
        oracle.bump = ctx.bumps.oracle;
        emit!(ValidationSet {
            subject,
            valid: true,
        });
        Ok(())
    }

    /// Push the live audit/policy verdict. `valid = false` sets `transfer`
    /// to `Rejected`, which makes MPL Core veto any transfer of the gated
    /// asset until it flips back. Authority-only.
    pub fn set_validation(ctx: Context<SetValidation>, valid: bool) -> Result<()> {
        let oracle = &mut ctx.accounts.oracle;
        oracle.validation = if valid {
            OracleValidation::valid()
        } else {
            OracleValidation::rejected()
        };
        emit!(ValidationSet {
            subject: oracle.subject,
            valid,
        });
        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(subject: Pubkey)]
pub struct InitOracle<'info> {
    #[account(
        init,
        payer = authority,
        space = OracleState::SPACE,
        seeds = [b"oracle", subject.as_ref()],
        bump,
    )]
    pub oracle: Account<'info, OracleState>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetValidation<'info> {
    #[account(mut, has_one = authority)]
    pub oracle: Account<'info, OracleState>,
    pub authority: Signer<'info>,
}

/// The oracle account. `validation` is first so that, with the asset's Oracle
/// plugin set to `ValidationResultsOffset::Anchor`, MPL Core reads it at byte
/// 8 (right after the Anchor discriminator). Always written as `V1`, so the
/// trailing fields sit at fixed offsets.
#[account]
pub struct OracleState {
    pub validation: OracleValidation,
    pub authority: Pubkey,
    pub subject: Pubkey,
    pub bump: u8,
}

impl OracleState {
    // disc(8) + validation V1(1 tag + 4 results) + authority(32) + subject(32) + bump(1)
    pub const SPACE: usize = 8 + 5 + 32 + 32 + 1;
}

#[event]
pub struct ValidationSet {
    pub subject: Pubkey,
    pub valid: bool,
}

/// Byte-compatible with `mpl_core::types::ExternalValidationResult`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExternalValidationResult {
    Approved,
    Rejected,
    Pass,
}

/// Byte-compatible with `mpl_core::types::OracleValidation`. MPL Core
/// deserializes this at the configured offset.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub enum OracleValidation {
    Uninitialized,
    V1 {
        create: ExternalValidationResult,
        transfer: ExternalValidationResult,
        burn: ExternalValidationResult,
        update: ExternalValidationResult,
    },
}

impl OracleValidation {
    /// Audit valid: the oracle does not block any lifecycle event (`Pass`
    /// defers to the asset's normal owner authority).
    fn valid() -> Self {
        OracleValidation::V1 {
            create: ExternalValidationResult::Pass,
            transfer: ExternalValidationResult::Pass,
            burn: ExternalValidationResult::Pass,
            update: ExternalValidationResult::Pass,
        }
    }

    /// Audit invalid / out of policy: veto transfers (MPL Core halts the
    /// event when the oracle returns `Rejected`).
    fn rejected() -> Self {
        OracleValidation::V1 {
            create: ExternalValidationResult::Pass,
            transfer: ExternalValidationResult::Rejected,
            burn: ExternalValidationResult::Pass,
            update: ExternalValidationResult::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::AnchorSerialize;

    // ExternalValidationResult: Approved=0, Rejected=1, Pass=2. OracleValidation:
    // Uninitialized=0, V1=1. These bytes are what MPL Core deserializes, so they
    // are a hard contract with mpl-core 0.12 — assert them, don't trust them.
    #[test]
    fn valid_is_all_pass() {
        assert_eq!(OracleValidation::valid().try_to_vec().unwrap(), vec![1, 2, 2, 2, 2]);
    }

    #[test]
    fn rejected_vetoes_transfer_only() {
        assert_eq!(OracleValidation::rejected().try_to_vec().unwrap(), vec![1, 2, 1, 2, 2]);
    }

    // With `validation` first, MPL Core's ValidationResultsOffset::Anchor (byte 8,
    // after the account discriminator) lands on the V1 tag, and the transfer
    // verdict on account byte 10. Devnet confirms: lifecycle.rs:806:Reject -> 0x9.
    #[test]
    fn validation_first_puts_transfer_verdict_at_account_offset_10() {
        let state = OracleState {
            validation: OracleValidation::rejected(),
            authority: Pubkey::default(),
            subject: Pubkey::default(),
            bump: 0,
        };
        let bytes = state.try_to_vec().unwrap();
        assert_eq!(bytes[0], 1, "V1 tag at account offset 8");
        assert_eq!(bytes[2], 1, "transfer = Rejected at account offset 10");
        assert_eq!(8 + bytes.len(), OracleState::SPACE);
    }
}
