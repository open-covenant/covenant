//! In-process EIP-1559 transaction submitter for EVM chains.
//!
//! [`crate::evm::EvmSigner`] never builds a transaction — it signs an
//! EIP-3009 authorization and lets a facilitator submit. That gasless path
//! needs no RPC and no gas. This module is the other half: when Covenant is
//! itself the facilitator (it funds `chargeCall` and settles
//! `releaseCallAttested`/`refundCallAttested` on a spend-grant escrow), the
//! daemon must submit its own transactions. It cannot lean on a human running
//! `cast`; "hand a funded agent a wallet and walk away" means the daemon
//! signs and broadcasts in-process.
//!
//! The build/sign core (RLP + type-0x2 envelope + recoverable secp256k1) is
//! always compiled and unit-tested against a `cast`-produced golden vector.
//! The JSON-RPC transport ([`EvmRpc`], which polls for a receipt and so needs
//! an async timer) is gated behind the `evm-rpc` feature so the default tree
//! stays RPC-free.
//!
//! Reuses [`crate::evm`]'s `keccak256`, `address_from_key`, and hex helpers so
//! there is one keccak and one address derivation in the crate, not two.

use k256::ecdsa::SigningKey;

use crate::evm::{address_from_key, hex_0x, keccak256};

/// A contract call or value transfer to submit as an EIP-1559 transaction.
#[derive(Debug, Clone)]
pub struct TxRequest {
    /// Callee — a contract for a call, a recipient for a plain transfer.
    pub to: [u8; 20],
    /// Native value in wei (0 for a pure contract call).
    pub value: u128,
    /// ABI calldata (empty for a plain transfer).
    pub data: Vec<u8>,
    /// Gas limit, or `None` to estimate (which also preflights the revert).
    pub gas_limit: Option<u64>,
}

impl TxRequest {
    /// A contract call with no native value.
    pub fn call(to: [u8; 20], data: Vec<u8>) -> Self {
        Self {
            to,
            value: 0,
            data,
            gas_limit: None,
        }
    }
}

/// The `maxPriorityFeePerGas` / `maxFeePerGas` pair for one transaction.
#[derive(Debug, Clone, Copy)]
pub struct Fees {
    pub max_priority: u128,
    pub max_fee: u128,
}

/// Signs EIP-1559 transactions with a 32-byte secp256k1 secret. This is the
/// escrow's funder/submitter key — the same custody model as
/// [`crate::evm::EvmSigner`], a different signing target (a whole tx, not an
/// EIP-712 struct).
pub struct EvmTxSigner {
    signing_key: SigningKey,
    address: [u8; 20],
}

impl EvmTxSigner {
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Result<Self, EvmTxError> {
        let signing_key = SigningKey::from_slice(secret)
            .map_err(|e| EvmTxError::Key(format!("invalid secp256k1 secret: {e}")))?;
        let address = address_from_key(&signing_key);
        Ok(Self {
            signing_key,
            address,
        })
    }

    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    pub fn address_hex(&self) -> String {
        hex_0x(&self.address)
    }

    /// Sign a 32-byte prehash, returning `(yParity, r, s)`. `yParity ∈ {0,1}`
    /// (typed-tx form, not the `v ∈ {27,28}` of legacy/EIP-712), `s` low-S
    /// normalized — both required by EIP-1559.
    fn sign_hash(&self, hash: &[u8; 32]) -> (u8, [u8; 32], [u8; 32]) {
        let (sig, recid) = self
            .signing_key
            .sign_prehash_recoverable(hash)
            .expect("RFC-6979 ECDSA over a fixed 32-byte prehash is infallible");
        let bytes = sig.to_bytes();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&bytes[..32]);
        s.copy_from_slice(&bytes[32..]);
        (recid.to_byte(), r, s)
    }

    /// Build and sign an EIP-1559 (type 0x2) transaction, returning the raw
    /// broadcast bytes and its transaction hash.
    pub fn sign_tx(
        &self,
        chain_id: u64,
        nonce: u64,
        fees: Fees,
        gas_limit: u64,
        tx: &TxRequest,
    ) -> (Vec<u8>, [u8; 32]) {
        let unsigned = encode_1559(chain_id, nonce, fees, gas_limit, tx, None);
        let sighash = keccak256(&unsigned);
        let sig = self.sign_hash(&sighash);
        let raw = encode_1559(chain_id, nonce, fees, gas_limit, tx, Some(sig));
        let tx_hash = keccak256(&raw);
        (raw, tx_hash)
    }
}

/// `0x02 ‖ RLP([chainId, nonce, maxPriorityFee, maxFee, gasLimit, to, value,
/// data, accessList (, yParity, r, s)])`. With `sig` this is the signed
/// broadcast form; without it, the payload keccak256'd to the signing hash.
fn encode_1559(
    chain_id: u64,
    nonce: u64,
    fees: Fees,
    gas_limit: u64,
    tx: &TxRequest,
    sig: Option<(u8, [u8; 32], [u8; 32])>,
) -> Vec<u8> {
    let mut fields = vec![
        rlp_uint(&chain_id.to_be_bytes()),
        rlp_uint(&nonce.to_be_bytes()),
        rlp_uint(&fees.max_priority.to_be_bytes()),
        rlp_uint(&fees.max_fee.to_be_bytes()),
        rlp_uint(&gas_limit.to_be_bytes()),
        rlp_str(&tx.to),
        rlp_uint(&tx.value.to_be_bytes()),
        rlp_str(&tx.data),
        rlp_list(&[]),
    ];
    if let Some((y_parity, r, s)) = sig {
        fields.push(rlp_uint(&[y_parity]));
        fields.push(rlp_uint(&r));
        fields.push(rlp_uint(&s));
    }
    let mut out = vec![0x02u8];
    out.extend_from_slice(&rlp_list(&fields));
    out
}

fn rlp_str(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return vec![bytes[0]];
    }
    let mut out = rlp_len(bytes.len(), 0x80);
    out.extend_from_slice(bytes);
    out
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = items.concat();
    let mut out = rlp_len(body.len(), 0xc0);
    out.extend_from_slice(&body);
    out
}

/// RLP length prefix: short form up to 55 bytes, else `offset+55+len_of_len`
/// followed by the big-endian length. `offset` is 0x80 for strings, 0xc0 for
/// lists.
fn rlp_len(len: usize, offset: u8) -> Vec<u8> {
    if len <= 55 {
        return vec![offset + len as u8];
    }
    let be = len.to_be_bytes();
    let be = &be[be.iter().position(|&b| b != 0).unwrap()..];
    let mut out = vec![offset + 55 + be.len() as u8];
    out.extend_from_slice(be);
    out
}

/// RLP-encode a big-endian integer: leading zero bytes stripped (so zero is
/// the empty string `0x80`), then string-encoded. `r`/`s`/quantities all ride
/// this.
fn rlp_uint(be: &[u8]) -> Vec<u8> {
    let start = be.iter().position(|&b| b != 0).unwrap_or(be.len());
    rlp_str(&be[start..])
}

/// Errors from building or broadcasting a transaction. The `Rpc` variant
/// carries the node's `data` field verbatim so the daemon can decode a custom
/// error selector (a bounded-spend revert surfaces as `LimitExceeded()` here).
#[derive(Debug, thiserror::Error)]
pub enum EvmTxError {
    #[error("secp256k1 key: {0}")]
    Key(String),
    /// `data` holds the node's revert bytes verbatim (e.g. a custom-error
    /// selector) for the caller to decode; it is deliberately kept out of the
    /// Display message so logs don't split on it.
    #[error("rpc {method}: {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
        data: Option<String>,
    },
    #[error("rpc {method} transport: {source}")]
    Transport {
        method: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("rpc {method} decode: {0}", .detail)]
    Decode { method: String, detail: String },
    #[error("chain id mismatch: config {expected}, rpc reports {got}")]
    ChainMismatch { expected: u64, got: u64 },
    #[error("tx {tx} reverted onchain (receipt status 0)")]
    Reverted { tx: String },
    #[error("tx {tx} not mined within {waited_secs}s")]
    Timeout { tx: String, waited_secs: u64 },
}

#[cfg(feature = "evm-rpc")]
pub use rpc::{EvmRpc, TxReceipt};

#[cfg(feature = "evm-rpc")]
mod rpc {
    use std::time::Duration;

    use serde_json::{json, Value};

    use super::{EvmTxError, EvmTxSigner, Fees, TxRequest};
    use crate::evm::hex_0x;

    /// How long [`EvmRpc::submit`] polls for a receipt before giving up.
    const RECEIPT_POLL: Duration = Duration::from_millis(1500);
    const RECEIPT_ATTEMPTS: u32 = 80; // ~2 min at 1.5s — Orbit blocks are sub-second

    /// The confirmed outcome of a submitted transaction.
    #[derive(Debug, Clone)]
    pub struct TxReceipt {
        pub tx_hash: [u8; 32],
        pub block_number: u64,
        pub gas_used: u128,
    }

    /// A JSON-RPC client that signs with an [`EvmTxSigner`] and broadcasts.
    ///
    /// `chain_id` is config's source of truth for what the signature commits
    /// to; [`EvmRpc::submit`] verifies the endpoint agrees before signing, so
    /// a misconfigured RPC pointing at the wrong chain fails closed instead of
    /// producing a replayable signature.
    pub struct EvmRpc {
        http: reqwest::Client,
        url: String,
        chain_id: u64,
    }

    impl EvmRpc {
        pub fn new(url: impl Into<String>, chain_id: u64) -> Self {
            Self {
                http: crate::client::http_client(),
                url: url.into(),
                chain_id,
            }
        }

        /// Construct with a caller-supplied client (tests point this at a
        /// wiremock server; the daemon can share its pooled client).
        pub fn with_http(http: reqwest::Client, url: impl Into<String>, chain_id: u64) -> Self {
            Self {
                http,
                url: url.into(),
                chain_id,
            }
        }

        pub fn chain_id(&self) -> u64 {
            self.chain_id
        }

        async fn rpc(&self, method: &str, params: Value) -> Result<Value, EvmTxError> {
            let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
            let resp = self
                .http
                .post(&self.url)
                .json(&body)
                .send()
                .await
                .map_err(|source| EvmTxError::Transport {
                    method: method.to_string(),
                    source,
                })?;
            let text = resp.text().await.map_err(|source| EvmTxError::Transport {
                method: method.to_string(),
                source,
            })?;
            let value: Value = serde_json::from_str(&text).map_err(|e| EvmTxError::Decode {
                method: method.to_string(),
                detail: format!("{e}: {text}"),
            })?;
            if let Some(err) = value.get("error") {
                return Err(EvmTxError::Rpc {
                    method: method.to_string(),
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    data: err.get("data").and_then(Value::as_str).map(str::to_string),
                });
            }
            Ok(value.get("result").cloned().unwrap_or(Value::Null))
        }

        async fn quantity(&self, method: &str, params: Value) -> Result<u128, EvmTxError> {
            let result = self.rpc(method, params).await?;
            let s = result.as_str().ok_or_else(|| EvmTxError::Decode {
                method: method.to_string(),
                detail: format!("expected a hex quantity, got {result}"),
            })?;
            parse_quantity(s).map_err(|detail| EvmTxError::Decode {
                method: method.to_string(),
                detail,
            })
        }

        pub async fn eth_chain_id(&self) -> Result<u64, EvmTxError> {
            Ok(self.quantity("eth_chainId", json!([])).await? as u64)
        }

        pub async fn nonce(&self, address: &[u8; 20]) -> Result<u64, EvmTxError> {
            Ok(self
                .quantity(
                    "eth_getTransactionCount",
                    json!([hex_0x(address), "pending"]),
                )
                .await? as u64)
        }

        pub async fn gas_price(&self) -> Result<u128, EvmTxError> {
            self.quantity("eth_gasPrice", json!([])).await
        }

        /// Suggested tip. Nodes that don't implement it (or return an error)
        /// yield 0 — correct on Orbit L2s, where the priority fee is 0.
        pub async fn max_priority_fee(&self) -> u128 {
            self.quantity("eth_maxPriorityFeePerGas", json!([]))
                .await
                .unwrap_or(0)
        }

        /// Estimate gas — this is also the revert preflight: a call that would
        /// revert (e.g. an over-ceiling `chargeCall`) errors here with the
        /// contract's custom-error selector in `data`.
        pub async fn estimate_gas(
            &self,
            from: &[u8; 20],
            tx: &TxRequest,
        ) -> Result<u64, EvmTxError> {
            let call = json!({
                "from": hex_0x(from),
                "to": hex_0x(&tx.to),
                "value": quantity_hex(tx.value),
                "data": hex_0x(&tx.data),
            });
            Ok(self.quantity("eth_estimateGas", json!([call])).await? as u64)
        }

        pub async fn send_raw(&self, raw: &[u8]) -> Result<[u8; 32], EvmTxError> {
            let result = self
                .rpc("eth_sendRawTransaction", json!([hex_0x(raw)]))
                .await?;
            let s = result.as_str().ok_or_else(|| EvmTxError::Decode {
                method: "eth_sendRawTransaction".into(),
                detail: format!("expected a tx hash, got {result}"),
            })?;
            parse_hash(s).map_err(|detail| EvmTxError::Decode {
                method: "eth_sendRawTransaction".into(),
                detail,
            })
        }

        pub async fn receipt(&self, hash: &[u8; 32]) -> Result<Option<TxReceipt>, EvmTxError> {
            let result = self
                .rpc("eth_getTransactionReceipt", json!([hex_0x(hash)]))
                .await?;
            if result.is_null() {
                return Ok(None);
            }
            let status = result
                .get("status")
                .and_then(Value::as_str)
                .and_then(|s| parse_quantity(s).ok())
                .ok_or_else(|| EvmTxError::Decode {
                    method: "eth_getTransactionReceipt".into(),
                    detail: "receipt missing status".into(),
                })?;
            if status != 1 {
                return Err(EvmTxError::Reverted { tx: hex_0x(hash) });
            }
            let block_number = result
                .get("blockNumber")
                .and_then(Value::as_str)
                .and_then(|s| parse_quantity(s).ok())
                .unwrap_or(0) as u64;
            let gas_used = result
                .get("gasUsed")
                .and_then(Value::as_str)
                .and_then(|s| parse_quantity(s).ok())
                .unwrap_or(0);
            Ok(Some(TxReceipt {
                tx_hash: *hash,
                block_number,
                gas_used,
            }))
        }

        /// The whole flow: verify chain, resolve nonce/fees/gas (estimate
        /// preflights the revert), sign, broadcast, then poll to a receipt.
        /// Returns `Reverted` if the tx mines with status 0, `Timeout` if it
        /// never mines.
        pub async fn submit(
            &self,
            signer: &EvmTxSigner,
            tx: &TxRequest,
        ) -> Result<TxReceipt, EvmTxError> {
            let onchain = self.eth_chain_id().await?;
            if onchain != self.chain_id {
                return Err(EvmTxError::ChainMismatch {
                    expected: self.chain_id,
                    got: onchain,
                });
            }
            let from = signer.address();
            let nonce = self.nonce(&from).await?;
            let base = self.gas_price().await?;
            let tip = self.max_priority_fee().await.min(base);
            let fees = Fees {
                max_priority: tip,
                max_fee: base.saturating_mul(2).saturating_add(tip),
            };
            let gas_limit = match tx.gas_limit {
                Some(g) => g,
                None => {
                    let est = self.estimate_gas(&from, tx).await?;
                    est.saturating_mul(12) / 10
                }
            };
            let (raw, _) = signer.sign_tx(self.chain_id, nonce, fees, gas_limit, tx);
            let hash = self.send_raw(&raw).await?;
            self.await_receipt(&hash).await
        }

        async fn await_receipt(&self, hash: &[u8; 32]) -> Result<TxReceipt, EvmTxError> {
            for _ in 0..RECEIPT_ATTEMPTS {
                if let Some(receipt) = self.receipt(hash).await? {
                    return Ok(receipt);
                }
                tokio::time::sleep(RECEIPT_POLL).await;
            }
            Err(EvmTxError::Timeout {
                tx: hex_0x(hash),
                waited_secs: (RECEIPT_POLL * RECEIPT_ATTEMPTS).as_secs(),
            })
        }
    }

    fn quantity_hex(value: u128) -> String {
        format!("0x{value:x}")
    }

    fn parse_quantity(s: &str) -> Result<u128, String> {
        let body = s.strip_prefix("0x").unwrap_or(s);
        if body.is_empty() {
            return Ok(0);
        }
        u128::from_str_radix(body, 16).map_err(|e| format!("bad quantity {s:?}: {e}"))
    }

    fn parse_hash(s: &str) -> Result<[u8; 32], String> {
        let body = s.strip_prefix("0x").unwrap_or(s);
        if body.len() != 64 {
            return Err(format!("expected a 32-byte hash, got {} chars", body.len()));
        }
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&body[2 * i..2 * i + 2], 16)
                .map_err(|e| format!("bad hash hex: {e}"))?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // keccak256("cow") — the canonical EIP-712 example secret, reused so the
    // signer address is a known quantity across the crate's tests.
    fn cow_secret() -> [u8; 32] {
        keccak256(b"cow")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rlp_encodes_canonical_scalars() {
        assert_eq!(rlp_uint(&0u64.to_be_bytes()), vec![0x80]); // zero -> empty string
        assert_eq!(rlp_uint(&1u64.to_be_bytes()), vec![0x01]); // small single byte is itself
        assert_eq!(rlp_uint(&0x7fu64.to_be_bytes()), vec![0x7f]);
        assert_eq!(rlp_uint(&0x80u64.to_be_bytes()), vec![0x81, 0x80]); // 0x80 needs a length prefix
        assert_eq!(rlp_str(&[]), vec![0x80]);
        assert_eq!(rlp_list(&[]), vec![0xc0]);
        // a 20-byte address is a 0x94-prefixed string
        let addr = [0x11u8; 20];
        let enc = rlp_str(&addr);
        assert_eq!(enc[0], 0x94);
        assert_eq!(&enc[1..], &addr);
    }

    #[test]
    fn signature_is_recoverable_to_the_signer() {
        use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

        let signer = EvmTxSigner::from_secret_bytes(&cow_secret()).unwrap();
        let tx = TxRequest::call([0xab; 20], vec![0xde, 0xad, 0xbe, 0xef]);
        let fees = Fees {
            max_priority: 1_000_000,
            max_fee: 2_000_000_000,
        };
        let (raw, tx_hash) = signer.sign_tx(46630, 7, fees, 90_000, &tx);

        assert_eq!(raw[0], 0x02, "typed-tx envelope prefix");
        assert_eq!(keccak256(&raw), tx_hash);

        // Recompute the signing hash and recover — the address the network
        // will attribute the tx to must be our signer.
        let unsigned = encode_1559(46630, 7, fees, 90_000, &tx, None);
        let sighash = keccak256(&unsigned);
        let (y, r, s) = signer.sign_hash(&sighash);
        let mut compact = [0u8; 64];
        compact[..32].copy_from_slice(&r);
        compact[32..].copy_from_slice(&s);
        let sig = Signature::from_slice(&compact).unwrap();
        let recid = RecoveryId::from_byte(y).unwrap();
        let key = VerifyingKey::recover_from_prehash(&sighash, &sig, recid).unwrap();
        let encoded = key.to_encoded_point(false);
        let recovered = &keccak256(&encoded.as_bytes()[1..])[12..];
        assert_eq!(recovered, &signer.address());
    }

    // Golden vector: foundry (an independent secp256k1/RLP implementation)
    // over the same key + fields must produce a byte-identical raw tx. Pins
    // RLP field order, the type-0x2 prefix, low-S, and yParity. Regenerate:
    //   K=$(cast keccak cow)
    //   cast mktx 0x57316dfc56f1f07fe7ff828f941e9d07f81e2534 0x27dcd974 \
    //     --nonce 3 --chain 46630 --gas-limit 120000 \
    //     --gas-price 1000000000 --priority-gas-price 100000000 --value 0 \
    //     --private-key "$K" --rpc-url "$RH_TESTNET_RPC"
    #[test]
    fn golden_raw_tx_matches_cast() {
        let signer = EvmTxSigner::from_secret_bytes(&cow_secret()).unwrap();
        let tx = TxRequest {
            to: hex20("0x57316dfc56f1f07fe7ff828f941e9d07f81e2534"),
            value: 0,
            data: vec![0x27, 0xdc, 0xd9, 0x74],
            gas_limit: Some(120_000),
        };
        let fees = Fees {
            max_priority: 100_000_000,
            max_fee: 1_000_000_000,
        };
        let (raw, _) = signer.sign_tx(46630, 3, fees, 120_000, &tx);
        assert_eq!(hex(&raw), GOLDEN_RAW_TX);
    }

    fn hex20(s: &str) -> [u8; 20] {
        let body = s.strip_prefix("0x").unwrap();
        let mut out = [0u8; 20];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&body[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    const GOLDEN_RAW_TX: &str = "02f87182b626038405f5e100843b9aca008301d4c09457316dfc56f1f07fe7ff828f941e9d07f81e2534808427dcd974c080a08205aaf63a390874024ce310aad77b696a2a41fcd812e61354ed52c0de9c5264a04a1517c247c0543eef54261e75474e764d48a89e7e6d671459f03afa6ad1c332";
}

#[cfg(all(test, feature = "evm-rpc"))]
mod rpc_tests {
    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::method as http_method;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn ok(result: Value) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": result}))
    }

    fn method_of(req: &Request) -> String {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        body["method"].as_str().unwrap().to_string()
    }

    const SENT_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    struct HappyPath;
    impl Respond for HappyPath {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            match method_of(req).as_str() {
                "eth_chainId" => ok(json!("0xb626")), // 46630
                "eth_getTransactionCount" => ok(json!("0x7")),
                "eth_gasPrice" => ok(json!("0x3b9aca00")), // 1 gwei
                "eth_maxPriorityFeePerGas" => ok(json!("0x0")),
                "eth_estimateGas" => ok(json!("0x15f90")), // 90_000
                "eth_sendRawTransaction" => ok(json!(SENT_HASH)),
                "eth_getTransactionReceipt" => ok(json!({
                    "status": "0x1",
                    "blockNumber": "0x2a", // 42
                    "gasUsed": "0xb1b0",   // 45_488
                    "transactionHash": SENT_HASH,
                })),
                other => panic!("unexpected rpc method {other}"),
            }
        }
    }

    #[tokio::test]
    async fn submit_drives_the_full_rpc_flow() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .respond_with(HappyPath)
            .mount(&server)
            .await;

        let rpc = EvmRpc::with_http(reqwest::Client::new(), server.uri(), 46630);
        let signer = EvmTxSigner::from_secret_bytes(&keccak256(b"cow")).unwrap();
        let tx = TxRequest::call([0xab; 20], vec![0x27, 0xdc, 0xd9, 0x74]);

        let receipt = rpc.submit(&signer, &tx).await.expect("submit");
        assert_eq!(receipt.block_number, 42);
        assert_eq!(receipt.gas_used, 0xb1b0);
    }

    struct EstimateReverts;
    impl Respond for EstimateReverts {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            match method_of(req).as_str() {
                "eth_chainId" => ok(json!("0xb626")),
                "eth_getTransactionCount" => ok(json!("0x7")),
                "eth_gasPrice" => ok(json!("0x3b9aca00")),
                "eth_maxPriorityFeePerGas" => ok(json!("0x0")),
                // A bounded-spend revert: the node returns the custom-error
                // selector for LimitExceeded() in `data`.
                "eth_estimateGas" => ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": { "code": 3, "message": "execution reverted", "data": "0x3261c792" },
                })),
                other => panic!("unexpected rpc method {other}"),
            }
        }
    }

    #[tokio::test]
    async fn submit_surfaces_a_revert_selector_and_never_broadcasts() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .respond_with(EstimateReverts)
            .mount(&server)
            .await;

        let rpc = EvmRpc::with_http(reqwest::Client::new(), server.uri(), 46630);
        let signer = EvmTxSigner::from_secret_bytes(&keccak256(b"cow")).unwrap();
        let tx = TxRequest::call([0xab; 20], vec![0xde, 0xad]);

        let err = rpc.submit(&signer, &tx).await.unwrap_err();
        match err {
            EvmTxError::Rpc { method, data, .. } => {
                assert_eq!(method, "eth_estimateGas");
                assert_eq!(data.as_deref(), Some("0x3261c792")); // LimitExceeded()
            }
            other => panic!("expected a preflight revert, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_fails_closed_on_chain_id_mismatch() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .respond_with(ok(json!("0x1"))) // RPC claims mainnet (1), config says 46630
            .mount(&server)
            .await;

        let rpc = EvmRpc::with_http(reqwest::Client::new(), server.uri(), 46630);
        let signer = EvmTxSigner::from_secret_bytes(&keccak256(b"cow")).unwrap();
        let tx = TxRequest::call([0xab; 20], vec![]);

        match rpc.submit(&signer, &tx).await.unwrap_err() {
            EvmTxError::ChainMismatch { expected, got } => {
                assert_eq!((expected, got), (46630, 1));
            }
            other => panic!("expected ChainMismatch, got {other:?}"),
        }
    }
}
