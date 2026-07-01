//! Turn a Covenant dual-signed audit-root VC into an EAS off-chain
//! attestation.
//!
//! [`covenant_attestation`] wraps a Covenant audit root as a Verifiable
//! Credential carrying two proofs: a canonical Ed25519 one and a
//! secp256k1/EIP-712 one an EVM contract can check. This crate takes that
//! VC and re-expresses the *same* statement as an [EAS] off-chain
//! attestation — the shape Base's trust stack (Coinbase Verifications and
//! the EAS explorer) already consumes.
//!
//! The bridge is the key, not a chain message. The secp256k1 issuer key
//! that signed the VC's EVM proof signs the EAS attestation, and
//! [`EasAttestationSigner::attest`] refuses to proceed unless the key it
//! holds is the one the VC's proof recovers to. So a single `ecrecover`
//! authenticates both artifacts, with no bridge and no gas: an off-chain
//! attestation is a signature, not a transaction.
//!
//! Two more surfaces build on the same primitives, and neither signs nor
//! submits: [`EasAttestationSigner::attest_reputation`] projects an
//! audit-derived reputation score into an off-chain attestation, and
//! [`stage_reputation_attestation`] builds the *unsigned* on-chain EAS `attest`
//! transaction that writes that score for verifiers who must enforce
//! in-contract. On-chain signing, broadcast, and mainnet stay operator-gated;
//! the library holds no key and never touches an RPC.
//!
//! [EAS]: https://attest.org

mod eip712;
mod eth;
mod relay;
mod reputation;
mod resolver;
mod uid;

use covenant_attestation::VerifiableCredential;
use covenant_identity::Secp256k1IssuerKey;
use serde_json::{json, Value};

pub use eip712::{recover_address, AttestMessage, EasDomain, DOMAIN_NAME, OFFCHAIN_VERSION};
pub use relay::{
    attest_selector, stage_reputation_attestation, RelayChain, RelayPolicy, RelayTransaction,
    ATTEST_SIGNATURE, BASE_MAINNET, BASE_SEPOLIA, EAS_ABI_SOURCE_COMMIT,
};
pub use reputation::{
    parse_reputation_projection, reputation_schema_uid, ReputationProjection, ReputationScore,
    REPUTATION_SCHEMA, SOLANA_MAINNET_CAIP2,
};
pub use resolver::{
    addr_selector, ccip_signature_digest, encode_addr_call, encode_addr_result,
    encode_resolve_call, encode_solana_addr_request, parse_addr_request, resolve_selector,
    AddrQuery, CcipResponse, ResolverGateway, ADDR_SIGNATURE, OFFCHAIN_RESOLVER_COMMIT,
    RESOLVE_SIGNATURE, SOLANA_COIN_TYPE,
};
pub use uid::{offchain_uid, schema_uid};

/// The EAS schema the Covenant audit-root attestation conforms to. Two
/// 32-byte words: the audit root the daemon anchors, and the hash of the
/// canonical credential that root came from.
pub const COVENANT_SCHEMA: &str = "bytes32 auditRoot,bytes32 credentialHash";

/// Base Sepolia's EAS deployment (a Base predeploy). Chain 84532, and the
/// EAS off-chain domain version its `version()` reports.
const BASE_SEPOLIA_CHAIN_ID: u64 = 84_532;
const BASE_SEPOLIA_EAS_VERSION: &str = "1.2.0";

#[derive(Debug, thiserror::Error)]
pub enum EvmSignerError {
    #[error("input credential did not verify: {0}")]
    Attestation(#[from] covenant_attestation::AttestationError),
    #[error("credential issuer 0x{credential} is not this signer's address 0x{signer}; the key that signs the EAS attestation must be the one the VC's EVM proof recovers to")]
    IssuerMismatch { credential: String, signer: String },
    #[error("credential exposed a malformed 32-byte field: {0}")]
    Field(String),
    #[error("secp256k1 recovery failed: {0}")]
    Signature(String),
    #[error("reputation projection is invalid: {0}")]
    Reputation(String),
    #[error("on-chain relaying to {0} is operator-gated; only Base Sepolia is autonomous")]
    MainnetGated(&'static str),
    #[error("attestation payload is {len} bytes, over the {max}-byte relay bound")]
    PayloadTooLarge { len: usize, max: usize },
    #[error("CCIP-Read gateway request is malformed: {0}")]
    MalformedRequest(String),
    #[error("this gateway resolves only Solana (coinType 501); refusing coinType {0}")]
    UnsupportedCoinType(u64),
}

impl EasDomain {
    /// The Base Sepolia EAS off-chain domain.
    pub fn base_sepolia() -> Self {
        Self {
            version: BASE_SEPOLIA_EAS_VERSION.to_string(),
            chain_id: BASE_SEPOLIA_CHAIN_ID,
            verifying_contract: eth::EAS_PREDEPLOY,
        }
    }
}

/// The schema UID a Covenant audit-root attestation references —
/// `getUID(COVENANT_SCHEMA, no resolver, revocable)`. This is the value a
/// resolver looks the schema up by.
pub fn covenant_schema_uid() -> [u8; 32] {
    schema_uid(COVENANT_SCHEMA, &eth::ZERO_ADDRESS, true)
}

/// Signs EAS off-chain attestations for one chain with one issuer key.
pub struct EasAttestationSigner {
    issuer: Secp256k1IssuerKey,
    domain: EasDomain,
}

impl EasAttestationSigner {
    pub fn new(issuer: Secp256k1IssuerKey, domain: EasDomain) -> Self {
        Self { issuer, domain }
    }

    /// A signer scoped to Base Sepolia's EAS.
    pub fn base_sepolia(issuer: Secp256k1IssuerKey) -> Self {
        Self::new(issuer, EasDomain::base_sepolia())
    }

    /// Verify `vc`, confirm it was issued by this signer's key, and emit
    /// the equivalent EAS off-chain attestation. The attestation's `time`
    /// / `expirationTime` mirror the credential's validity window, and its
    /// `data` is `abi.encode(auditRoot, credentialHash)`.
    pub fn attest(&self, vc: &VerifiableCredential) -> Result<EasOffchainAttestation, EvmSignerError> {
        let report = vc.verify()?;
        let signer = self.issuer.address();
        if report.issuer_address != signer {
            return Err(EvmSignerError::IssuerMismatch {
                credential: eth::hex_encode(&report.issuer_address),
                signer: eth::hex_encode(&signer),
            });
        }

        let audit_root = eth::hex_decode_32(&report.audit_root_hex)
            .map_err(|e| EvmSignerError::Field(format!("auditRoot: {e}")))?;
        let credential_hash = eth::hex_decode_32(&report.credential_hash_hex)
            .map_err(|e| EvmSignerError::Field(format!("credentialHash: {e}")))?;

        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&audit_root);
        data.extend_from_slice(&credential_hash);

        let message = AttestMessage {
            schema: covenant_schema_uid(),
            recipient: eth::ZERO_ADDRESS,
            time: report.valid_from_unix,
            expiration_time: report.valid_until_unix,
            revocable: true,
            ref_uid: [0u8; 32],
            data,
        };

        let digest = eip712::digest(&self.domain, &message);
        let signature = self.issuer.sign_eip712_digest(&digest);
        let uid = offchain_uid(&message);

        Ok(EasOffchainAttestation {
            domain: self.domain.clone(),
            message,
            uid,
            signature,
            signer,
        })
    }

    /// Sign an audit-derived reputation score as an EAS off-chain
    /// attestation Base can verify. Unlike [`Self::attest`], there is no
    /// input credential to match against: the score's authority *is* this
    /// signer's key, so a verifier trusts the attestation by recovering the
    /// issuer address it already knows.
    ///
    /// The attestation carries no EVM `recipient` — its identity binding is
    /// the `solana_attestation_pda` in the payload, deliberately a
    /// non-transferable Solana account rather than a sellable EVM token —
    /// and its `expirationTime` mirrors the payload's `expiry`, so the same
    /// bound is visible whether a verifier reads the EAS envelope or decodes
    /// the schema data.
    pub fn attest_reputation(
        &self,
        projection: &ReputationProjection,
    ) -> Result<EasOffchainAttestation, EvmSignerError> {
        let data = reputation::encode_data(projection)?;
        let message = AttestMessage {
            schema: reputation_schema_uid(),
            recipient: eth::ZERO_ADDRESS,
            time: projection.issued_at_unix,
            expiration_time: projection.expiry_unix,
            revocable: true,
            ref_uid: [0u8; 32],
            data,
        };

        let digest = eip712::digest(&self.domain, &message);
        let signature = self.issuer.sign_eip712_digest(&digest);
        let uid = offchain_uid(&message);

        Ok(EasOffchainAttestation {
            domain: self.domain.clone(),
            message,
            uid,
            signature,
            signer: self.issuer.address(),
        })
    }
}

/// A signed EAS off-chain attestation: everything an EAS verifier needs to
/// recompute the digest, recover the attester, and index it by UID.
#[derive(Debug, Clone)]
pub struct EasOffchainAttestation {
    pub domain: EasDomain,
    pub message: AttestMessage,
    pub uid: [u8; 32],
    /// The 65-byte `r ‖ s ‖ v` signature (`v ∈ {27, 28}`).
    pub signature: [u8; 65],
    pub signer: [u8; 20],
}

impl EasOffchainAttestation {
    /// Recover the attester from the attestation's own digest and
    /// signature — the check an EVM verifier or EAS's tooling performs.
    pub fn recover_signer(&self) -> Result<[u8; 20], EvmSignerError> {
        recover_address(&eip712::digest(&self.domain, &self.message), &self.signature)
    }

    /// The EAS off-chain attestation envelope, matching the shape the EAS
    /// SDK and explorer serialize (`{ sig: { domain, types, signature,
    /// uid, message }, signer }`).
    pub fn to_json(&self) -> Value {
        json!({
            "sig": {
                "domain": {
                    "name": DOMAIN_NAME,
                    "version": self.domain.version,
                    "chainId": self.domain.chain_id,
                    "verifyingContract": eth::hex_0x(&self.domain.verifying_contract),
                },
                "primaryType": "Attest",
                "types": { "Attest": attest_types() },
                "signature": {
                    "r": eth::hex_0x(&self.signature[..32]),
                    "s": eth::hex_0x(&self.signature[32..64]),
                    "v": self.signature[64],
                },
                "uid": eth::hex_0x(&self.uid),
                "message": {
                    "version": OFFCHAIN_VERSION,
                    "schema": eth::hex_0x(&self.message.schema),
                    "recipient": eth::hex_0x(&self.message.recipient),
                    "time": self.message.time,
                    "expirationTime": self.message.expiration_time,
                    "revocable": self.message.revocable,
                    "refUID": eth::hex_0x(&self.message.ref_uid),
                    "data": eth::hex_0x(&self.message.data),
                },
            },
            "signer": eth::hex_0x(&self.signer),
        })
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.to_json()).expect("a Value always serializes")
    }
}

fn attest_types() -> Value {
    json!([
        { "name": "version", "type": "uint16" },
        { "name": "schema", "type": "bytes32" },
        { "name": "recipient", "type": "address" },
        { "name": "time", "type": "uint64" },
        { "name": "expirationTime", "type": "uint64" },
        { "name": "revocable", "type": "bool" },
        { "name": "refUID", "type": "bytes32" },
        { "name": "data", "type": "bytes" },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_attestation::{AttestationInput, AttestationIssuer};
    use ed25519_dalek::SigningKey;

    const AUDIT_ROOT: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn input() -> AttestationInput {
        AttestationInput {
            credential_id: "urn:uuid:2c9f2d3e-0000-4000-8000-000000000001".into(),
            subject_id: "did:covenant:agent:research".into(),
            audit_root_hex: AUDIT_ROOT.into(),
            created_unix: 1_700_000_000,
            valid_from_unix: 1_700_000_000,
            valid_until_unix: 1_800_000_000,
            status: None,
        }
    }

    /// A fixed secp256k1 issuer key and a VC issued with it. The same key
    /// signs the VC's EVM proof and (later) the EAS attestation.
    fn issuer_key_and_vc() -> (Secp256k1IssuerKey, VerifiableCredential) {
        let secp = Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap();
        let att = AttestationIssuer::new(SigningKey::from_bytes(&[7u8; 32]), secp.clone());
        let vc = att.issue(&input()).unwrap();
        (secp, vc)
    }

    #[test]
    fn attest_recovers_the_issuer_address() {
        // The core contract: ecrecover over the produced attestation
        // returns the issuer's secp256k1 address.
        let (secp, vc) = issuer_key_and_vc();
        let att = EasAttestationSigner::base_sepolia(secp.clone()).attest(&vc).unwrap();

        assert_eq!(att.recover_signer().unwrap(), secp.address());
        assert_eq!(att.signer, secp.address());
    }

    #[test]
    fn attest_references_the_covenant_schema() {
        let (secp, vc) = issuer_key_and_vc();
        let att = EasAttestationSigner::base_sepolia(secp).attest(&vc).unwrap();
        assert_eq!(att.message.schema, covenant_schema_uid());
    }

    #[test]
    fn attest_data_is_audit_root_then_credential_hash() {
        let (secp, vc) = issuer_key_and_vc();
        let att = EasAttestationSigner::base_sepolia(secp).attest(&vc).unwrap();

        assert_eq!(att.message.data.len(), 64);
        assert_eq!(eth::hex_encode(&att.message.data[..32]), AUDIT_ROOT);
        // The second word is the VC's canonical hash, and the credential
        // subject's auditRoot round-trips back to what we anchored.
        let expected_hash = vc.canonical_hash_hex().unwrap();
        assert_eq!(eth::hex_encode(&att.message.data[32..]), expected_hash);
    }

    #[test]
    fn attest_mirrors_the_credential_validity_window() {
        let (secp, vc) = issuer_key_and_vc();
        let att = EasAttestationSigner::base_sepolia(secp).attest(&vc).unwrap();
        assert_eq!(att.message.time, 1_700_000_000);
        assert_eq!(att.message.expiration_time, 1_800_000_000);
    }

    #[test]
    fn attest_refuses_a_credential_from_another_key() {
        // A signer holding a different secp key than the VC's issuer must
        // refuse: otherwise the EAS attester would not be the VC issuer.
        let (_secp, vc) = issuer_key_and_vc();
        let other = Secp256k1IssuerKey::from_secret_bytes(&[11u8; 32]).unwrap();
        let err = EasAttestationSigner::base_sepolia(other).attest(&vc).unwrap_err();
        assert!(matches!(err, EvmSignerError::IssuerMismatch { .. }));
    }

    #[test]
    fn emitted_json_round_trips_through_ecrecover() {
        // The wire envelope must verify on its own terms: reparse the
        // emitted message + domain + signature, recompute the digest, and
        // recover the signer named in the JSON.
        let (secp, vc) = issuer_key_and_vc();
        let att = EasAttestationSigner::base_sepolia(secp.clone()).attest(&vc).unwrap();
        let value = att.to_json();
        let sig = &value["sig"];

        assert_eq!(sig["domain"]["name"], DOMAIN_NAME);
        assert_eq!(sig["domain"]["version"], "1.2.0");
        assert_eq!(sig["domain"]["chainId"], 84_532);
        assert_eq!(sig["primaryType"], "Attest");
        assert_eq!(sig["message"]["version"], 1);
        assert_eq!(value["signer"], eth::hex_0x(&secp.address()));

        let domain = EasDomain {
            version: sig["domain"]["version"].as_str().unwrap().to_string(),
            chain_id: sig["domain"]["chainId"].as_u64().unwrap(),
            verifying_contract: eth::hex_decode_20(sig["domain"]["verifyingContract"].as_str().unwrap()).unwrap(),
        };
        let message = AttestMessage {
            schema: eth::hex_decode_32(sig["message"]["schema"].as_str().unwrap()).unwrap(),
            recipient: eth::hex_decode_20(sig["message"]["recipient"].as_str().unwrap()).unwrap(),
            time: sig["message"]["time"].as_u64().unwrap(),
            expiration_time: sig["message"]["expirationTime"].as_u64().unwrap(),
            revocable: sig["message"]["revocable"].as_bool().unwrap(),
            ref_uid: eth::hex_decode_32(sig["message"]["refUID"].as_str().unwrap()).unwrap(),
            data: eth::hex_decode(sig["message"]["data"].as_str().unwrap()).unwrap(),
        };
        let mut signature = [0u8; 65];
        signature[..32].copy_from_slice(&eth::hex_decode_32(sig["signature"]["r"].as_str().unwrap()).unwrap());
        signature[32..64].copy_from_slice(&eth::hex_decode_32(sig["signature"]["s"].as_str().unwrap()).unwrap());
        signature[64] = sig["signature"]["v"].as_u64().unwrap() as u8;

        let digest = eip712::digest(&domain, &message);
        assert_eq!(recover_address(&digest, &signature).unwrap(), secp.address());
    }

    #[test]
    fn base_sepolia_domain_is_pinned() {
        // Guards the "defaults to Base mainnet, not Sepolia" failure mode.
        let d = EasDomain::base_sepolia();
        assert_eq!(d.chain_id, 84_532);
        assert_eq!(d.version, "1.2.0");
        assert_eq!(eth::hex_0x(&d.verifying_contract), "0x4200000000000000000000000000000000000021");
    }

    #[test]
    fn attestation_is_deterministic() {
        // Fixed key + fixed VC: RFC-6979 ECDSA makes the whole attestation
        // byte-identical, so a re-issue dedupes rather than forking.
        let (secp, vc) = issuer_key_and_vc();
        let a = EasAttestationSigner::base_sepolia(secp.clone()).attest(&vc).unwrap();
        let b = EasAttestationSigner::base_sepolia(secp).attest(&vc).unwrap();
        assert_eq!(a.to_json_string(), b.to_json_string());
        assert_eq!(a.uid, b.uid);
    }

    const REPUTATION_PDA: [u8; 32] = [0xAB; 32];

    fn reputation_projection() -> ReputationProjection {
        ReputationProjection::new(
            ReputationScore::from_ratio(95, 100, 4).unwrap(),
            SOLANA_MAINNET_CAIP2,
            REPUTATION_PDA,
            1_700_000_000,
            1_800_000_000,
        )
    }

    fn reputation_signer() -> (Secp256k1IssuerKey, EasAttestationSigner) {
        let secp = Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap();
        (secp.clone(), EasAttestationSigner::base_sepolia(secp))
    }

    #[test]
    fn reputation_attestation_recovers_the_issuer() {
        // The core contract on Base: ecrecover over the produced attestation
        // returns the issuer's secp256k1 address, no bridge involved.
        let (secp, signer) = reputation_signer();
        let att = signer.attest_reputation(&reputation_projection()).unwrap();
        assert_eq!(att.recover_signer().unwrap(), secp.address());
        assert_eq!(att.signer, secp.address());
    }

    #[test]
    fn reputation_is_bound_to_the_solana_pda_not_a_recipient() {
        // Non-transferable by construction: no EVM recipient a token could
        // ride on; the only identity in the payload is the Solana PDA.
        let (_secp, signer) = reputation_signer();
        let att = signer.attest_reputation(&reputation_projection()).unwrap();
        assert_eq!(att.message.recipient, eth::ZERO_ADDRESS);
        assert_eq!(&att.message.data[128..160], &REPUTATION_PDA);
    }

    #[test]
    fn reputation_expiry_is_mirrored_onto_the_eas_envelope() {
        // The EAS expirationTime and the schema's own expiry field must name
        // the same instant, so a stale score expires whichever surface a
        // verifier trusts.
        let (_secp, signer) = reputation_signer();
        let att = signer.attest_reputation(&reputation_projection()).unwrap();
        assert_eq!(att.message.time, 1_700_000_000);
        assert_eq!(att.message.expiration_time, 1_800_000_000);
        let data_expiry = u64::from_be_bytes(att.message.data[2 * 32 + 24..2 * 32 + 32].try_into().unwrap());
        assert_eq!(data_expiry, att.message.expiration_time);
    }

    #[test]
    fn reputation_uses_its_own_schema() {
        let (_secp, signer) = reputation_signer();
        let att = signer.attest_reputation(&reputation_projection()).unwrap();
        assert_eq!(att.message.schema, reputation_schema_uid());
        assert_ne!(att.message.schema, covenant_schema_uid());
    }

    #[test]
    fn reputation_refuses_an_unexpiring_or_unanchored_score() {
        let (_secp, signer) = reputation_signer();
        let mut stale = reputation_projection();
        stale.expiry_unix = 0;
        assert!(matches!(
            signer.attest_reputation(&stale),
            Err(EvmSignerError::Reputation(_))
        ));

        let mut unanchored = reputation_projection();
        unanchored.solana_attestation_pda = [0u8; 32];
        assert!(matches!(
            signer.attest_reputation(&unanchored),
            Err(EvmSignerError::Reputation(_))
        ));
    }

    #[test]
    fn reputation_json_round_trips_through_ecrecover() {
        // The wire envelope verifies on its own terms: reparse message +
        // domain + signature, recompute the digest, recover the named signer.
        let (secp, signer) = reputation_signer();
        let att = signer.attest_reputation(&reputation_projection()).unwrap();
        let value = att.to_json();
        let sig = &value["sig"];

        assert_eq!(sig["message"]["schema"], eth::hex_0x(&reputation_schema_uid()));
        assert_eq!(value["signer"], eth::hex_0x(&secp.address()));

        let domain = EasDomain {
            version: sig["domain"]["version"].as_str().unwrap().to_string(),
            chain_id: sig["domain"]["chainId"].as_u64().unwrap(),
            verifying_contract: eth::hex_decode_20(sig["domain"]["verifyingContract"].as_str().unwrap()).unwrap(),
        };
        let message = AttestMessage {
            schema: eth::hex_decode_32(sig["message"]["schema"].as_str().unwrap()).unwrap(),
            recipient: eth::hex_decode_20(sig["message"]["recipient"].as_str().unwrap()).unwrap(),
            time: sig["message"]["time"].as_u64().unwrap(),
            expiration_time: sig["message"]["expirationTime"].as_u64().unwrap(),
            revocable: sig["message"]["revocable"].as_bool().unwrap(),
            ref_uid: eth::hex_decode_32(sig["message"]["refUID"].as_str().unwrap()).unwrap(),
            data: eth::hex_decode(sig["message"]["data"].as_str().unwrap()).unwrap(),
        };
        let mut signature = [0u8; 65];
        signature[..32].copy_from_slice(&eth::hex_decode_32(sig["signature"]["r"].as_str().unwrap()).unwrap());
        signature[32..64].copy_from_slice(&eth::hex_decode_32(sig["signature"]["s"].as_str().unwrap()).unwrap());
        signature[64] = sig["signature"]["v"].as_u64().unwrap() as u8;

        let digest = eip712::digest(&domain, &message);
        assert_eq!(recover_address(&digest, &signature).unwrap(), secp.address());
    }

    #[test]
    fn reputation_attestation_is_deterministic() {
        let (_secp, signer) = reputation_signer();
        let a = signer.attest_reputation(&reputation_projection()).unwrap();
        let b = signer.attest_reputation(&reputation_projection()).unwrap();
        assert_eq!(a.to_json_string(), b.to_json_string());
        assert_eq!(a.uid, b.uid);
    }
}
