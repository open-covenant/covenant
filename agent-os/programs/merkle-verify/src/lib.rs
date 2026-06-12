//! On-chain merkle inclusion-proof verifier for receipt batches.
//!
//! The instruction data is a packed batch the program verifies in full:
//!
//!   root[32] | depth:u8 | count:u32le | count × { index:u32le | leaf[32] | siblings[depth*32] }
//!
//! It returns Ok iff every proof recomputes `root`; the first invalid proof
//! returns `Custom(1)`. The CU bench feeds an all-valid batch so the program
//! always does full work (deterministic CU); correctness against invalid
//! proofs is checked by the held-out tests.
//!
//! The verify kernel between the EVOLVE-BLOCK markers is the optimization
//! target. It must agree bit-for-bit with the off-chain `covenant-merkle`
//! reference: leaf = sha256(0x00||receipt), node = sha256(0x01||l||r), folded
//! bottom-up. Hashing goes through `sol_sha256` (the `hashv` wrapper).

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program_error::ProgramError,
    pubkey::Pubkey,
};

#[cfg(target_os = "solana")]
solana_program::entrypoint!(process_instruction);

pub const LEAF_PREFIX: [u8; 1] = [0x00];
pub const NODE_PREFIX: [u8; 1] = [0x01];

pub fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.len() < 37 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let root: &[u8; 32] = data[0..32].try_into().unwrap();
    let depth = data[32] as usize;
    let count = u32::from_le_bytes(data[33..37].try_into().unwrap()) as usize;
    let stride = 4 + 32 + depth * 32;
    let mut off = 37;
    for _ in 0..count {
        if off + stride > data.len() {
            return Err(ProgramError::InvalidInstructionData);
        }
        let index = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        let leaf = &data[off + 4..off + 36];
        let siblings = &data[off + 36..off + 36 + depth * 32];
        if !verify_inclusion(leaf, index, depth, siblings, root) {
            return Err(ProgramError::Custom(1));
        }
        off += stride;
    }
    Ok(())
}

// EVOLVE-BLOCK-START
pub fn verify_inclusion(
    leaf: &[u8],
    index: u32,
    depth: usize,
    siblings: &[u8],
    root: &[u8; 32],
) -> bool {
    let mut h = hashv(&[&LEAF_PREFIX, leaf]).to_bytes();
    let mut idx = index;
    for level in 0..depth {
        let sib = &siblings[level * 32..level * 32 + 32];
        h = if idx & 1 == 0 {
            hashv(&[&NODE_PREFIX, &h, sib]).to_bytes()
        } else {
            hashv(&[&NODE_PREFIX, sib, &h]).to_bytes()
        };
        idx >>= 1;
    }
    &h == root
}
// EVOLVE-BLOCK-END

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_merkle::{MerkleTree, Hash};

    fn receipts(n: usize) -> Vec<Hash> {
        (0..n)
            .map(|i| {
                let mut r = [0u8; 32];
                r[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
                r
            })
            .collect()
    }

    #[test]
    fn matches_reference_on_every_leaf() {
        for n in [1usize, 2, 5, 64, 1000] {
            let rs = receipts(n);
            let tree = MerkleTree::build(&rs);
            let root = tree.root();
            let depth = tree.depth();
            for (i, r) in rs.iter().enumerate() {
                let proof = tree.proof(i);
                let sibs: Vec<u8> = proof.iter().flatten().copied().collect();
                assert!(
                    verify_inclusion(r, i as u32, depth, &sibs, &root),
                    "n={n} i={i}: on-chain kernel must accept a valid proof"
                );
            }
        }
    }

    #[test]
    fn rejects_tampered_proofs() {
        let rs = receipts(256);
        let tree = MerkleTree::build(&rs);
        let root = tree.root();
        let depth = tree.depth();
        let i = 42usize;
        let proof = tree.proof(i);
        let sibs: Vec<u8> = proof.iter().flatten().copied().collect();
        assert!(verify_inclusion(&rs[i], i as u32, depth, &sibs, &root));

        let mut leaf = rs[i];
        leaf[0] ^= 1;
        assert!(!verify_inclusion(&leaf, i as u32, depth, &sibs, &root), "tampered leaf");
        assert!(!verify_inclusion(&rs[i], (i + 1) as u32, depth, &sibs, &root), "wrong index");
        let mut bad = sibs.clone();
        bad[0] ^= 1;
        assert!(!verify_inclusion(&rs[i], i as u32, depth, &bad, &root), "tampered sibling");
        let mut br = root;
        br[0] ^= 1;
        assert!(!verify_inclusion(&rs[i], i as u32, depth, &sibs, &br), "wrong root");
    }
}
