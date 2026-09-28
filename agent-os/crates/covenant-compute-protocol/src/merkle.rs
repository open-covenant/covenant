//! A binary Merkle tree over settlement leaves, following the RFC 6962
//! transparency-log construction: a leaf is hashed under a `0x00` prefix and
//! an interior node under `0x01`, and an odd node is carried up unchanged
//! rather than hashed against a copy of itself. That last rule is the reason
//! this is a separate implementation from [`covenant_settlement`]'s
//! Bitcoin-style tree: promoting the odd node instead of duplicating it closes
//! the second-preimage ambiguity (CVE-2012-2459) where a tree over `n` leaves
//! and one over `n` leaves with the last one repeated share a root. A batch
//! commitment a reader treats as final must not have two preimages.
//!
//! The tree operates on 32-byte leaf hashes. Callers hash their own entries
//! with [`hash_leaf`] first, so the domain of what a leaf commits to stays with
//! the caller; the tree only combines hashes.

use sha2::{Digest, Sha256};

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// The leaf hash of an entry: `SHA-256(0x00 || entry)`. The prefix keeps a leaf
/// hash from ever colliding with an interior node, so an entry can't be passed
/// off as a subtree.
pub fn hash_leaf(entry: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(entry);
    hasher.finalize().into()
}

fn hash_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// The largest power of two strictly less than `n`, for `n >= 2` — the split
/// point RFC 6962 uses so the left subtree is always a full, complete tree.
fn split(n: usize) -> usize {
    debug_assert!(n >= 2);
    let mut k = 1;
    while k << 1 < n {
        k <<= 1;
    }
    k
}

/// The Merkle Tree Hash over `leaves`, already leaf-hashed. An empty tree
/// hashes to `SHA-256("")`; a single leaf is its own root.
pub fn root(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves {
        [] => Sha256::digest([]).into(),
        [only] => *only,
        _ => {
            let k = split(leaves.len());
            hash_node(&root(&leaves[..k]), &root(&leaves[k..]))
        }
    }
}

/// The audit path proving `leaves[index]` sits under [`root`]: the sibling
/// hashes from the leaf up to the root, leaf-adjacent sibling first. Empty for
/// a single-leaf tree. Returns `None` if `index` is out of range.
pub fn audit_path(index: usize, leaves: &[[u8; 32]]) -> Option<Vec<[u8; 32]>> {
    if index >= leaves.len() {
        return None;
    }
    Some(path_within(index, leaves))
}

fn path_within(index: usize, leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    if leaves.len() <= 1 {
        return Vec::new();
    }
    let k = split(leaves.len());
    if index < k {
        let mut path = path_within(index, &leaves[..k]);
        path.push(root(&leaves[k..]));
        path
    } else {
        let mut path = path_within(index - k, &leaves[k..]);
        path.push(root(&leaves[..k]));
        path
    }
}

/// Reconstruct the tree root from a leaf and its audit path, per the RFC 9162
/// inclusion-proof algorithm. Returns the root a verifier compares against the
/// committed one, or `None` if `index`/`tree_size` and the path length are not
/// consistent (a path too long or too short for the claimed position).
pub fn root_from_path(
    leaf: [u8; 32],
    index: usize,
    tree_size: usize,
    path: &[[u8; 32]],
) -> Option<[u8; 32]> {
    if index >= tree_size {
        return None;
    }
    let mut node_index = index;
    let mut last_index = tree_size - 1;
    let mut hash = leaf;
    for sibling in path {
        if last_index == 0 {
            return None;
        }
        if node_index & 1 == 1 || node_index == last_index {
            hash = hash_node(sibling, &hash);
            if node_index & 1 == 0 {
                while node_index & 1 == 0 && node_index != 0 {
                    node_index >>= 1;
                    last_index >>= 1;
                }
            }
        } else {
            hash = hash_node(&hash, sibling);
        }
        node_index >>= 1;
        last_index >>= 1;
    }
    (last_index == 0).then_some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<[u8; 32]> {
        (0..n).map(|i| hash_leaf(&[i as u8])).collect()
    }

    fn hex(bytes: &[u8; 32]) -> String {
        let mut out = String::with_capacity(64);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }

    #[test]
    fn empty_tree_is_the_hash_of_no_bytes() {
        // The RFC 6962 empty-tree root, SHA-256 of the empty string.
        assert_eq!(
            hex(&root(&[])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_single_leaf_is_its_own_root() {
        let only = hash_leaf(b"solo");
        assert_eq!(root(&[only]), only);
        assert_eq!(audit_path(0, &[only]).unwrap(), Vec::<[u8; 32]>::new());
    }

    #[test]
    fn two_leaves_hash_under_the_node_prefix() {
        let l = leaves(2);
        assert_eq!(root(&l), hash_node(&l[0], &l[1]));
    }

    #[test]
    fn an_odd_leaf_is_promoted_not_duplicated() {
        // The whole reason for this tree: root([a,b,c]) must not equal
        // root([a,b,c,c]). A Bitcoin-style tree duplicates c and the two
        // collide; RFC 6962 promotes c and they stay distinct.
        let three = leaves(3);
        let mut padded = leaves(3);
        padded.push(three[2]);
        assert_ne!(root(&three), root(&padded));

        // And the promoted shape is explicit: H(H(a,b), c).
        let left = hash_node(&three[0], &three[1]);
        assert_eq!(root(&three), hash_node(&left, &three[2]));
    }

    #[test]
    fn every_leaf_of_every_small_tree_proves_inclusion() {
        for n in 1..=17 {
            let l = leaves(n);
            let expected = root(&l);
            for index in 0..n {
                let path = audit_path(index, &l).unwrap();
                assert_eq!(
                    root_from_path(l[index], index, n, &path),
                    Some(expected),
                    "n={n} index={index}"
                );
            }
            assert!(audit_path(n, &l).is_none(), "out-of-range index for n={n}");
        }
    }

    #[test]
    fn a_tampered_leaf_or_path_does_not_reconstruct_the_root() {
        let l = leaves(6);
        let expected = root(&l);
        let path = audit_path(4, &l).unwrap();
        assert_eq!(root_from_path(l[4], 4, 6, &path), Some(expected));

        // Wrong leaf.
        assert_ne!(
            root_from_path(hash_leaf(b"forged"), 4, 6, &path),
            Some(expected)
        );
        // Wrong claimed position.
        assert_ne!(root_from_path(l[4], 3, 6, &path), Some(expected));
        // A flipped sibling.
        let mut bent = path.clone();
        bent[0][0] ^= 0x01;
        assert_ne!(root_from_path(l[4], 4, 6, &bent), Some(expected));
    }

    #[test]
    fn a_path_of_the_wrong_length_is_rejected() {
        let l = leaves(5);
        let mut path = audit_path(1, &l).unwrap();
        // Too short.
        let short = &path[..path.len() - 1];
        assert_eq!(root_from_path(l[1], 1, 5, short), None);
        // Too long.
        path.push([0u8; 32]);
        assert_eq!(root_from_path(l[1], 1, 5, &path), None);
        // Out of range.
        assert_eq!(root_from_path(l[1], 5, 5, &[]), None);
    }
}
