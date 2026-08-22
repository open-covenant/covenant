use crate::{
    instruction::{BindArgs, EscrowInstruction, FundArgs, ResolveArgs},
    state::{GUARD_LEN, STATE_LEN, VAULT_LEN},
};
use serde_json::Value;
use solana_sdk::pubkey::Pubkey;

fn spec() -> Value {
    serde_json::from_str(include_str!("../abi/mizuki-escrow-v1.json")).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn machine_spec_matches_program_constants() {
    let spec = spec();
    assert_eq!(spec["state"]["length"], STATE_LEN);
    assert_eq!(spec["vault"]["length"], VAULT_LEN);
    assert_eq!(spec["guard"]["length"], GUARD_LEN);
    let expected = [
        ("fund", 0, 84),
        ("bind", 1, 105),
        ("release", 2, 65),
        ("refund", 3, 65),
    ];
    for (index, (name, discriminator, length)) in expected.into_iter().enumerate() {
        let instruction = &spec["instructions"][index];
        assert_eq!(instruction["name"], name);
        assert_eq!(instruction["discriminator"], discriminator);
        assert_eq!(instruction["dataLength"], length);
    }
}

#[test]
fn golden_instruction_vectors_are_byte_exact() {
    let spec = spec();
    let bounty_id = [0x11; 32];
    let vectors = [
        (
            "fund",
            EscrowInstruction::Fund(FundArgs {
                bounty_id,
                amount_lamports: 0x0102_0304_0506_0708,
                offer_expires_at: 1_700_000_000,
                acceptance_commitment: [0xaa; 32],
                state_bump: 255,
                vault_bump: 254,
                guard_bump: 253,
            }),
        ),
        (
            "bind",
            EscrowInstruction::Bind(BindArgs {
                bounty_id,
                claimant: Pubkey::new_from_array([0x22; 32]),
                claim_expires_at: 1_700_003_600,
                claim_commitment: [0xbb; 32],
            }),
        ),
        (
            "release",
            EscrowInstruction::Release(ResolveArgs {
                bounty_id,
                resolution_evidence: [0xcc; 32],
            }),
        ),
        (
            "refund",
            EscrowInstruction::Refund(ResolveArgs {
                bounty_id,
                resolution_evidence: [0xdd; 32],
            }),
        ),
    ];
    for (name, instruction) in vectors {
        assert_eq!(
            hex(&instruction.encode()),
            spec["goldenVectors"][name]["dataHex"]
        );
    }
}
