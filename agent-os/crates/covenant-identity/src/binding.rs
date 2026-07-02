//! Secp256k1 issuer key and the bidirectional ed25519↔secp256k1 identity
//! binding — the cross-chain keystone.
//!
//! Covenant identity is canonically ed25519 (`did:pkh:solana`), but EVM
//! chains have no cheap ed25519 verification (~2M gas, no precompile).
//! Every artifact that must be checked on an EVM chain therefore also
//! carries a secp256k1 signature, which verifies via EVM `ecrecover`
//! (~3k gas) and the Solana secp256k1 precompile. To make that second
//! key trustworthy it is bound to the canonical ed25519 identity **once,
//! bidirectionally**:
//!
//! * the ed25519 key signs an *authorize-secp256k1-address* statement, and
//! * the secp256k1 key signs an *authorize-ed25519-pubkey* statement,
//!   shaped as an EIP-712 typed-data digest so an EVM contract recovers
//!   the issuer address with a plain `ecrecover`.
//!
//! Both statements are domain-separated and anchored to the agent's
//! 32-byte audit root. [`IdentityBinding::verify`] checks **both**
//! directions; a binding that only proves one direction is rejected, so a
//! compromised secp256k1 key cannot forge Solana-canonical provenance and
//! a stolen ed25519 key cannot silently graft on a new issuer address.
//!
//! The secp256k1 key is drawn from independent random material — it is
//! never derived from the ed25519 seed, which would collapse the two-key
//! assumption. It is persisted (when persisted at all) in the same
//! hardened on-disk store as the ed25519 seed; this module introduces no
//! new key-custody mechanism.

use crate::IdentityError;
use ed25519_dalek::{
    Signature as Ed25519Signature, Signer, SigningKey as Ed25519SigningKey,
    VerifyingKey as Ed25519VerifyingKey,
};
use k256::ecdsa::{
    RecoveryId, Signature as K256Signature, SigningKey as K256SigningKey,
    VerifyingKey as K256VerifyingKey,
};
use rand::RngCore;
use sha3::{Digest, Keccak256};
use std::path::Path;
use tracing::info;

/// Domain tag for the ed25519→secp256k1 direction. Distinct from every
/// other ed25519 signing context in the codebase (capability tokens,
/// audit rows, cross-host envelopes) so a signature here can never be
/// replayed as one of those, and vice versa.
const ED25519_DIRECTION_DOMAIN: &[u8] = b"covenant/identity-binding/ed25519->secp256k1/v1";

/// EIP-712 `EIP712Domain` fields for the secp256k1→ed25519 direction. No
/// `chainId` or `verifyingContract`: the binding authorizes an identity,
/// not a chain-specific transaction, so it must verify identically on
/// Base, on the Solana precompile, and off-chain. `salt` carries the
/// domain separation instead.
const EIP712_DOMAIN_NAME: &str = "Covenant Identity Binding";
const EIP712_DOMAIN_VERSION: &str = "1";
const EIP712_DOMAIN_SALT_PREIMAGE: &[u8] = b"covenant/identity-binding/secp256k1<-ed25519/v1";

/// EIP-712 struct type for the authorize-ed25519-pubkey statement.
const EIP712_BINDING_TYPE: &[u8] =
    b"IdentityBinding(bytes32 ed25519Pubkey,address secp256k1Issuer,bytes32 auditRoot)";

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

/// The 20-byte EVM address of a secp256k1 public key:
/// `keccak256(uncompressed_pubkey[1..])[12..]`.
fn eth_address(vk: &K256VerifyingKey) -> [u8; 20] {
    let encoded = vk.to_encoded_point(false);
    let bytes = encoded.as_bytes();
    debug_assert_eq!(bytes.len(), 65, "uncompressed SEC1 point is 0x04 || X || Y");
    let hash = keccak256(&bytes[1..]);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

/// The exact bytes the ed25519 key signs to authorize a secp256k1 issuer
/// address. Every field is fixed-length and follows a fixed domain tag,
/// so the concatenation is unambiguous.
fn ed25519_direction_message(
    ed25519_pubkey: &[u8; 32],
    secp256k1_address: &[u8; 20],
    audit_root: &[u8; 32],
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(ED25519_DIRECTION_DOMAIN.len() + 32 + 20 + 32);
    msg.extend_from_slice(ED25519_DIRECTION_DOMAIN);
    msg.extend_from_slice(ed25519_pubkey);
    msg.extend_from_slice(secp256k1_address);
    msg.extend_from_slice(audit_root);
    msg
}

fn eip712_domain_separator() -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 4);
    buf.extend_from_slice(&keccak256(
        b"EIP712Domain(string name,string version,bytes32 salt)",
    ));
    buf.extend_from_slice(&keccak256(EIP712_DOMAIN_NAME.as_bytes()));
    buf.extend_from_slice(&keccak256(EIP712_DOMAIN_VERSION.as_bytes()));
    buf.extend_from_slice(&keccak256(EIP712_DOMAIN_SALT_PREIMAGE));
    keccak256(&buf)
}

fn eip712_struct_hash(
    ed25519_pubkey: &[u8; 32],
    secp256k1_address: &[u8; 20],
    audit_root: &[u8; 32],
) -> [u8; 32] {
    // `address` encodes as a left-padded 32-byte word.
    let mut addr_word = [0u8; 32];
    addr_word[12..].copy_from_slice(secp256k1_address);
    let mut buf = Vec::with_capacity(32 * 4);
    buf.extend_from_slice(&keccak256(EIP712_BINDING_TYPE));
    buf.extend_from_slice(ed25519_pubkey);
    buf.extend_from_slice(&addr_word);
    buf.extend_from_slice(audit_root);
    keccak256(&buf)
}

/// The EIP-712 digest (`keccak256(0x19 0x01 ‖ domainSeparator ‖
/// structHash)`) the secp256k1 key signs, and that an EVM contract
/// reconstructs before calling `ecrecover`.
fn eip712_digest(
    ed25519_pubkey: &[u8; 32],
    secp256k1_address: &[u8; 20],
    audit_root: &[u8; 32],
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(2 + 32 + 32);
    buf.push(0x19);
    buf.push(0x01);
    buf.extend_from_slice(&eip712_domain_separator());
    buf.extend_from_slice(&eip712_struct_hash(
        ed25519_pubkey,
        secp256k1_address,
        audit_root,
    ));
    keccak256(&buf)
}

/// A secp256k1 issuer key held alongside the canonical ed25519 identity.
///
/// The secret material is independent of the ed25519 seed. The public
/// identity is the 20-byte EVM address; signatures are produced over
/// pre-hashed 32-byte digests (deterministic RFC-6979 ECDSA) in the
/// 65-byte `r ‖ s ‖ v` form EVM `ecrecover` expects, with `v ∈ {27, 28}`.
#[derive(Clone)]
pub struct Secp256k1IssuerKey {
    signing_key: K256SigningKey,
}

impl std::fmt::Debug for Secp256k1IssuerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the secret scalar.
        f.debug_struct("Secp256k1IssuerKey")
            .field("address", &hex20(&self.address()))
            .finish()
    }
}

impl Secp256k1IssuerKey {
    /// Generate a fresh issuer key from independent random material,
    /// rejection-sampling the vanishingly rare invalid scalar (zero or
    /// ≥ curve order). Uses the same RNG path as the ed25519 seed but
    /// never the same bytes.
    pub fn generate() -> Self {
        loop {
            let mut scalar = [0u8; 32];
            rand::rng().fill_bytes(&mut scalar);
            if let Ok(signing_key) = K256SigningKey::from_slice(&scalar) {
                return Self { signing_key };
            }
        }
    }

    /// Reconstruct from a persisted 32-byte big-endian secret scalar.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, IdentityError> {
        let signing_key = K256SigningKey::from_slice(bytes)
            .map_err(|e| IdentityError::Secp256k1(e.to_string()))?;
        Ok(Self { signing_key })
    }

    /// The 32-byte big-endian secret scalar, for persistence through the
    /// hardened identity store. Handle like the ed25519 seed.
    pub fn secret_bytes(&self) -> [u8; 32] {
        let fb = self.signing_key.to_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(fb.as_slice());
        out
    }

    /// The 20-byte EVM issuer address.
    pub fn address(&self) -> [u8; 20] {
        eth_address(self.signing_key.verifying_key())
    }

    /// The uncompressed SEC1 public key (`0x04 ‖ X ‖ Y`, 65 bytes).
    pub fn verifying_key_uncompressed(&self) -> [u8; 65] {
        let encoded = self.signing_key.verifying_key().to_encoded_point(false);
        let mut out = [0u8; 65];
        out.copy_from_slice(encoded.as_bytes());
        out
    }

    /// Sign a pre-hashed 32-byte digest, returning the EVM/`ecrecover`
    /// 65-byte `r ‖ s ‖ v` form (`v ∈ {27, 28}`, low-S normalized).
    ///
    /// Reused by the higher multichain layers to dual-sign VC/EAS
    /// attestations, not only by the binding.
    pub fn sign_eip712_digest(&self, digest: &[u8; 32]) -> [u8; 65] {
        let (sig, recid) = self
            .signing_key
            .sign_prehash_recoverable(digest)
            .expect("deterministic RFC-6979 ECDSA over a fixed 32-byte prehash is infallible");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(sig.to_bytes().as_slice());
        out[64] = 27 + recid.to_byte();
        out
    }

    /// Load a persisted issuer key, or generate and persist one, using the
    /// exact hardened store as the ed25519 seed: symlink rejection,
    /// `0o600` file / `0o700` parent, and in-place mode repair.
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            crate::set_dir_mode_0700(parent)?;
        }
        if path.exists() {
            crate::require_identity_key_secure(path)?;
            let bytes = std::fs::read(path)?;
            if bytes.len() != 32 {
                return Err(IdentityError::BadSize {
                    path: path.to_path_buf(),
                    got: bytes.len(),
                });
            }
            let mut scalar = [0u8; 32];
            scalar.copy_from_slice(&bytes);
            let key = Self::from_secret_bytes(&scalar)?;
            info!(path = %path.display(), "secp256k1 issuer key loaded");
            return Ok(key);
        }
        let key = Self::generate();
        crate::write_with_mode_0600(path, &key.secret_bytes())?;
        info!(path = %path.display(), "secp256k1 issuer key created");
        Ok(key)
    }
}

/// A failure to verify one leg of an [`IdentityBinding`]. The variant
/// names the exact check that failed so a security reviewer or operator
/// can tell a stale/forged binding from a malformed one.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("bound ed25519 pubkey is not a valid curve point")]
    Ed25519Pubkey,
    #[error("ed25519 authorize-secp256k1 signature failed to verify")]
    Ed25519Direction,
    #[error("secp256k1 signature has recovery byte {0} (expected 27 or 28)")]
    BadRecoveryByte(u8),
    #[error("secp256k1 signature is malformed or non-canonical (high-S)")]
    MalformedSecp256k1Signature,
    #[error("secp256k1 public-key recovery from the EIP-712 digest failed")]
    Secp256k1Recover,
    #[error("recovered secp256k1 issuer {recovered} does not match the bound address {expected}")]
    AddressMismatch { expected: String, recovered: String },
}

/// A bidirectional cryptographic link between a canonical ed25519 identity
/// and a secp256k1 issuer key, anchored to the agent's audit root.
///
/// Construct with [`IdentityBinding::create`] (or
/// [`crate::LocalIdentity::bind_issuer`]); check with
/// [`IdentityBinding::verify`], which enforces **both** directions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityBinding {
    ed25519_pubkey: [u8; 32],
    secp256k1_address: [u8; 20],
    audit_root: [u8; 32],
    ed25519_sig: [u8; 64],
    secp256k1_sig: [u8; 65],
}

impl IdentityBinding {
    /// Produce a fresh bidirectional binding: the ed25519 key signs the
    /// authorize-secp256k1-address statement and the secp256k1 key signs
    /// the EIP-712 authorize-ed25519-pubkey digest, both anchored to
    /// `audit_root`.
    pub fn create(
        ed25519: &Ed25519SigningKey,
        secp256k1: &Secp256k1IssuerKey,
        audit_root: [u8; 32],
    ) -> Self {
        let ed25519_pubkey = ed25519.verifying_key().to_bytes();
        let secp256k1_address = secp256k1.address();

        let d1 = ed25519_direction_message(&ed25519_pubkey, &secp256k1_address, &audit_root);
        let ed25519_sig = ed25519.sign(&d1).to_bytes();

        let digest = eip712_digest(&ed25519_pubkey, &secp256k1_address, &audit_root);
        let secp256k1_sig = secp256k1.sign_eip712_digest(&digest);

        Self {
            ed25519_pubkey,
            secp256k1_address,
            audit_root,
            ed25519_sig,
            secp256k1_sig,
        }
    }

    /// Reconstruct a binding from its wire fields (e.g. after
    /// deserialization) for verification.
    pub fn from_parts(
        ed25519_pubkey: [u8; 32],
        secp256k1_address: [u8; 20],
        audit_root: [u8; 32],
        ed25519_sig: [u8; 64],
        secp256k1_sig: [u8; 65],
    ) -> Self {
        Self {
            ed25519_pubkey,
            secp256k1_address,
            audit_root,
            ed25519_sig,
            secp256k1_sig,
        }
    }

    pub fn ed25519_pubkey(&self) -> [u8; 32] {
        self.ed25519_pubkey
    }

    pub fn secp256k1_address(&self) -> [u8; 20] {
        self.secp256k1_address
    }

    pub fn audit_root(&self) -> [u8; 32] {
        self.audit_root
    }

    pub fn ed25519_signature(&self) -> [u8; 64] {
        self.ed25519_sig
    }

    pub fn secp256k1_signature(&self) -> [u8; 65] {
        self.secp256k1_sig
    }

    /// Verify **both** directions of the binding. Returns `Ok(())` only if
    /// the ed25519 key provably authorized this secp256k1 address *and*
    /// the secp256k1 key provably authorized this ed25519 pubkey, each
    /// anchored to `audit_root`. A one-directional binding fails.
    pub fn verify(&self) -> Result<(), BindingError> {
        // Direction 1 — ed25519 authorizes the secp256k1 address.
        let ed_vk = Ed25519VerifyingKey::from_bytes(&self.ed25519_pubkey)
            .map_err(|_| BindingError::Ed25519Pubkey)?;
        let d1 = ed25519_direction_message(
            &self.ed25519_pubkey,
            &self.secp256k1_address,
            &self.audit_root,
        );
        let ed_sig = Ed25519Signature::from_bytes(&self.ed25519_sig);
        ed_vk
            .verify_strict(&d1, &ed_sig)
            .map_err(|_| BindingError::Ed25519Direction)?;

        // Direction 2 — secp256k1 authorizes the ed25519 pubkey (EVM ecrecover).
        let digest = eip712_digest(
            &self.ed25519_pubkey,
            &self.secp256k1_address,
            &self.audit_root,
        );
        let recovered = recover_eth_address(&digest, &self.secp256k1_sig)?;
        if recovered != self.secp256k1_address {
            return Err(BindingError::AddressMismatch {
                expected: hex20(&self.secp256k1_address),
                recovered: hex20(&recovered),
            });
        }
        Ok(())
    }
}

/// Recover the 20-byte EVM address that produced `sig65` over `digest`,
/// enforcing the EVM `v ∈ {27, 28}` recovery byte and rejecting the
/// malleable high-S twin of a signature (we only ever emit low-S).
fn recover_eth_address(digest: &[u8; 32], sig65: &[u8; 65]) -> Result<[u8; 20], BindingError> {
    let v = sig65[64];
    let recid_byte = v.checked_sub(27).ok_or(BindingError::BadRecoveryByte(v))?;
    let recid = RecoveryId::from_byte(recid_byte).ok_or(BindingError::BadRecoveryByte(v))?;
    let sig = K256Signature::from_slice(&sig65[..64])
        .map_err(|_| BindingError::MalformedSecp256k1Signature)?;
    if sig.normalize_s().is_some() {
        return Err(BindingError::MalformedSecp256k1Signature);
    }
    let vk = K256VerifyingKey::recover_from_prehash(digest, &sig, recid)
        .map_err(|_| BindingError::Secp256k1Recover)?;
    Ok(eth_address(&vk))
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex20(bytes: &[u8; 20]) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn ed25519_key(seed: u8) -> Ed25519SigningKey {
        Ed25519SigningKey::from_bytes(&[seed; 32])
    }

    fn parse_hex20(s: &str) -> [u8; 20] {
        let s = s.strip_prefix("0x").unwrap_or(s);
        let mut out = [0u8; 20];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    // secp256k1 group order N, big-endian, for building the high-S twin.
    const SECP256K1_N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];

    fn be_sub(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut borrow: i16 = 0;
        for i in (0..32).rev() {
            let mut d = a[i] as i16 - b[i] as i16 - borrow;
            if d < 0 {
                d += 256;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out[i] = d as u8;
        }
        out
    }

    /// The malleable ECDSA twin: same r, s' = N - s, flipped recovery bit.
    fn high_s_twin(sig65: &[u8; 65]) -> [u8; 65] {
        let mut s = [0u8; 32];
        s.copy_from_slice(&sig65[32..64]);
        let s_high = be_sub(&SECP256K1_N, &s);
        let mut out = *sig65;
        out[32..64].copy_from_slice(&s_high);
        out[64] = if sig65[64] == 27 { 28 } else { 27 };
        out
    }

    #[test]
    fn bind_issuer_verifies_both_directions() {
        let ed = ed25519_key(1);
        let secp = Secp256k1IssuerKey::generate();
        let root = [7u8; 32];
        let binding = IdentityBinding::create(&ed, &secp, root);

        assert_eq!(binding.ed25519_pubkey(), ed.verifying_key().to_bytes());
        assert_eq!(binding.secp256k1_address(), secp.address());
        assert_eq!(binding.audit_root(), root);
        binding
            .verify()
            .expect("a freshly created binding must verify both directions");
    }

    #[test]
    fn known_answer_privkey_one_yields_canonical_eth_address() {
        // privkey = 1 → pubkey = G → the canonical "address of one".
        // Anchors the whole address pipeline (uncompressed SEC1 →
        // keccak256 → low-20-bytes); a refactor that hashed the
        // compressed point, kept the 0x04 prefix, or took the high 20
        // bytes would diverge from this fixed vector.
        let mut one = [0u8; 32];
        one[31] = 1;
        let key = Secp256k1IssuerKey::from_secret_bytes(&one).unwrap();
        assert_eq!(
            key.address(),
            parse_hex20("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"),
        );
    }

    #[test]
    fn sign_eip712_digest_recovers_to_signer_with_evm_recovery_byte() {
        // Pins the secp256k1 sign→recover→address pipeline independent of
        // the binding: a signature over a digest must recover the signer's
        // own address, and the 65-byte form must carry an EVM-canonical
        // v ∈ {27, 28}. This is the "byte-compatible with EVM ecrecover"
        // guarantee (expectedFailureMode 4).
        let key = Secp256k1IssuerKey::generate();
        let digest = keccak256(b"covenant identity binding recovery pin");
        let sig = key.sign_eip712_digest(&digest);
        assert!(
            sig[64] == 27 || sig[64] == 28,
            "recovery byte must be EVM-canonical 27/28, got {}",
            sig[64],
        );
        let recovered = recover_eth_address(&digest, &sig).unwrap();
        assert_eq!(recovered, key.address());
    }

    #[test]
    fn verify_rejects_missing_secp256k1_direction() {
        // A one-directional binding — valid ed25519 leg, absent secp256k1
        // leg (all-zero signature) — must be rejected. Directly covers
        // expectedFailureMode 1 (binding verifies in only one direction).
        let ed = ed25519_key(2);
        let secp = Secp256k1IssuerKey::generate();
        let root = [3u8; 32];
        let good = IdentityBinding::create(&ed, &secp, root);

        let one_directional = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            good.audit_root(),
            good.ed25519_signature(),
            [0u8; 65],
        );
        assert!(
            one_directional.verify().is_err(),
            "a binding missing the secp256k1 direction must not verify",
        );
    }

    #[test]
    fn verify_rejects_secp256k1_signature_from_a_different_key() {
        // The secp256k1 leg must be signed by the *bound* address, not just
        // by *some* valid key. Swap in a signature from a different issuer
        // over the same (bound-address) digest: it recovers to the wrong
        // address → AddressMismatch. Without the recovered == bound check,
        // a compromised secp256k1 key could forge provenance
        // (expectedFailureMode 1).
        let ed = ed25519_key(4);
        let secp = Secp256k1IssuerKey::generate();
        let attacker = Secp256k1IssuerKey::generate();
        let root = [5u8; 32];
        let good = IdentityBinding::create(&ed, &secp, root);

        // Attacker signs the exact digest for the *honest* bound address.
        let digest = eip712_digest(&good.ed25519_pubkey(), &good.secp256k1_address(), &root);
        let forged_sig = attacker.sign_eip712_digest(&digest);

        let forged = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            root,
            good.ed25519_signature(),
            forged_sig,
        );
        assert!(
            matches!(forged.verify(), Err(BindingError::AddressMismatch { .. })),
            "a secp256k1 signature that recovers to a different address must fail",
        );
    }

    #[test]
    fn verify_rejects_tampered_ed25519_direction() {
        let ed = ed25519_key(6);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [8u8; 32]);

        let mut sig = good.ed25519_signature();
        sig[0] ^= 0x01;
        let tampered = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            good.audit_root(),
            sig,
            good.secp256k1_signature(),
        );
        assert!(
            matches!(tampered.verify(), Err(BindingError::Ed25519Direction)),
            "a tampered ed25519 signature must fail the ed25519 direction",
        );
    }

    #[test]
    fn verify_rejects_a_different_audit_root() {
        // Both legs are anchored to the audit root; presenting the same
        // signatures under a different root must fail. Covers the
        // missing-anchor / replay failure mode (expectedFailureMode 3):
        // a binding is only valid for the root it was signed over.
        let ed = ed25519_key(9);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [10u8; 32]);

        let rebased = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            [11u8; 32],
            good.ed25519_signature(),
            good.secp256k1_signature(),
        );
        assert!(
            rebased.verify().is_err(),
            "a binding presented under a different audit root must not verify",
        );
    }

    #[test]
    fn verify_rejects_swapped_ed25519_pubkey() {
        let ed = ed25519_key(12);
        let other = ed25519_key(13);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [14u8; 32]);

        let swapped = IdentityBinding::from_parts(
            other.verifying_key().to_bytes(),
            good.secp256k1_address(),
            good.audit_root(),
            good.ed25519_signature(),
            good.secp256k1_signature(),
        );
        assert!(
            matches!(swapped.verify(), Err(BindingError::Ed25519Direction)),
            "swapping the bound ed25519 pubkey must fail the ed25519 direction",
        );
    }

    #[test]
    fn verify_rejects_high_s_malleable_secp256k1_signature() {
        // We only ever emit low-S signatures; the malleable high-S twin
        // (same r, s' = N - s, flipped v) recovers to the same address but
        // is a distinct byte encoding. Rejecting it keeps a binding's bytes
        // canonical so the downstream "stable hash for anchoring" cannot be
        // duplicated. The error must be MalformedSecp256k1Signature (the
        // low-S guard), not AddressMismatch, proving the guard is what
        // fired.
        let ed = ed25519_key(15);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [16u8; 32]);
        good.verify().expect("baseline low-S binding must verify");

        let twin = high_s_twin(&good.secp256k1_signature());
        let malleable = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            good.audit_root(),
            good.ed25519_signature(),
            twin,
        );
        assert!(
            matches!(
                malleable.verify(),
                Err(BindingError::MalformedSecp256k1Signature)
            ),
            "the high-S malleable twin must be rejected as non-canonical",
        );
    }

    #[test]
    fn verify_rejects_bad_recovery_byte() {
        let ed = ed25519_key(17);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [18u8; 32]);

        let mut sig = good.secp256k1_signature();
        sig[64] = 26; // below the EVM 27/28 range
        let bad = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            good.audit_root(),
            good.ed25519_signature(),
            sig,
        );
        assert!(
            matches!(bad.verify(), Err(BindingError::BadRecoveryByte(26))),
            "a recovery byte outside 27/28 must be rejected",
        );
    }

    #[test]
    fn verify_rejects_recovery_byte_above_the_recid_range() {
        // The sibling v=26 test exercises the checked_sub(27) underflow arm
        // of recover_eth_address; this pins the second arm: v >= 31 survives
        // the subtraction (recid_byte >= 4) and must be rejected by
        // RecoveryId::from_byte rather than fall through to point recovery.
        // v=31 is the boundary (first recid byte past 3), v=255 the extreme.
        let ed = ed25519_key(21);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [22u8; 32]);

        for v in [31u8, 255] {
            let mut sig = good.secp256k1_signature();
            sig[64] = v;
            let bad = IdentityBinding::from_parts(
                good.ed25519_pubkey(),
                good.secp256k1_address(),
                good.audit_root(),
                good.ed25519_signature(),
                sig,
            );
            assert!(
                matches!(bad.verify(), Err(BindingError::BadRecoveryByte(b)) if b == v),
                "recovery byte {v} maps past the RecoveryId range and must be rejected",
            );
        }
    }

    #[test]
    fn from_parts_roundtrips_the_wire_fields() {
        let ed = ed25519_key(19);
        let secp = Secp256k1IssuerKey::generate();
        let good = IdentityBinding::create(&ed, &secp, [20u8; 32]);

        let rebuilt = IdentityBinding::from_parts(
            good.ed25519_pubkey(),
            good.secp256k1_address(),
            good.audit_root(),
            good.ed25519_signature(),
            good.secp256k1_signature(),
        );
        assert_eq!(good, rebuilt);
        rebuilt
            .verify()
            .expect("a binding rebuilt from its parts must still verify");
    }

    #[test]
    fn two_generated_issuer_keys_differ() {
        // Guards expectedFailureMode 2: fresh independent material, not a
        // constant or a value derived from a shared seed.
        let a = Secp256k1IssuerKey::generate();
        let b = Secp256k1IssuerKey::generate();
        assert_ne!(a.address(), b.address());
        assert_ne!(a.secret_bytes(), b.secret_bytes());
    }

    #[test]
    fn secret_bytes_roundtrips_through_from_secret_bytes() {
        let key = Secp256k1IssuerKey::generate();
        let restored = Secp256k1IssuerKey::from_secret_bytes(&key.secret_bytes()).unwrap();
        assert_eq!(key.address(), restored.address());
        assert_eq!(
            key.verifying_key_uncompressed(),
            restored.verifying_key_uncompressed()
        );
    }

    #[test]
    fn from_secret_bytes_rejects_the_zero_scalar() {
        assert!(matches!(
            Secp256k1IssuerKey::from_secret_bytes(&[0u8; 32]),
            Err(IdentityError::Secp256k1(_))
        ));
    }

    #[test]
    fn issuer_key_load_or_create_persists_and_reloads_same_address() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity").join("issuer-secp256k1.key");
        let first = Secp256k1IssuerKey::load_or_create(&path).unwrap();
        let addr = first.address();
        let second = Secp256k1IssuerKey::load_or_create(&path).unwrap();
        assert_eq!(addr, second.address());

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 32, "secret scalar persists as 32 bytes");
    }

    #[cfg(unix)]
    #[test]
    fn issuer_key_load_or_create_writes_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity").join("issuer-secp256k1.key");
        let _ = Secp256k1IssuerKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the issuer secret must reuse the hardened 0o600 store, got {:#o}",
            mode,
        );
    }
}
