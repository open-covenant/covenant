//! Slash evidence + the pure-function verifier shared by the
//! on-chain program and off-chain tooling.
//!
//! A slasher's job: watch the audit chain, find a settlement
//! receipt for a keeper-executed action that the keeper's scope
//! didn't actually permit, package it as [`SlashEvidence`], submit
//! the on-chain `Slash` instruction. The program calls
//! [`verify_slash`] and, if it returns [`Slashable`], moves the
//! bond's lamports to the slash recipient.
//!
//! The verifier is intentionally pure: same inputs always produce
//! the same `Result`. Cryptographic signature checks (the keeper
//! signing the receipt; the operator signing the scope) are *not*
//! performed here — they're delegated to the on-chain runtime
//! (ed25519 program / sysvar) at submission time. This module
//! verifies the *content* contract: scope-hash match, action vs.
//! scope, replay, slot ordering. Composed at submission, this is
//! the full security envelope.

use crate::scope::BondScope;
use crate::state::BondAccount;

/// One executed action the slasher is attesting violated scope.
/// Mirrors a flattened `covenant_types::SettlementReceipt` against
/// the keeper's recorded action label. The slasher derives this
/// from the audit chain — the bond program doesn't trust it
/// without [`verify_slash`] firing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedAction {
    /// The receipt's own id (uuid bytes). Replay-protection PDA
    /// is keyed on this so the same receipt can't slash twice.
    pub receipt_id: [u8; 16],
    /// Slot the action was executed at. Must be ≥ bond.created_slot
    /// to be slashable (the bond was already in force).
    pub executed_slot: u64,
    /// The market the keeper acted against. Should equal bond scope's
    /// market for the violation to apply.
    pub market: [u8; 32],
    /// `ActionMask::*` bit for the action label. Exactly one bit set.
    pub action_bit: u8,
    /// Asset the action targeted, when applicable. `None` for `Crank`.
    pub asset_index: Option<u16>,
}

/// The slasher's complete evidence packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashEvidence {
    /// The full scope. Hashed and compared against `BondAccount::scope_hash`.
    pub scope: BondScope,
    /// The action being attested as a violation.
    pub action: AttestedAction,
}

/// Verifier verdict — positive case. The on-chain program reads
/// `slash_lamports` and transfers exactly that many. v1 always
/// slashes the entire bond on a confirmed violation; future
/// versions may make this proportional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slashable {
    pub slash_lamports: u64,
    pub recipient: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashRejection {
    /// The supplied scope's hash does not match the bond's stored hash.
    ScopeHashMismatch,
    /// The scope actually permits this action. Not a violation.
    NoViolation,
    /// Bond is already slashed.
    AlreadySlashed,
    /// Action's executed_slot < bond.created_slot.
    BeforeBondStart,
    /// Bond has zero lamports — nothing to slash.
    EmptyBond,
}

/// Pure verification. The on-chain program calls this exactly; tests
/// call it directly; the slasher calls it before submission to
/// avoid wasting a transaction.
///
/// **Security note:** a keeper executing on a *different market* than
/// the scope's market is the strongest kind of violation — the scope
/// explicitly named one market and the keeper went elsewhere.
/// [`BondScope::allows`] returns `false` for any market mismatch, so
/// this case flows naturally into the slash path (NOT a rejection).
/// Earlier versions surfaced a "MarketMismatch" rejection here, which
/// inadvertently *protected* keepers acting outside their market.
pub fn verify_slash(
    bond: &BondAccount,
    evidence: &SlashEvidence,
) -> Result<Slashable, SlashRejection> {
    if bond.slashed != 0 {
        return Err(SlashRejection::AlreadySlashed);
    }
    if bond.lamports == 0 {
        return Err(SlashRejection::EmptyBond);
    }
    if evidence.scope.hash() != bond.scope_hash {
        return Err(SlashRejection::ScopeHashMismatch);
    }
    if evidence.action.executed_slot < bond.created_slot {
        return Err(SlashRejection::BeforeBondStart);
    }
    // The contradiction itself: scope says "this is not allowed",
    // yet a settled receipt says the keeper did it. `allows` covers
    // market mismatch, action mask, AND asset list in one check —
    // any of those failing means the action was outside scope.
    if evidence
        .scope
        .allows(&evidence.action.market, evidence.action.action_bit, evidence.action.asset_index)
    {
        return Err(SlashRejection::NoViolation);
    }
    Ok(Slashable {
        slash_lamports: bond.lamports,
        recipient: bond.slash_recipient,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{ActionMask, ScopeHash};

    fn build_bond(scope: &BondScope) -> BondAccount {
        BondAccount::new(
            [9u8; 32],
            [8u8; 32],
            scope.hash(),
            [7u8; 32],
            1_000_000,
            100,
        )
    }

    fn happy_scope() -> BondScope {
        BondScope {
            version: 1,
            market: [1u8; 32],
            allowed_actions: ActionMask(ActionMask::PUSH_MARK | ActionMask::CRANK),
            allowed_assets: Some(vec![0, 1]),
            max_actions_per_tick: 4,
        }
    }

    #[test]
    fn unscoped_asset_is_slashable() {
        let scope = happy_scope();
        let bond = build_bond(&scope);
        // Keeper executed push_mark on asset 7 — outside allowed_assets.
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        let v = verify_slash(
            &bond,
            &SlashEvidence {
                scope: scope.clone(),
                action,
            },
        )
        .unwrap();
        assert_eq!(v.slash_lamports, bond.lamports);
        assert_eq!(v.recipient, bond.slash_recipient);
    }

    #[test]
    fn unscoped_action_label_is_slashable() {
        let mut scope = happy_scope();
        scope.allowed_actions = ActionMask(ActionMask::CRANK); // no push_mark, no recover
        let bond = build_bond(&scope);
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0),
        };
        let r = verify_slash(&bond, &SlashEvidence { scope, action }).unwrap();
        assert_eq!(r.slash_lamports, bond.lamports);
    }

    /// A keeper executing on a market other than the one the scope
    /// names is the textbook scope violation — slashable. (Earlier
    /// versions rejected this as `MarketMismatch`, which had the
    /// inverse effect of *protecting* the keeper. The verifier now
    /// flows market-mismatch through `BondScope::allows → false`,
    /// landing in the slash branch.)
    #[test]
    fn unscoped_market_is_slashable() {
        let scope = happy_scope();
        let bond = build_bond(&scope);
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [2u8; 32], // wrong market
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0),
        };
        let r = verify_slash(&bond, &SlashEvidence { scope, action }).unwrap();
        assert_eq!(r.slash_lamports, bond.lamports);
        assert_eq!(r.recipient, bond.slash_recipient);
    }

    #[test]
    fn permitted_action_is_not_slashable() {
        let scope = happy_scope();
        let bond = build_bond(&scope);
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0),
        };
        assert_eq!(
            verify_slash(&bond, &SlashEvidence { scope, action }),
            Err(SlashRejection::NoViolation)
        );
    }

    #[test]
    fn scope_hash_mismatch_is_rejected() {
        let scope = happy_scope();
        let mut bond = build_bond(&scope);
        // Replace bond's scope_hash to break the match.
        bond.scope_hash = ScopeHash([0; 32]);
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        assert_eq!(
            verify_slash(&bond, &SlashEvidence { scope, action }),
            Err(SlashRejection::ScopeHashMismatch)
        );
    }

    #[test]
    fn pre_bond_receipts_are_not_slashable() {
        let scope = happy_scope();
        let mut bond = build_bond(&scope);
        bond.created_slot = 500;
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200, // before bond was created
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        assert_eq!(
            verify_slash(&bond, &SlashEvidence { scope, action }),
            Err(SlashRejection::BeforeBondStart)
        );
    }

    #[test]
    fn already_slashed_bond_cannot_be_re_slashed() {
        let scope = happy_scope();
        let mut bond = build_bond(&scope);
        bond.slashed = 1;
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        assert_eq!(
            verify_slash(&bond, &SlashEvidence { scope, action }),
            Err(SlashRejection::AlreadySlashed)
        );
    }

    #[test]
    fn empty_bond_rejected() {
        let scope = happy_scope();
        let mut bond = build_bond(&scope);
        bond.lamports = 0;
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 200,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        };
        assert_eq!(
            verify_slash(&bond, &SlashEvidence { scope, action }),
            Err(SlashRejection::EmptyBond)
        );
    }
}
