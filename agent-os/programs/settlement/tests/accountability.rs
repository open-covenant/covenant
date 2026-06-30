mod common;

use common::*;

const AGENT: [u8; 32] = [0xACu8; 32];

// The agent-accountability loop, end to end.
//
// Covenant's pitch to MagicBlock is "an accountability layer for agents". This
// pins the loop that makes that concrete, tying the economic side (a slashable
// bond on L1) to the verifiable side (a per-action provenance hash-chain that
// rides the ER):
//
//   1. Bond     — register the agent and stake a slashable position against it.
//   2. Act      — the agent meters its work; every receipt folds into the
//                 credit account's on-chain provenance root (the same fold that
//                 runs gaslessly in the ephemeral rollup and commits to L1).
//   3. Slash    — on a violation, the bond is burned with a reason that points
//                 at that provenance root, so the penalty is anchored to the
//                 verifiable record of what the agent actually did, not an
//                 arbitrary claim.
#[test]
fn agent_accountability_bond_provenance_slash() {
    let mut env = boot();

    // 1. Bond.
    register_agent(&mut env, &AGENT);
    let (position, stake_vault, _) = stake_locked(&mut env, &AGENT, 1_000, 0);
    assert_eq!(agent_stake(&env, &AGENT), 1_000, "agent is bonded");

    // 2. Act: two metered actions, each folded into the provenance chain.
    let credits = open_credit_account(&mut env);
    let owner_covnt = funded_covnt(&mut env, 1_000);
    buy_credits(&mut env, &credits, &owner_covnt, 10).expect("buy credits");
    consume_credits_with(&mut env, &credits, 1, [0xA1u8; 32]).expect("action 1");
    consume_credits_with(&mut env, &credits, 1, [0xA2u8; 32]).expect("action 2");
    let provenance = credit_provenance_root(&env, &credits);
    assert_ne!(provenance, [0u8; 32], "the agent's actions are recorded on chain");

    // 3. Slash, referencing the provenance root as the reason.
    let slash_vault = env.treasury;
    let vault_before = token_balance(&env, &slash_vault);
    slash_stake_with(
        &mut env,
        &AGENT,
        &position,
        &stake_vault,
        &slash_vault,
        1_000,
        provenance,
    )
    .expect("slash the bond, citing the on-chain provenance");

    assert_eq!(agent_stake(&env, &AGENT), 0, "bond fully slashed");
    assert_eq!(
        token_balance(&env, &slash_vault) - vault_before,
        1_000,
        "the slashed bond moved to the slash vault"
    );
}
