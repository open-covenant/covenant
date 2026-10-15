//! The smallest EVM client the node needs to pay for work on an
//! Ethereum-family chain: a secp256k1 signer, EIP-1559 transactions,
//! the JSON-RPC calls around them, and the ABI words for an ERC-20
//! approval and a call with a bytes argument. The chain the node
//! actually pays on is whatever its executor configures; nothing here
//! knows about IMD.
//!
//! Hand-rolled on purpose. The workspace already carries `k256` and
//! `sha3` through its Solana dependencies, so signing and hashing cost
//! no new crate; RLP and ABI encoding for the three calls this module
//! makes are a few dozen lines each, and every encoder is pinned to a
//! vector produced by Foundry's `cast` in the tests below.

use std::time::Duration;

use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};

#[derive(Debug, thiserror::Error)]
pub enum EvmError {
    #[error("rpc transport: {0}")]
    Transport(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("rpc answered {0}")]
    Malformed(String),
    #[error("transaction {0} reverted")]
    Reverted(String),
    #[error("{0}")]
    Invalid(String),
}

/// A 20-byte account address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address(pub [u8; 20]);

impl Address {
    /// Parses `0x`-prefixed or bare hex of exactly 40 digits.
    pub fn parse(text: &str) -> Result<Self, EvmError> {
        let bytes = decode_hex(text)?;
        let arr: [u8; 20] = bytes
            .try_into()
            .map_err(|_| EvmError::Invalid(format!("address {text:?} is not 20 bytes")))?;
        Ok(Address(arr))
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", encode_hex(&self.0))
    }

    pub const ZERO: Address = Address([0u8; 20]);
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Keccak-256 of `bytes`, the hash every EVM identity and signature rests on.
pub fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Parses hex with or without `0x`, refusing odd lengths and non-hex digits.
pub fn decode_hex(text: &str) -> Result<Vec<u8>, EvmError> {
    let digits = text.trim().strip_prefix("0x").unwrap_or(text.trim());
    if !digits.len().is_multiple_of(2) {
        return Err(EvmError::Invalid(format!("odd-length hex {text:?}")));
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .map_err(|_| EvmError::Invalid(format!("not hex: {text:?}")))
        })
        .collect()
}

pub fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses a JSON-RPC quantity (`0x`-prefixed hex, no leading zeros).
pub fn parse_quantity(text: &str) -> Result<u128, EvmError> {
    let digits = text.trim().strip_prefix("0x").unwrap_or(text.trim());
    if digits.is_empty() {
        return Err(EvmError::Malformed(format!("empty quantity {text:?}")));
    }
    u128::from_str_radix(digits, 16).map_err(|_| EvmError::Malformed(format!("quantity {text:?}")))
}

// ---------------------------------------------------------------- signer

/// A secp256k1 key and the address it controls.
pub struct Signer {
    key: SigningKey,
    address: Address,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("address", &self.address)
            .finish()
    }
}

impl Signer {
    /// Loads a key from 32 bytes of hex (`0x` optional).
    pub fn from_hex(text: &str) -> Result<Self, EvmError> {
        let bytes = decode_hex(text)?;
        if bytes.len() != 32 {
            return Err(EvmError::Invalid("a private key is 32 bytes".into()));
        }
        let key = SigningKey::from_slice(&bytes)
            .map_err(|_| EvmError::Invalid("private key is not a valid secp256k1 scalar".into()))?;
        let point = key.verifying_key().to_encoded_point(false);
        let hash = keccak256(&point.as_bytes()[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        Ok(Signer {
            key,
            address: Address(address),
        })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Signs a 32-byte digest, returning `(r, s, y_parity)` with a low-S
    /// signature as every EVM client requires.
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<([u8; 32], [u8; 32], u8), EvmError> {
        let (signature, recovery) = self
            .key
            .sign_prehash_recoverable(digest)
            .map_err(|e| EvmError::Invalid(format!("sign: {e}")))?;
        // k256 normalizes to low-S already; keep the guard so a future
        // version that stops doing so cannot produce a malleable
        // signature here. A flipped S flips the parity.
        let (signature, parity) = match signature.normalize_s() {
            Some(normalized) => (normalized, recovery.is_y_odd() as u8 ^ 1),
            None => (signature, recovery.is_y_odd() as u8),
        };
        let bytes = signature.to_bytes();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&bytes[..32]);
        s.copy_from_slice(&bytes[32..]);
        Ok((r, s, parity))
    }
}

// ------------------------------------------------------------------ rlp

fn rlp_length(len: usize, offset: u8) -> Vec<u8> {
    if len < 56 {
        vec![offset + len as u8]
    } else {
        let be = len.to_be_bytes();
        let trimmed: Vec<u8> = be.iter().copied().skip_while(|b| *b == 0).collect();
        let mut out = vec![offset + 55 + trimmed.len() as u8];
        out.extend(trimmed);
        out
    }
}

/// RLP-encodes a byte string.
pub fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return bytes.to_vec();
    }
    let mut out = rlp_length(bytes.len(), 0x80);
    out.extend_from_slice(bytes);
    out
}

/// RLP-encodes an unsigned integer as its minimal big-endian bytes.
pub fn rlp_uint(value: u128) -> Vec<u8> {
    if value == 0 {
        return vec![0x80];
    }
    let be = value.to_be_bytes();
    let trimmed: Vec<u8> = be.iter().copied().skip_while(|b| *b == 0).collect();
    rlp_bytes(&trimmed)
}

/// RLP-encodes a 32-byte word as an integer (leading zeros dropped).
fn rlp_word(word: &[u8; 32]) -> Vec<u8> {
    let trimmed: Vec<u8> = word.iter().copied().skip_while(|b| *b == 0).collect();
    if trimmed.is_empty() {
        vec![0x80]
    } else {
        rlp_bytes(&trimmed)
    }
}

/// RLP-encodes a list of already-encoded items.
pub fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.iter().flatten().copied().collect();
    let mut out = rlp_length(payload.len(), 0xc0);
    out.extend(payload);
    out
}

// ------------------------------------------------------------- eip-1559

/// The fields of a type-2 transaction before signing.
#[derive(Debug, Clone)]
pub struct Eip1559Transaction {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub gas_limit: u64,
    pub to: Address,
    pub value: u128,
    pub data: Vec<u8>,
}

/// A signed transaction: the raw bytes to broadcast and their hash.
#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub raw: Vec<u8>,
    pub hash: [u8; 32],
}

impl SignedTransaction {
    pub fn raw_hex(&self) -> String {
        format!("0x{}", encode_hex(&self.raw))
    }

    pub fn hash_hex(&self) -> String {
        format!("0x{}", encode_hex(&self.hash))
    }
}

impl Eip1559Transaction {
    fn unsigned_fields(&self) -> Vec<Vec<u8>> {
        vec![
            rlp_uint(self.chain_id as u128),
            rlp_uint(self.nonce as u128),
            rlp_uint(self.max_priority_fee_per_gas),
            rlp_uint(self.max_fee_per_gas),
            rlp_uint(self.gas_limit as u128),
            rlp_bytes(&self.to.0),
            rlp_uint(self.value),
            rlp_bytes(&self.data),
            rlp_list(&[]),
        ]
    }

    /// Signs with `signer`, producing the EIP-2718 envelope
    /// `0x02 || rlp([... fields ..., y_parity, r, s])`.
    pub fn sign(&self, signer: &Signer) -> Result<SignedTransaction, EvmError> {
        let mut preimage = vec![0x02u8];
        preimage.extend(rlp_list(&self.unsigned_fields()));
        let digest = keccak256(&preimage);
        let (r, s, parity) = signer.sign_digest(&digest)?;
        let mut fields = self.unsigned_fields();
        fields.push(rlp_uint(parity as u128));
        fields.push(rlp_word(&r));
        fields.push(rlp_word(&s));
        let mut raw = vec![0x02u8];
        raw.extend(rlp_list(&fields));
        let hash = keccak256(&raw);
        Ok(SignedTransaction { raw, hash })
    }
}

// ------------------------------------------------------------------ abi

fn word_from_bytes_right(bytes: &[u8]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[32 - bytes.len()..].copy_from_slice(bytes);
    word
}

fn word_from_bytes_left(bytes: &[u8]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[..bytes.len()].copy_from_slice(bytes);
    word
}

fn word_u128(value: u128) -> [u8; 32] {
    word_from_bytes_right(&value.to_be_bytes())
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// `approve(address spender, uint256 amount)`.
pub fn encode_approve(spender: Address, amount: u128) -> Vec<u8> {
    let mut data = selector("approve(address,uint256)").to_vec();
    data.extend(word_from_bytes_right(&spender.0));
    data.extend(word_u128(amount));
    data
}

/// `approve` with the maximum `uint256`, the one-time allowance pattern.
pub fn encode_approve_max(spender: Address) -> Vec<u8> {
    let mut data = selector("approve(address,uint256)").to_vec();
    data.extend(word_from_bytes_right(&spender.0));
    data.extend([0xffu8; 32]);
    data
}

/// `allowance(address owner, address spender)`.
pub fn encode_allowance(owner: Address, spender: Address) -> Vec<u8> {
    let mut data = selector("allowance(address,address)").to_vec();
    data.extend(word_from_bytes_right(&owner.0));
    data.extend(word_from_bytes_right(&spender.0));
    data
}

/// `balanceOf(address)`.
pub fn encode_balance_of(owner: Address) -> Vec<u8> {
    let mut data = selector("balanceOf(address)").to_vec();
    data.extend(word_from_bytes_right(&owner.0));
    data
}

/// `request(bytes32 action, bytes body, (address target, bytes4 selector)
/// callback, address asset, uint256 amount)`: one ask and its payment, the
/// call IMD's Intake contract takes. The tuple is static, so the head is
/// six words and the body is the only tail item.
pub fn encode_intake_request(
    action: [u8; 32],
    body: &[u8],
    callback: (Address, [u8; 4]),
    asset: Address,
    amount: u128,
) -> Vec<u8> {
    let mut data = selector("request(bytes32,bytes,(address,bytes4),address,uint256)").to_vec();
    data.extend(action);
    data.extend(word_u128(6 * 32));
    data.extend(word_from_bytes_right(&callback.0 .0));
    data.extend(word_from_bytes_left(&callback.1));
    data.extend(word_from_bytes_right(&asset.0));
    data.extend(word_u128(amount));
    data.extend(word_u128(body.len() as u128));
    data.extend(body);
    let pad = (32 - body.len() % 32) % 32;
    data.extend(std::iter::repeat_n(0u8, pad));
    data
}

/// A string right-padded with zeros to 32 bytes, the way Solidity reads
/// `bytes32 action = "job.open@job-1"`.
pub fn bytes32_string(text: &str) -> Result<[u8; 32], EvmError> {
    let bytes = text.as_bytes();
    if bytes.len() > 32 {
        return Err(EvmError::Invalid(format!(
            "{text:?} does not fit in bytes32"
        )));
    }
    Ok(word_from_bytes_left(bytes))
}

/// Reads a `uint256` return word as a `u128` (anything larger saturates,
/// which is the right answer for a balance or allowance comparison).
pub fn decode_uint(data: &[u8]) -> Result<u128, EvmError> {
    if data.len() < 32 {
        return Err(EvmError::Malformed(format!(
            "expected a 32-byte word, got {} bytes",
            data.len()
        )));
    }
    let word = &data[data.len() - 32..];
    if word[..16].iter().any(|b| *b != 0) {
        return Ok(u128::MAX);
    }
    let mut low = [0u8; 16];
    low.copy_from_slice(&word[16..]);
    Ok(u128::from_be_bytes(low))
}

// ------------------------------------------------------------- json-rpc

/// A transaction receipt's two facts the node acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub success: bool,
    pub block_number: u64,
}

/// JSON-RPC over HTTP against one node URL.
#[derive(Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcClient").field("url", &self.url).finish()
    }
}

impl RpcClient {
    pub fn new(url: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("a client with a timeout builds");
        RpcClient {
            http,
            url: url.into(),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, EvmError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| EvmError::Transport(format!("{method}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| EvmError::Transport(format!("{method}: {e}")))?;
        if !status.is_success() {
            return Err(EvmError::Transport(format!(
                "{method}: HTTP {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EvmError::Malformed(format!("{method}: {e}")))?;
        if let Some(err) = value.get("error") {
            return Err(EvmError::Rpc {
                code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            });
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| EvmError::Malformed(format!("{method}: no result")))
    }

    async fn quantity(&self, method: &str, params: serde_json::Value) -> Result<u128, EvmError> {
        let value = self.call(method, params).await?;
        let text = value
            .as_str()
            .ok_or_else(|| EvmError::Malformed(format!("{method}: not a quantity")))?;
        parse_quantity(text)
    }

    pub async fn chain_id(&self) -> Result<u64, EvmError> {
        Ok(self.quantity("eth_chainId", serde_json::json!([])).await? as u64)
    }

    pub async fn pending_nonce(&self, address: Address) -> Result<u64, EvmError> {
        Ok(self
            .quantity(
                "eth_getTransactionCount",
                serde_json::json!([address.to_hex(), "pending"]),
            )
            .await? as u64)
    }

    pub async fn native_balance(&self, address: Address) -> Result<u128, EvmError> {
        self.quantity(
            "eth_getBalance",
            serde_json::json!([address.to_hex(), "latest"]),
        )
        .await
    }

    /// `eth_call` returning the raw return data.
    pub async fn eth_call(&self, to: Address, data: &[u8]) -> Result<Vec<u8>, EvmError> {
        let value = self
            .call(
                "eth_call",
                serde_json::json!([
                    { "to": to.to_hex(), "data": format!("0x{}", encode_hex(data)) },
                    "latest"
                ]),
            )
            .await?;
        let text = value
            .as_str()
            .ok_or_else(|| EvmError::Malformed("eth_call: not hex".into()))?;
        decode_hex(text)
    }

    pub async fn estimate_gas(
        &self,
        from: Address,
        to: Address,
        data: &[u8],
    ) -> Result<u64, EvmError> {
        Ok(self
            .quantity(
                "eth_estimateGas",
                serde_json::json!([{
                    "from": from.to_hex(),
                    "to": to.to_hex(),
                    "data": format!("0x{}", encode_hex(data)),
                }]),
            )
            .await? as u64)
    }

    /// The latest block's base fee, or `None` on a chain without one.
    pub async fn base_fee(&self) -> Result<Option<u128>, EvmError> {
        let block = self
            .call("eth_getBlockByNumber", serde_json::json!(["latest", false]))
            .await?;
        match block.get("baseFeePerGas").and_then(|v| v.as_str()) {
            Some(text) => Ok(Some(parse_quantity(text)?)),
            None => Ok(None),
        }
    }

    /// The node's suggested priority fee, or zero where the method is
    /// unsupported (many rollups carry no priority market).
    pub async fn priority_fee(&self) -> Result<u128, EvmError> {
        match self
            .quantity("eth_maxPriorityFeePerGas", serde_json::json!([]))
            .await
        {
            Ok(fee) => Ok(fee),
            Err(EvmError::Rpc { .. }) => Ok(0),
            Err(other) => Err(other),
        }
    }

    pub async fn gas_price(&self) -> Result<u128, EvmError> {
        self.quantity("eth_gasPrice", serde_json::json!([])).await
    }

    /// Broadcasts a signed transaction, returning the hash the node reports.
    pub async fn send_raw(&self, raw_hex: &str) -> Result<String, EvmError> {
        let value = self
            .call("eth_sendRawTransaction", serde_json::json!([raw_hex]))
            .await?;
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| EvmError::Malformed("eth_sendRawTransaction: not a hash".into()))
    }

    /// The receipt once mined; `None` while pending or unknown.
    pub async fn receipt(&self, hash_hex: &str) -> Result<Option<Receipt>, EvmError> {
        let value = self
            .call("eth_getTransactionReceipt", serde_json::json!([hash_hex]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        let status = value
            .get("status")
            .and_then(|s| s.as_str())
            .map(parse_quantity)
            .transpose()?
            .unwrap_or(0);
        let block_number = value
            .get("blockNumber")
            .and_then(|s| s.as_str())
            .map(parse_quantity)
            .transpose()?
            .unwrap_or(0) as u64;
        Ok(Some(Receipt {
            success: status == 1,
            block_number,
        }))
    }

    /// Whether the chain knows the transaction at all (pending or mined).
    pub async fn knows_transaction(&self, hash_hex: &str) -> Result<bool, EvmError> {
        let value = self
            .call("eth_getTransactionByHash", serde_json::json!([hash_hex]))
            .await?;
        Ok(!value.is_null())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTOR_KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
    const VECTOR_ADDRESS: &str = "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23";
    const IMD_ON_ROBINHOOD: &str = "0x5f7bb59365ce557c26dbcaa4ee9d39a4b95b7127";
    const INTAKE: &str = "0xa43e6f75ee006411f79ac1c84120606c2330de82";

    #[test]
    fn rlp_matches_the_standard_vectors() {
        assert_eq!(rlp_bytes(b"dog"), vec![0x83, b'd', b'o', b'g']);
        assert_eq!(
            rlp_list(&[rlp_bytes(b"cat"), rlp_bytes(b"dog")]),
            vec![0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
        assert_eq!(rlp_bytes(b""), vec![0x80]);
        assert_eq!(rlp_list(&[]), vec![0xc0]);
        assert_eq!(rlp_uint(0), vec![0x80]);
        assert_eq!(rlp_uint(15), vec![0x0f]);
        assert_eq!(rlp_uint(1024), vec![0x82, 0x04, 0x00]);
        let long = vec![b'a'; 56];
        let encoded = rlp_bytes(&long);
        assert_eq!(&encoded[..2], &[0xb8, 56]);
        assert_eq!(encoded.len(), 58);
    }

    #[test]
    fn the_signer_derives_the_address_cast_derives() {
        let signer = Signer::from_hex(VECTOR_KEY).unwrap();
        assert_eq!(
            signer.address().to_hex(),
            VECTOR_ADDRESS.to_ascii_lowercase()
        );
        assert!(Signer::from_hex("0x00").is_err());
        assert!(
            Signer::from_hex(&"00".repeat(32)).is_err(),
            "zero is not a scalar"
        );
    }

    #[test]
    fn a_signed_transaction_matches_cast_mktx_byte_for_byte() {
        // cast mktx --private-key <VECTOR_KEY> --chain 4663 --nonce 7
        //   --gas-limit 90000 --gas-price 2000000000 --priority-gas-price 1000000000
        //   --value 0 <IMD_ON_ROBINHOOD> "approve(address,uint256)" <INTAKE> 1000
        let expected = "0x02f8b282123707843b9aca00847735940083015f90945f7bb59365ce557c26dbcaa4ee9d39a4b95b712780b844095ea7b3000000000000000000000000a43e6f75ee006411f79ac1c84120606c2330de8200000000000000000000000000000000000000000000000000000000000003e8c080a009bae65e2c2b4b5bfca18d6957c4828c3ccd66a2a77e02fe3c3dddb19a13de47a0413e6c879e505eb79f24712dcc97d3691d842cab2f3e1ad918129c8cd9a503e8";
        let signer = Signer::from_hex(VECTOR_KEY).unwrap();
        let tx = Eip1559Transaction {
            chain_id: 4663,
            nonce: 7,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 90_000,
            to: Address::parse(IMD_ON_ROBINHOOD).unwrap(),
            value: 0,
            data: encode_approve(Address::parse(INTAKE).unwrap(), 1000),
        };
        let signed = tx.sign(&signer).unwrap();
        assert_eq!(signed.raw_hex(), expected);
        assert_eq!(
            signed.hash_hex(),
            format!(
                "0x{}",
                encode_hex(&keccak256(&decode_hex(expected).unwrap()))
            )
        );
    }

    #[test]
    fn the_intake_request_matches_cast_calldata() {
        // cast calldata "request(bytes32,bytes,(address,bytes4),address,uint256)"
        //   $(cast format-bytes32-string "job.open@job-1") 0x7b227822223a317d
        //   "(0x0000000000000000000000000000000000000000,0x00000000)"
        //   <IMD_ON_ROBINHOOD> 500000000000000000
        // The body is cast's eight literal bytes `{"x"":1}`: what matters is
        // the word layout, not that the sample is well-formed JSON.
        let expected = "0x380c2cda6a6f622e6f70656e406a6f622d3100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000005f7bb59365ce557c26dbcaa4ee9d39a4b95b712700000000000000000000000000000000000000000000000006f05b59d3b2000000000000000000000000000000000000000000000000000000000000000000087b227822223a317d000000000000000000000000000000000000000000000000";
        let data = encode_intake_request(
            bytes32_string("job.open@job-1").unwrap(),
            br#"{"x"":1}"#,
            (Address::ZERO, [0u8; 4]),
            Address::parse(IMD_ON_ROBINHOOD).unwrap(),
            500_000_000_000_000_000,
        );
        assert_eq!(format!("0x{}", encode_hex(&data)), expected);
        // A body that is a multiple of 32 bytes takes no padding word.
        let data = encode_intake_request(
            bytes32_string("oracle.request@oracle-1").unwrap(),
            &[0x41u8; 64],
            (Address::ZERO, [0u8; 4]),
            Address::parse(IMD_ON_ROBINHOOD).unwrap(),
            1,
        );
        assert_eq!(data.len(), 4 + 7 * 32 + 64);
        assert!(bytes32_string(&"x".repeat(33)).is_err());
    }

    #[test]
    fn erc20_calls_use_the_well_known_selectors() {
        let owner = Address::parse(VECTOR_ADDRESS).unwrap();
        let spender = Address::parse(INTAKE).unwrap();
        assert_eq!(
            &encode_allowance(owner, spender)[..4],
            &[0xdd, 0x62, 0xed, 0x3e]
        );
        assert_eq!(&encode_balance_of(owner)[..4], &[0x70, 0xa0, 0x82, 0x31]);
        let max = encode_approve_max(spender);
        assert_eq!(&max[..4], &[0x09, 0x5e, 0xa7, 0xb3]);
        assert!(max[36..].iter().all(|b| *b == 0xff));
    }

    #[test]
    fn uint_words_decode_and_saturate() {
        let mut word = [0u8; 32];
        word[31] = 7;
        assert_eq!(decode_uint(&word).unwrap(), 7);
        assert_eq!(decode_uint(&[0xffu8; 32]).unwrap(), u128::MAX);
        assert!(decode_uint(&[0u8; 4]).is_err());
        assert_eq!(parse_quantity("0x1a").unwrap(), 26);
        assert!(parse_quantity("0x").is_err());
        assert!(decode_hex("0xabc").is_err());
        assert!(Address::parse("0x1234").is_err());
    }

    #[tokio::test]
    async fn the_rpc_client_surfaces_errors_and_results() {
        use axum::routing::post;
        use axum::{Json, Router};
        let router = Router::new().route(
            "/",
            post(|Json(req): Json<serde_json::Value>| async move {
                let method = req["method"].as_str().unwrap_or("").to_string();
                let body = match method.as_str() {
                    "eth_chainId" => serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1237"}),
                    "eth_maxPriorityFeePerGas" => serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "error":{"code":-32601,"message":"method not found"}
                    }),
                    "eth_getTransactionReceipt" => serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"status":"0x1","blockNumber":"0x10"}
                    }),
                    "eth_getBlockByNumber" => serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"baseFeePerGas":"0x1440000"}
                    }),
                    _ => serde_json::json!({"jsonrpc":"2.0","id":1,"result":null}),
                };
                Json(body)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let rpc = RpcClient::new(format!("http://{addr}/"), Duration::from_secs(5));
        assert_eq!(rpc.chain_id().await.unwrap(), 4663);
        assert_eq!(
            rpc.priority_fee().await.unwrap(),
            0,
            "an unsupported method reads as zero"
        );
        assert_eq!(
            rpc.receipt("0xabc").await.unwrap(),
            Some(Receipt {
                success: true,
                block_number: 16
            })
        );
        assert_eq!(rpc.base_fee().await.unwrap(), Some(21_233_664));
        assert!(!rpc.knows_transaction("0xabc").await.unwrap());
    }
}
