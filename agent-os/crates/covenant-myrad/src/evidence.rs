//! The evidence half of the receipt: what a signal was derived from.
//!
//! A buyer receives an aggregate. What they cannot see is whether it came from
//! the contributions it claims: N distinct consenting subjects, each backed by a
//! verified Reclaim proof, none of them counted twice. This module turns each
//! contribution into a commitment and folds the set into one Merkle root that
//! the receipt carries.
//!
//! The commitment is a hash of the contribution's provenance fields, never its
//! content, so the root reveals nothing. Two properties make it useful:
//!
//! - **Membership.** Myrad can later show a specific contribution is under the
//!   root ([`MerkleTree::inclusion_proof`]) without publishing the rest of the
//!   set. A buyer auditing a dispute checks one leaf, not the population.
//! - **Distinctness.** The leaf commits to a salted subject pseudonym, so one
//!   subject appearing twice in a cohort produces two identical
//!   `subject_commitment` values and is caught by
//!   [`crate::integrity`] before the root is signed. k-anonymity hides who a
//!   contributor is; this is what stops one contributor being counted as five.
//!
//! Leaves are sorted before folding, so the root depends on the set and not on
//! the order Myrad's query happened to return.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::signal::{ProofRef, SignalRecord};

pub const CONTRIBUTION_SCHEMA: &str = "covenant.myrad.contribution.v1";

/// Domain separation: a leaf hash can never be read as an interior node, so no
/// interior node can be passed off as a contribution.
const LEAF_TAG: u8 = 0x00;
const NODE_TAG: u8 = 0x01;

/// One contributing record, reduced to provenance. No behavior data, no
/// identifiers, nothing that describes a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    pub schema: String,
    pub provider: String,
    pub proof: ProofRef,
    /// Salted pseudonym from Myrad. `None` when the source didn't supply one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_commitment: Option<String>,
    /// Digest of the exact `sellable_data` this contribution carried.
    pub payload_sha256: String,
    /// Emission time as recorded by the pipeline.
    pub generated_at: String,
    pub opt_out: bool,
}

impl Contribution {
    pub fn from_record(record: &SignalRecord) -> Result<Self> {
        Ok(Self {
            schema: CONTRIBUTION_SCHEMA.to_string(),
            provider: record.provider.clone(),
            proof: record.proof.clone(),
            subject_commitment: record.subject_commitment.clone(),
            payload_sha256: record.payload_sha256()?,
            generated_at: record.generated_at().unwrap_or_default().to_string(),
            opt_out: record.opt_out,
        })
    }

    /// sha256 over the RFC 8785 canonical form. This is the Merkle leaf value.
    pub fn commitment_hex(&self) -> Result<String> {
        let canon = serde_jcs::to_string(self).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(hex::encode(Sha256::digest(canon.as_bytes())))
    }
}

/// A Merkle tree over contribution commitments.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    leaves: Vec<String>,
    levels: Vec<Vec<[u8; 32]>>,
}

fn leaf_hash(commitment_hex: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([LEAF_TAG]);
    h.update(commitment_hex.as_bytes());
    h.finalize().into()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([NODE_TAG]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

impl MerkleTree {
    /// Build over the given commitments. Leaves are sorted, so the root is a
    /// function of the set. An odd node at any level is promoted rather than
    /// duplicated, which keeps a two-leaf set from colliding with the one-leaf
    /// set that duplication would produce.
    pub fn build(commitments: &[String]) -> Result<Self> {
        if commitments.is_empty() {
            return Err(Error::Evidence("no contributions".into()));
        }
        let mut leaves = commitments.to_vec();
        leaves.sort();

        let mut levels = vec![leaves.iter().map(|c| leaf_hash(c)).collect::<Vec<_>>()];
        while levels.last().map(Vec::len).unwrap_or(0) > 1 {
            let prev = levels.last().expect("non-empty");
            let mut next = Vec::with_capacity(prev.len().div_ceil(2));
            for pair in prev.chunks(2) {
                next.push(match pair {
                    [l, r] => node_hash(l, r),
                    [l] => *l,
                    _ => unreachable!("chunks(2) yields 1 or 2"),
                });
            }
            levels.push(next);
        }

        Ok(Self { leaves, levels })
    }

    pub fn root_hex(&self) -> String {
        hex::encode(self.levels.last().expect("built")[0])
    }

    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// The sibling path proving `commitment_hex` is under the root.
    pub fn inclusion_proof(&self, commitment_hex: &str) -> Result<InclusionProof> {
        let mut index = self
            .leaves
            .iter()
            .position(|c| c == commitment_hex)
            .ok_or_else(|| Error::Evidence(format!("not a leaf: {commitment_hex}")))?;

        let mut path = Vec::new();
        for level in &self.levels[..self.levels.len() - 1] {
            let sibling = if index % 2 == 0 { index + 1 } else { index - 1 };
            if let Some(node) = level.get(sibling) {
                path.push(PathStep {
                    sibling_hex: hex::encode(node),
                    sibling_is_left: index % 2 == 1,
                });
            }
            index /= 2;
        }

        Ok(InclusionProof {
            commitment_hex: commitment_hex.to_string(),
            path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathStep {
    pub sibling_hex: String,
    pub sibling_is_left: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionProof {
    pub commitment_hex: String,
    pub path: Vec<PathStep>,
}

impl InclusionProof {
    /// Recompute the root from the leaf and the sibling path.
    pub fn verify(&self, root_hex: &str) -> bool {
        let mut acc = leaf_hash(&self.commitment_hex);
        for step in &self.path {
            let Ok(sibling) = hex::decode(&step.sibling_hex) else {
                return false;
            };
            let Ok(sibling): std::result::Result<[u8; 32], _> = sibling.try_into() else {
                return false;
            };
            acc = if step.sibling_is_left {
                node_hash(&sibling, &acc)
            } else {
                node_hash(&acc, &sibling)
            };
        }
        hex::encode(acc) == root_hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commitments(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| hex::encode(Sha256::digest(format!("contribution-{i}").as_bytes())))
            .collect()
    }

    #[test]
    fn root_is_order_independent() {
        let c = commitments(5);
        let forward = MerkleTree::build(&c).unwrap().root_hex();
        let mut reversed = c.clone();
        reversed.reverse();
        assert_eq!(forward, MerkleTree::build(&reversed).unwrap().root_hex());
    }

    #[test]
    fn root_changes_when_the_set_changes() {
        let five = MerkleTree::build(&commitments(5)).unwrap().root_hex();
        let six = MerkleTree::build(&commitments(6)).unwrap().root_hex();
        assert_ne!(five, six);
    }

    #[test]
    fn every_leaf_proves_inclusion() {
        for n in [1usize, 2, 3, 4, 7, 12, 33] {
            let c = commitments(n);
            let tree = MerkleTree::build(&c).unwrap();
            let root = tree.root_hex();
            for leaf in &c {
                let proof = tree.inclusion_proof(leaf).unwrap();
                assert!(proof.verify(&root), "n={n} leaf={leaf}");
            }
        }
    }

    #[test]
    fn a_foreign_leaf_does_not_prove_inclusion() {
        let tree = MerkleTree::build(&commitments(8)).unwrap();
        let root = tree.root_hex();
        let outsider = hex::encode(Sha256::digest(b"not in the set"));
        assert!(tree.inclusion_proof(&outsider).is_err());

        let mut forged = tree.inclusion_proof(&commitments(8)[0]).unwrap();
        forged.commitment_hex = outsider;
        assert!(!forged.verify(&root));
    }

    #[test]
    fn empty_set_is_rejected() {
        assert!(MerkleTree::build(&[]).is_err());
    }

    #[test]
    fn commitment_reveals_no_payload() {
        let contribution = Contribution {
            schema: CONTRIBUTION_SCHEMA.to_string(),
            provider: "netflix".into(),
            proof: ProofRef {
                id: "5a9f3035aa9cf93f653f4b15".into(),
                kind: crate::signal::ProofRefKind::ReclaimProof,
            },
            subject_commitment: Some("aa".repeat(32)),
            payload_sha256: "bb".repeat(32),
            generated_at: "2026-05-19T17:40:20.697Z".into(),
            opt_out: false,
        };
        let canon = serde_jcs::to_string(&contribution).unwrap();
        assert!(!canon.contains("watch_hours"));
        assert_eq!(contribution.commitment_hex().unwrap().len(), 64);
    }
}
