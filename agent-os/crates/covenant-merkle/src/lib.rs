//! Domain-separated merkle tree over receipt hashes.
//!
//! Scheme (RFC-6962-style domain separation, prevents leaf/node confusion):
//! - leaf node  = sha256(0x00 || receipt[32])
//! - inner node = sha256(0x01 || left[32] || right[32])
//!
//! Batches are padded to the next power of two with `EMPTY_LEAF` so every
//! tree is perfect: a proof is exactly `depth` siblings and verification is a
//! fixed-length fold, which is what makes the on-chain CU cost predictable
//! and the kernel a clean optimization target.
//!
//! This crate is the off-chain reference. The on-chain verifier
//! (`covenant-merkle-verify`) recomputes the same hashes with the
//! `sol_sha256` syscall and must agree bit-for-bit.

use sha2::{Digest, Sha256};

pub const LEAF_PREFIX: u8 = 0x00;
pub const NODE_PREFIX: u8 = 0x01;

pub type Hash = [u8; 32];

pub fn leaf_hash(receipt: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([LEAF_PREFIX]);
    h.update(receipt);
    h.finalize().into()
}

pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([NODE_PREFIX]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Hash of the padding leaf (`leaf_hash([0u8; 32])`), used to pad a batch up
/// to a power of two.
pub fn empty_leaf() -> Hash {
    leaf_hash(&[0u8; 32])
}

/// A perfect binary merkle tree, levels bottom-up: `levels[0]` are the leaf
/// hashes (padded to a power of two), `levels.last()` is `[root]`.
pub struct MerkleTree {
    pub levels: Vec<Vec<Hash>>,
}

impl MerkleTree {
    /// Build over `receipts` (raw 32-byte receipt hashes), padding to the
    /// next power of two. An empty batch yields a single `EMPTY_LEAF` root.
    pub fn build(receipts: &[Hash]) -> Self {
        let n = receipts.len().max(1).next_power_of_two();
        let empty = empty_leaf();
        let mut level: Vec<Hash> = (0..n)
            .map(|i| receipts.get(i).map(leaf_hash).unwrap_or(empty))
            .collect();
        let mut levels = vec![level.clone()];
        while level.len() > 1 {
            level = level
                .chunks_exact(2)
                .map(|pair| node_hash(&pair[0], &pair[1]))
                .collect();
            levels.push(level.clone());
        }
        Self { levels }
    }

    pub fn root(&self) -> Hash {
        self.levels.last().expect("non-empty levels")[0]
    }

    pub fn depth(&self) -> usize {
        self.levels.len() - 1
    }

    /// Inclusion proof for the leaf at `index`: the sibling hash at each level
    /// from the bottom up. Verify with [`verify`].
    pub fn proof(&self, index: usize) -> Vec<Hash> {
        let mut siblings = Vec::with_capacity(self.depth());
        let mut idx = index;
        for level in &self.levels[..self.depth()] {
            let sib = idx ^ 1;
            siblings.push(level[sib]);
            idx >>= 1;
        }
        siblings
    }
}

/// Recompute the root from a leaf, its index, and its sibling path; return
/// true iff it equals `root`. The reference the on-chain kernel must match.
pub fn verify(receipt: &Hash, index: usize, siblings: &[Hash], root: &Hash) -> bool {
    let mut h = leaf_hash(receipt);
    let mut idx = index;
    for sib in siblings {
        h = if idx & 1 == 0 {
            node_hash(&h, sib)
        } else {
            node_hash(sib, &h)
        };
        idx >>= 1;
    }
    &h == root
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn every_leaf_verifies() {
        for n in [1usize, 2, 3, 5, 8, 17, 64, 1000] {
            let rs = receipts(n);
            let tree = MerkleTree::build(&rs);
            let root = tree.root();
            for (i, r) in rs.iter().enumerate() {
                let p = tree.proof(i);
                assert!(verify(r, i, &p, &root), "n={n} i={i} valid proof must verify");
            }
        }
    }

    #[test]
    fn depth_is_log2_padded() {
        assert_eq!(MerkleTree::build(&receipts(1)).depth(), 0);
        assert_eq!(MerkleTree::build(&receipts(2)).depth(), 1);
        assert_eq!(MerkleTree::build(&receipts(3)).depth(), 2);
        assert_eq!(MerkleTree::build(&receipts(1000)).depth(), 10);
    }

    #[test]
    fn tampered_inputs_rejected() {
        let rs = receipts(64);
        let tree = MerkleTree::build(&rs);
        let root = tree.root();
        let i = 7;
        let p = tree.proof(i);
        assert!(verify(&rs[i], i, &p, &root));

        // wrong leaf
        let mut bad = rs[i];
        bad[0] ^= 1;
        assert!(!verify(&bad, i, &p, &root));
        // wrong index
        assert!(!verify(&rs[i], i + 1, &p, &root));
        // wrong sibling
        let mut bp = p.clone();
        bp[0][0] ^= 1;
        assert!(!verify(&rs[i], i, &bp, &root));
        // wrong root
        let mut br = root;
        br[0] ^= 1;
        assert!(!verify(&rs[i], i, &p, &br));
        // truncated proof
        assert!(!verify(&rs[i], i, &p[..p.len() - 1], &root));
    }

    #[test]
    fn domain_separation_holds() {
        // a leaf and an inner node over the same 32 bytes must differ
        let z = [0u8; 32];
        assert_ne!(leaf_hash(&z), node_hash(&z, &z));
    }
}
