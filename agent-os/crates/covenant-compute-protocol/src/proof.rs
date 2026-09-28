//! The coordinator's published settlement citation. A [`SignedWorkReceipt`]
//! proves what an operator committed to; [`verify_payout_transaction`] proves
//! what the chain paid. This ties the two to the transaction the coordinator
//! cited, so a reader holding one record can walk receipt → memo → transfer
//! without trusting the coordinator: it re-checks the receipt's signature and
//! the release arithmetic here, fetches the payout with
//! [`SettlementProof::payout_rpc_request`], and settles the whole claim with
//! [`SettlementProof::verify`].

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::merkle;
use crate::payout::{payout_transaction_rpc_request, verify_payout_transaction, PayoutProof};
use crate::receipt::SignedWorkReceipt;
use crate::sign::{to_canonical_json, ProtocolError};

fn to_hex(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn decode_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let byte = std::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(byte, 16).ok()?;
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementProof {
    pub job_id: Uuid,
    /// The operator's signed receipt — self-verifying, and the payload the
    /// on-chain memo commits to.
    pub receipt: SignedWorkReceipt,
    /// Gross released from escrow for this job: the envelope price for a
    /// one-shot job, the metered draw for a lease. Never above the receipt's
    /// signed price, which a lease carries as its window ceiling.
    pub gross_micro_usdc: u64,
    /// The marketplace fee withheld from the gross.
    pub fee_micro_usdc: u64,
    /// The operator's net, transferred on-chain: `gross - fee`, and the figure
    /// the payout transaction credits.
    pub net_micro_usdc: u64,
    /// The mint the net was paid in — pinned so a payout in some worthless
    /// token can't pass for one in USDC.
    pub mint_b58: String,
    /// The wallet the net was paid to — the payout transaction's payee.
    pub payout_address_b58: String,
    /// The on-chain payout the coordinator cited.
    pub tx_signature: String,
    /// The memo that payout must carry, `compute-payout:v1:<job>:<sig>`.
    /// Derived from `receipt`; carried so a reader can scan a transaction's
    /// memos without recomputing it, and re-derived by [`Self::verify_offline`]
    /// so a doctored copy is caught.
    pub payout_memo: String,
}

impl SettlementProof {
    /// Everything checkable without the chain: the receipt's own signature,
    /// that the top-level job id and cited memo are the ones this receipt
    /// derives, and that the release split is sound (`net + fee == gross`, and
    /// the gross never exceeds the signed price). A reader runs this first,
    /// then confirms the transfer with [`Self::verify`].
    pub fn verify_offline(&self) -> Result<(), ProtocolError> {
        self.receipt.verify()?;
        if self.job_id != self.receipt.receipt.job_id {
            return Err(ProtocolError::Invalid(
                "job_id does not match the receipt".into(),
            ));
        }
        if self.payout_memo != self.receipt.payout_memo() {
            return Err(ProtocolError::Invalid(
                "payout_memo is not the one this receipt derives".into(),
            ));
        }
        if self.net_micro_usdc.checked_add(self.fee_micro_usdc) != Some(self.gross_micro_usdc) {
            return Err(ProtocolError::Invalid(format!(
                "net {} + fee {} does not sum to the released gross {}",
                self.net_micro_usdc, self.fee_micro_usdc, self.gross_micro_usdc
            )));
        }
        if self.gross_micro_usdc > self.receipt.receipt.price_micro_usdc {
            return Err(ProtocolError::Invalid(format!(
                "released gross {} exceeds the signed price {}",
                self.gross_micro_usdc, self.receipt.receipt.price_micro_usdc
            )));
        }
        Ok(())
    }

    /// The `getTransaction` request that fetches the cited payout in the
    /// encoding [`Self::verify`] parses — one definition so no fetcher drifts
    /// from the shape the verifier reads.
    pub fn payout_rpc_request(&self) -> Value {
        payout_transaction_rpc_request(&self.tx_signature)
    }

    /// The whole claim, given the fetched payout transaction: the offline
    /// checks, then that the transaction carries this receipt's memo and
    /// credited exactly `net_micro_usdc` in `mint_b58` to `payout_address_b58`.
    /// Returns what the chain actually paid, read off the transaction itself.
    pub fn verify(&self, tx: &Value) -> Result<PayoutProof, ProtocolError> {
        self.verify_offline()?;
        let paid = verify_payout_transaction(&self.payout_memo, tx)?;
        if paid.amount_micro_usdc != self.net_micro_usdc {
            return Err(ProtocolError::Invalid(format!(
                "payout moved {} micro-USDC, not the cited net {}",
                paid.amount_micro_usdc, self.net_micro_usdc
            )));
        }
        if paid.mint_b58 != self.mint_b58 {
            return Err(ProtocolError::Invalid(format!(
                "payout was in mint {}, not the cited {}",
                paid.mint_b58, self.mint_b58
            )));
        }
        if paid.recipient_owner_b58 != self.payout_address_b58 {
            return Err(ProtocolError::Invalid(format!(
                "payout paid {}, not the cited operator wallet {}",
                paid.recipient_owner_b58, self.payout_address_b58
            )));
        }
        Ok(paid)
    }

    /// This proof's Merkle leaf: `SHA-256(0x00 || canonical-json(self))`. The
    /// leaf commits to the whole proof — the signed receipt, the release split,
    /// and the cited payout — so any change to the settlement moves the leaf,
    /// and the encoding is one a reader can reproduce from the proof it fetched.
    pub fn batch_leaf(&self) -> [u8; 32] {
        let bytes = to_canonical_json(self).unwrap_or_default().into_bytes();
        merkle::hash_leaf(&bytes)
    }
}

/// A batch commitment over a set of settled jobs: one Merkle root that binds
/// exactly those settlements in that order. Given the root and a
/// [`BatchInclusionProof`], a reader confirms one settlement's membership with
/// O(log n) hashes instead of the whole set, and a reader that keeps a root and
/// a proof can later show the coordinator committed to that settlement even if
/// a later feed omits it. (Proving the feed only ever grew between two roots is
/// a consistency proof, which this does not yet carry.) The root is computed
/// with the RFC 6962 construction in [`crate::merkle`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementBatch {
    /// The Merkle root over the batch's leaves, hex.
    pub root_hex: String,
    /// The number of leaves, i.e. `job_ids.len()`.
    pub tree_size: usize,
    /// Leaf order: `job_ids[i]` is the job committed at leaf `i`. A reader maps
    /// a job to its position, then requests that leaf's inclusion proof.
    pub job_ids: Vec<Uuid>,
}

impl SettlementBatch {
    /// Commit to `proofs` in the given order. The order is the leaf order and
    /// is part of the commitment, so the caller fixes a canonical one (the
    /// coordinator commits oldest-settled first).
    pub fn commit(proofs: &[SettlementProof]) -> Self {
        let leaves: Vec<[u8; 32]> = proofs.iter().map(SettlementProof::batch_leaf).collect();
        SettlementBatch {
            root_hex: to_hex(&merkle::root(&leaves)),
            tree_size: proofs.len(),
            job_ids: proofs.iter().map(|proof| proof.job_id).collect(),
        }
    }
}

/// A single settlement's inclusion under a [`SettlementBatch`] root: the
/// settlement itself plus the audit path from its leaf up to the root. Verifies
/// in two steps — the settlement is internally sound, and its leaf reconstructs
/// the pinned root — so a reader confirms one settlement without fetching the
/// rest of the batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchInclusionProof {
    /// The settlement being proven — self-verifying in its own right.
    pub proof: SettlementProof,
    /// The leaf's position in the committed order.
    pub leaf_index: usize,
    /// The batch size the audit path was cut for; the root only reconstructs
    /// against the size it was built at.
    pub tree_size: usize,
    /// Sibling hashes from the leaf up to the root, leaf-adjacent first, hex.
    pub audit_path_hex: Vec<String>,
}

impl BatchInclusionProof {
    /// Build the proof for `proofs[leaf_index]` against the batch
    /// [`SettlementBatch::commit`] would produce over the same `proofs`.
    /// `None` if `leaf_index` is out of range.
    pub fn build(proofs: &[SettlementProof], leaf_index: usize) -> Option<Self> {
        let leaves: Vec<[u8; 32]> = proofs.iter().map(SettlementProof::batch_leaf).collect();
        let path = merkle::audit_path(leaf_index, &leaves)?;
        Some(BatchInclusionProof {
            proof: proofs[leaf_index].clone(),
            leaf_index,
            tree_size: proofs.len(),
            audit_path_hex: path.iter().map(to_hex).collect(),
        })
    }

    /// Everything checkable without the chain: the settlement's own offline
    /// checks, then that its leaf reconstructs `root_hex` through the audit
    /// path at the claimed position. A reader who pinned `root_hex` now knows
    /// this exact settlement is the one the batch committed at `leaf_index`.
    pub fn verify_offline(&self, root_hex: &str) -> Result<(), ProtocolError> {
        self.proof.verify_offline()?;
        let mut path = Vec::with_capacity(self.audit_path_hex.len());
        for node in &self.audit_path_hex {
            path.push(decode_hash(node).ok_or_else(|| {
                ProtocolError::Invalid("audit path node is not a 32-byte hex hash".into())
            })?);
        }
        let reconstructed = merkle::root_from_path(
            self.proof.batch_leaf(),
            self.leaf_index,
            self.tree_size,
            &path,
        )
        .ok_or_else(|| {
            ProtocolError::Invalid("audit path does not fit the claimed leaf position".into())
        })?;
        if to_hex(&reconstructed) != root_hex {
            return Err(ProtocolError::Invalid(format!(
                "settlement does not sit under the batch root {root_hex}"
            )));
        }
        Ok(())
    }

    /// The whole claim: the offline checks against `root_hex`, then the cited
    /// payout on-chain — the same on-chain settle [`SettlementProof::verify`]
    /// offers, now anchored to a pinned batch.
    pub fn verify(&self, root_hex: &str, tx: &Value) -> Result<PayoutProof, ProtocolError> {
        self.verify_offline(root_hex)?;
        self.proof.verify(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payout::payout_memo_for;
    use crate::receipt::{JobMeter, WorkReceiptPayload};
    use covenant_a2a::A2ATaskStatus;
    use covenant_identity::LocalIdentity;

    const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    const PAYOUT_WALLET: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";
    const PAYER: &str = "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4T";

    fn signed_receipt(operator: &LocalIdentity, price: u64) -> SignedWorkReceipt {
        let payload = WorkReceiptPayload {
            job_id: Uuid::new_v4(),
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: JobMeter {
                wall_ms: 164_952,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: Some(164.952),
                finish_reason: None,
            },
            price_micro_usdc: price,
            status: A2ATaskStatus::Ok,
            executed_at_ms: 1_725_000_000_000,
            node_audit_root_hex: "cc".repeat(32),
        };
        SignedWorkReceipt::sign(payload, operator).expect("sign")
    }

    fn proof(receipt: SignedWorkReceipt, gross: u64, fee: u64) -> SettlementProof {
        let payout_memo = receipt.payout_memo();
        SettlementProof {
            job_id: receipt.receipt.job_id,
            gross_micro_usdc: gross,
            fee_micro_usdc: fee,
            net_micro_usdc: gross - fee,
            mint_b58: MINT.into(),
            payout_address_b58: PAYOUT_WALLET.into(),
            tx_signature: "5Gf1v2SJLreKoDcutE7JTNVEwM9mFNbexPuoYt3if83iu8KpS9GnqMhSskG9c1RhNL1V"
                .into(),
            payout_memo,
            receipt,
        }
    }

    /// Mirrors a `getTransaction` `jsonParsed` result: memo instructions plus
    /// pre/post token balances, the shape [`verify_payout_transaction`] reads.
    fn payout_tx(memos: &[&str], transfers: &[(&str, u64, u64)]) -> Value {
        let instructions: Vec<Value> = memos
            .iter()
            .map(|m| serde_json::json!({ "program": "spl-memo", "parsed": m }))
            .collect();
        let rows = |pick_post: bool| -> Vec<Value> {
            transfers
                .iter()
                .map(|(owner, pre, post)| {
                    serde_json::json!({
                        "owner": owner,
                        "mint": MINT,
                        "uiTokenAmount": { "amount": (if pick_post { post } else { pre }).to_string() },
                    })
                })
                .collect()
        };
        serde_json::json!({
            "meta": { "err": null, "preTokenBalances": rows(false), "postTokenBalances": rows(true) },
            "transaction": { "message": { "instructions": instructions } },
        })
    }

    #[test]
    fn a_settled_proof_verifies_offline_and_against_its_payout() {
        let operator = LocalIdentity::generate("operator@local");
        let proof = proof(signed_receipt(&operator, 60_000), 16_496, 165);
        proof.verify_offline().expect("offline");

        let tx = payout_tx(
            &["gm", &proof.payout_memo],
            &[(PAYER, 1_000_000, 983_504), (PAYOUT_WALLET, 0, 16_331)],
        );
        let paid = proof.verify(&tx).expect("verify");
        assert_eq!(paid.amount_micro_usdc, proof.net_micro_usdc);
        assert_eq!(paid.recipient_owner_b58, PAYOUT_WALLET);
        assert_eq!(paid.mint_b58, MINT);
    }

    #[test]
    fn a_whole_window_lease_and_a_one_shot_job_both_verify() {
        let operator = LocalIdentity::generate("operator@local");
        // A one-shot job: gross is the whole signed price.
        let job = proof(signed_receipt(&operator, 1_000), 1_000, 10);
        job.verify_offline().expect("job");
        // A lease that ran short: gross is the metered draw, under the ceiling.
        let lease = proof(signed_receipt(&operator, 60_000), 16_496, 0);
        lease.verify_offline().expect("lease");
    }

    #[test]
    fn verify_offline_rejects_a_tampered_receipt() {
        let operator = LocalIdentity::generate("operator@local");
        let mut proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        proof.receipt.receipt_json = proof.receipt.receipt_json.replace("1000", "1");
        assert!(proof.verify_offline().is_err());
    }

    #[test]
    fn verify_offline_rejects_a_release_split_that_does_not_sum() {
        let operator = LocalIdentity::generate("operator@local");
        let mut proof = proof(signed_receipt(&operator, 1_000), 1_000, 10);
        proof.net_micro_usdc = 995;
        let err = proof.verify_offline().unwrap_err().to_string();
        assert!(err.contains("does not sum"), "got: {err}");
    }

    #[test]
    fn verify_offline_rejects_a_gross_above_the_signed_price() {
        let operator = LocalIdentity::generate("operator@local");
        let mut proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        proof.gross_micro_usdc = 1_500;
        proof.net_micro_usdc = 1_500;
        let err = proof.verify_offline().unwrap_err().to_string();
        assert!(err.contains("exceeds the signed price"), "got: {err}");
    }

    #[test]
    fn verify_offline_rejects_a_memo_for_other_work() {
        let operator = LocalIdentity::generate("operator@local");
        let mut proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        proof.payout_memo = payout_memo_for(Uuid::new_v4(), "some-other-sig");
        assert!(proof.verify_offline().is_err());
    }

    #[test]
    fn verify_offline_rejects_a_job_id_that_disagrees_with_the_receipt() {
        // The top-level job_id is what maps a settlement into a batch's leaf
        // order; the receipt's job_id is what the operator actually signed. If
        // the two could diverge unchecked, a batch could list a settlement
        // under a job whose work was never signed for.
        let operator = LocalIdentity::generate("operator@local");
        let mut proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        proof.job_id = Uuid::new_v4();
        let err = proof.verify_offline().unwrap_err().to_string();
        assert!(err.contains("does not match the receipt"), "got: {err}");
    }

    #[test]
    fn verify_rejects_a_payout_of_the_wrong_amount() {
        let operator = LocalIdentity::generate("operator@local");
        let proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        let tx = payout_tx(&[&proof.payout_memo], &[(PAYOUT_WALLET, 0, 999)]);
        let err = proof.verify(&tx).unwrap_err().to_string();
        assert!(err.contains("not the cited net"), "got: {err}");
    }

    #[test]
    fn verify_rejects_a_payout_to_the_wrong_wallet() {
        let operator = LocalIdentity::generate("operator@local");
        let proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        let tx = payout_tx(&[&proof.payout_memo], &[(PAYER, 0, 1_000)]);
        let err = proof.verify(&tx).unwrap_err().to_string();
        assert!(err.contains("not the cited operator wallet"), "got: {err}");
    }

    #[test]
    fn verify_rejects_a_payout_in_the_wrong_mint() {
        let operator = LocalIdentity::generate("operator@local");
        let proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        let mut tx = payout_tx(&[&proof.payout_memo], &[(PAYOUT_WALLET, 0, 1_000)]);
        tx["meta"]["postTokenBalances"][0]["mint"] =
            Value::String("So11111111111111111111111111111111111111112".into());
        tx["meta"]["preTokenBalances"][0]["mint"] =
            Value::String("So11111111111111111111111111111111111111112".into());
        let err = proof.verify(&tx).unwrap_err().to_string();
        assert!(err.contains("not the cited"), "got: {err}");
    }

    #[test]
    fn the_rpc_request_fetches_the_cited_signature() {
        let operator = LocalIdentity::generate("operator@local");
        let proof = proof(signed_receipt(&operator, 1_000), 1_000, 0);
        let req = proof.payout_rpc_request();
        assert_eq!(req["params"][0], proof.tx_signature);
    }

    fn batch_of(n: usize) -> Vec<SettlementProof> {
        let operator = LocalIdentity::generate("operator@local");
        (0..n)
            .map(|i| {
                proof(
                    signed_receipt(&operator, 1_000 + i as u64),
                    1_000 + i as u64,
                    10,
                )
            })
            .collect()
    }

    #[test]
    fn a_batch_commits_to_its_jobs_in_order() {
        let proofs = batch_of(5);
        let batch = SettlementBatch::commit(&proofs);
        assert_eq!(batch.tree_size, 5);
        assert_eq!(batch.root_hex.len(), 64);
        assert_eq!(
            batch.job_ids,
            proofs.iter().map(|p| p.job_id).collect::<Vec<_>>()
        );
        // The root is a pure function of the proofs, order included.
        assert_eq!(batch.root_hex, SettlementBatch::commit(&proofs).root_hex);
        let mut reordered = proofs.clone();
        reordered.swap(0, 4);
        assert_ne!(batch.root_hex, SettlementBatch::commit(&reordered).root_hex);
    }

    #[test]
    fn every_settlement_proves_inclusion_under_the_batch_root() {
        for n in 1..=9 {
            let proofs = batch_of(n);
            let batch = SettlementBatch::commit(&proofs);
            for index in 0..n {
                let inclusion = BatchInclusionProof::build(&proofs, index).expect("in range");
                inclusion
                    .verify_offline(&batch.root_hex)
                    .unwrap_or_else(|e| panic!("n={n} index={index}: {e}"));
                assert_eq!(inclusion.proof.job_id, proofs[index].job_id);
            }
            assert!(BatchInclusionProof::build(&proofs, n).is_none());
        }
    }

    #[test]
    fn inclusion_under_the_wrong_root_is_rejected() {
        let proofs = batch_of(4);
        let other = batch_of(4);
        let wrong_root = SettlementBatch::commit(&other).root_hex;
        let inclusion = BatchInclusionProof::build(&proofs, 2).unwrap();
        let err = inclusion
            .verify_offline(&wrong_root)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not sit under the batch root"),
            "got: {err}"
        );
    }

    #[test]
    fn a_swapped_settlement_body_fails_inclusion() {
        let proofs = batch_of(6);
        let batch = SettlementBatch::commit(&proofs);
        let mut inclusion = BatchInclusionProof::build(&proofs, 3).unwrap();
        // Keep the audit path and position, substitute a different settlement:
        // its leaf no longer reconstructs the committed root.
        inclusion.proof = proofs[4].clone();
        assert!(inclusion.verify_offline(&batch.root_hex).is_err());
    }

    #[test]
    fn a_tampered_receipt_fails_inclusion_before_the_path_is_walked() {
        let proofs = batch_of(3);
        let batch = SettlementBatch::commit(&proofs);
        let mut inclusion = BatchInclusionProof::build(&proofs, 1).unwrap();
        inclusion.proof.receipt.receipt_json =
            inclusion.proof.receipt.receipt_json.replace("1001", "1");
        assert!(inclusion.verify_offline(&batch.root_hex).is_err());
    }

    #[test]
    fn a_bent_audit_path_node_fails_inclusion() {
        let proofs = batch_of(7);
        let batch = SettlementBatch::commit(&proofs);
        let mut inclusion = BatchInclusionProof::build(&proofs, 5).unwrap();
        inclusion
            .verify_offline(&batch.root_hex)
            .expect("clean path verifies");
        // Corrupt one hex nibble of the first sibling.
        let first = &mut inclusion.audit_path_hex[0];
        let flipped = if first.starts_with('0') { '1' } else { '0' };
        first.replace_range(0..1, &flipped.to_string());
        assert!(inclusion.verify_offline(&batch.root_hex).is_err());
    }

    #[test]
    fn a_non_hex_audit_path_node_is_rejected() {
        let proofs = batch_of(4);
        let batch = SettlementBatch::commit(&proofs);
        let mut inclusion = BatchInclusionProof::build(&proofs, 1).unwrap();
        inclusion.audit_path_hex[0] = "zz".repeat(32);
        let err = inclusion
            .verify_offline(&batch.root_hex)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a 32-byte hex hash"), "got: {err}");
    }

    #[test]
    fn a_single_settlement_batch_has_the_proof_as_its_own_root() {
        let proofs = batch_of(1);
        let batch = SettlementBatch::commit(&proofs);
        assert_eq!(batch.root_hex, to_hex(&proofs[0].batch_leaf()));
        let inclusion = BatchInclusionProof::build(&proofs, 0).unwrap();
        assert!(inclusion.audit_path_hex.is_empty());
        inclusion
            .verify_offline(&batch.root_hex)
            .expect("solo inclusion");
    }
}
