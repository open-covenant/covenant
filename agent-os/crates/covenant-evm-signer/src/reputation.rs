//! Project a Covenant audit-derived reputation score into an EAS off-chain
//! attestation Base's trust stack can read.
//!
//! Covenant computes one canonical reputation score from an agent's audit
//! chain and anchors it on Solana. This module re-expresses that score as an
//! [EAS] off-chain attestation — the shape a Base verifier or the EAS
//! explorer consumes — signed by the same secp256k1 issuer key, so a single
//! `ecrecover` authenticates it with no bridge and no gas.
//!
//! The schema is modeled on [Human Passport]'s score attestation: a
//! `score`/`score_decimals` pair, so a fractional score survives as an
//! integer without a float ever touching the wire. Covenant adds the
//! provenance the trust hinges on — `source_chain` and the
//! `solana_attestation_pda` the score is anchored to. That anchor is the
//! binding: the reputation belongs to a Solana identity PDA, never to a
//! transferable EVM token. ERC-8004 identity NFTs can be sold; a score bound
//! to one could be laundered. A score bound to a non-transferable Solana PDA
//! cannot.
//!
//! ## Anchor account
//!
//! `solana_attestation_pda` is the 32-byte Solana account the score's
//! provenance traces to — the agent's audit-root attestation. Two recording
//! paths exist: the MPL Core AppData attestation asset (the live production
//! anchor; the deployed accounts are recorded in
//! `docs/metaplex-integration.md`), and the SAP attestation PDA the
//! `@covenant/sap-bridge` worker derives as `[b"sap_attest", agent,
//! attester]` under program `SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ`.
//! Both are base58 account addresses on the wire;
//! [`solana_account_bytes`] converts either into the schema's `bytes32`.
//!
//! [EAS]: https://attest.org
//! [Human Passport]: https://passport.human.tech

use serde_json::Value;

use crate::eth::{self, hex_decode_32, word_address, word_u256};
use crate::uid::schema_uid;
use crate::EvmSignerError;

/// Solana mainnet's CAIP-2 chain id — the canonical chain a Covenant
/// reputation is anchored on. The 32-character reference is the leading
/// slice of the genesis-block hash, and matches `covenant-hyre`'s
/// `SOLANA_NETWORK`, so the reputation names the same rail the rest of the
/// stack settles on.
pub const SOLANA_MAINNET_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// The EAS schema a Covenant reputation attestation conforms to. `score`
/// and `score_decimals` carry the value together — the reader recovers the
/// real number as `score / 10^score_decimals` — so the decimal scale can
/// never be lost between issuer and verifier. `expiry` is a hard bound (an
/// EAS attestation with no expiry stays trusted forever); `source_chain`
/// and `solana_attestation_pda` trace the score back to its Solana anchor.
pub const REPUTATION_SCHEMA: &str =
    "uint32 score,uint8 score_decimals,uint64 expiry,string source_chain,bytes32 solana_attestation_pda";

/// Number of head words in the ABI encoding of the reputation tuple, one
/// per schema field. The single dynamic field (`source_chain`) stores its
/// offset here and its bytes past the head.
const HEAD_WORDS: u64 = 5;

/// An audit-derived reputation score, pre-scaled to an integer: the real
/// value is `score / 10^decimals`. Keeping the scale explicit is the whole
/// point — a bare `95` is meaningless until you know whether it is `0.95`,
/// `9.5`, or `95`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReputationScore {
    pub score: u32,
    pub decimals: u8,
}

impl ReputationScore {
    pub fn new(score: u32, decimals: u8) -> Self {
        Self { score, decimals }
    }

    /// Scale a rational score `numerator / denominator` to `decimals`
    /// places, rounding half up. This is the safe way to reach a
    /// `(score, decimals)` pair: `from_ratio(95, 100, 4)` is `9500` at 4
    /// decimals, i.e. `0.95` — not `95000` and not `0.0095`. Rejects a zero
    /// denominator, a `10^decimals` that overflows `u64`, and a result past
    /// `u32`.
    pub fn from_ratio(
        numerator: u64,
        denominator: u64,
        decimals: u8,
    ) -> Result<Self, EvmSignerError> {
        if denominator == 0 {
            return Err(EvmSignerError::Reputation("denominator is zero".into()));
        }
        let scale = 10u64
            .checked_pow(decimals as u32)
            .ok_or_else(|| EvmSignerError::Reputation(format!("10^{decimals} overflows u64")))?;
        let scaled = numerator as u128 * scale as u128;
        let denom = denominator as u128;
        let rounded = scaled / denom + u128::from(scaled % denom * 2 >= denom);
        let score = u32::try_from(rounded).map_err(|_| {
            EvmSignerError::Reputation(format!("scaled score {rounded} exceeds uint32"))
        })?;
        Ok(Self { score, decimals })
    }

    /// The real value the `(score, decimals)` pair denotes. For display and
    /// test assertions; the wire never carries a float.
    pub fn as_f64(&self) -> f64 {
        self.score as f64 / 10f64.powi(self.decimals as i32)
    }
}

/// Everything a reputation projection commits to: the score, the Solana
/// anchor it is bound to, and its validity window. The `solana_attestation_pda`
/// is the 32-byte Solana account (SAS/MPL) that records the score on-chain,
/// carried as `bytes32` so the EVM side can trace provenance back to Solana.
#[derive(Debug, Clone)]
pub struct ReputationProjection {
    pub score: ReputationScore,
    /// CAIP-2 source chain, e.g. [`SOLANA_MAINNET_CAIP2`].
    pub source_chain: String,
    pub solana_attestation_pda: [u8; 32],
    pub issued_at_unix: u64,
    pub expiry_unix: u64,
}

impl ReputationProjection {
    pub fn new(
        score: ReputationScore,
        source_chain: impl Into<String>,
        solana_attestation_pda: [u8; 32],
        issued_at_unix: u64,
        expiry_unix: u64,
    ) -> Self {
        Self {
            score,
            source_chain: source_chain.into(),
            solana_attestation_pda,
            issued_at_unix,
            expiry_unix,
        }
    }

    /// Build a projection from a canonical audit-derived score
    /// ([`covenant_audit::reputation::AuditReputation`]), the Solana anchor the
    /// score is committed to, and the validity window. Refuses a score with no
    /// history: an agent that has taken no governed actions has no reputation to
    /// project, and attesting one would dress a blank record up as a number.
    pub fn from_audit(
        audit: &covenant_audit::reputation::AuditReputation,
        source_chain: impl Into<String>,
        solana_attestation_pda: [u8; 32],
        issued_at_unix: u64,
        expiry_unix: u64,
    ) -> Result<Self, EvmSignerError> {
        let scaled = audit.score.ok_or_else(|| {
            EvmSignerError::Reputation(
                "audit chain has no governed actions to score; nothing to attest".into(),
            )
        })?;
        Ok(Self::new(
            ReputationScore::new(scaled, audit.decimals),
            source_chain,
            solana_attestation_pda,
            issued_at_unix,
            expiry_unix,
        ))
    }

    /// Build a projection with the Solana anchor supplied as the base58
    /// account address Solana tooling emits — a DAS asset id or an
    /// on-chain PDA. The staging path reads the recorded live attestation
    /// account through this.
    pub fn from_pda_base58(
        score: ReputationScore,
        source_chain: impl Into<String>,
        pda_base58: &str,
        issued_at_unix: u64,
        expiry_unix: u64,
    ) -> Result<Self, EvmSignerError> {
        Ok(Self::new(
            score,
            source_chain,
            solana_account_bytes(pda_base58)?,
            issued_at_unix,
            expiry_unix,
        ))
    }

    /// Build a projection with the Solana anchor supplied as hex (`0x…`
    /// optional). The convenience the sidecar reads its stdin through.
    pub fn from_pda_hex(
        score: ReputationScore,
        source_chain: impl Into<String>,
        pda_hex: &str,
        issued_at_unix: u64,
        expiry_unix: u64,
    ) -> Result<Self, EvmSignerError> {
        let pda = hex_decode_32(pda_hex)
            .map_err(|e| EvmSignerError::Reputation(format!("solana_attestation_pda: {e}")))?;
        Ok(Self::new(
            score,
            source_chain,
            pda,
            issued_at_unix,
            expiry_unix,
        ))
    }

    /// Fail closed on the ways a projection would silently under-attest: a
    /// score that never expires, an expiry that predates issuance, a missing
    /// Solana anchor, or an empty source chain. Each maps to a documented
    /// failure mode — a stale score trusted forever, or a projection that
    /// cannot be traced back to Solana.
    fn validate(&self) -> Result<(), EvmSignerError> {
        if self.expiry_unix == 0 {
            return Err(EvmSignerError::Reputation(
                "expiry must be set: EAS treats expirationTime 0 as never-expiring".into(),
            ));
        }
        if self.expiry_unix <= self.issued_at_unix {
            return Err(EvmSignerError::Reputation(format!(
                "expiry {} must be after issued_at {}",
                self.expiry_unix, self.issued_at_unix
            )));
        }
        if self.solana_attestation_pda == [0u8; 32] {
            return Err(EvmSignerError::Reputation(
                "solana_attestation_pda is all-zero: the score must reference its Solana anchor"
                    .into(),
            ));
        }
        if self
            .solana_attestation_pda
            .iter()
            .all(|b| *b == self.solana_attestation_pda[0])
        {
            // The 0xab..ab staging placeholder and its whole class: no real
            // Solana account is 32 repeats of one byte, and only this check
            // stands between a placeholder and a mainnet attestation
            // permanently pointing the back-reference at garbage.
            return Err(EvmSignerError::Reputation(format!(
                "solana_attestation_pda is 32 repeats of 0x{:02x} — a placeholder pattern, not a real Solana account",
                self.solana_attestation_pda[0]
            )));
        }
        if self.source_chain.is_empty() {
            return Err(EvmSignerError::Reputation("source_chain is empty".into()));
        }
        // A reader recovers the real value as `score / 10^decimals`. Past 18
        // decimals that computation overflows or, under `unchecked` math,
        // wraps, so a real score could read as an absurd value. Cap well
        // inside the [0,1]/[0,100] range these scores actually use.
        if self.score.decimals > 18 {
            return Err(EvmSignerError::Reputation(format!(
                "score_decimals {} exceeds 18",
                self.score.decimals
            )));
        }
        Ok(())
    }
}

/// Decode a base58 Solana account address into the `bytes32` the schema
/// carries. Exactly 32 bytes or refusal: a truncated or overlong decode
/// silently pointing the back-reference at a wrong account is this
/// field's worst failure mode, so length is never padded or trimmed.
pub fn solana_account_bytes(base58: &str) -> Result<[u8; 32], EvmSignerError> {
    let trimmed = base58.trim();
    let decoded = bs58::decode(trimmed).into_vec().map_err(|e| {
        EvmSignerError::Reputation(format!("solana account {trimmed:?} is not base58: {e}"))
    })?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| {
        EvmSignerError::Reputation(format!(
            "solana account {trimmed:?} decodes to {} bytes, expected 32",
            decoded.len()
        ))
    })
}

/// ABI-encode the reputation tuple exactly as EAS's `SchemaEncoder` would:
/// `abi.encode(uint32, uint8, uint64, string, bytes32)`. The four static
/// fields sit in the 5-word head; `source_chain`, the one dynamic field,
/// stores its offset in the head and its length-prefixed bytes in the tail.
pub(crate) fn encode_data(projection: &ReputationProjection) -> Result<Vec<u8>, EvmSignerError> {
    projection.validate()?;
    let source = projection.source_chain.as_bytes();
    let padded_len = source.len().next_multiple_of(32);

    let mut data = Vec::with_capacity(HEAD_WORDS as usize * 32 + 32 + padded_len);
    data.extend_from_slice(&word_u256(projection.score.score as u64));
    data.extend_from_slice(&word_u256(projection.score.decimals as u64));
    data.extend_from_slice(&word_u256(projection.expiry_unix));
    data.extend_from_slice(&word_u256(HEAD_WORDS * 32));
    data.extend_from_slice(&projection.solana_attestation_pda);
    data.extend_from_slice(&word_u256(source.len() as u64));
    data.extend_from_slice(source);
    data.resize(data.len() + (padded_len - source.len()), 0);
    Ok(data)
}

/// The schema UID a reputation attestation references —
/// `getUID(REPUTATION_SCHEMA, no resolver, revocable)`.
pub fn reputation_schema_uid() -> [u8; 32] {
    schema_uid(REPUTATION_SCHEMA, &eth::ZERO_ADDRESS, true)
}

/// EAS v1.3.0's `attest` function: one `AttestationRequest` struct —
/// `(bytes32 schema, (address recipient, uint64 expirationTime, bool
/// revocable, bytes32 refUID, bytes data, uint256 value))`. The selector is
/// derived from this string, never hard-coded, so a signature typo cannot
/// survive the pinned-selector test.
pub const ATTEST_SIGNATURE: &str = "attest((bytes32,(address,uint64,bool,bytes32,bytes,uint256)))";

/// Hard bound on the encoded schema-data payload a staged relay transaction
/// may carry, mirrored into the staged artifact's `policy.maxDataBytes`. The
/// bound is enforced at build time: oversized calldata is refused here, not
/// discovered at submission.
pub const RELAY_MAX_DATA_BYTES: usize = 512;

/// The 4-byte selector of [`ATTEST_SIGNATURE`].
pub fn attest_selector() -> [u8; 4] {
    eth::keccak256(ATTEST_SIGNATURE.as_bytes())[..4]
        .try_into()
        .expect("4-byte slice")
}

/// Build the full unsigned `attest` calldata for a reputation projection —
/// every byte from the same encoder the off-chain attestation uses, so a
/// staged transaction can never diverge from what [`encode_data`] signs.
///
/// The request carries no recipient and no value; `expirationTime` mirrors
/// the projection's expiry so the bound is identical on the EAS envelope and
/// inside the schema data. Offsets are fixed by the shape: the request
/// struct at `0x20`, its `AttestationRequestData` at `0x40` past the schema
/// word, and the `bytes data` at `0xc0` past the six-word request-data head.
pub fn attest_calldata(projection: &ReputationProjection) -> Result<Vec<u8>, EvmSignerError> {
    let data = encode_data(projection)?;
    if data.len() > RELAY_MAX_DATA_BYTES {
        return Err(EvmSignerError::PayloadTooLarge {
            len: data.len(),
            max: RELAY_MAX_DATA_BYTES,
        });
    }
    let mut call = Vec::with_capacity(4 + 10 * 32 + data.len());
    call.extend_from_slice(&attest_selector());
    call.extend_from_slice(&word_u256(0x20));
    call.extend_from_slice(&reputation_schema_uid());
    call.extend_from_slice(&word_u256(0x40));
    call.extend_from_slice(&word_address(&eth::ZERO_ADDRESS));
    call.extend_from_slice(&word_u256(projection.expiry_unix));
    call.extend_from_slice(&word_u256(1));
    call.extend_from_slice(&[0u8; 32]);
    call.extend_from_slice(&word_u256(0xc0));
    call.extend_from_slice(&word_u256(0));
    call.extend_from_slice(&word_u256(data.len() as u64));
    call.extend_from_slice(&data);
    Ok(call)
}

/// Parse a reputation projection from the wire JSON the sidecar and the relay
/// staging binary both read. Accepts camelCase (the default) with snake_case
/// fallbacks; the score and its decimal scale are read together so the two can
/// never drift apart. The anchor accepts both wire spellings: exactly 64 hex
/// chars (`0x` optional) reads as hex; anything else reads as the base58
/// account address Solana tooling emits. The forms cannot collide — a 32-byte
/// account is 32–44 base58 chars, never 64.
pub fn parse_reputation_projection(v: &Value) -> Result<ReputationProjection, EvmSignerError> {
    let u64_field = |names: &[&str]| -> Result<u64, EvmSignerError> {
        names
            .iter()
            .find_map(|n| v.get(*n).and_then(Value::as_u64))
            .ok_or_else(|| {
                EvmSignerError::Reputation(format!("missing unsigned integer field '{}'", names[0]))
            })
    };
    let str_field = |names: &[&str]| -> Result<&str, EvmSignerError> {
        names
            .iter()
            .find_map(|n| v.get(*n).and_then(Value::as_str))
            .ok_or_else(|| {
                EvmSignerError::Reputation(format!("missing string field '{}'", names[0]))
            })
    };

    let score = u32::try_from(u64_field(&["score"])?)
        .map_err(|_| EvmSignerError::Reputation("'score' exceeds uint32".into()))?;
    let decimals = u8::try_from(u64_field(&["scoreDecimals", "score_decimals"])?)
        .map_err(|_| EvmSignerError::Reputation("'scoreDecimals' exceeds uint8".into()))?;

    let pda = str_field(&["solanaAttestationPda", "solana_attestation_pda"])?;
    let body = pda.strip_prefix("0x").unwrap_or(pda);
    let is_hex32 = body.len() == 64 && body.bytes().all(|b| b.is_ascii_hexdigit());

    let score = ReputationScore::new(score, decimals);
    let source_chain = str_field(&["sourceChain", "source_chain"])?.to_string();
    let issued_at = u64_field(&["issuedAt", "issued_at"])?;
    let expiry = u64_field(&["expiry"])?;
    if is_hex32 {
        ReputationProjection::from_pda_hex(score, source_chain, pda, issued_at, expiry)
    } else {
        ReputationProjection::from_pda_base58(score, source_chain, pda, issued_at, expiry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::reputation::AuditReputation;

    /// The live Base-mainnet-facing anchor: the production audit-root
    /// attestation asset (MPL Core AppData), recorded in
    /// `docs/metaplex-integration.md`.
    const LIVE_ATTESTATION_ASSET: &str = "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH";
    /// Its 32 bytes, cross-generated with an independent implementation
    /// (`@solana/web3.js` `new PublicKey(LIVE_ATTESTATION_ASSET).toBuffer()`)
    /// so the crate's base58 decode is pinned against a second decoder,
    /// not itself.
    const LIVE_ATTESTATION_ASSET_HEX: &str =
        "5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6";

    fn live_anchor() -> [u8; 32] {
        solana_account_bytes(LIVE_ATTESTATION_ASSET).expect("live anchor decodes")
    }

    #[test]
    fn solana_account_bytes_decodes_the_live_attestation_asset() {
        // The conversion this task exists for, pinned against the
        // cross-implementation vector: alphabet, endianness, and length
        // all have to agree with what Solana tooling derives.
        let bytes = live_anchor();
        assert_eq!(crate::eth::hex_encode(&bytes), LIVE_ATTESTATION_ASSET_HEX);
        assert_eq!(bs58::encode(&bytes).into_string(), LIVE_ATTESTATION_ASSET);
    }

    #[test]
    fn solana_account_bytes_is_conversion_not_validation() {
        // The system program is a real, well-known 32-zero-byte account:
        // the converter must decode it faithfully. Refusing placeholder
        // patterns is validate()'s job, at projection time.
        let zeros = solana_account_bytes("11111111111111111111111111111111").unwrap();
        assert_eq!(zeros, [0u8; 32]);
    }

    #[test]
    fn solana_account_bytes_rejects_wrong_length_and_alphabet() {
        // Too short (decodes to 2 bytes), and the base58 alphabet's
        // excluded lookalikes (0, O, I, l). The error names the length so
        // a truncated paste is diagnosable.
        assert!(matches!(
            solana_account_bytes("abc"),
            Err(EvmSignerError::Reputation(m)) if m.contains("expected 32")
        ));
        assert!(matches!(
            solana_account_bytes("0OIl"),
            Err(EvmSignerError::Reputation(m)) if m.contains("not base58")
        ));
    }

    #[test]
    fn from_audit_maps_a_real_score_and_refuses_a_blank_history() {
        let scored = AuditReputation {
            compliant: 3,
            violations: 1,
            score: Some(7_500),
            decimals: 4,
        };
        let p = ReputationProjection::from_audit(
            &scored,
            crate::reputation::SOLANA_MAINNET_CAIP2,
            live_anchor(),
            1_700_000_000,
            1_800_000_000,
        )
        .unwrap();
        assert_eq!(p.score.score, 7_500);
        assert_eq!(p.score.decimals, 4);

        let blank = AuditReputation {
            compliant: 0,
            violations: 0,
            score: None,
            decimals: 4,
        };
        assert!(matches!(
            ReputationProjection::from_audit(
                &blank,
                crate::reputation::SOLANA_MAINNET_CAIP2,
                live_anchor(),
                1_700_000_000,
                1_800_000_000,
            ),
            Err(EvmSignerError::Reputation(_))
        ));
    }

    /// Decode the reputation tuple the way a Base verifier or EAS's
    /// `SchemaDecoder` would, so the round-trip proves the encoding is
    /// consumable, not merely reproducible.
    fn decode(data: &[u8]) -> (u32, u8, u64, String, [u8; 32]) {
        // Low 8 bytes of head word `i`, where every value here fits u64.
        let low64 =
            |i: usize| u64::from_be_bytes(data[i * 32 + 24..i * 32 + 32].try_into().unwrap());
        let score = low64(0) as u32;
        let decimals = data[63]; // last byte of word 1
        let expiry = low64(2);
        let offset = low64(3) as usize;
        let mut pda = [0u8; 32];
        pda.copy_from_slice(&data[128..160]);
        let len = u64::from_be_bytes(data[offset + 24..offset + 32].try_into().unwrap()) as usize;
        let source = String::from_utf8(data[offset + 32..offset + 32 + len].to_vec()).unwrap();
        (score, decimals, expiry, source, pda)
    }

    fn projection() -> ReputationProjection {
        ReputationProjection::new(
            ReputationScore::from_ratio(95, 100, 4).unwrap(),
            SOLANA_MAINNET_CAIP2,
            live_anchor(),
            1_700_000_000,
            1_800_000_000,
        )
    }

    #[test]
    fn from_ratio_keeps_the_decimal_scale() {
        // The headline failure mode: 0.95 must read as 0.95, never 95000 or
        // 0.0095. score and decimals only mean 0.95 together.
        let quarters = ReputationScore::from_ratio(95, 100, 4).unwrap();
        assert_eq!((quarters.score, quarters.decimals), (9_500, 4));
        assert!((quarters.as_f64() - 0.95).abs() < 1e-12);

        let cents = ReputationScore::from_ratio(95, 100, 2).unwrap();
        assert_eq!((cents.score, cents.decimals), (95, 2));
        assert!((cents.as_f64() - 0.95).abs() < 1e-12);

        // A percentage in [0, 100] at whole-number precision.
        assert_eq!(ReputationScore::from_ratio(73, 1, 0).unwrap().score, 73);
    }

    #[test]
    fn from_ratio_rounds_half_up_and_guards_overflow() {
        assert_eq!(ReputationScore::from_ratio(1, 8, 2).unwrap().score, 13); // 0.125 -> 0.13
        assert_eq!(ReputationScore::from_ratio(1, 2, 0).unwrap().score, 1); // 0.5 -> 1
        assert!(matches!(
            ReputationScore::from_ratio(1, 0, 2),
            Err(EvmSignerError::Reputation(_))
        ));
        // 5 * 10^9 exceeds uint32.
        assert!(matches!(
            ReputationScore::from_ratio(5, 1, 9),
            Err(EvmSignerError::Reputation(_))
        ));
    }

    #[test]
    fn encoded_data_round_trips_through_a_verifier() {
        let data = encode_data(&projection()).unwrap();
        let (score, decimals, expiry, source, pda) = decode(&data);
        assert_eq!(score, 9_500);
        assert_eq!(decimals, 4);
        assert!((score as f64 / 10f64.powi(decimals as i32) - 0.95).abs() < 1e-12);
        assert_eq!(expiry, 1_800_000_000);
        assert_eq!(source, SOLANA_MAINNET_CAIP2);
        assert_eq!(pda, live_anchor());
    }

    #[test]
    fn dynamic_field_offset_is_the_head_size() {
        // The string offset must point exactly past the 5-word head, or a
        // decoder reads garbage for source_chain.
        let data = encode_data(&projection()).unwrap();
        let offset = u64::from_be_bytes(data[3 * 32 + 24..3 * 32 + 32].try_into().unwrap());
        assert_eq!(offset, 160);
    }

    #[test]
    fn encoding_pads_the_source_chain_to_a_word() {
        // solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp is 39 bytes -> padded to 64.
        let data = encode_data(&projection()).unwrap();
        assert_eq!(SOLANA_MAINNET_CAIP2.len(), 39);
        assert_eq!(data.len(), 5 * 32 + 32 + 64);
        assert_eq!(data.len() % 32, 0);
    }

    #[test]
    fn validate_rejects_the_under_attestation_modes() {
        let base = projection();
        let with = |mutate: &dyn Fn(&mut ReputationProjection)| {
            let mut p = base.clone();
            mutate(&mut p);
            encode_data(&p)
        };
        assert!(with(&|p| p.expiry_unix = 0).is_err());
        assert!(with(&|p| p.expiry_unix = p.issued_at_unix).is_err());
        assert!(with(&|p| p.expiry_unix = p.issued_at_unix - 1).is_err());
        assert!(with(&|p| p.solana_attestation_pda = [0u8; 32]).is_err());
        assert!(with(&|p| p.source_chain = String::new()).is_err());
        assert!(encode_data(&base).is_ok());
    }

    #[test]
    fn validate_rejects_repeated_byte_placeholder_anchors() {
        // The 0xab..ab staging placeholder sat behind a validate() that
        // only refused all-zero, so it could have been attested on
        // mainnet as a real anchor. Any 32-repeats-of-one-byte pattern is
        // now refused; the real recorded anchor still passes.
        let with_pda = |pda: [u8; 32]| {
            let mut p = projection();
            p.solana_attestation_pda = pda;
            encode_data(&p)
        };
        for byte in [0xABu8, 0x11, 0xFF] {
            assert!(
                matches!(
                    with_pda([byte; 32]),
                    Err(EvmSignerError::Reputation(m)) if m.contains("placeholder pattern")
                ),
                "[{byte:#04x}; 32] must be refused"
            );
        }
        assert!(with_pda(live_anchor()).is_ok());
    }

    #[test]
    fn schema_uid_is_pinned() {
        // Freeze the schema string via its UID: any field rename or reorder
        // changes it, which would silently fork the schema a verifier looks
        // up. keccak256(schema ‖ zero-resolver ‖ 0x01), and distinct from the
        // audit-root schema so the two never collide.
        assert_eq!(
            eth::hex_0x(&reputation_schema_uid()),
            "0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39"
        );
        assert_ne!(reputation_schema_uid(), crate::covenant_schema_uid());
    }

    #[test]
    fn attest_selector_is_derived_and_pinned() {
        // keccak256("attest((bytes32,(address,uint64,bool,bytes32,bytes,uint256)))")[..4],
        // the value the Base Sepolia dry-run confirmed the live predeploy accepts.
        // A drifted signature string changes this and fails here, not on-chain.
        assert_eq!(eth::hex_encode(&attest_selector()), "f17325e7");
    }

    #[test]
    fn attest_calldata_wraps_encode_data_verbatim() {
        // The staged transaction's schema bytes must be the exact encoder
        // output — the invariant whose violation this task exists to fix:
        // hand-assembled calldata froze a corrupt source_chain the encoder
        // could never produce.
        let p = projection();
        let call = attest_calldata(&p).unwrap();
        let data = encode_data(&p).unwrap();

        assert_eq!(&call[..4], &attest_selector());
        let word = |i: usize| &call[4 + i * 32..4 + (i + 1) * 32];
        assert_eq!(word(0), &word_u256(0x20)); // AttestationRequest offset
        assert_eq!(word(1), &reputation_schema_uid());
        assert_eq!(word(2), &word_u256(0x40)); // AttestationRequestData offset
        assert_eq!(word(3), &word_address(&eth::ZERO_ADDRESS));
        assert_eq!(word(4), &word_u256(p.expiry_unix));
        assert_eq!(word(5), &word_u256(1)); // revocable
        assert_eq!(word(6), &[0u8; 32]); // refUID
        assert_eq!(word(7), &word_u256(0xc0)); // bytes offset
        assert_eq!(word(8), &word_u256(0)); // value
        assert_eq!(word(9), &word_u256(data.len() as u64));
        assert_eq!(&call[4 + 10 * 32..], &data);
        assert_eq!(call.len(), 4 + 10 * 32 + data.len());
    }

    #[test]
    fn attest_calldata_matches_the_staged_golden() {
        // The exact bytes staged for operator submission
        // (autonomy/multichain/staging/reputation-attest-base-sepolia.json),
        // pasted from the stage_reputation_attest example run that staged
        // the live anchor — never hand-assembled. Differs from the prior
        // golden only in schema word 4: the 0xab..ab placeholder became
        // the live attestation asset's bytes.
        let call = attest_calldata(&ReputationProjection::new(
            ReputationScore::from_ratio(95, 100, 4).unwrap(),
            SOLANA_MAINNET_CAIP2,
            live_anchor(),
            1_700_000_000,
            1_800_000_000,
        ))
        .unwrap();
        assert_eq!(
            eth::hex_0x(&call),
            "0xf17325e7000000000000000000000000000000000000000000000000000000000000002084738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc3900000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006b49d2000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000251c0000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000a05ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e60000000000000000000000000000000000000000000000000000000000000027736f6c616e613a3565796b7434557346763850384e4a64545245705931767a714b715a4b76647000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn attest_calldata_bounds_the_payload() {
        // 321 padded source_chain bytes push the encoded data past the
        // 512-byte relay bound; the builder must refuse, not stage it.
        let mut p = projection();
        p.source_chain = "solana:".to_string() + &"x".repeat(340);
        match attest_calldata(&p) {
            Err(EvmSignerError::PayloadTooLarge { len, max }) => {
                assert!(len > max);
                assert_eq!(max, RELAY_MAX_DATA_BYTES);
            }
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn attest_calldata_propagates_projection_validation() {
        // An unanchored projection must fail at build time exactly as the
        // off-chain path does; the relay is not a validation bypass.
        let mut p = projection();
        p.solana_attestation_pda = [0u8; 32];
        assert!(matches!(
            attest_calldata(&p),
            Err(EvmSignerError::Reputation(_))
        ));
    }

    #[test]
    fn parse_reputation_projection_reads_camel_and_snake_case() {
        use serde_json::json;
        let camel = parse_reputation_projection(&json!({
            "score": 9_500, "scoreDecimals": 4,
            "sourceChain": SOLANA_MAINNET_CAIP2,
            "solanaAttestationPda": "0xabababababababababababababababababababababababababababababababab",
            "issuedAt": 1_700_000_000, "expiry": 1_800_000_000
        }))
        .unwrap();
        assert_eq!((camel.score.score, camel.score.decimals), (9_500, 4));

        let snake = parse_reputation_projection(&json!({
            "score": 9_500, "score_decimals": 4,
            "source_chain": SOLANA_MAINNET_CAIP2,
            "solana_attestation_pda": "0xabababababababababababababababababababababababababababababababab",
            "issued_at": 1_700_000_000, "expiry": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(snake.solana_attestation_pda, [0xAB; 32]);

        // A missing field is reported, not defaulted.
        assert!(parse_reputation_projection(&json!({
            "score": 1, "scoreDecimals": 0, "sourceChain": "solana:x",
            "issuedAt": 1, "expiry": 2
        }))
        .is_err());
    }

    #[test]
    fn parse_reputation_projection_reads_a_base58_anchor() {
        use serde_json::json;
        // The base58 spelling Solana tooling emits must parse to the same
        // bytes as the hex spelling — and a string that is neither form is
        // reported through the base58 arm, never silently zero-filled.
        let p = parse_reputation_projection(&json!({
            "score": 9_500, "scoreDecimals": 4,
            "sourceChain": SOLANA_MAINNET_CAIP2,
            "solanaAttestationPda": LIVE_ATTESTATION_ASSET,
            "issuedAt": 1_700_000_000, "expiry": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(p.solana_attestation_pda, live_anchor());

        let hex_spelling = parse_reputation_projection(&json!({
            "score": 9_500, "scoreDecimals": 4,
            "sourceChain": SOLANA_MAINNET_CAIP2,
            "solanaAttestationPda": format!("0x{LIVE_ATTESTATION_ASSET_HEX}"),
            "issuedAt": 1_700_000_000, "expiry": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(hex_spelling.solana_attestation_pda, live_anchor());

        assert!(matches!(
            parse_reputation_projection(&json!({
                "score": 1, "scoreDecimals": 0, "sourceChain": "solana:x",
                "solanaAttestationPda": "not-any-spelling",
                "issuedAt": 1, "expiry": 2
            })),
            Err(EvmSignerError::Reputation(m)) if m.contains("not base58")
        ));
    }

    #[test]
    fn from_pda_hex_parses_the_anchor() {
        let p = ReputationProjection::from_pda_hex(
            ReputationScore::new(9_500, 4),
            SOLANA_MAINNET_CAIP2,
            "0xabababababababababababababababababababababababababababababababab",
            1_700_000_000,
            1_800_000_000,
        )
        .unwrap();
        assert_eq!(p.solana_attestation_pda, [0xAB; 32]);
        assert!(ReputationProjection::from_pda_hex(
            ReputationScore::new(1, 0),
            SOLANA_MAINNET_CAIP2,
            "0xnothex",
            1,
            2
        )
        .is_err());
    }

    #[test]
    fn from_pda_base58_parses_the_anchor() {
        let p = ReputationProjection::from_pda_base58(
            ReputationScore::new(9_500, 4),
            SOLANA_MAINNET_CAIP2,
            LIVE_ATTESTATION_ASSET,
            1_700_000_000,
            1_800_000_000,
        )
        .unwrap();
        assert_eq!(p.solana_attestation_pda, live_anchor());
        assert!(ReputationProjection::from_pda_base58(
            ReputationScore::new(1, 0),
            SOLANA_MAINNET_CAIP2,
            "0OIl",
            1,
            2
        )
        .is_err());
    }
}
