//! End-to-end slash flow: operator-signed `KeeperScope` → bridge →
//! `BondScope` → bond → attested violation → host-simulated slash.
//!
//! This is the test a paranoid operator runs before posting real
//! lamports. It exercises the whole accountability surface in one
//! pass and proves the bridge's canonical encoding round-trips
//! exactly: the slash hash computed from the bridge's BondScope
//! matches the hash the slasher submits, and the on-chain handler
//! drains the bond when the keeper acts out of scope.

#![cfg(feature = "bridge")]

use covenant_percolator::capability::KeeperScope;
use covenant_percolator::state::ActionLabel;
use covenant_percolator_bond::{
    evidence::AttestedAction,
    program::{handle_initialize, handle_slash, HostAccounts},
    scope::ActionMask,
    BondScope, SlashEvidence,
};

const MARKET: &str = "BhkMic5gHLjj5Uxkg6rBBXofUzeTZVwmV4uFzfhwtgQw";
const OPERATOR: [u8; 32] = [0xA1; 32];
const KEEPER: [u8; 32] = [0xB2; 32];
const RECIPIENT: [u8; 32] = [0xC3; 32];

fn signed_scope() -> KeeperScope {
    // The shape an operator would sign: live mainnet market, narrow
    // asset list, mark+crank only (no recovery authority).
    KeeperScope {
        version: 1,
        market: MARKET.into(),
        allowed_assets: Some(vec![0, 1]),
        allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
        max_actions_per_tick: Some(8),
    }
}

#[test]
fn keeper_acting_on_unscoped_asset_loses_its_bond() {
    let scope = signed_scope();
    let bond_scope = BondScope::from_keeper_scope(&scope).unwrap();
    let mut acc = HostAccounts {
        operator_signer: true,
        operator_pubkey: Some(OPERATOR),
        bond_lamports: 2_000_000_000, // 2 SOL
        ..Default::default()
    };
    handle_initialize(&mut acc, OPERATOR, KEEPER, bond_scope.hash(), RECIPIENT, 100)
        .unwrap();

    // Keeper executed PushAuthMark on asset 7 — outside the
    // operator-signed allowed_assets `[0, 1]`.
    let evidence = SlashEvidence {
        scope: bond_scope.clone(),
        action: AttestedAction {
            receipt_id: [0xAB; 16],
            executed_slot: 250,
            market: bond_scope.market,
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(7),
        },
    };
    let slashed = handle_slash(&mut acc, &evidence).unwrap();
    assert_eq!(slashed, 2_000_000_000);
    assert_eq!(acc.bond.as_ref().unwrap().slashed, 1);
    assert_eq!(acc.recipient_lamports, 2_000_000_000);
}

#[test]
fn keeper_acting_on_a_different_market_loses_its_bond() {
    let scope = signed_scope();
    let bond_scope = BondScope::from_keeper_scope(&scope).unwrap();
    let mut acc = HostAccounts {
        operator_signer: true,
        operator_pubkey: Some(OPERATOR),
        bond_lamports: 1_500_000_000,
        ..Default::default()
    };
    handle_initialize(&mut acc, OPERATOR, KEEPER, bond_scope.hash(), RECIPIENT, 100)
        .unwrap();

    let evidence = SlashEvidence {
        scope: bond_scope.clone(),
        action: AttestedAction {
            receipt_id: [0xCD; 16],
            executed_slot: 250,
            market: [0xFF; 32], // wrong market — not the one in scope
            action_bit: ActionMask::CRANK,
            asset_index: Some(0),
        },
    };
    let slashed = handle_slash(&mut acc, &evidence).unwrap();
    assert_eq!(slashed, 1_500_000_000);
    assert_eq!(acc.bond.as_ref().unwrap().slashed, 1);
}

#[test]
fn well_behaved_keeper_keeps_its_bond() {
    let scope = signed_scope();
    let bond_scope = BondScope::from_keeper_scope(&scope).unwrap();
    let mut acc = HostAccounts {
        operator_signer: true,
        operator_pubkey: Some(OPERATOR),
        bond_lamports: 1_000_000_000,
        ..Default::default()
    };
    handle_initialize(&mut acc, OPERATOR, KEEPER, bond_scope.hash(), RECIPIENT, 100)
        .unwrap();

    // Slasher tries to claim "PushAuthMark on asset 0" is a violation.
    // It's exactly what the operator signed for — verifier refuses.
    let evidence = SlashEvidence {
        scope: bond_scope.clone(),
        action: AttestedAction {
            receipt_id: [0xEF; 16],
            executed_slot: 250,
            market: bond_scope.market,
            action_bit: ActionMask::PUSH_MARK,
            asset_index: Some(0),
        },
    };
    let err = handle_slash(&mut acc, &evidence).unwrap_err();
    assert!(
        format!("{err}").contains("violation"),
        "well-behaved keeper got slashed: {err:?}"
    );
    assert_eq!(acc.bond.as_ref().unwrap().lamports, 1_000_000_000);
    assert_eq!(acc.bond.as_ref().unwrap().slashed, 0);
}

#[test]
fn slasher_cannot_swap_in_a_different_scope() {
    // A slasher who computed a violation against scope A can't
    // re-package it against a bond initialized with scope B — the
    // hash check rejects the swap. The bridge's canonical encoding
    // is what makes that check possible.
    let real = signed_scope();
    let real_bond = BondScope::from_keeper_scope(&real).unwrap();
    let mut acc = HostAccounts {
        operator_signer: true,
        operator_pubkey: Some(OPERATOR),
        bond_lamports: 500_000_000,
        ..Default::default()
    };
    handle_initialize(&mut acc, OPERATOR, KEEPER, real_bond.hash(), RECIPIENT, 100)
        .unwrap();

    // Hostile slasher submits a *different* scope (allow recovery!)
    // hoping the verifier doesn't notice.
    let mut hostile_keeper_scope = real.clone();
    hostile_keeper_scope.allowed_actions = Some(vec![ActionLabel::PushMark]); // narrower
    let hostile_bond = BondScope::from_keeper_scope(&hostile_keeper_scope).unwrap();

    let evidence = SlashEvidence {
        scope: hostile_bond,
        action: AttestedAction {
            receipt_id: [0x99; 16],
            executed_slot: 250,
            market: real_bond.market,
            action_bit: ActionMask::CRANK, // not in the hostile scope's mask
            asset_index: Some(0),
        },
    };
    let err = handle_slash(&mut acc, &evidence).unwrap_err();
    assert!(
        format!("{err}").contains("scope hash"),
        "hostile scope swap not detected: {err:?}"
    );
    assert_eq!(acc.bond.as_ref().unwrap().lamports, 500_000_000);
    assert_eq!(acc.bond.as_ref().unwrap().slashed, 0);
}
