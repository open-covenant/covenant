//! The inbound payment rail seam: how a buyer's claimed deposit gets
//! verified before it credits a balance. Mirrors [`crate::payout`]'s
//! posture on the outbound side — the coordinator stays solana-free:
//! [`SolanaRpcRail`] verifies over raw JSON-RPC, and the hermetic
//! default is [`MockRail`].
//!
//! The rail, not the claimant, is authoritative for who a deposit
//! belongs to and how much it was: a `DepositClaim` arrives on an open
//! endpoint carrying nothing but a deposit id and an asserted buyer,
//! and the rail derives the real buyer + amount from the payment
//! itself (on an on-chain rail: the transaction's memo and transferred
//! amount). The HTTP layer cross-checks the claimed buyer against the
//! rail's answer and refuses mismatches, so claiming someone else's
//! transaction credits them, never the claimant.

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub use covenant_compute_protocol::{BOND_MEMO_PREFIX, DEPOSIT_MEMO_PREFIX};

/// Every inbound memo prefix this rail attributes a payment by. A single
/// transfer may name exactly one of these — [`SolanaRpcRail::key_from_memo`]
/// rejects a memo set spanning more than one so one on-chain deposit can
/// never be credited to both the buyer and the bond books.
const COVENANT_MEMO_PREFIXES: [&str; 2] = [DEPOSIT_MEMO_PREFIX, BOND_MEMO_PREFIX];

/// An unauthenticated assertion that `deposit_id` paid the coordinator
/// on behalf of `buyer_pubkey_b58`. Verified by an [`InboundRail`]
/// before anything is credited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositClaim {
    pub buyer_pubkey_b58: String,
    /// The rail's canonical payment identifier — a transaction
    /// signature on an on-chain rail. Doubles as the idempotency key.
    pub deposit_id: String,
}

/// What the rail actually observed for a deposit id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDeposit {
    pub deposit_id: String,
    pub buyer_pubkey_b58: String,
    pub amount_micro_usdc: u64,
}

/// The stake-side [`DepositClaim`]: an unauthenticated assertion that
/// `bond_id` posted stake for `operator_pubkey_b58`. Same trust shape —
/// the rail, not the claimant, answers whose stake it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BondClaim {
    pub operator_pubkey_b58: String,
    pub bond_id: String,
}

/// What the rail actually observed for a bond id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBond {
    pub bond_id: String,
    pub operator_pubkey_b58: String,
    pub amount_micro_usdc: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RailError {
    /// No confirmed payment exists under this deposit id.
    #[error("deposit not found on the rail: {0}")]
    NotFound(String),
    /// The signature is on-chain but not yet finalized, so the rail
    /// can't read the transfer yet. Distinct from [`NotFound`] so a
    /// just-sent deposit reads as "retry shortly", not "does not exist".
    #[error("payment not yet finalized: {0}")]
    Pending(String),
    /// A payment exists but is not a creditable deposit (wrong
    /// recipient, wrong asset, unparsable memo, failed transaction).
    #[error("deposit rejected: {0}")]
    Rejected(String),
    #[error("rail backend: {0}")]
    Backend(String),
}

#[async_trait]
pub trait InboundRail: Send + Sync {
    async fn verify_deposit(&self, claim: &DepositClaim) -> Result<VerifiedDeposit, RailError>;

    /// The stake-side verify: same rail, same account, the
    /// [`BOND_MEMO_PREFIX`] memo instead of the deposit one.
    /// Deliberately not defaulted — a rail that silently rejected bonds
    /// would read as "no one ever bonds", not as a wiring gap.
    async fn verify_bond(&self, claim: &BondClaim) -> Result<VerifiedBond, RailError>;

    /// One line for the boot log, so a deployment states which rail —
    /// if any — its balances are backed by.
    fn describe(&self) -> String;

    /// Machine-readable instructions for making a creditable payment
    /// on this rail — what `/federation/deposit-info` hands buyer
    /// tooling so "fund once and go" needs no out-of-band homework.
    fn deposit_info(&self) -> Value {
        serde_json::json!({ "rail": self.describe() })
    }
}

/// Hermetic rail: verifies exactly the deposits and bonds preloaded
/// into it. The test/dev stand-in for a real on-chain rail.
#[derive(Default)]
pub struct MockRail {
    deposits: Mutex<HashMap<String, VerifiedDeposit>>,
    bonds: Mutex<HashMap<String, VerifiedBond>>,
}

impl MockRail {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `deposit` verifiable, as if its payment had confirmed on
    /// the rail.
    pub fn preload(&self, deposit: VerifiedDeposit) {
        self.deposits
            .lock()
            .insert(deposit.deposit_id.clone(), deposit);
    }

    /// Makes `bond` verifiable, as if its post had confirmed on the
    /// rail.
    pub fn preload_bond(&self, bond: VerifiedBond) {
        self.bonds.lock().insert(bond.bond_id.clone(), bond);
    }
}

#[async_trait]
impl InboundRail for MockRail {
    async fn verify_deposit(&self, claim: &DepositClaim) -> Result<VerifiedDeposit, RailError> {
        self.deposits
            .lock()
            .get(&claim.deposit_id)
            .cloned()
            .ok_or_else(|| RailError::NotFound(claim.deposit_id.clone()))
    }

    async fn verify_bond(&self, claim: &BondClaim) -> Result<VerifiedBond, RailError> {
        self.bonds
            .lock()
            .get(&claim.bond_id)
            .cloned()
            .ok_or_else(|| RailError::NotFound(claim.bond_id.clone()))
    }

    fn describe(&self) -> String {
        "mock rail (preloaded deposits only)".into()
    }
}

/// Real on-chain rail: verifies a claimed deposit by reading the
/// transaction straight off a Solana RPC over raw JSON-RPC — no
/// solana-sdk, no covenant-x402, no keys; this is a read-only
/// verifier, so unlike the payout push there is nothing to isolate in
/// a sidecar. A creditable deposit is a confirmed, successful
/// transaction that (a) increased the SPL balance of `deposit_owner`'s
/// account for `mint`, and (b) carries a
/// [`DEPOSIT_MEMO_PREFIX`]-tagged memo naming the buyer it funds. The
/// rail answers from the chain alone — the claimant only ever supplies
/// the transaction signature.
pub struct SolanaRpcRail {
    http: reqwest::Client,
    rpc_url: String,
    deposit_owner_b58: String,
    mint_b58: String,
}

impl SolanaRpcRail {
    pub fn new(rpc_url: String, deposit_owner_b58: String, mint_b58: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            rpc_url,
            deposit_owner_b58,
            mint_b58,
        }
    }

    /// One JSON-RPC round-trip, returning the `result` field (or
    /// `Null`). Transport, non-2xx, decode, and an `error` envelope all
    /// map to [`RailError::Backend`] — the shared body for every method
    /// this rail calls.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, RailError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RailError::Backend(format!("rpc transport: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RailError::Backend(format!("rpc returned {status}")));
        }
        let envelope: Value = resp
            .json()
            .await
            .map_err(|e| RailError::Backend(format!("rpc response decode: {e}")))?;
        if let Some(err) = envelope.get("error") {
            return Err(RailError::Backend(format!("rpc error: {err}")));
        }
        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn get_transaction(&self, signature: &str) -> Result<Value, RailError> {
        self.rpc(
            "getTransaction",
            serde_json::json!([signature, {
                "encoding": "jsonParsed",
                "commitment": "finalized",
                "maxSupportedTransactionVersion": 0,
            }]),
        )
        .await
    }

    /// The confirmation level the cluster reports for `signature` from
    /// its recent-status cache (`getSignatureStatuses`), or `None` when
    /// the cluster has never seen it. Used only to tell a confirmed-but-
    /// not-yet-finalized deposit apart from a signature that does not
    /// exist — `getTransaction` reads finalized transactions only, so a
    /// deposit sent seconds ago reads as null there for both cases.
    async fn signature_status(&self, signature: &str) -> Result<Option<String>, RailError> {
        let result = self
            .rpc(
                "getSignatureStatuses",
                serde_json::json!([[signature], { "searchTransactionHistory": false }]),
            )
            .await?;
        Ok(result
            .pointer("/value/0/confirmationStatus")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// The identity named by the transaction's own memo under `prefix`.
    /// Exactly one such memo must be present and its pubkey must decode
    /// to a 32-byte key — a payment that doesn't say who it funds is
    /// rejected, never guessed at. One parser for both memo shapes, so
    /// deposits and bond posts can never drift on attribution rules.
    ///
    /// A payment may name exactly one covenant purpose. Deposits and
    /// bonds land at the same treasury owner and keep separate
    /// idempotency sets, so a transaction carrying both a deposit and a
    /// bond memo would otherwise verify as each — crediting one on-chain
    /// transfer to two books. A memo set tagging more than one book is
    /// ambiguous and rejected outright.
    fn key_from_memo(tx: &Value, prefix: &str) -> Result<String, RailError> {
        let instructions = tx
            .pointer("/transaction/message/instructions")
            .and_then(Value::as_array)
            .ok_or_else(|| RailError::Rejected("transaction has no instruction list".into()))?;
        let parsed_memos: Vec<&str> = instructions
            .iter()
            .filter(|ix| ix.get("program").and_then(Value::as_str) == Some("spl-memo"))
            .filter_map(|ix| ix.get("parsed").and_then(Value::as_str))
            .collect();
        if parsed_memos.iter().any(|memo| {
            COVENANT_MEMO_PREFIXES
                .iter()
                .any(|&p| p != prefix && memo.starts_with(p))
        }) {
            return Err(RailError::Rejected(
                "transaction also carries a memo for another covenant book; a payment must \
                 name exactly one purpose"
                    .into(),
            ));
        }
        let memos: Vec<&str> = parsed_memos
            .iter()
            .filter_map(|memo| memo.strip_prefix(prefix))
            .collect();
        let [key] = memos.as_slice() else {
            return Err(RailError::Rejected(format!(
                "expected exactly one {prefix} memo, found {}",
                memos.len()
            )));
        };
        let decoded = bs58::decode(key)
            .into_vec()
            .map_err(|e| RailError::Rejected(format!("memo pubkey is not base58: {e}")))?;
        if decoded.len() != 32 {
            return Err(RailError::Rejected(format!(
                "memo pubkey decodes to {} bytes, not a 32-byte key",
                decoded.len()
            )));
        }
        Ok(key.to_string())
    }

    /// Shared verify body: the transaction must exist, have succeeded,
    /// carry exactly one `prefix` memo naming a real key, and have
    /// grown the coordinator's account. Returns (named key, amount).
    async fn verify_inbound(&self, id: &str, prefix: &str) -> Result<(String, u64), RailError> {
        let tx = self.get_transaction(id).await?;
        if tx.is_null() {
            // getTransaction returns finalized transactions only, so a
            // deposit sent seconds ago reads as null. Ask whether the
            // cluster knows the signature at all: if it does, it is on
            // the way ("retry shortly"); if not, it truly doesn't exist.
            return match self.signature_status(id).await? {
                Some(level) => Err(RailError::Pending(format!(
                    "{id} is on-chain ({level}) but not finalized yet — it credits once \
                     finalized (a few seconds on Solana); retry shortly"
                ))),
                None => Err(RailError::NotFound(id.to_string())),
            };
        }
        let err = tx.pointer("/meta/err");
        if !matches!(err, None | Some(Value::Null)) {
            return Err(RailError::Rejected(
                "transaction failed on-chain".to_string(),
            ));
        }
        let key = Self::key_from_memo(&tx, prefix)?;
        let amount_micro_usdc = self.amount_credited(&tx)?;
        Ok((key, amount_micro_usdc))
    }

    /// How much the deposit account's balance actually grew: the
    /// post-minus-pre token-balance diff for accounts owned by the
    /// deposit owner in the configured mint. Balance diffs are robust
    /// to how the transfer was written (transfer, transferChecked,
    /// multiple instructions); the instruction list is not.
    fn amount_credited(&self, tx: &Value) -> Result<u64, RailError> {
        let balance_of = |key: &str| -> u64 {
            tx.pointer(&format!("/meta/{key}"))
                .and_then(Value::as_array)
                .map(|balances| {
                    balances
                        .iter()
                        .filter(|b| {
                            b.get("owner").and_then(Value::as_str)
                                == Some(self.deposit_owner_b58.as_str())
                                && b.get("mint").and_then(Value::as_str)
                                    == Some(self.mint_b58.as_str())
                        })
                        .filter_map(|b| {
                            b.pointer("/uiTokenAmount/amount")
                                .and_then(Value::as_str)
                                .and_then(|a| a.parse::<u64>().ok())
                        })
                        .sum()
                })
                .unwrap_or(0)
        };
        let credited =
            balance_of("postTokenBalances").saturating_sub(balance_of("preTokenBalances"));
        if credited == 0 {
            return Err(RailError::Rejected(format!(
                "no balance increase for owner {} in mint {}",
                self.deposit_owner_b58, self.mint_b58
            )));
        }
        Ok(credited)
    }
}

#[async_trait]
impl InboundRail for SolanaRpcRail {
    async fn verify_deposit(&self, claim: &DepositClaim) -> Result<VerifiedDeposit, RailError> {
        let (buyer_pubkey_b58, amount_micro_usdc) = self
            .verify_inbound(&claim.deposit_id, DEPOSIT_MEMO_PREFIX)
            .await?;
        Ok(VerifiedDeposit {
            deposit_id: claim.deposit_id.clone(),
            buyer_pubkey_b58,
            amount_micro_usdc,
        })
    }

    async fn verify_bond(&self, claim: &BondClaim) -> Result<VerifiedBond, RailError> {
        let (operator_pubkey_b58, amount_micro_usdc) = self
            .verify_inbound(&claim.bond_id, BOND_MEMO_PREFIX)
            .await?;
        Ok(VerifiedBond {
            bond_id: claim.bond_id.clone(),
            operator_pubkey_b58,
            amount_micro_usdc,
        })
    }

    fn describe(&self) -> String {
        format!(
            "solana rpc rail ({} — deposits to {} in mint {})",
            self.rpc_url, self.deposit_owner_b58, self.mint_b58
        )
    }

    fn deposit_info(&self) -> Value {
        serde_json::json!({
            "rail": "solana-rpc",
            "deposit_owner_b58": self.deposit_owner_b58,
            "mint_b58": self.mint_b58,
            "memo_format": format!("{DEPOSIT_MEMO_PREFIX}<buyer_pubkey_b58>"),
            "how": "SPL-transfer the mint to the deposit owner's associated token account \
                    with the memo naming your buyer pubkey, then claim the transaction \
                    signature at POST /federation/buyers/deposit",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2";
    const MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    const BUYER: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";

    /// A `getTransaction` result in the jsonParsed shape the rail
    /// reads: memo instructions plus pre/post token balances for the
    /// deposit account.
    fn tx_fixture(err: Value, memos: &[&str], pre: u64, post: u64) -> Value {
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
        serde_json::json!({
            "blockTime": 1_751_000_000,
            "meta": {
                "err": err,
                "preTokenBalances": [{
                    "accountIndex": 1,
                    "mint": MINT,
                    "owner": OWNER,
                    "uiTokenAmount": { "amount": pre.to_string(), "decimals": 6 },
                }],
                "postTokenBalances": [{
                    "accountIndex": 1,
                    "mint": MINT,
                    "owner": OWNER,
                    "uiTokenAmount": { "amount": post.to_string(), "decimals": 6 },
                }],
            },
            "transaction": { "message": { "instructions": instructions } },
        })
    }

    /// Serves a fixed JSON-RPC envelope for every request — the rail
    /// only ever calls `getTransaction`.
    async fn spawn_rpc(result: Value) -> String {
        use axum::routing::post;
        let response = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result });
        let app = axum::Router::new().route(
            "/",
            post(move || {
                let response = response.clone();
                async move { axum::Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn rail(rpc_url: String) -> SolanaRpcRail {
        SolanaRpcRail::new(rpc_url, OWNER.into(), MINT.into())
    }

    fn claim(buyer: &str) -> DepositClaim {
        DepositClaim {
            buyer_pubkey_b58: buyer.into(),
            deposit_id: "5sig".into(),
        }
    }

    #[tokio::test]
    async fn a_confirmed_memo_tagged_transfer_verifies_with_the_chain_derived_amount() {
        let memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo], 0, 250_000)).await;
        let verified = rail(url).verify_deposit(&claim(BUYER)).await.unwrap();
        assert_eq!(verified.buyer_pubkey_b58, BUYER);
        assert_eq!(verified.amount_micro_usdc, 250_000);
        assert_eq!(verified.deposit_id, "5sig");
    }

    #[tokio::test]
    async fn the_rail_answers_from_the_chain_not_the_claim() {
        // The claimant asserts someone else's identity; the rail
        // returns the memo's buyer, and the HTTP layer's mismatch
        // check does the refusing.
        let memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo], 0, 250_000)).await;
        let verified = rail(url).verify_deposit(&claim(OWNER)).await.unwrap();
        assert_eq!(verified.buyer_pubkey_b58, BUYER);
    }

    #[tokio::test]
    async fn an_unknown_signature_is_not_found() {
        let url = spawn_rpc(Value::Null).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(err, RailError::NotFound(_)));
    }

    #[tokio::test]
    async fn a_bond_post_verifies_on_the_same_rail_with_the_bond_memo() {
        let memo = format!("{BOND_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo], 0, 500_000)).await;
        let verified = rail(url)
            .verify_bond(&BondClaim {
                operator_pubkey_b58: BUYER.into(),
                bond_id: "5sig".into(),
            })
            .await
            .unwrap();
        assert_eq!(verified.operator_pubkey_b58, BUYER);
        assert_eq!(verified.amount_micro_usdc, 500_000);
        assert_eq!(verified.bond_id, "5sig");
    }

    #[tokio::test]
    async fn deposit_and_bond_memos_never_verify_as_each_other() {
        // A deposit-tagged payment claimed as a bond carries no bond
        // memo, and vice versa — the transfer is rejected, never
        // credited to the wrong book.
        let deposit_memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&deposit_memo], 0, 250_000)).await;
        let err = rail(url)
            .verify_bond(&BondClaim {
                operator_pubkey_b58: BUYER.into(),
                bond_id: "5sig".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("exactly one")));

        let bond_memo = format!("{BOND_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&bond_memo], 0, 250_000)).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("exactly one")));
    }

    #[tokio::test]
    async fn a_transfer_tagged_for_both_books_verifies_as_neither() {
        // One transfer carrying a buyer memo AND a bond memo would
        // otherwise credit the same on-chain amount to both books — they
        // share a treasury owner and keep separate idempotency sets, so
        // one deposit could be spent twice. It must verify as neither.
        let deposit_memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let bond_memo = format!("{BOND_MEMO_PREFIX}{OWNER}");

        let url = spawn_rpc(tx_fixture(
            Value::Null,
            &[&deposit_memo, &bond_memo],
            0,
            500_000,
        ))
        .await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("another covenant book")));

        let url = spawn_rpc(tx_fixture(
            Value::Null,
            &[&deposit_memo, &bond_memo],
            0,
            500_000,
        ))
        .await;
        let err = rail(url)
            .verify_bond(&BondClaim {
                operator_pubkey_b58: OWNER.into(),
                bond_id: "5sig".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("another covenant book")));
    }

    #[tokio::test]
    async fn a_failed_transaction_is_rejected() {
        let memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(
            serde_json::json!({ "InstructionError": [0, "Custom"] }),
            &[&memo],
            0,
            250_000,
        ))
        .await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("failed on-chain")));
    }

    #[tokio::test]
    async fn a_transfer_without_the_memo_is_rejected_never_guessed() {
        let url = spawn_rpc(tx_fixture(Value::Null, &[], 0, 250_000)).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("exactly one")));
    }

    #[tokio::test]
    async fn two_buyer_memos_are_ambiguous_and_rejected() {
        let memo_a = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let memo_b = format!("{DEPOSIT_MEMO_PREFIX}{OWNER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo_a, &memo_b], 0, 250_000)).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("found 2")));
    }

    #[tokio::test]
    async fn a_memo_that_is_not_a_32_byte_key_is_rejected() {
        let memo = format!("{DEPOSIT_MEMO_PREFIX}abc");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo], 0, 250_000)).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("32-byte")));
    }

    #[tokio::test]
    async fn a_transfer_that_grew_nothing_for_the_deposit_account_is_rejected() {
        // Same signature, memo present, but the balances never moved —
        // e.g. the transfer went to a different mint or owner.
        let memo = format!("{DEPOSIT_MEMO_PREFIX}{BUYER}");
        let url = spawn_rpc(tx_fixture(Value::Null, &[&memo], 250_000, 250_000)).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::Rejected(m) if m.contains("no balance increase")));
    }

    #[tokio::test]
    async fn an_rpc_error_envelope_is_a_backend_error() {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/",
            post(|| async {
                axum::Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": { "code": -32005, "message": "node is behind" },
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let err = rail(format!("http://{addr}"))
            .verify_deposit(&claim(BUYER))
            .await
            .unwrap_err();
        assert!(matches!(&err, RailError::Backend(m) if m.contains("node is behind")));
    }

    /// An RPC stub that answers `getSignatureStatuses` and every other
    /// method (i.e. `getTransaction`) from separate canned results, so a
    /// test can model "not finalized yet": a null transaction alongside a
    /// signature the cluster already knows at a lower commitment.
    async fn spawn_rpc_split(get_transaction: Value, signature_status: Value) -> String {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/",
            post(move |axum::Json(req): axum::Json<Value>| {
                let get_transaction = get_transaction.clone();
                let signature_status = signature_status.clone();
                async move {
                    let result = if req.get("method").and_then(Value::as_str)
                        == Some("getSignatureStatuses")
                    {
                        signature_status
                    } else {
                        get_transaction
                    };
                    axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_confirmed_but_unfinalized_deposit_reads_as_pending_not_missing() {
        // getTransaction (finalized-only) has nothing yet, but the
        // cluster knows the signature at a lower commitment — the deposit
        // is on the way, so the buyer must be told to retry, not that it
        // is missing.
        let url = spawn_rpc_split(
            Value::Null,
            serde_json::json!({ "value": [{ "confirmationStatus": "confirmed" }] }),
        )
        .await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(
            matches!(&err, RailError::Pending(m) if m.contains("confirmed") && m.contains("retry")),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn an_unfinalized_bond_post_reads_as_pending_on_the_same_path() {
        let url = spawn_rpc_split(
            Value::Null,
            serde_json::json!({ "value": [{ "confirmationStatus": "processed" }] }),
        )
        .await;
        let err = rail(url)
            .verify_bond(&BondClaim {
                operator_pubkey_b58: BUYER.into(),
                bond_id: "5sig".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(&err, RailError::Pending(_)), "got: {err}");
    }

    #[tokio::test]
    async fn an_unfinalized_and_unknown_signature_still_reads_as_not_found() {
        // The cluster returns an empty status array (never saw it): a
        // genuine typo or a signature from the wrong network is not a
        // pending deposit and must not tell the buyer to keep retrying.
        let url = spawn_rpc_split(Value::Null, serde_json::json!({ "value": [null] })).await;
        let err = rail(url).verify_deposit(&claim(BUYER)).await.unwrap_err();
        assert!(matches!(&err, RailError::NotFound(_)), "got: {err}");
    }
}
