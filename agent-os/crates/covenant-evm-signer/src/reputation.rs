//! Encode an experimental local event-count heuristic in an EAS off-chain
//! statement. This module is a format/projection utility and is not wired into
//! covenantd publication. The caller supplies the score, source-chain label,
//! and Solana account reference; this code does not verify an anchor, identity,
//! event completeness, or reputation. `ecrecover` verifies only that the
//! configured EVM key signed the exact bytes; key attribution is external.
//!
//! The schema is modeled on [Human Passport]'s score attestation: a
//! `score`/`score_decimals` pair, so a fractional score survives as an
//! integer without a float ever touching the wire. Covenant adds the
//! caller-supplied references `source_chain` and `solana_attestation_pda`.
//! Carrying those bytes does not show the account exists, is non-transferable,
//! contains the score, or belongs to the subject.
//!
//! [EAS]: https://attest.org
//! [Human Passport]: https://passport.human.tech

use serde_json::Value;

use crate::eth::{self, hex_decode_32, word_u256};
use crate::uid::schema_uid;
use crate::EvmSignerError;

/// Solana mainnet's CAIP-2 chain id used as a default source label. The
/// 32-character reference is the leading
/// slice of the genesis-block hash, and matches `covenant-hyre`'s
/// `SOLANA_NETWORK`, so the caller can label the same rail the rest of the
/// stack uses.
pub const SOLANA_MAINNET_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// The experimental EAS projection schema. `score`
/// and `score_decimals` carry the value together — the reader recovers the
/// real number as `score / 10^score_decimals` — so the decimal scale can
/// never be lost between issuer and verifier. `expiry` is a hard bound (an
/// EAS attestation with no expiry remains usable forever). `source_chain`
/// and `solana_attestation_pda` are caller-supplied references, not verified
/// provenance or an authenticated anchor.
pub const REPUTATION_SCHEMA: &str =
    "uint32 score,uint8 score_decimals,uint64 expiry,string source_chain,bytes32 solana_attestation_pda";

/// Number of head words in the ABI encoding of the reputation tuple, one
/// per schema field. The single dynamic field (`source_chain`) stores its
/// offset here and its bytes past the head.
const HEAD_WORDS: u64 = 5;

/// A caller-supplied experimental score, pre-scaled to an integer. The real
/// value is `score / 10^decimals`; this type preserves that scale but does not
/// establish how the score was produced or what it means.
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

/// Values the signed projection commits to. `solana_attestation_pda` is a
/// caller-supplied 32-byte account reference; this crate does not fetch it or
/// establish what it stores.
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

    /// Build a projection from the experimental audit event heuristic
    /// ([`covenant_audit::reputation::AuditReputation`]), the Solana anchor the
    /// caller-supplied account reference, and validity window. Refuses an empty
    /// metric, but does not verify the event slice or referenced account.
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
    /// score that never expires, an expiry that predates issuance, an all-zero
    /// reference, or an empty source chain. These are shape constraints only;
    /// they do not validate the referenced account.
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
                "solana_attestation_pda is all-zero: a non-zero caller reference is required"
                    .into(),
            ));
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

/// Parse a reputation projection from the wire JSON the sidecar and the relay
/// staging binary both read. Accepts camelCase (the default) with snake_case
/// fallbacks; the score and its decimal scale are read together so the two can
/// never drift apart.
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

    ReputationProjection::from_pda_hex(
        ReputationScore::new(score, decimals),
        str_field(&["sourceChain", "source_chain"])?.to_string(),
        str_field(&["solanaAttestationPda", "solana_attestation_pda"])?,
        u64_field(&["issuedAt", "issued_at"])?,
        u64_field(&["expiry"])?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::reputation::AuditReputation;

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
            [0xAB; 32],
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
                [0xAB; 32],
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
            [0xAB; 32],
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
        assert_eq!(pda, [0xAB; 32]);
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
}
