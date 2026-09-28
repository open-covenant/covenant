//! The chain-record half of the verifiable payout loop: pure checks
//! against a `getTransaction` result in `jsonParsed` encoding, the
//! shape any Solana RPC serves. Lives beside the memo definition it
//! enforces — one crate owns what the payout writer stamps and what
//! every verifier (buyer, operator, third party) reads back. No RPC,
//! no solana dependency, no trust in the coordinator.

use serde_json::Value;
use uuid::Uuid;

use crate::receipt::{parse_payout_memo, PAYOUT_MEMO_PREFIX};
use crate::sign::ProtocolError;

/// The memo a payout for this job under this receipt signature must
/// carry — [`crate::SignedWorkReceipt::payout_memo`] for callers who
/// journaled the signature without keeping the whole receipt (an
/// operator's earnings ledger).
pub fn payout_memo_for(job_id: Uuid, receipt_signature_b58: &str) -> String {
    format!("{PAYOUT_MEMO_PREFIX}{job_id}:{receipt_signature_b58}")
}

/// The exact `getTransaction` request whose result
/// [`verify_payout_transaction`] reads: `jsonParsed` encoding,
/// `confirmed` commitment. One definition so no fetcher drifts from
/// the shape the verifier parses.
pub fn payout_transaction_rpc_request(signature: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [signature, {
            "encoding": "jsonParsed",
            "commitment": "confirmed",
            "maxSupportedTransactionVersion": 0,
        }],
    })
}

/// What an on-chain payout transaction actually did, read straight off
/// the chain's own record — the last link of the verifiable loop:
/// envelope → receipt → memo → this transfer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PayoutProof {
    /// Token base units delivered (micro-USDC for a 6-decimal mint).
    pub amount_micro_usdc: u64,
    pub mint_b58: String,
    /// The wallet whose balance grew — the payee's owner address.
    pub recipient_owner_b58: String,
}

/// Checks that a `getTransaction` result is the payout `expected_memo`
/// names: the transaction succeeded, carries exactly one compute
/// payout memo, that memo is the expected one, and somebody's token
/// balance actually grew.
///
/// The memo can't be forged for different work (it embeds the
/// operator's signature over the whole receipt), so a match proves the
/// chain paid for this job; the returned [`PayoutProof`] says how
/// much, in what mint, to whom, all read from the transaction itself.
pub fn verify_payout_transaction(
    expected_memo: &str,
    tx: &Value,
) -> Result<PayoutProof, ProtocolError> {
    let Some((expected_job, _)) = parse_payout_memo(expected_memo) else {
        return Err(ProtocolError::Invalid(
            "expected memo is not a well-formed compute payout memo".into(),
        ));
    };
    if tx.is_null() {
        return Err(ProtocolError::Invalid(
            "transaction not found on chain".into(),
        ));
    }
    let err = tx.pointer("/meta/err");
    if !matches!(err, None | Some(Value::Null)) {
        return Err(ProtocolError::Invalid(
            "payout transaction failed on-chain".into(),
        ));
    }

    let instructions = tx
        .pointer("/transaction/message/instructions")
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::Invalid("transaction has no instruction list".into()))?;
    let compute_memos: Vec<&str> = instructions
        .iter()
        .filter(|ix| ix.get("program").and_then(Value::as_str) == Some("spl-memo"))
        .filter_map(|ix| ix.get("parsed").and_then(Value::as_str))
        .filter(|memo| memo.starts_with(PAYOUT_MEMO_PREFIX))
        .collect();
    let [memo] = compute_memos.as_slice() else {
        return Err(ProtocolError::Invalid(format!(
            "expected exactly one {PAYOUT_MEMO_PREFIX} memo, found {}",
            compute_memos.len()
        )));
    };
    if *memo != expected_memo {
        let named = parse_payout_memo(memo)
            .map(|(job_id, _)| format!("job {job_id}"))
            .unwrap_or_else(|| "a malformed memo".into());
        return Err(ProtocolError::Invalid(format!(
            "transaction memo is for {named}, not job {expected_job} under the expected \
             receipt signature"
        )));
    }

    // Post-minus-pre token-balance growth per (owner, mint) — robust
    // to how the transfer was written. Exactly one wallet must have
    // been paid.
    let balances = |key: &str| -> Vec<(String, String, u64)> {
        tx.pointer(&format!("/meta/{key}"))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|b| {
                        let owner = b.get("owner").and_then(Value::as_str)?;
                        let mint = b.get("mint").and_then(Value::as_str)?;
                        let amount = b
                            .pointer("/uiTokenAmount/amount")
                            .and_then(Value::as_str)
                            .and_then(|a| a.parse::<u64>().ok())?;
                        Some((owner.to_string(), mint.to_string(), amount))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let pre = balances("preTokenBalances");
    let grew: Vec<(String, String, u64)> = balances("postTokenBalances")
        .into_iter()
        .filter_map(|(owner, mint, post)| {
            let before: u64 = pre
                .iter()
                .filter(|(o, m, _)| *o == owner && *m == mint)
                .map(|(_, _, amount)| *amount)
                .sum();
            let credited = post.saturating_sub(before);
            (credited > 0).then_some((owner, mint, credited))
        })
        .collect();
    match grew.as_slice() {
        [] => Err(ProtocolError::Invalid(
            "no token balance grew — the transaction moved nothing".into(),
        )),
        [(owner, mint, credited)] => Ok(PayoutProof {
            amount_micro_usdc: *credited,
            mint_b58: mint.clone(),
            recipient_owner_b58: owner.clone(),
        }),
        many => Err(ProtocolError::Invalid(format!(
            "ambiguous payout: {} wallets gained tokens in this transaction",
            many.len()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPIENT: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";
    const PAYER: &str = "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4T";
    const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

    fn tx(memos: &[&str], transfers: &[(&str, u64, u64)]) -> Value {
        let instructions: Vec<Value> = memos
            .iter()
            .map(|m| {
                serde_json::json!({
                    "program": "spl-memo",
                    "programId": "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
                    "parsed": m,
                })
            })
            .collect();
        let rows = |index: usize| -> Vec<Value> {
            transfers
                .iter()
                .map(|(owner, pre, post)| {
                    let amount = if index == 0 { pre } else { post };
                    serde_json::json!({
                        "owner": owner,
                        "mint": MINT,
                        "uiTokenAmount": { "amount": amount.to_string() },
                    })
                })
                .collect()
        };
        serde_json::json!({
            "meta": {
                "err": null,
                "preTokenBalances": rows(0),
                "postTokenBalances": rows(1),
            },
            "transaction": { "message": { "instructions": instructions } },
        })
    }

    #[test]
    fn accepts_the_transfer_the_memo_names_and_reads_the_proof_off_it() {
        let job = Uuid::new_v4();
        let memo = payout_memo_for(job, "sig111");
        let tx = tx(
            &["gm", &memo],
            &[(PAYER, 5_000, 4_000), (RECIPIENT, 0, 1_000)],
        );
        let proof = verify_payout_transaction(&memo, &tx).expect("verify");
        assert_eq!(
            proof,
            PayoutProof {
                amount_micro_usdc: 1_000,
                mint_b58: MINT.into(),
                recipient_owner_b58: RECIPIENT.into(),
            },
            "foreign memos are ignored; the payer's own decrease is not a payout"
        );
    }

    #[test]
    fn credits_only_the_increment_when_the_operator_already_holds_the_mint() {
        // An operator paid before carries a standing balance in the mint, so
        // the payout is the post-minus-pre delta, never the whole post total.
        let memo = payout_memo_for(Uuid::new_v4(), "sig-repeat");
        let tx = tx(
            &[&memo],
            &[
                (PAYER, 5_000_000, 4_999_000),
                (RECIPIENT, 1_000_000, 1_001_000),
            ],
        );
        let proof = verify_payout_transaction(&memo, &tx).expect("verify");
        assert_eq!(proof.amount_micro_usdc, 1_000);
        assert_eq!(proof.recipient_owner_b58, RECIPIENT);
    }

    #[test]
    fn rejects_a_memo_for_different_work_naming_whose_it_is() {
        let mine = payout_memo_for(Uuid::new_v4(), "sig-mine");
        let other_job = Uuid::new_v4();
        let tx = tx(
            &[&payout_memo_for(other_job, "sig-other")],
            &[(RECIPIENT, 0, 1_000)],
        );
        let err = verify_payout_transaction(&mine, &tx).unwrap_err();
        assert!(
            err.to_string().contains(&format!("job {other_job}")),
            "got: {err}"
        );
    }

    #[test]
    fn requires_exactly_one_compute_memo_and_a_well_formed_expectation() {
        let memo = payout_memo_for(Uuid::new_v4(), "sig");
        let none = tx(&["gm"], &[(RECIPIENT, 0, 1_000)]);
        assert!(verify_payout_transaction(&memo, &none)
            .unwrap_err()
            .to_string()
            .contains("found 0"));
        let two = tx(&[&memo, &memo], &[(RECIPIENT, 0, 1_000)]);
        assert!(verify_payout_transaction(&memo, &two)
            .unwrap_err()
            .to_string()
            .contains("found 2"));
        let malformed = verify_payout_transaction("not-a-memo", &none).unwrap_err();
        assert!(malformed.to_string().contains("well-formed"));
    }

    #[test]
    fn rejects_failure_absence_no_movement_and_ambiguity() {
        let memo = payout_memo_for(Uuid::new_v4(), "sig");

        let mut failed = tx(&[&memo], &[(RECIPIENT, 0, 1_000)]);
        failed["meta"]["err"] = serde_json::json!({"InstructionError": [1, "Custom"]});
        assert!(verify_payout_transaction(&memo, &failed)
            .unwrap_err()
            .to_string()
            .contains("failed on-chain"));

        assert!(verify_payout_transaction(&memo, &Value::Null)
            .unwrap_err()
            .to_string()
            .contains("not found"));

        let still = tx(&[&memo], &[(RECIPIENT, 500, 500)]);
        assert!(verify_payout_transaction(&memo, &still)
            .unwrap_err()
            .to_string()
            .contains("moved nothing"));

        let split = tx(&[&memo], &[(RECIPIENT, 0, 600), (PAYER, 0, 400)]);
        assert!(verify_payout_transaction(&memo, &split)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
    }

    #[test]
    fn rpc_request_pins_the_encoding_the_verifier_parses() {
        let req = payout_transaction_rpc_request("siggy");
        assert_eq!(req["method"], "getTransaction");
        assert_eq!(req["params"][0], "siggy");
        assert_eq!(req["params"][1]["encoding"], "jsonParsed");
        assert_eq!(req["params"][1]["commitment"], "confirmed");
    }
}
