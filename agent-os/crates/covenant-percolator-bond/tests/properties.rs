//! Property tests for the slash verifier.
//!
//! The verifier is the security envelope: if it slashes when it
//! shouldn't, real lamports are lost; if it doesn't slash when it
//! should, the accountability story falls apart. These properties
//! sweep random inputs to make sure both directions hold.

use covenant_percolator_bond::evidence::{verify_slash, AttestedAction, SlashEvidence, SlashRejection};
use covenant_percolator_bond::scope::{ActionMask, BondScope};
use covenant_percolator_bond::state::BondAccount;
use proptest::prelude::*;

fn scope(
    market_byte: u8,
    mask: u8,
    assets: Option<Vec<u16>>,
    cap: u32,
) -> BondScope {
    BondScope {
        version: 1,
        market: [market_byte; 32],
        allowed_actions: ActionMask(mask),
        allowed_assets: assets,
        max_actions_per_tick: cap,
    }
}

fn bond_for(scope: &BondScope, lamports: u64, created_slot: u64) -> BondAccount {
    BondAccount::new(
        [9u8; 32],
        [8u8; 32],
        scope.hash(),
        [7u8; 32],
        lamports,
        created_slot,
    )
}

fn ev(scope: BondScope, action: AttestedAction) -> SlashEvidence {
    SlashEvidence { scope, action }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// P1 — In-scope actions are never slashable, no matter what
    /// other parameters look like. This is the strongest safety
    /// property: a well-behaved keeper cannot be slashed.
    #[test]
    fn in_scope_actions_never_slash(
        market_byte in 0u8..16,
        mask in 1u8..8,
        scope_assets in proptest::collection::vec(0u16..32, 1..8),
        cap in 1u32..16,
        bond_lamports in 1u64..1_000_000_000,
        bond_slot in 0u64..1000,
        action_slot_delta in 0i64..1000,
        action_asset_idx in 0usize..8,
        action_bit_idx in 0u8..3,
    ) {
        let scope_obj = scope(market_byte, mask, Some(scope_assets.clone()), cap);
        let bond = bond_for(&scope_obj, bond_lamports, bond_slot);
        // Pick an asset that IS in the scope.
        let asset_index = scope_assets[action_asset_idx % scope_assets.len()];
        // Pick an action bit that IS in the mask.
        let bits: Vec<u8> = [ActionMask::PUSH_MARK, ActionMask::CRANK, ActionMask::RECOVER]
            .into_iter()
            .filter(|b| mask & b == *b)
            .collect();
        let action_bit = bits[action_bit_idx as usize % bits.len()];
        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: (bond_slot as i64 + action_slot_delta).max(0) as u64,
            market: [market_byte; 32],
            action_bit,
            // Crank has no asset; mark/recover do. To keep the
            // property total we always supply Some; the verifier
            // ignores asset_index unless the scope's allowed_assets
            // list constrains it.
            asset_index: Some(asset_index),
        };
        let result = verify_slash(&bond, &ev(scope_obj, action));
        prop_assert!(
            matches!(result, Err(SlashRejection::NoViolation)
                | Err(SlashRejection::BeforeBondStart)),
            "in-scope action wrongly slashed: {:?}",
            result
        );
    }

    /// P2 — A keeper acting on a market different from the scoped
    /// one is always slashable. (Previous versions rejected this as
    /// "MarketMismatch"; that was a security bug — wrong market is
    /// the textbook out-of-scope violation. The verifier now flows
    /// wrong-market through `BondScope::allows → false` into the
    /// slash branch.)
    #[test]
    fn wrong_market_is_always_slashable(
        scope_market in 0u8..255,
        action_market in 0u8..255,
        bond_lamports in 1u64..1_000_000_000,
    ) {
        prop_assume!(scope_market != action_market);
        let scope_obj = scope(
            scope_market,
            ActionMask::PUSH_MARK | ActionMask::CRANK,
            Some(vec![0]),
            1,
        );
        let bond = bond_for(&scope_obj, bond_lamports, 0);
        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: 1000,
            market: [action_market; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0),
        };
        let r = verify_slash(&bond, &ev(scope_obj, action)).unwrap();
        prop_assert_eq!(r.slash_lamports, bond.lamports);
    }

    /// P3 — A scope-hash mismatch trumps everything else, no matter
    /// how blatant the underlying violation looks. This prevents a
    /// slasher from submitting a *different* scope that conveniently
    /// fails to permit the action.
    #[test]
    fn hash_mismatch_blocks_slash_regardless_of_violation(
        wrong_hash_byte in 1u8..=255,
        action_asset in 0u16..1000,
    ) {
        let scope_obj = scope(1, ActionMask::CRANK, Some(vec![0]), 1);
        let mut bond = bond_for(&scope_obj, 1_000_000, 0);
        // Corrupt one byte of the stored scope hash. `wrong_hash_byte`
        // ranges over 1..=255 so the xor is always non-zero — a zero
        // xor would leave the hash unchanged and a real violation
        // would slash, contradicting the property.
        let mut h = bond.scope_hash.0;
        h[0] ^= wrong_hash_byte;
        bond.scope_hash = covenant_percolator_bond::scope::ScopeHash(h);

        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: 1000,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK, // not in scope mask
            asset_index: Some(action_asset),
        };
        prop_assert_eq!(
            verify_slash(&bond, &ev(scope_obj, action)),
            Err(SlashRejection::ScopeHashMismatch)
        );
    }

    /// P4 — Out-of-scope-assets always slash (when other gates pass).
    #[test]
    fn out_of_scope_asset_slashes(
        scope_assets in proptest::collection::vec(0u16..16, 1..4),
        outsider_seed in 100u16..1000,
        bond_lamports in 1u64..1_000_000_000,
    ) {
        // Pick an asset definitely NOT in scope.
        prop_assume!(!scope_assets.contains(&outsider_seed));
        let scope_obj = scope(
            1,
            ActionMask::PUSH_MARK | ActionMask::CRANK,
            Some(scope_assets),
            4,
        );
        let bond = bond_for(&scope_obj, bond_lamports, 0);
        let action = AttestedAction {
            receipt_id: [0xAA; 16],
            executed_slot: 1000,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(outsider_seed),
        };
        let r = verify_slash(&bond, &ev(scope_obj, action)).unwrap();
        prop_assert_eq!(r.slash_lamports, bond.lamports);
    }

    /// P5 — When `allowed_assets = None` (any asset), no action with
    /// a target asset can be slashed on the asset gate; only an
    /// action-mask or market mismatch can. The semantic that "None
    /// means any" must not regress to "None means deny all".
    #[test]
    fn allowed_assets_none_does_not_slash_on_asset_gate(
        target_asset in 0u16..10_000,
        bond_lamports in 1u64..1_000_000_000,
    ) {
        let scope_obj = scope(
            1,
            ActionMask::PUSH_MARK | ActionMask::CRANK,
            None,
            4,
        );
        let bond = bond_for(&scope_obj, bond_lamports, 0);
        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: 1000,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(target_asset),
        };
        prop_assert_eq!(
            verify_slash(&bond, &ev(scope_obj, action)),
            Err(SlashRejection::NoViolation)
        );
    }

    /// P6 — When `allowed_assets = Some(vec![])` (deny all assets),
    /// ANY action with a target asset is slashable. This is the
    /// dangerous-and-correct distinction from P5 that the previous
    /// encoding silently collapsed.
    #[test]
    fn allowed_assets_some_empty_slashes_any_asset_target(
        target_asset in 0u16..10_000,
        bond_lamports in 1u64..1_000_000_000,
    ) {
        let scope_obj = scope(
            1,
            ActionMask::PUSH_MARK | ActionMask::CRANK,
            Some(vec![]),
            4,
        );
        let bond = bond_for(&scope_obj, bond_lamports, 0);
        let action = AttestedAction {
            receipt_id: [0; 16],
            executed_slot: 1000,
            market: [1u8; 32],
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(target_asset),
        };
        let r = verify_slash(&bond, &ev(scope_obj, action)).unwrap();
        prop_assert_eq!(r.slash_lamports, bond.lamports);
    }
}
