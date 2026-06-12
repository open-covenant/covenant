//! Deterministic compute-unit meter for the merkle-verify program, the
//! on-chain twin of the wasmtime fuel-runner. Builds a frozen all-valid proof
//! batch, sends it to the loaded program under litesvm, and prints the
//! transaction's `compute_units_consumed`. Same program + same batch ->
//! identical CU, every run.
//!
//! Usage: cu-runner <program.so> [--seed N] [--batch RECEIPTS] [--proofs K]
//!   [--baseline CU]
//! Prints: DIGEST <hex> (root, so behavioral drift shows), CU <consumed>,
//! and SCALAR baseline/consumed when --baseline is given.

use covenant_merkle::{Hash, MerkleTree};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signer::{keypair::Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).cloned().unwrap_or_else(|| default.into())
}

// Seeded receipts, deterministic per seed.
fn receipts(n: usize, seed: u64) -> Vec<Hash> {
    let mut s = seed.wrapping_add(0x9e3779b97f4a7c15);
    (0..n)
        .map(|_| {
            let mut r = [0u8; 32];
            for chunk in r.chunks_mut(8) {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                chunk.copy_from_slice(&s.to_le_bytes()[..chunk.len()]);
            }
            r
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let so = a.get(1).expect("usage: cu-runner <program.so> [--seed N] [--batch R] [--proofs K] [--baseline CU]");
    let seed: u64 = arg("--seed", "1").parse().unwrap();
    let batch: usize = arg("--batch", "16384").parse().unwrap();
    let proofs: usize = arg("--proofs", "16").parse().unwrap();
    let baseline: Option<u64> = a.iter().position(|x| x == "--baseline").and_then(|i| a.get(i + 1)).map(|v| v.parse().unwrap());

    let rs = receipts(batch, seed);
    let tree = MerkleTree::build(&rs);
    let root = tree.root();
    let depth = tree.depth();

    // Packed batch: root | depth:u8 | count:u32 | count × { index:u32 | leaf[32] | siblings[depth*32] }
    // Proof indices are spread across the tree (deterministic, seed-stable).
    let mut data = Vec::new();
    data.extend_from_slice(&root);
    data.push(depth as u8);
    data.extend_from_slice(&(proofs as u32).to_le_bytes());
    for k in 0..proofs {
        let idx = (k * batch / proofs.max(1)) % batch;
        data.extend_from_slice(&(idx as u32).to_le_bytes());
        data.extend_from_slice(&rs[idx]);
        for sib in tree.proof(idx) {
            data.extend_from_slice(&sib);
        }
    }

    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, so).expect("load program .so");
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    // Raise the CU limit so a big batch is never capped; the limit ix costs a
    // small constant that doesn't affect relative comparisons.
    let cb_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap();
    let mut cb_data = vec![0x02u8];
    cb_data.extend_from_slice(&1_400_000u32.to_le_bytes());
    let cb_ix = Instruction { program_id: cb_id, accounts: vec![], data: cb_data };
    let verify_ix = Instruction { program_id, accounts: vec![], data };

    let tx = Transaction::new_signed_with_payer(&[cb_ix, verify_ix], Some(&payer.pubkey()), &[&payer], svm.latest_blockhash());
    let meta = match svm.send_transaction(tx) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("verification tx failed (a proof was rejected — the all-valid batch should never fail): {:?}", e.err);
            std::process::exit(1);
        }
    };

    let digest: String = root.iter().map(|b| format!("{b:02x}")).collect();
    let cu = meta.compute_units_consumed;
    println!("DIGEST {digest}");
    println!("CU {cu}");
    if let Some(b) = baseline {
        println!("SCALAR {:.6}", b as f64 / cu as f64);
    }
}
