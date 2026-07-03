//! A CCIP-Read (EIP-3668) gateway that resolves an ENS name to the
//! Solana-canonical agent identity.
//!
//! An ENS `OffchainResolver` cannot hold the Solana address on L1 cheaply, so
//! it reverts [`OffchainLookup`] and defers to a gateway URL. The gateway reads
//! the address out of the Solana MPL Agent registry / SAS, signs it, and the
//! resolver's `resolveWithProof` callback recovers the signer and checks it
//! against an on-chain allowlist. This module is that gateway's signing core:
//! given the resolver's request and the resolved Solana address, it produces the
//! signed, ABI-encoded response the callback verifies.
//!
//! Two boundaries are load-bearing, and both are closed by construction:
//!
//! - **The gateway signs one thing.** It only answers `addr(node, 501)` — the
//!   ENSIP-9 Solana record. [`ResolverGateway::resolve_solana`] parses the
//!   request and refuses any other selector or coin type, so an authorized
//!   signer key cannot be steered into signing an arbitrary `(request, result)`
//!   binding for some other record.
//! - **Autonomous ends at the signature.** Building and signing a gateway
//!   response is local and keyless-of-consequence (a testnet signer). The ENS
//!   name, the deployed resolver, its signer allowlist, the DNS/gateway URL, and
//!   the production key custody are all operator-gated; none of them live here.
//!
//! The response digest is the ENS `SignatureVerifier` scheme — EIP-191 version
//! `0x00` (an intended-validator signature), *not* an EIP-712 typed-data hash:
//!
//! ```text
//! keccak256(0x1900 ‖ resolver ‖ expires_u64 ‖ keccak256(request) ‖ keccak256(result))
//! ```
//!
//! pinned from `ensdomains/offchain-resolver` at [`OFFCHAIN_RESOLVER_COMMIT`],
//! where the Solidity `SignatureVerifier.makeSignatureHash` and the TypeScript
//! gateway server independently encode the same preimage.

use serde_json::{json, Value};

use crate::eth::{self, keccak256, word_u256};
use crate::{recover_address, EvmSignerError};
use covenant_identity::Secp256k1IssuerKey;

/// SLIP-44 coin type for Solana. ENSIP-9's `addr(node, coinType)` multichain
/// record keys non-EVM chains by their SLIP-44 index; 501 is Solana.
pub const SOLANA_COIN_TYPE: u64 = 501;

/// The ENSIP-9 multichain address resolver call. The 4-byte selector is
/// *derived* from this string (see [`addr_selector`]), never hard-coded.
pub const ADDR_SIGNATURE: &str = "addr(bytes32,uint256)";

/// `IResolverService.resolve(bytes name, bytes data)` — the call an
/// `OffchainResolver` re-encodes into its `OffchainLookup` and the gateway
/// receives as the CCIP request. Its selector is derived, never hard-coded.
pub const RESOLVE_SIGNATURE: &str = "resolve(bytes,bytes)";

/// `ensdomains/offchain-resolver` commit the response scheme is pinned to. The
/// `SignatureVerifier` (Solidity) and the gateway server (TypeScript) at this
/// commit encode the same `0x1900` preimage; the pin makes a drift in either a
/// review event, not a silent break.
pub const OFFCHAIN_RESOLVER_COMMIT: &str = "099b7e9827899efcf064e71b7125f7b4fc2e342f";

/// The 4-byte selector for an ABI function signature.
fn selector(signature: &str) -> [u8; 4] {
    let digest = keccak256(signature.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

/// The `addr(bytes32,uint256)` selector, `0xf1cb7e06`.
pub fn addr_selector() -> [u8; 4] {
    selector(ADDR_SIGNATURE)
}

/// The `resolve(bytes,bytes)` selector, `0x9061b923`.
pub fn resolve_selector() -> [u8; 4] {
    selector(RESOLVE_SIGNATURE)
}

/// ABI-encode an `addr(bytes32 node, uint256 coinType)` call — the inner
/// resolver query an `OffchainResolver` wraps. `selector ‖ node ‖ coinType`.
pub fn encode_addr_call(node: &[u8; 32], coin_type: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 64);
    out.extend_from_slice(&addr_selector());
    out.extend_from_slice(node);
    out.extend_from_slice(&word_u256(coin_type));
    out
}

/// ABI-encode `resolve(bytes name, bytes data)` — the CCIP request the gateway
/// receives, wrapping the DNS-encoded `name` and the inner `data` call. Two
/// dynamic arguments: a two-word head of offsets, then each length-prefixed and
/// right-padded to a word.
pub fn encode_resolve_call(name: &[u8], data: &[u8]) -> Vec<u8> {
    let name_padded = name.len().next_multiple_of(32);
    let mut out = Vec::with_capacity(4 + 64 + 64 + name_padded + data.len().next_multiple_of(32));
    out.extend_from_slice(&resolve_selector());
    out.extend_from_slice(&word_u256(0x40)); // offset to name
    out.extend_from_slice(&word_u256(0x40 + 32 + name_padded as u64)); // offset to data
    push_bytes(&mut out, name);
    push_bytes(&mut out, data);
    out
}

/// The canonical Solana-record request: `resolve(name, addr(node, 501))`. What
/// an `OffchainResolver` for `<agent>.opencovenant.eth` sends the gateway.
pub fn encode_solana_addr_request(name: &[u8], node: &[u8; 32]) -> Vec<u8> {
    encode_resolve_call(name, &encode_addr_call(node, SOLANA_COIN_TYPE))
}

/// ABI-encode the return value of `addr(bytes32,uint256) -> bytes` for a raw
/// address: `abi.encode(bytes)` = `offset(0x20) ‖ length ‖ address padded`. A
/// Solana address is a 32-byte ed25519 public key, so this is exactly 96 bytes.
pub fn encode_addr_result(address: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + address.len().next_multiple_of(32));
    out.extend_from_slice(&word_u256(0x20));
    push_bytes(&mut out, address);
    out
}

/// The ENS `SignatureVerifier` response digest — EIP-191 version `0x00` with the
/// resolver as intended validator:
/// `keccak256(0x1900 ‖ resolver ‖ expires ‖ keccak256(request) ‖ keccak256(result))`.
///
/// Binding `resolver` stops a signature made for one resolver from being
/// replayed at another; binding `expires` bounds its lifetime; binding
/// `keccak256(request)` ties it to the exact query answered.
pub fn ccip_signature_digest(
    resolver: &[u8; 20],
    expires: u64,
    request: &[u8],
    result: &[u8],
) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(2 + 20 + 8 + 32 + 32);
    preimage.extend_from_slice(&[0x19, 0x00]);
    preimage.extend_from_slice(resolver);
    preimage.extend_from_slice(&expires.to_be_bytes());
    preimage.extend_from_slice(&keccak256(request));
    preimage.extend_from_slice(&keccak256(result));
    keccak256(&preimage)
}

/// Append a `bytes` value in ABI tail form: a length word, then the bytes
/// right-padded to a 32-byte boundary.
fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&word_u256(bytes.len() as u64));
    out.extend_from_slice(bytes);
    let padded = bytes.len().next_multiple_of(32);
    out.resize(out.len() + (padded - bytes.len()), 0);
}

/// Read a 32-byte word at `offset`, or `None` if it runs past the end. The
/// `checked_add` keeps a hostile near-`usize::MAX` offset from wrapping the
/// range computation into a panic.
fn word_at(data: &[u8], offset: usize) -> Option<[u8; 32]> {
    let end = offset.checked_add(32)?;
    data.get(offset..end)?.try_into().ok()
}

/// A 32-byte word read as a `usize` offset or length. Rejects any word whose
/// high 24 bytes are set (a value beyond `u64`); the admitted `u64` range is
/// still far larger than any real request, so every caller adds it with
/// `checked_add` and fails closed rather than trusting it to land in bounds.
fn word_as_usize(word: &[u8; 32]) -> Result<usize, EvmSignerError> {
    if word[..24].iter().any(|&b| b != 0) {
        return Err(EvmSignerError::MalformedRequest(
            "length or offset exceeds addressable range".into(),
        ));
    }
    Ok(u64::from_be_bytes(word[24..].try_into().unwrap()) as usize)
}

/// Read a length-prefixed `bytes` value whose length word sits at `head`
/// (relative to the start of the argument block), returning its payload. Every
/// index is `checked_add`'d so a hostile length or offset word errors rather
/// than overflowing into a panic.
fn read_bytes(args: &[u8], head: usize) -> Result<Vec<u8>, EvmSignerError> {
    let len = word_as_usize(
        &word_at(args, head).ok_or_else(|| malformed("bytes length word out of range"))?,
    )?;
    let start = head
        .checked_add(32)
        .ok_or_else(|| malformed("bytes offset overflows"))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| malformed("bytes length overflows"))?;
    args.get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| malformed("bytes payload out of range"))
}

fn malformed(reason: &str) -> EvmSignerError {
    EvmSignerError::MalformedRequest(reason.into())
}

/// The `(node, coinType)` an `addr` query resolves, parsed out of a
/// `resolve(name, data)` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddrQuery {
    pub name: Vec<u8>,
    pub node: [u8; 32],
    pub coin_type: u64,
}

/// Parse a CCIP `resolve(bytes name, bytes data)` request whose inner `data` is
/// an `addr(bytes32, uint256)` call. Every field is bounds-checked against
/// untrusted input; anything that is not a well-formed `resolve`-wrapping-`addr`
/// request fails closed with [`EvmSignerError::MalformedRequest`].
pub fn parse_addr_request(request: &[u8]) -> Result<AddrQuery, EvmSignerError> {
    let selector = request
        .get(..4)
        .ok_or_else(|| malformed("request too short"))?;
    if selector != resolve_selector() {
        return Err(malformed("not a resolve(bytes,bytes) call"));
    }
    let args = &request[4..];

    let name_off =
        word_as_usize(&word_at(args, 0).ok_or_else(|| malformed("missing name offset"))?)?;
    let data_off =
        word_as_usize(&word_at(args, 32).ok_or_else(|| malformed("missing data offset"))?)?;

    let name = read_bytes(args, name_off)?;
    let data = read_bytes(args, data_off)?;

    if data
        .get(..4)
        .ok_or_else(|| malformed("inner call too short"))?
        != addr_selector()
    {
        return Err(malformed("inner call is not addr(bytes32,uint256)"));
    }
    let node: [u8; 32] = data
        .get(4..36)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| malformed("missing addr node"))?;
    let coin_type = word_as_usize(
        &data
            .get(36..68)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| malformed("missing addr coinType"))?,
    )? as u64;

    Ok(AddrQuery {
        name,
        node,
        coin_type,
    })
}

/// A signed CCIP-Read gateway response: the resolved record, its expiry, and the
/// signature the resolver's `resolveWithProof` callback recovers and checks.
#[derive(Debug, Clone)]
pub struct CcipResponse {
    /// The resolver the signature is bound to (the intended validator).
    pub resolver: [u8; 20],
    /// The exact request bytes the signature commits to.
    pub request: Vec<u8>,
    /// The `(node, coinType)` the request asked for.
    pub query: AddrQuery,
    /// `abi.encode(bytes)` of the resolved address — the `addr(...)` return.
    pub result: Vec<u8>,
    /// The Unix time after which the response is stale and must be re-fetched.
    pub expires: u64,
    /// The 65-byte `r ‖ s ‖ v` signature (`v ∈ {27, 28}`) over the digest.
    pub signature: [u8; 65],
}

impl CcipResponse {
    /// The `bytes` the resolver's callback decodes as `(bytes result, uint64
    /// expires, bytes sig)`. This is the `data` field of the gateway's HTTP
    /// response.
    pub fn abi_encode(&self) -> Vec<u8> {
        let result_padded = self.result.len().next_multiple_of(32);
        let mut out = Vec::with_capacity(96 + 32 + result_padded + 32 + 96);
        out.extend_from_slice(&word_u256(0x60)); // offset to result
        out.extend_from_slice(&word_u256(self.expires));
        out.extend_from_slice(&word_u256(0x60 + 32 + result_padded as u64)); // offset to sig
        push_bytes(&mut out, &self.result);
        push_bytes(&mut out, &self.signature);
        out
    }

    /// The digest this response is signed over.
    pub fn digest(&self) -> [u8; 32] {
        ccip_signature_digest(&self.resolver, self.expires, &self.request, &self.result)
    }

    /// Recover the signer address from the digest and signature — the check the
    /// resolver's `SignatureVerifier.verify` performs before consulting its
    /// on-chain signer allowlist.
    pub fn recover_signer(&self) -> Result<[u8; 20], EvmSignerError> {
        recover_address(&self.digest(), &self.signature)
    }

    /// The gateway's HTTP response body plus the operator obligations that
    /// travel with it: the trust anchor is off-chain, so the signer must be
    /// allowlisted on the deployed resolver for any of this to be trusted.
    pub fn to_json(&self) -> Value {
        json!({
            "data": eth::hex_0x(&self.abi_encode()),
            "resolver": eth::hex_0x(&self.resolver),
            "coinType": self.query.coin_type,
            "node": eth::hex_0x(&self.query.node),
            "expires": self.expires,
            "signer": eth::hex_0x(&self.recover_signer().unwrap_or(eth::ZERO_ADDRESS)),
            "scheme": "ENS SignatureVerifier (EIP-191 v0x00)",
            "sourceCommit": OFFCHAIN_RESOLVER_COMMIT,
            "notes": self.notes(),
        })
    }

    fn notes(&self) -> Vec<String> {
        vec![
            "The signature only asserts provenance: it is trusted iff this signer is on the resolver's on-chain allowlist. Deploying the resolver and setting that allowlist is a gated operator step.".into(),
            "Bound to this resolver address; a response signed for one resolver does not verify at another.".into(),
            format!(
                "Expires at {}; the operator gateway must set a near-future TTL so a stale binding is not trusted indefinitely.",
                self.expires
            ),
            "The Solana address is authoritative on Solana (MPL Agent registry / SAS); this record is a convenience projection for EVM tooling, not a second source of truth.".into(),
        ]
    }
}

/// A CCIP-Read gateway scoped to one `OffchainResolver`, signing Solana address
/// records with its issuer key.
pub struct ResolverGateway {
    signer: Secp256k1IssuerKey,
    resolver: [u8; 20],
}

impl ResolverGateway {
    pub fn new(signer: Secp256k1IssuerKey, resolver: [u8; 20]) -> Self {
        Self { signer, resolver }
    }

    /// The gateway's signer address — the value an operator allowlists on the
    /// resolver.
    pub fn signer_address(&self) -> [u8; 20] {
        self.signer.address()
    }

    pub fn resolver_address(&self) -> [u8; 20] {
        self.resolver
    }

    /// Answer a CCIP `resolve(...)` request with the signed Solana address
    /// `solana_address`, valid until `expires`.
    ///
    /// The request is parsed and must be an `addr(node, 501)` query: any other
    /// coin type is refused with [`EvmSignerError::UnsupportedCoinType`], so this
    /// gateway's key never signs a non-Solana record. An all-zero address (the
    /// "no record" sentinel) and a zero `expires` (which never goes stale) are
    /// both refused.
    pub fn resolve_solana(
        &self,
        request: &[u8],
        solana_address: &[u8; 32],
        expires: u64,
    ) -> Result<CcipResponse, EvmSignerError> {
        let query = parse_addr_request(request)?;
        if query.coin_type != SOLANA_COIN_TYPE {
            return Err(EvmSignerError::UnsupportedCoinType(query.coin_type));
        }
        if solana_address == &[0u8; 32] {
            return Err(malformed("refusing to sign the zero Solana address"));
        }
        if expires == 0 {
            return Err(malformed("refusing to sign a response that never expires"));
        }

        let result = encode_addr_result(solana_address);
        let digest = ccip_signature_digest(&self.resolver, expires, request, &result);
        let signature = self.signer.sign_eip712_digest(&digest);

        Ok(CcipResponse {
            resolver: self.resolver,
            request: request.to_vec(),
            query,
            result,
            expires,
            signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOLVER: [u8; 20] = [0x11; 20];
    const NODE: [u8; 32] = [0x22; 32];
    const SOLANA: [u8; 32] = [0x33; 32];

    fn gateway() -> ResolverGateway {
        ResolverGateway::new(
            Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap(),
            RESOLVER,
        )
    }

    fn request() -> Vec<u8> {
        encode_solana_addr_request(b"\x05agent\x0copencovenant\x03eth\x00", &NODE)
    }

    #[test]
    fn selectors_are_pinned() {
        // Derived from the signature strings and confirmed against the
        // 4byte directory; a typo in either string moves the selector and
        // fails here rather than producing calldata no resolver answers.
        assert_eq!(eth::hex_0x(&addr_selector()), "0xf1cb7e06");
        assert_eq!(eth::hex_0x(&resolve_selector()), "0x9061b923");
    }

    #[test]
    fn keccak_matches_the_known_empty_vector() {
        // Anchors the hash the whole scheme rests on: keccak256("").
        assert_eq!(
            eth::hex_0x(&keccak256(b"")),
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    #[test]
    fn request_round_trips_through_the_parser() {
        let query = parse_addr_request(&request()).unwrap();
        assert_eq!(query.node, NODE);
        assert_eq!(query.coin_type, SOLANA_COIN_TYPE);
        assert_eq!(query.name, b"\x05agent\x0copencovenant\x03eth\x00");
    }

    #[test]
    fn addr_result_is_the_abi_bytes_encoding() {
        let result = encode_addr_result(&SOLANA);
        assert_eq!(result.len(), 96);
        assert_eq!(
            super::word_as_usize(&result[..32].try_into().unwrap()).unwrap(),
            0x20
        );
        assert_eq!(
            super::word_as_usize(&result[32..64].try_into().unwrap()).unwrap(),
            32
        );
        assert_eq!(&result[64..], &SOLANA);
    }

    #[test]
    fn digest_binds_resolver_expires_request_and_result() {
        let result = encode_addr_result(&SOLANA);
        let base = ccip_signature_digest(&RESOLVER, 1_800_000_000, &request(), &result);

        // Each bound field, changed alone, changes the digest.
        assert_ne!(
            base,
            ccip_signature_digest(&[0x99; 20], 1_800_000_000, &request(), &result)
        );
        assert_ne!(
            base,
            ccip_signature_digest(&RESOLVER, 1_800_000_001, &request(), &result)
        );
        assert_ne!(
            base,
            ccip_signature_digest(&RESOLVER, 1_800_000_000, b"other", &result)
        );
        assert_ne!(
            base,
            ccip_signature_digest(&RESOLVER, 1_800_000_000, &request(), b"other")
        );
    }

    #[test]
    fn digest_uses_the_ens_signature_verifier_preimage() {
        // Recompute the 94-byte 0x1900 preimage independently and compare, so a
        // change to the encoding order is caught, not silently absorbed.
        let result = encode_addr_result(&SOLANA);
        let expires: u64 = 1_800_000_000;
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"\x19\x00");
        preimage.extend_from_slice(&RESOLVER);
        preimage.extend_from_slice(&expires.to_be_bytes());
        preimage.extend_from_slice(&keccak256(&request()));
        preimage.extend_from_slice(&keccak256(&result));
        assert_eq!(preimage.len(), 2 + 20 + 8 + 32 + 32);
        assert_eq!(
            ccip_signature_digest(&RESOLVER, expires, &request(), &result),
            keccak256(&preimage)
        );
    }

    #[test]
    fn signed_response_recovers_to_the_gateway_signer() {
        let gw = gateway();
        let response = gw
            .resolve_solana(&request(), &SOLANA, 1_800_000_000)
            .unwrap();
        assert_eq!(response.recover_signer().unwrap(), gw.signer_address());
    }

    #[test]
    fn response_encodes_result_expires_and_signature() {
        let gw = gateway();
        let response = gw
            .resolve_solana(&request(), &SOLANA, 1_800_000_000)
            .unwrap();
        let encoded = response.abi_encode();
        // Head: offset(result)=0x60, expires, offset(sig).
        assert_eq!(
            super::word_as_usize(&encoded[..32].try_into().unwrap()).unwrap(),
            0x60
        );
        assert_eq!(
            super::word_as_usize(&encoded[32..64].try_into().unwrap()).unwrap(),
            1_800_000_000
        );
        let sig_off = super::word_as_usize(&encoded[64..96].try_into().unwrap()).unwrap();
        // result tail: length 96 then the addr encoding.
        assert_eq!(
            super::word_as_usize(&encoded[96..128].try_into().unwrap()).unwrap(),
            96
        );
        assert_eq!(&encoded[128..224], response.result.as_slice());
        // sig tail: length 65 then the signature.
        assert_eq!(
            super::word_as_usize(&encoded[sig_off..sig_off + 32].try_into().unwrap()).unwrap(),
            65
        );
        assert_eq!(
            &encoded[sig_off + 32..sig_off + 32 + 65],
            &response.signature
        );
    }

    #[test]
    fn a_non_solana_coin_type_is_refused() {
        let evm_request = encode_resolve_call(b"\x00", &encode_addr_call(&NODE, 60));
        assert!(matches!(
            gateway().resolve_solana(&evm_request, &SOLANA, 1_800_000_000),
            Err(EvmSignerError::UnsupportedCoinType(60))
        ));
    }

    #[test]
    fn a_non_addr_request_is_refused() {
        // resolve() wrapping a text(node, "key") call, not addr(...).
        let text = {
            let mut c = Vec::new();
            c.extend_from_slice(&selector("text(bytes32,string)"));
            c.extend_from_slice(&NODE);
            c
        };
        let bad = encode_resolve_call(b"\x00", &text);
        assert!(matches!(
            gateway().resolve_solana(&bad, &SOLANA, 1_800_000_000),
            Err(EvmSignerError::MalformedRequest(_))
        ));
    }

    #[test]
    fn a_truncated_request_does_not_panic() {
        for cut in 0..request().len() {
            // Every prefix either parses or errors; none panics.
            let _ = parse_addr_request(&request()[..cut]);
        }
    }

    #[test]
    fn a_hostile_offset_or_length_errors_without_panicking() {
        // An offset word whose low 8 bytes are maxed would overflow the range
        // computation if it were added unchecked.
        let mut evil_offset = Vec::new();
        evil_offset.extend_from_slice(&resolve_selector());
        evil_offset.extend_from_slice(&word_u256(u64::MAX)); // name offset
        evil_offset.extend_from_slice(&word_u256(0x40)); // data offset
        assert!(matches!(
            parse_addr_request(&evil_offset),
            Err(EvmSignerError::MalformedRequest(_))
        ));

        // A valid offset pointing at a length word whose low 8 bytes are maxed.
        let mut evil_len = Vec::new();
        evil_len.extend_from_slice(&resolve_selector());
        evil_len.extend_from_slice(&word_u256(0x40)); // name offset
        evil_len.extend_from_slice(&word_u256(0x80)); // data offset
        evil_len.extend_from_slice(&word_u256(u64::MAX)); // name length
        assert!(matches!(
            parse_addr_request(&evil_len),
            Err(EvmSignerError::MalformedRequest(_))
        ));
    }

    #[test]
    fn the_zero_address_and_zero_expiry_are_refused() {
        let gw = gateway();
        assert!(matches!(
            gw.resolve_solana(&request(), &[0u8; 32], 1_800_000_000),
            Err(EvmSignerError::MalformedRequest(_))
        ));
        assert!(matches!(
            gw.resolve_solana(&request(), &SOLANA, 0),
            Err(EvmSignerError::MalformedRequest(_))
        ));
    }

    #[test]
    fn a_response_for_one_resolver_does_not_verify_at_another() {
        // The signer recovers correctly only against the resolver it signed for;
        // recovering against a different resolver yields a different address.
        let gw = gateway();
        let response = gw
            .resolve_solana(&request(), &SOLANA, 1_800_000_000)
            .unwrap();
        let elsewhere = ccip_signature_digest(
            &[0x99; 20],
            response.expires,
            &response.request,
            &response.result,
        );
        assert_ne!(
            recover_address(&elsewhere, &response.signature).unwrap(),
            gw.signer_address()
        );
    }

    #[test]
    fn to_json_carries_the_response_and_trust_obligations() {
        let gw = gateway();
        let response = gw
            .resolve_solana(&request(), &SOLANA, 1_800_000_000)
            .unwrap();
        let v = response.to_json();
        assert_eq!(v["coinType"], SOLANA_COIN_TYPE);
        assert_eq!(v["expires"], 1_800_000_000);
        assert_eq!(v["sourceCommit"], OFFCHAIN_RESOLVER_COMMIT);
        assert_eq!(v["signer"], eth::hex_0x(&gw.signer_address()));
        let notes = v["notes"].as_array().unwrap();
        let joined = notes.iter().filter_map(Value::as_str).collect::<String>();
        assert!(joined.contains("allowlist"));
        assert!(joined.contains("resolver"));
    }
}
