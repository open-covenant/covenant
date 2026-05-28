//! `KeeperAction` → on-chain `Instruction` mapping. The keeper's
//! decision layer is wire-agnostic; this module is where decisions
//! turn into transactions, keeping the IX layout knowledge in one
//! file alongside [`crate::instruction`].
//!
//! Submission, signing, and confirmation are deliberately *not* here.
//! Operators wire in their own RPC client / bundler — Jito, custom
//! sender, etc. — and the keeper just hands them an `Instruction`.

use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;

use crate::instruction::{self, crank_action};
use crate::state::KeeperAction;

/// What an operator needs to know to translate a decision into an
/// instruction. The market + program are fixed for a keeper; payer
/// is the keeper's signer key.
#[derive(Debug, Clone)]
pub struct BuildContext {
    pub program_id: Pubkey,
    pub market: Pubkey,
    /// The keeper's signer for `PermissionlessCrank` and
    /// `PushAuthMark` (auth-mode market admin path).
    pub keeper: Pubkey,
    /// The portfolio account `PermissionlessCrank` / recovery IXs
    /// target. `None` for actions that don't need it (crank-only
    /// flows still need a portfolio account; the keeper resolves
    /// it from on-chain state before calling this).
    pub portfolio: Option<Pubkey>,
    /// Cluster slot at submission. The program rejects stale slots.
    pub now_slot: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("action {0} requires a portfolio account in BuildContext")]
    MissingPortfolio(&'static str),
}

/// Translate one synthetic `KeeperAction` into a concrete on-chain
/// `Instruction`. Pure function — no signer, no RPC, no clock —
/// callers schedule submission separately.
pub fn build_instruction(action: &KeeperAction, ctx: &BuildContext) -> Result<Instruction, BuildError> {
    match action {
        KeeperAction::PushAuthMark {
            asset_index,
            mark_e6,
        } => Ok(instruction::push_auth_mark(
            ctx.program_id,
            ctx.market,
            ctx.keeper,
            *asset_index,
            ctx.now_slot,
            *mark_e6,
        )),
        KeeperAction::CrankAsset { asset_index } => {
            // Refresh is the only routine path. Recovery / liquidation
            // surfaces still go through PermissionlessCrank but with
            // distinct policies; the keeper's policy layer emits
            // those as recovery actions instead, so a plain
            // `CrankAsset` is always action=Refresh, asset_index from
            // the variant.
            let portfolio = ctx
                .portfolio
                .ok_or(BuildError::MissingPortfolio("CrankAsset"))?;
            Ok(instruction::permissionless_crank(
                ctx.program_id,
                ctx.market,
                portfolio,
                ctx.keeper,
                None,
                crank_action::REFRESH,
                *asset_index,
                ctx.now_slot,
                0,
                0,
                0,
                0,
            ))
        }
        KeeperAction::RecoveryForfeitLeg {
            asset_index,
            b_delta_budget,
        } => {
            let portfolio = ctx
                .portfolio
                .ok_or(BuildError::MissingPortfolio("RecoveryForfeitLeg"))?;
            // The synthetic field is i128 to mirror engine math;
            // ForfeitRecoveryLeg's wire field is u128. We saturate
            // negatives to 0 — they're invalid input the policy
            // layer should have refused, but the boundary keeps
            // the conversion total.
            let b = (*b_delta_budget).max(0) as u128;
            Ok(instruction::forfeit_recovery_leg(
                ctx.program_id,
                ctx.market,
                portfolio,
                ctx.keeper,
                *asset_index,
                b,
            ))
        }
        KeeperAction::RecoveryRebalanceReduce {
            asset_index,
            reduce_q,
        } => {
            let portfolio = ctx
                .portfolio
                .ok_or(BuildError::MissingPortfolio("RecoveryRebalanceReduce"))?;
            let q = (*reduce_q).max(0) as u128;
            Ok(instruction::rebalance_reduce(
                ctx.program_id,
                ctx.market,
                portfolio,
                ctx.keeper,
                *asset_index,
                q,
            ))
        }
        KeeperAction::RecoveryFinalize { asset_index, side } => Ok(
            instruction::finalize_reset_side(ctx.program_id, ctx.market, *asset_index, *side),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }
    fn ctx() -> BuildContext {
        BuildContext {
            program_id: pk(0xAA),
            market: pk(0x01),
            keeper: pk(0x02),
            portfolio: Some(pk(0x03)),
            now_slot: 1_000,
        }
    }

    #[test]
    fn push_auth_mark_uses_tag_63() {
        let ix = build_instruction(
            &KeeperAction::PushAuthMark {
                asset_index: 5,
                mark_e6: 200_000_000,
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(ix.data[0], 63);
    }

    #[test]
    fn crank_asset_uses_tag_5_refresh_and_threads_asset_index() {
        let ix =
            build_instruction(&KeeperAction::CrankAsset { asset_index: 7 }, &ctx())
                .unwrap();
        assert_eq!(ix.data[0], 5);
        assert_eq!(ix.data[1], 0, "CrankAsset maps to Refresh action");
        // tag(1) + action(1) + asset_index_le(2) — assert the asset
        // gets passed through, NOT hardcoded to 0.
        assert_eq!(&ix.data[2..4], &7u16.to_le_bytes());
    }

    #[test]
    fn crank_without_portfolio_errors() {
        let mut c = ctx();
        c.portfolio = None;
        assert!(matches!(
            build_instruction(&KeeperAction::CrankAsset { asset_index: 0 }, &c),
            Err(BuildError::MissingPortfolio("CrankAsset"))
        ));
    }

    #[test]
    fn recovery_trio_dispatches_correctly() {
        let c = ctx();
        let f = build_instruction(
            &KeeperAction::RecoveryForfeitLeg {
                asset_index: 1,
                b_delta_budget: 1_000,
            },
            &c,
        )
        .unwrap();
        assert_eq!(f.data[0], 43);

        let r = build_instruction(
            &KeeperAction::RecoveryRebalanceReduce {
                asset_index: 1,
                reduce_q: 1_000,
            },
            &c,
        )
        .unwrap();
        assert_eq!(r.data[0], 44);

        let s = build_instruction(
            &KeeperAction::RecoveryFinalize {
                asset_index: 1,
                side: 1,
            },
            &c,
        )
        .unwrap();
        assert_eq!(s.data[0], 45);
        // FinalizeResetSide is permissionless and needs no portfolio.
        assert_eq!(s.accounts.len(), 1);
    }

    #[test]
    fn negative_budget_saturates_to_zero() {
        // Policy should never produce a negative budget, but the wire
        // boundary stays total either way.
        let ix = build_instruction(
            &KeeperAction::RecoveryForfeitLeg {
                asset_index: 0,
                b_delta_budget: -42,
            },
            &ctx(),
        )
        .unwrap();
        // tag(1) + asset(2) + 16B zero
        assert_eq!(&ix.data[3..19], &[0u8; 16]);
    }
}
