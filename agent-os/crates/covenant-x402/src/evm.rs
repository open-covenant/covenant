//! EVM (Base) signer for the x402 `exact` scheme.
//!
//! Where [`crate::SolanaSigner`] builds and signs an SPL transfer, the EVM
//! path never builds a transaction at all. It signs an EIP-3009
//! [`TransferWithAuthorization`] over EIP-712 typed data; the facilitator
//! submits and pays gas for the on-chain `transferWithAuthorization` call.
//! The paying agent holds no ETH and needs no RPC — signing is a pure
//! local keccak-and-secp256k1 operation. Payment value is always the
//! token's own units (USDC), never a native or protocol token.
//!
//! [TransferWithAuthorization]: https://eips.ethereum.org/EIPS/eip-3009
//!
//! ## Envelope
//!
//! The `x-payment` header value is base64-encoded JSON matching the x402
//! `exact` EVM scheme:
//!
//! ```json
//! {
//!   "x402Version": 1,
//!   "scheme":      "exact",
//!   "network":     "<echoed from the requirement>",
//!   "payload": {
//!     "signature": "0x… (65 bytes)",
//!     "authorization": {
//!       "from":        "0x…",
//!       "to":          "0x…",
//!       "value":       "10000",
//!       "validAfter":  "1740672089",
//!       "validBefore": "1740672209",
//!       "nonce":       "0x… (32 bytes)"
//!     }
//!   }
//! }
//! ```
//!
//! `value`, `validAfter`, and `validBefore` are decimal strings; `nonce`
//! and `signature` are `0x` hex. The signature recovers to `from`, so a
//! facilitator authenticates the payment with the same `ecrecover` the
//! USDC contract performs.
//!
//! ## Asset pin
//!
//! The `asset` of a 402 challenge is attacker-controlled input. The
//! signer signs only for the chain-local USDC it pins per chain
//! ([`USDC_BASE_MAINNET`] on 8453, [`USDC_BASE_SEPOLIA`] on 84532); any
//! other `(chain, asset)` pair fails closed unless the operator opted it
//! in explicitly via [`EXTRA_ASSETS_ENV`]. Without the pin, "chain-local
//! USDC" is operator convention rather than signer enforcement, and a
//! malicious facilitator can obtain a valid EIP-3009
//! `TransferWithAuthorization` for an arbitrary ERC-20 just by naming it
//! as `asset` and supplying its domain in `extra`.
//!
//! ## Domain
//!
//! The EIP-712 domain is `(name, version, chainId, verifyingContract)`.
//! `chainId` comes from the requirement's `network`; `verifyingContract`
//! is the token `asset`. `name`/`version` are the token's EIP-712 domain
//! values — x402 challenges carry them in `extra`, and the signer reads
//! them there. When a challenge omits them the signer falls back only
//! for the tokens whose domains it pins with certainty (Base mainnet
//! USDC: `"USD Coin"`/`"2"`; Base Sepolia USDC: `"USDC"`/`"2"`) and
//! otherwise fails closed, because a wrong domain name silently yields a
//! signature the facilitator rejects.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use k256::ecdsa::SigningKey;
use rand::RngCore;
use serde_json::json;
use sha3::{Digest, Keccak256};

use crate::{PaymentRequirements, Result, Signer, X402Error};

/// USDC (native) on Base mainnet — 6 decimals, EIP-3009 capable. Its
/// EIP-712 domain name is `"USD Coin"`, version `"2"` — carried by a
/// live zauth 402 challenge for this exact contract (covenant-zauth
/// `challenge.rs` `LIVE_DECODED`, captured 2026-06-03) and re-verified
/// against the chain by `tests/live_evm_usdc_domain.rs`.
pub const USDC_BASE_MAINNET: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
/// USDC on Base Sepolia (Circle's testnet deployment) — 6 decimals,
/// EIP-3009 capable. Its EIP-712 domain name is `"USDC"`, version `"2"`
/// (re-verified against the chain by `tests/live_evm_usdc_domain.rs`).
pub const USDC_BASE_SEPOLIA: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

/// Env var naming operator-authorized `(chain, asset)` pairs beyond the
/// chain-local USDC pins: comma-separated `<chainId>:<0xaddress>`
/// entries, e.g. `8453:0x…,84532:0x…`. Matching is chain-scoped and
/// address-case-insensitive; a malformed entry is a hard error, never a
/// silently dropped or widened allowlist.
pub const EXTRA_ASSETS_ENV: &str = "COVENANT_X402_EVM_EXTRA_ASSETS";

/// x402 envelope version this client emits, matching [`crate::SolanaSigner`].
const X402_VERSION: u8 = 1;

/// Default settlement window: `validBefore = now + this`. Sized to the
/// client's own per-request ceiling so a legitimately slow settlement is
/// not rejected as expired.
const DEFAULT_VALID_FOR_SECS: u64 = 120;

/// Backdate `validAfter` by this much to absorb clock skew between the
/// signer and the facilitator, so a settlement submitted immediately is
/// never rejected as not-yet-valid. Replay is bounded by `nonce` and
/// `validBefore`, not by a tight lower edge.
const DEFAULT_CLOCK_SKEW_SECS: u64 = 600;

const EIP712_DOMAIN_TYPE: &[u8] =
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const TRANSFER_WITH_AUTHORIZATION_TYPE: &[u8] = b"TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)";

/// x402 `exact`-scheme signer for EVM chains, built for Base.
///
/// Holds the payer's secp256k1 funding key. Construct one from the same
/// 32-byte secret the daemon custodies (a [`covenant_identity`] secp256k1
/// issuer key exposes it via `secret_bytes()`), so there is no second key
/// system. The Solana signer is a separate type on a separate feature —
/// neither depends on the other.
///
/// [`covenant_identity`]: https://docs.rs/covenant-identity
pub struct EvmSigner {
    signing_key: SigningKey,
    address: [u8; 20],
    valid_for_secs: u64,
    clock_skew_secs: u64,
    extra_assets: Vec<(u64, [u8; 20])>,
}

impl EvmSigner {
    /// Build a signer from a 32-byte big-endian secp256k1 secret scalar.
    /// The error message omits the key material.
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Result<Self> {
        let signing_key = SigningKey::from_slice(secret)
            .map_err(|e| X402Error::Sign(format!("invalid secp256k1 funding secret: {e}")))?;
        let address = address_from_key(&signing_key);
        Ok(Self {
            signing_key,
            address,
            valid_for_secs: DEFAULT_VALID_FOR_SECS,
            clock_skew_secs: DEFAULT_CLOCK_SKEW_SECS,
            extra_assets: Vec::new(),
        })
    }

    /// Override the settlement window: `validBefore = now + secs`.
    pub fn with_valid_for_secs(mut self, secs: u64) -> Self {
        self.valid_for_secs = secs;
        self
    }

    /// Authorize extra `(chain id, asset)` pairs beyond the chain-local
    /// USDC pins — the parsed form of [`EXTRA_ASSETS_ENV`] (see
    /// [`parse_extra_assets`] / [`extra_assets_from_env`]). Explicit
    /// opt-in only: the default is the pins alone.
    pub fn with_extra_assets(mut self, extra_assets: Vec<(u64, [u8; 20])>) -> Self {
        self.extra_assets = extra_assets;
        self
    }

    /// The payer's 20-byte EVM address — the `from` of every authorization
    /// and the address a facilitator recovers from the signature.
    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// The payer address as a lowercase `0x` hex string.
    pub fn address_hex(&self) -> String {
        hex_0x(&self.address)
    }

    /// Deterministic core of [`Signer::build_payment`], split out so tests
    /// can pin `now` and `nonce` and recover the signature independently.
    fn build_envelope(
        &self,
        requirements: &PaymentRequirements,
        now_secs: u64,
        nonce: [u8; 32],
    ) -> Result<String> {
        let chain_id = chain_id_for_network(&requirements.network)?;
        let to = parse_address(&requirements.pay_to)
            .map_err(|e| X402Error::Sign(format!("parse pay_to {:?}: {e}", requirements.pay_to)))?;
        let verifying_contract = parse_address(&requirements.asset)
            .map_err(|e| X402Error::Sign(format!("parse asset {:?}: {e}", requirements.asset)))?;
        if !asset_permitted(chain_id, &verifying_contract, &self.extra_assets) {
            // Deliberately no paste-ready opt-in value here: the pair is
            // attacker-influenced, and an error that pre-fills it invites
            // authorizing an unvetted token to make the error go away.
            return Err(X402Error::Sign(format!(
                "EvmSigner: asset 0x{} is not the chain-local USDC for chain {chain_id}; \
                 refusing to sign a transfer authorization for an unpinned token — operators \
                 opt a vetted pair in via {EXTRA_ASSETS_ENV} (<chainId>:<0xaddress>, \
                 comma-separated)",
                hex_encode(&verifying_contract),
            )));
        }
        let value: u128 = requirements
            .amount
            .parse()
            .map_err(|e| X402Error::Sign(format!("parse amount {:?}: {e}", requirements.amount)))?;
        if value == 0 {
            return Err(X402Error::Sign(
                "amount must be positive; a zero-value authorization is a malformed challenge"
                    .into(),
            ));
        }
        let (name, version) = domain_name_version(requirements, chain_id, &verifying_contract)?;

        let auth = Authorization {
            from: self.address,
            to,
            value,
            valid_after: now_secs.saturating_sub(self.clock_skew_secs),
            valid_before: now_secs.saturating_add(self.valid_for_secs),
            nonce,
        };
        let digest = eip712_digest(
            &domain_separator(&name, &version, chain_id, &verifying_contract),
            &transfer_struct_hash(&auth),
        );
        let signature = self.sign_digest(&digest);

        let envelope = json!({
            "x402Version": X402_VERSION,
            "scheme":      requirements.scheme,
            "network":     requirements.network,
            "payload": {
                "signature": hex_0x(&signature),
                "authorization": {
                    "from":        hex_0x(&auth.from),
                    "to":          hex_0x(&auth.to),
                    "value":       auth.value.to_string(),
                    "validAfter":  auth.valid_after.to_string(),
                    "validBefore": auth.valid_before.to_string(),
                    "nonce":       hex_0x(&auth.nonce),
                },
            },
        });
        Ok(BASE64.encode(envelope.to_string().as_bytes()))
    }

    /// Sign a 32-byte EIP-712 digest, returning the 65-byte `r ‖ s ‖ v`
    /// form `ecrecover` expects (`v ∈ {27, 28}`, low-S normalized).
    fn sign_digest(&self, digest: &[u8; 32]) -> [u8; 65] {
        let (sig, recid) = self
            .signing_key
            .sign_prehash_recoverable(digest)
            .expect("deterministic RFC-6979 ECDSA over a fixed 32-byte prehash is infallible");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(sig.to_bytes().as_slice());
        out[64] = 27 + recid.to_byte();
        out
    }
}

#[async_trait::async_trait]
impl Signer for EvmSigner {
    async fn build_payment(&self, requirements: &PaymentRequirements) -> Result<String> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| X402Error::Sign(format!("system clock is before the unix epoch: {e}")))?
            .as_secs();
        let mut nonce = [0u8; 32];
        rand::rng().fill_bytes(&mut nonce);
        self.build_envelope(requirements, now_secs, nonce)
    }
}

/// The EIP-3009 authorization fields, in EIP-712 `TransferWithAuthorization`
/// order.
struct Authorization {
    from: [u8; 20],
    to: [u8; 20],
    value: u128,
    valid_after: u64,
    valid_before: u64,
    nonce: [u8; 32],
}

/// The chain-local USDC deployment the signer pins per chain id.
///
/// [`EvmSigner::build_envelope`] refuses every other asset unless the
/// operator opted the exact `(chain, asset)` pair in via
/// [`EXTRA_ASSETS_ENV`]: the `asset` of a 402 challenge is
/// attacker-controlled, and an unpinned signer hands out a valid
/// EIP-3009 authorization on an arbitrary ERC-20.
fn pinned_usdc_for_chain(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        8453 => Some(USDC_BASE_MAINNET),
        84532 => Some(USDC_BASE_SEPOLIA),
        _ => None,
    }
}

/// True when `contract` is the pinned chain-local USDC for `chain_id` or
/// an operator-authorized pair. Both dimensions must match: an allowlist
/// entry never authorizes the same address on a different chain.
fn asset_permitted(chain_id: u64, contract: &[u8; 20], extra_assets: &[(u64, [u8; 20])]) -> bool {
    if pinned_usdc_for_chain(chain_id).is_some_and(|pinned| eq_address_hex(contract, pinned)) {
        return true;
    }
    extra_assets
        .iter()
        .any(|(chain, asset)| *chain == chain_id && asset == contract)
}

/// Selection-side mirror of the signer's asset pin: an EVM-network
/// requirement whose asset is neither the chain-local USDC nor
/// operator-authorized must not be picked — the signer would refuse it
/// anyway, and skipping it early keeps `NoMatch` semantics. Networks
/// this module cannot resolve (e.g. `solana:`) are out of its scope and
/// pass through to their own signer's checks.
pub(crate) fn requirement_asset_permitted(
    network: &str,
    asset: &str,
    extra_assets: &[(u64, [u8; 20])],
) -> bool {
    let Ok(chain_id) = chain_id_for_network(network) else {
        return true;
    };
    let Ok(contract) = parse_address(asset) else {
        return false;
    };
    asset_permitted(chain_id, &contract, extra_assets)
}

/// Parse an [`EXTRA_ASSETS_ENV`]-shaped allowlist. Empty input is an
/// empty allowlist; any malformed entry is an error, so an operator typo
/// cannot silently change what the signer will pay.
pub fn parse_extra_assets(raw: &str) -> Result<Vec<(u64, [u8; 20])>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        let Some((chain, address)) = entry.split_once(':') else {
            return Err(X402Error::Sign(format!(
                "{EXTRA_ASSETS_ENV}: entry {entry:?} must be <chainId>:<0xaddress>"
            )));
        };
        let chain_id: u64 = chain.trim().parse().map_err(|_| {
            X402Error::Sign(format!(
                "{EXTRA_ASSETS_ENV}: unparseable chain id in entry {entry:?}"
            ))
        })?;
        let contract = parse_address(address.trim())
            .map_err(|e| X402Error::Sign(format!("{EXTRA_ASSETS_ENV}: entry {entry:?}: {e}")))?;
        out.push((chain_id, contract));
    }
    Ok(out)
}

/// Read the operator allowlist from [`EXTRA_ASSETS_ENV`]. Absent means
/// empty — the chain-local USDC pins alone decide.
pub fn extra_assets_from_env() -> Result<Vec<(u64, [u8; 20])>> {
    match std::env::var(EXTRA_ASSETS_ENV) {
        Ok(raw) => parse_extra_assets(&raw),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(std::env::VarError::NotUnicode(_)) => Err(X402Error::Sign(format!(
            "{EXTRA_ASSETS_ENV} is set but is not valid UTF-8; an unreadable allowlist \
             must not silently become an empty one"
        ))),
    }
}

/// Resolves the EVM chain id the EIP-712 domain binds to.
///
/// Accepts the CAIP-2 `eip155:<id>` form, this codebase's `base:<id>`
/// convention, and the Coinbase network aliases (`base`, `base-sepolia`).
/// A `solana:` network — or anything unrecognised — is rejected, so an
/// `EvmSigner` placed beside a [`crate::SolanaSigner`] can never sign the
/// wrong chain's requirement.
fn chain_id_for_network(network: &str) -> Result<u64> {
    if let Some(rest) = network
        .strip_prefix("eip155:")
        .or_else(|| network.strip_prefix("base:"))
    {
        return rest.parse::<u64>().map_err(|_| {
            X402Error::Sign(format!(
                "EvmSigner: unparseable chain id in network {network:?}"
            ))
        });
    }
    match network {
        "base" | "base-mainnet" => Ok(8453),
        "base-sepolia" => Ok(84532),
        _ => Err(X402Error::Sign(format!(
            "EvmSigner cannot handle network {network:?}"
        ))),
    }
}

/// The EIP-712 domain `name`/`version` for the paying token.
///
/// Prefers the values the challenge carries in `extra`; that is the
/// authoritative source for any token. Falls back only to the domains
/// the signer can assert with certainty — the pinned Base USDC
/// deployments, each keyed to its own chain id, so an operator
/// allowlisting a pinned address on a foreign chain gets no guessed
/// domain there. Any other token with no `extra` domain fails closed
/// rather than guess.
fn domain_name_version(
    requirements: &PaymentRequirements,
    chain_id: u64,
    verifying_contract: &[u8; 20],
) -> Result<(String, String)> {
    if let Some(extra) = &requirements.extra {
        if let (Some(name), Some(version)) = (extra.name.as_deref(), extra.version.as_deref()) {
            return Ok((name.to_string(), version.to_string()));
        }
    }
    if chain_id == 8453 && eq_address_hex(verifying_contract, USDC_BASE_MAINNET) {
        // Evidenced by a live zauth 402 challenge for this contract
        // (covenant-zauth challenge.rs LIVE_DECODED, captured
        // 2026-06-03: eip155:8453 with extra {"name":"USD Coin",
        // "version":"2"}) and re-verified on-chain by
        // tests/live_evm_usdc_domain.rs.
        return Ok(("USD Coin".to_string(), "2".to_string()));
    }
    if chain_id == 84532 && eq_address_hex(verifying_contract, USDC_BASE_SEPOLIA) {
        return Ok(("USDC".to_string(), "2".to_string()));
    }
    Err(X402Error::Sign(format!(
        "EvmSigner: no EIP-712 domain for asset 0x{} — set requirement.extra.name and .version",
        hex_encode(verifying_contract)
    )))
}

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

/// A `uint256` ABI word: `value` big-endian, right-aligned in 32 bytes.
fn word_u256(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

/// An `address` ABI word: the 20 address bytes right-aligned in 32.
fn word_address(address: &[u8; 20]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address);
    word
}

fn domain_separator(
    name: &str,
    version: &str,
    chain_id: u64,
    verifying_contract: &[u8; 20],
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 5);
    buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
    buf.extend_from_slice(&keccak256(name.as_bytes()));
    buf.extend_from_slice(&keccak256(version.as_bytes()));
    buf.extend_from_slice(&word_u256(chain_id as u128));
    buf.extend_from_slice(&word_address(verifying_contract));
    keccak256(&buf)
}

fn transfer_struct_hash(auth: &Authorization) -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 7);
    buf.extend_from_slice(&keccak256(TRANSFER_WITH_AUTHORIZATION_TYPE));
    buf.extend_from_slice(&word_address(&auth.from));
    buf.extend_from_slice(&word_address(&auth.to));
    buf.extend_from_slice(&word_u256(auth.value));
    buf.extend_from_slice(&word_u256(auth.valid_after as u128));
    buf.extend_from_slice(&word_u256(auth.valid_before as u128));
    buf.extend_from_slice(&auth.nonce);
    keccak256(&buf)
}

/// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖ structHash)`.
fn eip712_digest(domain_separator: &[u8; 32], struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(2 + 32 + 32);
    buf.push(0x19);
    buf.push(0x01);
    buf.extend_from_slice(domain_separator);
    buf.extend_from_slice(struct_hash);
    keccak256(&buf)
}

fn address_from_key(signing_key: &SigningKey) -> [u8; 20] {
    let encoded = signing_key.verifying_key().to_encoded_point(false);
    let hash = keccak256(&encoded.as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_0x(bytes: &[u8]) -> String {
    format!("0x{}", hex_encode(bytes))
}

fn nibble(c: u8) -> std::result::Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("non-hex byte {c:#04x}")),
    }
}

fn parse_address(value: &str) -> std::result::Result<[u8; 20], String> {
    let body = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if body.len() != 40 {
        return Err(format!(
            "expected a 20-byte (40 hex char) address, got {} chars",
            body.len()
        ));
    }
    let mut out = [0u8; 20];
    for (i, pair) in body.as_bytes().chunks_exact(2).enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(out)
}

/// Case-insensitive equality of a 20-byte address against a `0x` hex literal.
fn eq_address_hex(address: &[u8; 20], known_0x_hex: &str) -> bool {
    known_0x_hex
        .strip_prefix("0x")
        .map(|h| h.eq_ignore_ascii_case(&hex_encode(address)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
    use serde_json::Value;

    use crate::types::PaymentExtra;

    // A fixed, non-secret test key so envelopes are reproducible. This is
    // the canonical EIP-712 example key: the secp256k1 secret keccak256("cow").
    fn cow_secret() -> [u8; 32] {
        keccak256(b"cow")
    }

    fn base_sepolia_usdc_req(amount: &str) -> PaymentRequirements {
        PaymentRequirements {
            network: "base:84532".into(),
            asset: USDC_BASE_SEPOLIA.into(),
            amount: amount.into(),
            amount_usdc: 0.01,
            pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            scheme: "exact".into(),
            extra: Some(PaymentExtra {
                fee_payer: None,
                name: Some("USDC".into()),
                version: Some("2".into()),
            }),
        }
    }

    fn decode_envelope(header: &str) -> Value {
        let bytes = BASE64.decode(header).expect("base64 decode");
        serde_json::from_slice(&bytes).expect("json decode")
    }

    fn recover(digest: &[u8; 32], signature: &[u8]) -> [u8; 20] {
        assert_eq!(signature.len(), 65, "signature must be r‖s‖v");
        let recid = RecoveryId::from_byte(signature[64] - 27).expect("recovery id");
        let sig = EcdsaSignature::from_slice(&signature[..64]).expect("signature");
        let key = VerifyingKey::recover_from_prehash(digest, &sig, recid).expect("recover");
        let encoded = key.to_encoded_point(false);
        let hash = keccak256(&encoded.as_bytes()[1..]);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash[12..]);
        addr
    }

    // --- Canonical external anchors (published constants, not self-derived) ---

    #[test]
    fn transfer_with_authorization_typehash_matches_published() {
        // EIP-3009's TRANSFER_WITH_AUTHORIZATION_TYPEHASH, as pinned in
        // Circle's FiatTokenV2 / the EIP-3009 reference. If our type string
        // drifts (field order, spacing, solidity types) this diverges.
        let expected =
            parse_bytes32("0x7c7c6cdb67a18743f49ec6fa9b35f50d52ed05cbed4cc592e13b44501c1a2267");
        assert_eq!(keccak256(TRANSFER_WITH_AUTHORIZATION_TYPE), expected);
    }

    #[test]
    fn eip712_domain_typehash_matches_canonical() {
        // keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
        // from the EIP-712 specification.
        let expected =
            parse_bytes32("0x8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f");
        assert_eq!(keccak256(EIP712_DOMAIN_TYPE), expected);
    }

    #[test]
    fn domain_separator_matches_eip712_ether_mail_vector() {
        // The canonical EIP-712 "Ether Mail" domain, whose separator is a
        // published constant. Same field set (name, version, chainId,
        // verifyingContract) as the USDC domain, so this independently pins
        // our domain encoding.
        let verifying = parse_address("0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC").unwrap();
        let sep = domain_separator("Ether Mail", "1", 1, &verifying);
        let expected =
            parse_bytes32("0xf2cee375fa42b42143804025fc449deafd50cc031ca257e0b194a650a912090f");
        assert_eq!(sep, expected);
    }

    #[test]
    fn full_digest_reproduces_ether_mail_vector() {
        // Reconstruct the EIP-712 "Ether Mail" message hash from primitives
        // and assert the full 0x1901 digest equals the spec's published
        // value. This pins domain_separator + eip712_digest end to end
        // against an external reference — a wrong keccak, word layout, or
        // 0x1901 assembly cannot pass.
        let verifying = parse_address("0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC").unwrap();
        let domain = domain_separator("Ether Mail", "1", 1, &verifying);

        let person_typehash = keccak256(b"Person(string name,address wallet)");
        let mail_typehash = keccak256(
            b"Mail(Person from,Person to,string contents)Person(string name,address wallet)",
        );

        let hash_person = |name: &str, wallet_hex: &str| {
            let wallet = parse_address(wallet_hex).unwrap();
            let mut buf = Vec::new();
            buf.extend_from_slice(&person_typehash);
            buf.extend_from_slice(&keccak256(name.as_bytes()));
            buf.extend_from_slice(&word_address(&wallet));
            keccak256(&buf)
        };
        let from = hash_person("Cow", "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826");
        let to = hash_person("Bob", "0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB");

        let mut mail = Vec::new();
        mail.extend_from_slice(&mail_typehash);
        mail.extend_from_slice(&from);
        mail.extend_from_slice(&to);
        mail.extend_from_slice(&keccak256(b"Hello, Bob!"));
        let mail_hash = keccak256(&mail);

        let digest = eip712_digest(&domain, &mail_hash);
        let expected =
            parse_bytes32("0xbe609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2");
        assert_eq!(digest, expected);
    }

    #[test]
    fn cow_key_derives_canonical_ether_mail_address() {
        // The EIP-712 example key keccak256("cow") derives address
        // 0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826. Pins address_from_key.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        assert_eq!(
            signer.address_hex(),
            "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826"
        );
    }

    // --- Signer behaviour ---

    #[tokio::test]
    async fn build_payment_signature_recovers_to_payer() {
        // The facilitator authenticates by ecrecovering the signature over
        // the reconstructed TransferWithAuthorization digest and checking it
        // equals `from`. Reproduce that here from the decoded envelope.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let req = base_sepolia_usdc_req("10000");
        let header = signer
            .build_envelope(&req, 1_740_672_089, [0x11; 32])
            .unwrap();
        let env = decode_envelope(&header);

        let auth = &env["payload"]["authorization"];
        let from = parse_address(auth["from"].as_str().unwrap()).unwrap();
        let to = parse_address(auth["to"].as_str().unwrap()).unwrap();
        let value: u128 = auth["value"].as_str().unwrap().parse().unwrap();
        let valid_after: u64 = auth["validAfter"].as_str().unwrap().parse().unwrap();
        let valid_before: u64 = auth["validBefore"].as_str().unwrap().parse().unwrap();
        let nonce = parse_bytes32(auth["nonce"].as_str().unwrap());

        let recomputed = eip712_digest(
            &domain_separator(
                "USDC",
                "2",
                84532,
                &parse_address(USDC_BASE_SEPOLIA).unwrap(),
            ),
            &transfer_struct_hash(&Authorization {
                from,
                to,
                value,
                valid_after,
                valid_before,
                nonce,
            }),
        );
        let sig = parse_hex(env["payload"]["signature"].as_str().unwrap());
        assert_eq!(
            recover(&recomputed, &sig),
            signer.address(),
            "recovers to payer"
        );
        assert_eq!(from, signer.address(), "authorization.from is the payer");
        assert_eq!(value, 10000);
    }

    #[tokio::test]
    async fn envelope_matches_x402_exact_shape() {
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let req = base_sepolia_usdc_req("10000");
        let env = decode_envelope(
            &signer
                .build_envelope(&req, 1_740_672_089, [0x22; 32])
                .unwrap(),
        );

        assert_eq!(env["x402Version"], 1);
        assert_eq!(env["scheme"], "exact");
        assert_eq!(env["network"], "base:84532");
        let sig = env["payload"]["signature"].as_str().unwrap();
        assert!(
            sig.starts_with("0x") && sig.len() == 2 + 130,
            "65-byte hex sig"
        );
        let auth = &env["payload"]["authorization"];
        assert_eq!(auth["value"], "10000");
        assert_eq!(auth["validAfter"], (1_740_672_089u64 - 600).to_string());
        assert_eq!(auth["validBefore"], (1_740_672_089u64 + 120).to_string());
        let nonce = auth["nonce"].as_str().unwrap();
        assert!(
            nonce.starts_with("0x") && nonce.len() == 2 + 64,
            "32-byte hex nonce"
        );
    }

    #[tokio::test]
    async fn build_payment_draws_a_fresh_nonce_each_call() {
        // A constant nonce would make every authorization collide in the
        // token's replay map. Two live calls must differ.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let req = base_sepolia_usdc_req("10000");
        let a = decode_envelope(&signer.build_payment(&req).await.unwrap());
        let b = decode_envelope(&signer.build_payment(&req).await.unwrap());
        assert_ne!(
            a["payload"]["authorization"]["nonce"],
            b["payload"]["authorization"]["nonce"]
        );
    }

    #[tokio::test]
    async fn rejects_non_evm_network() {
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.network = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into();
        let err = signer.build_payment(&req).await.expect_err("non-evm");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("cannot handle network")));
    }

    #[tokio::test]
    async fn rejects_malformed_fields() {
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();

        let mut req = base_sepolia_usdc_req("10000");
        req.pay_to = "0xnothex".into();
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad pay_to"),
            X402Error::Sign(m) if m.contains("parse pay_to")
        ));

        let mut req = base_sepolia_usdc_req("10000");
        req.asset = "0x1234".into();
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad asset"),
            X402Error::Sign(m) if m.contains("parse asset")
        ));

        let req = base_sepolia_usdc_req("not-a-number");
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("bad amount"),
            X402Error::Sign(m) if m.contains("parse amount")
        ));

        let req = base_sepolia_usdc_req("0");
        assert!(matches!(
            signer.build_payment(&req).await.expect_err("zero amount"),
            X402Error::Sign(m) if m.contains("must be positive")
        ));
    }

    #[tokio::test]
    async fn domain_falls_back_for_base_sepolia_usdc_without_extra() {
        // No `extra`: the signer must still build, using the pinned Base
        // Sepolia USDC domain, and the signature must recover to the payer.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.extra = None;
        let env = decode_envelope(
            &signer
                .build_envelope(&req, 1_740_672_089, [0x33; 32])
                .unwrap(),
        );
        let auth = &env["payload"]["authorization"];
        let digest = eip712_digest(
            &domain_separator(
                "USDC",
                "2",
                84532,
                &parse_address(USDC_BASE_SEPOLIA).unwrap(),
            ),
            &transfer_struct_hash(&Authorization {
                from: signer.address(),
                to: parse_address(auth["to"].as_str().unwrap()).unwrap(),
                value: 10000,
                valid_after: auth["validAfter"].as_str().unwrap().parse().unwrap(),
                valid_before: auth["validBefore"].as_str().unwrap().parse().unwrap(),
                nonce: parse_bytes32(auth["nonce"].as_str().unwrap()),
            }),
        );
        let sig = parse_hex(env["payload"]["signature"].as_str().unwrap());
        assert_eq!(recover(&digest, &sig), signer.address());
    }

    #[tokio::test]
    async fn domain_falls_back_for_base_mainnet_usdc_without_extra() {
        // The zauth-evidenced mainnet USDC domain ("USD Coin", "2"): no
        // extra, mainnet chain id, pinned asset — must build, and the
        // signature must recover to the payer under exactly that domain.
        // Before the fallback, a mainnet challenge without extra failed.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.network = "eip155:8453".into();
        req.asset = USDC_BASE_MAINNET.into();
        req.extra = None;
        let env = decode_envelope(
            &signer
                .build_envelope(&req, 1_740_672_089, [0x44; 32])
                .unwrap(),
        );
        let auth = &env["payload"]["authorization"];
        let digest = eip712_digest(
            &domain_separator(
                "USD Coin",
                "2",
                8453,
                &parse_address(USDC_BASE_MAINNET).unwrap(),
            ),
            &transfer_struct_hash(&Authorization {
                from: signer.address(),
                to: parse_address(auth["to"].as_str().unwrap()).unwrap(),
                value: 10000,
                valid_after: auth["validAfter"].as_str().unwrap().parse().unwrap(),
                valid_before: auth["validBefore"].as_str().unwrap().parse().unwrap(),
                nonce: parse_bytes32(auth["nonce"].as_str().unwrap()),
            }),
        );
        let sig = parse_hex(env["payload"]["signature"].as_str().unwrap());
        assert_eq!(recover(&digest, &sig), signer.address());
    }

    // --- Asset pin ---

    /// A valid 20-byte address that is not a pinned USDC deployment.
    const ATTACKER_ASSET: &str = "0x4141414141414141414141414141414141414141";

    fn full_extra(name: &str, version: &str) -> Option<PaymentExtra> {
        Some(PaymentExtra {
            fee_payer: None,
            name: Some(name.into()),
            version: Some(version.into()),
        })
    }

    #[tokio::test]
    async fn arbitrary_asset_with_full_wire_domain_fails_closed() {
        // The pin's reason to exist: before it, a full extra
        // (name+version) sufficed to sign ANY ERC-20 a facilitator named
        // — a valid TransferWithAuthorization on an attacker token. The
        // refusal names the env var that opts a pair in.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.asset = ATTACKER_ASSET.into();
        req.extra = full_extra("Evil Token", "1");
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("unpinned asset");
        assert!(matches!(
            err,
            X402Error::Sign(m) if m.contains("not the chain-local USDC") && m.contains(EXTRA_ASSETS_ENV)
        ));
    }

    #[tokio::test]
    async fn mainnet_usdc_on_the_sepolia_chain_fails_closed() {
        // Chain and asset must agree: mainnet USDC on the Sepolia chain
        // id is a mispinned (or hostile) challenge. The pin refuses
        // before any domain resolution runs.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.asset = USDC_BASE_MAINNET.into();
        req.extra = None;
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("cross-chain asset");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("not the chain-local USDC")));
    }

    #[tokio::test]
    async fn sepolia_usdc_on_the_mainnet_chain_fails_closed() {
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.network = "eip155:8453".into();
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("cross-chain asset");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("not the chain-local USDC")));
    }

    #[tokio::test]
    async fn unpinned_chain_without_allowlist_fails_closed() {
        // chain_id_for_network resolves any eip155:<n>, but a chain with
        // no pinned USDC and no operator pair must never sign — even for
        // an address that is a real USDC deployment elsewhere.
        let signer = EvmSigner::from_secret_bytes(&cow_secret()).unwrap();
        let mut req = base_sepolia_usdc_req("10000");
        req.network = "eip155:1".into();
        req.asset = USDC_BASE_MAINNET.into();
        req.extra = full_extra("USD Coin", "2");
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("unpinned chain");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("not the chain-local USDC")));
    }

    #[tokio::test]
    async fn operator_allowlisted_asset_signs_with_the_wire_domain() {
        // The explicit opt-in path: the exact (chain, asset) pair is
        // authorized, and the signature must verify under the
        // wire-supplied domain for that asset.
        let signer = EvmSigner::from_secret_bytes(&cow_secret())
            .unwrap()
            .with_extra_assets(parse_extra_assets(&format!("84532:{ATTACKER_ASSET}")).unwrap());
        let mut req = base_sepolia_usdc_req("10000");
        req.asset = ATTACKER_ASSET.into();
        req.extra = full_extra("Wrapped Test", "1");
        let env = decode_envelope(
            &signer
                .build_envelope(&req, 1_740_672_089, [0x55; 32])
                .unwrap(),
        );
        let auth = &env["payload"]["authorization"];
        let digest = eip712_digest(
            &domain_separator(
                "Wrapped Test",
                "1",
                84532,
                &parse_address(ATTACKER_ASSET).unwrap(),
            ),
            &transfer_struct_hash(&Authorization {
                from: signer.address(),
                to: parse_address(auth["to"].as_str().unwrap()).unwrap(),
                value: 10000,
                valid_after: auth["validAfter"].as_str().unwrap().parse().unwrap(),
                valid_before: auth["validBefore"].as_str().unwrap().parse().unwrap(),
                nonce: parse_bytes32(auth["nonce"].as_str().unwrap()),
            }),
        );
        let sig = parse_hex(env["payload"]["signature"].as_str().unwrap());
        assert_eq!(recover(&digest, &sig), signer.address());
    }

    #[tokio::test]
    async fn allowlist_is_chain_scoped_on_both_dimensions() {
        // The allowlist match is (chain AND asset). Feed two near-miss
        // entries — the right asset on the wrong chain, and the right
        // chain with a different asset — so an &&->|| mutation cannot
        // pass: under ||, either near-miss alone would authorize.
        let other = "0x4242424242424242424242424242424242424242";
        let signer = EvmSigner::from_secret_bytes(&cow_secret())
            .unwrap()
            .with_extra_assets(
                parse_extra_assets(&format!("8453:{ATTACKER_ASSET},84532:{other}")).unwrap(),
            );
        let mut req = base_sepolia_usdc_req("10000");
        req.asset = ATTACKER_ASSET.into();
        req.extra = full_extra("Evil Token", "1");
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("near-miss entries");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("not the chain-local USDC")));
    }

    #[tokio::test]
    async fn allowlisted_asset_without_full_domain_fails_closed() {
        // Opt-in authorizes paying the asset; it does not conjure a
        // domain. Half a wire domain (or none) must still not sign: both
        // name and version are required before a wire-supplied domain is
        // trusted, and only the pinned USDCs have fallback domains.
        let signer = EvmSigner::from_secret_bytes(&cow_secret())
            .unwrap()
            .with_extra_assets(parse_extra_assets(&format!("84532:{ATTACKER_ASSET}")).unwrap());

        let mut req = base_sepolia_usdc_req("10000");
        req.asset = ATTACKER_ASSET.into();
        req.extra = Some(PaymentExtra {
            name: Some("Evil Token".into()),
            ..Default::default()
        });
        let err = signer.build_payment(&req).await.expect_err("half a domain");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("no EIP-712 domain")));

        let mut req = base_sepolia_usdc_req("10000");
        req.asset = ATTACKER_ASSET.into();
        req.extra = None;
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("no domain at all");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("no EIP-712 domain")));
    }

    #[tokio::test]
    async fn pinned_usdc_fallback_is_chain_keyed() {
        // Allowlisting a pinned USDC address on a foreign chain
        // authorizes paying it there; it must NOT drag the Base domain
        // fallback along — that domain was never verified for that
        // chain, so absent a wire extra the signer refuses.
        let signer = EvmSigner::from_secret_bytes(&cow_secret())
            .unwrap()
            .with_extra_assets(parse_extra_assets(&format!("1:{USDC_BASE_MAINNET}")).unwrap());
        let mut req = base_sepolia_usdc_req("10000");
        req.network = "eip155:1".into();
        req.asset = USDC_BASE_MAINNET.into();
        req.extra = None;
        let err = signer
            .build_payment(&req)
            .await
            .expect_err("foreign-chain pinned address");
        assert!(matches!(err, X402Error::Sign(m) if m.contains("no EIP-712 domain")));
    }

    #[test]
    fn usdc_pin_table_is_chain_exact() {
        assert_eq!(pinned_usdc_for_chain(8453), Some(USDC_BASE_MAINNET));
        assert_eq!(pinned_usdc_for_chain(84532), Some(USDC_BASE_SEPOLIA));
        assert_eq!(pinned_usdc_for_chain(1), None);
    }

    #[test]
    fn parse_extra_assets_accepts_the_documented_shape() {
        assert_eq!(parse_extra_assets("").unwrap(), vec![]);
        assert_eq!(parse_extra_assets("   ").unwrap(), vec![]);
        assert_eq!(
            parse_extra_assets(&format!("8453:{USDC_BASE_MAINNET}")).unwrap(),
            vec![(8453, parse_address(USDC_BASE_MAINNET).unwrap())]
        );
        let two = parse_extra_assets(&format!(
            " 8453:{ATTACKER_ASSET} , 84532:0x4242424242424242424242424242424242424242 "
        ))
        .unwrap();
        assert_eq!(two[0], (8453, parse_address(ATTACKER_ASSET).unwrap()));
        assert_eq!(two[1].0, 84532);
    }

    #[test]
    fn parse_extra_assets_rejects_malformed_entries_loudly() {
        // A typo'd allowlist is a hard error naming the env var, never a
        // silently narrowed (or widened) authorization set.
        for raw in [
            "8453",
            &format!("notachain:{ATTACKER_ASSET}") as &str,
            "8453:0x1234",
            &format!("8453:{ATTACKER_ASSET},,") as &str,
        ] {
            let err = parse_extra_assets(raw).expect_err("malformed allowlist");
            assert!(
                matches!(err, X402Error::Sign(m) if m.contains(EXTRA_ASSETS_ENV)),
                "raw {raw:?}"
            );
        }
    }

    #[test]
    fn allowlisted_address_match_is_case_insensitive() {
        // Wire challenges may checksum-case the asset; the allowlist
        // entry may not. Address equality is byte equality, not string
        // equality.
        let upper = ATTACKER_ASSET.to_uppercase().replace("0X", "0x");
        let extra = parse_extra_assets(&format!("84532:{upper}")).unwrap();
        assert!(requirement_asset_permitted(
            "base:84532",
            ATTACKER_ASSET,
            &extra
        ));
    }

    #[test]
    fn chain_id_mapping() {
        assert_eq!(chain_id_for_network("base:84532").unwrap(), 84532);
        assert_eq!(chain_id_for_network("base:8453").unwrap(), 8453);
        assert_eq!(chain_id_for_network("eip155:8453").unwrap(), 8453);
        assert_eq!(chain_id_for_network("base-sepolia").unwrap(), 84532);
        assert_eq!(chain_id_for_network("base").unwrap(), 8453);
        assert!(chain_id_for_network("solana:5eykt4Us").is_err());
        assert!(chain_id_for_network("base:notanumber").is_err());
        assert!(chain_id_for_network("ethereum").is_err());
    }

    #[test]
    fn extra_domain_overrides_and_round_trips_on_wire() {
        // extra.name/version serialize under their own keys and don't
        // disturb the feePayer field, so an EVM challenge round-trips.
        let extra = PaymentExtra {
            fee_payer: None,
            name: Some("USDC".into()),
            version: Some("2".into()),
        };
        let v = serde_json::to_value(&extra).unwrap();
        assert_eq!(v["name"], "USDC");
        assert_eq!(v["version"], "2");
        assert!(v.get("feePayer").is_none());
    }

    #[test]
    fn word_u256_is_right_aligned_big_endian() {
        assert_eq!(word_u256(1)[31], 1);
        assert_eq!(word_u256(1)[..31], [0u8; 31]);
        let max = word_u256(u128::MAX);
        assert_eq!(max[..16], [0u8; 16]);
        assert_eq!(max[16..], [0xffu8; 16]);
    }

    fn parse_hex(value: &str) -> Vec<u8> {
        let body = value.strip_prefix("0x").unwrap_or(value);
        body.as_bytes()
            .chunks_exact(2)
            .map(|p| (nibble(p[0]).unwrap() << 4) | nibble(p[1]).unwrap())
            .collect()
    }

    fn parse_bytes32(value: &str) -> [u8; 32] {
        parse_hex(value).as_slice().try_into().expect("32 bytes")
    }
}
