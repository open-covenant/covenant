//! Promote a reputation projection to an *on-chain* EAS attestation on Base.
//!
//! [`attest_reputation`][crate::EasAttestationSigner::attest_reputation]
//! produces an off-chain attestation — a signature a verifier checks with one
//! `ecrecover`. Some verifiers must enforce *in-contract*: read the score out
//! of EAS storage inside a transaction. For them, this module builds the EAS
//! `attest` transaction that writes the identical reputation on-chain, so both
//! surfaces commit to one score, one expiry, one Solana anchor.
//!
//! Two boundaries are load-bearing, and both are closed by construction:
//!
//! - **No key, ever.** [`stage_reputation_attestation`] returns an *unsigned*
//!   [`RelayTransaction`] — calldata, `to`, and `chainId`, with no signature,
//!   nonce, gas, or `from`. This crate never holds a signing key, never signs,
//!   and never submits. Signing and broadcast are an operator step under the
//!   operator's own key custody.
//! - **Sepolia is autonomous; mainnet is gated.** Only [`BASE_SEPOLIA`] is a
//!   permitted autonomous target. [`BASE_MAINNET`] is refused with
//!   [`EvmSignerError::MainnetGated`], so an autonomous loop cannot stage a
//!   mainnet write. The mainnet constant exists to *document* the target an
//!   operator will submit against, not to enable it here.
//!
//! Calldata is `attest((bytes32,(address,uint64,bool,bytes32,bytes,uint256)))`
//! — the ABI pinned from `ethereum-attestation-service/eas-contracts` at
//! [`EAS_ABI_SOURCE_COMMIT`]. The inner `bytes data` is the same reputation ABI
//! blob the off-chain attestation carries.

use serde_json::{json, Value};

use crate::eth::{self, keccak256, word_address, word_u256, EAS_PREDEPLOY, ZERO_ADDRESS};
use crate::reputation::{encode_data, reputation_schema_uid};
use crate::{EvmSignerError, ReputationProjection};

/// `eas-contracts` commit (tag `v1.3.0`) the `attest` ABI is pinned to. The
/// deployed EAS predeploy reports `version()` `1.2.0`; the `attest` request
/// shape is stable across those versions.
pub const EAS_ABI_SOURCE_COMMIT: &str = "0c51c77cccd68e19ddbfeb832f153e75fac1af19";

/// The canonical `attest` signature. The 4-byte selector is *derived* from
/// this (see [`attest_selector`]), never hard-coded, so a typo in the tuple
/// shape changes the selector and fails the pin test rather than silently
/// staging calldata no contract answers.
pub const ATTEST_SIGNATURE: &str = "attest((bytes32,(address,uint64,bool,bytes32,bytes,uint256)))";

/// A Base chain the relayer can target. `autonomous` marks the only chain an
/// autonomous session may stage without operator approval.
///
/// `#[non_exhaustive]`, so only this crate's own [`BASE_SEPOLIA`] and
/// [`BASE_MAINNET`] constants can exist: a downstream consumer cannot forge an
/// `autonomous: true` mainnet target to smuggle a mainnet write past the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelayChain {
    pub network: &'static str,
    pub chain_id: u64,
    /// The EAS contract calldata is submitted to.
    pub eas: [u8; 20],
    /// Whether an autonomous session may stage a write here.
    pub autonomous: bool,
}

/// Base Sepolia — the autonomous target. Real contract, testnet funds, no
/// production consequence.
pub const BASE_SEPOLIA: RelayChain = RelayChain {
    network: "Base Sepolia",
    chain_id: 84_532,
    eas: EAS_PREDEPLOY,
    autonomous: true,
};

/// Base mainnet — documented but gated. Staging against it returns
/// [`EvmSignerError::MainnetGated`]; a real submission is an operator decision.
pub const BASE_MAINNET: RelayChain = RelayChain {
    network: "Base",
    chain_id: 8_453,
    eas: EAS_PREDEPLOY,
    autonomous: false,
};

/// Bounds one attestation's cost, and names where the rate bound lives.
///
/// `max_data_bytes` caps the schema payload, and so per-write calldata gas —
/// enforced here, at build time. A reputation payload is small and near
/// constant; a pathological `source_chain` string is the only way `data`
/// grows, so a generous ceiling caps cost without constraining a real score.
///
/// `max_writes_per_day` is the write-*rate* ceiling. The builder cannot enforce
/// a rate — it holds no clock and no history — so it stamps the ceiling onto
/// every [`RelayTransaction`], and the operator's (gated) submission layer, the
/// only place with the key and the state to count, must honor it. Naming it on
/// the artifact keeps the "unbounded write cost" failure mode from hiding in an
/// unwritten assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayPolicy {
    pub max_data_bytes: usize,
    pub max_writes_per_day: u32,
}

impl Default for RelayPolicy {
    fn default() -> Self {
        Self {
            max_data_bytes: 512,
            max_writes_per_day: 24,
        }
    }
}

/// The 4-byte selector for [`ATTEST_SIGNATURE`].
pub fn attest_selector() -> [u8; 4] {
    let digest = keccak256(ATTEST_SIGNATURE.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

/// An unsigned EAS `attest` transaction: everything an operator needs to review
/// and submit, and nothing that could submit it. No `from`, signature, nonce,
/// or gas — those are the operator's to fill under their own key custody.
#[derive(Debug, Clone)]
pub struct RelayTransaction {
    pub network: &'static str,
    pub chain_id: u64,
    pub to: [u8; 20],
    pub schema: [u8; 32],
    pub recipient: [u8; 20],
    pub expiration_time: u64,
    pub revocable: bool,
    /// Full `attest(...)` calldata: selector ‖ ABI-encoded `AttestationRequest`.
    pub data: Vec<u8>,
    pub policy: RelayPolicy,
}

impl RelayTransaction {
    pub fn calldata_hex(&self) -> String {
        eth::hex_0x(&self.data)
    }

    /// The operator-review artifact: the transaction fields plus the custody,
    /// gating, expiry, and rate obligations that travel with it.
    pub fn to_json(&self) -> Value {
        json!({
            "network": self.network,
            "chainId": self.chain_id,
            "to": eth::hex_0x(&self.to),
            "function": ATTEST_SIGNATURE,
            "schemaUID": eth::hex_0x(&self.schema),
            "recipient": eth::hex_0x(&self.recipient),
            "expirationTime": self.expiration_time,
            "revocable": self.revocable,
            "value": "0x0",
            "data": self.calldata_hex(),
            "abiSourceCommit": EAS_ABI_SOURCE_COMMIT,
            "policy": {
                "maxDataBytes": self.policy.max_data_bytes,
                "maxWritesPerDay": self.policy.max_writes_per_day,
            },
            "notes": self.notes(),
        })
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.to_json()).expect("a Value always serializes")
    }

    fn notes(&self) -> Vec<String> {
        vec![
            "Attester model: EAS records the SUBMITTING address as `attester` (msg.sender). Under Covenant's Option-A trust model the attester of record must be the secp256k1 issuer, which this direct-`attest` path cannot satisfy from a relayer EOA. Do NOT submit this under Option A; use the off-chain issuer-signed attestation, or `attestByDelegation` carrying the issuer's signature so `attester` stays the issuer.".into(),
            "Unsigned: no signature, nonce, gas, or from. Submission needs an operator-custodied funded key; this crate never holds one.".into(),
            format!(
                "EAS is an OP-Stack predeploy at {}; re-verify the live contract's attest ABI against this calldata immediately before submission.",
                eth::hex_0x(&self.to)
            ),
            "recipient is zero: the identity binding is the Solana PDA inside the payload, not an EVM token, so the attestation is non-transferable and cannot be laundered onto a sellable NFT.".into(),
            format!(
                "revocable={}, expirationTime={}: the score expires on-chain and can be revoked, so a stale attestation does not stay trusted forever.",
                self.revocable, self.expiration_time
            ),
            format!(
                "Cost: at most {} writes/day. The builder cannot rate-limit; the operator submission layer must enforce this.",
                self.policy.max_writes_per_day
            ),
            format!(
                "Precondition: the reputation schema ({}) must be registered in the EAS SchemaRegistry on {} first, or attest reverts InvalidSchema. That registration is itself a gated on-chain write.",
                eth::hex_0x(&self.schema),
                self.network
            ),
        ]
    }
}

/// Build an unsigned EAS `attest` transaction that writes `projection` as an
/// on-chain reputation attestation. Autonomous target only: [`BASE_MAINNET`]
/// returns [`EvmSignerError::MainnetGated`].
///
/// The projection is validated the same way the off-chain path validates it (no
/// missing expiry, no missing Solana anchor), and the encoded payload is bounded
/// by `policy.max_data_bytes`.
pub fn stage_reputation_attestation(
    projection: &ReputationProjection,
    chain: RelayChain,
    policy: RelayPolicy,
) -> Result<RelayTransaction, EvmSignerError> {
    // `autonomous` is the semantic gate, but do not trust the flag alone:
    // refuse the mainnet chain id outright, so even an in-crate misconfiguration
    // (a mainnet target mistakenly marked autonomous) still fails closed.
    if !chain.autonomous || chain.chain_id == BASE_MAINNET.chain_id {
        return Err(EvmSignerError::MainnetGated(chain.network));
    }

    let schema = reputation_schema_uid();
    let payload = encode_data(projection)?;
    if payload.len() > policy.max_data_bytes {
        return Err(EvmSignerError::PayloadTooLarge {
            len: payload.len(),
            max: policy.max_data_bytes,
        });
    }

    let data = encode_attest_calldata(
        &schema,
        &ZERO_ADDRESS,
        projection.expiry_unix,
        true,
        &[0u8; 32],
        0,
        &payload,
    );

    Ok(RelayTransaction {
        network: chain.network,
        chain_id: chain.chain_id,
        to: chain.eas,
        schema,
        recipient: ZERO_ADDRESS,
        expiration_time: projection.expiry_unix,
        revocable: true,
        data,
        policy,
    })
}

/// ABI-encode `attest(AttestationRequest)` calldata. The sole argument is a
/// dynamic tuple (its inner `bytes data` makes it dynamic), so the head is one
/// offset word to the tuple. `AttestationRequest` lays `schema` inline and
/// points at its `AttestationRequestData`; that inner tuple lays its four static
/// fields plus `value` inline and points at the `bytes data` blob.
fn encode_attest_calldata(
    schema: &[u8; 32],
    recipient: &[u8; 20],
    expiration_time: u64,
    revocable: bool,
    ref_uid: &[u8; 32],
    value: u64,
    data: &[u8],
) -> Vec<u8> {
    let padded = data.len().next_multiple_of(32);
    let mut out = Vec::with_capacity(4 + 32 * 10 + padded);

    out.extend_from_slice(&attest_selector());
    // The sole parameter, a dynamic tuple, begins one word in.
    out.extend_from_slice(&word_u256(0x20));
    // AttestationRequest head: schema, then the offset to its data tuple (past
    // this 2-word head).
    out.extend_from_slice(schema);
    out.extend_from_slice(&word_u256(0x40));
    // AttestationRequestData head: four static words, the offset to `bytes data`
    // (past this 6-word head), then value.
    out.extend_from_slice(&word_address(recipient));
    out.extend_from_slice(&word_u256(expiration_time));
    out.extend_from_slice(&word_u256(revocable as u64));
    out.extend_from_slice(ref_uid);
    out.extend_from_slice(&word_u256(0xC0));
    out.extend_from_slice(&word_u256(value));
    // `bytes data` tail: length word, then the reputation blob padded to a word.
    out.extend_from_slice(&word_u256(data.len() as u64));
    out.extend_from_slice(data);
    out.resize(out.len() + (padded - data.len()), 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reputation::{ReputationScore, SOLANA_MAINNET_CAIP2};

    const PDA: [u8; 32] = [0xAB; 32];

    fn projection() -> ReputationProjection {
        ReputationProjection::new(
            ReputationScore::from_ratio(95, 100, 4).unwrap(),
            SOLANA_MAINNET_CAIP2,
            PDA,
            1_700_000_000,
            1_800_000_000,
        )
    }

    fn word_at(data: &[u8], i: usize) -> [u8; 32] {
        data[4 + i * 32..4 + (i + 1) * 32].try_into().unwrap()
    }

    fn low64(word: [u8; 32]) -> u64 {
        u64::from_be_bytes(word[24..].try_into().unwrap())
    }

    #[test]
    fn attest_selector_is_pinned() {
        // keccak256(ATTEST_SIGNATURE)[..4]. Pinned so a tuple-shape typo fails
        // here, and confirmed live against the Base Sepolia EAS predeploy.
        assert_eq!(eth::hex_0x(&attest_selector()), "0xf17325e7");
    }

    #[test]
    fn calldata_has_the_attestation_request_layout() {
        let tx = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        let payload = encode_data(&projection()).unwrap();

        assert_eq!(&tx.data[..4], &attest_selector());
        assert_eq!(low64(word_at(&tx.data, 0)), 0x20); // offset to the request tuple
        assert_eq!(word_at(&tx.data, 1), reputation_schema_uid()); // schema
        assert_eq!(low64(word_at(&tx.data, 2)), 0x40); // offset to the data tuple
        assert_eq!(word_at(&tx.data, 3), [0u8; 32]); // recipient (zero)
        assert_eq!(low64(word_at(&tx.data, 4)), 1_800_000_000); // expirationTime
        assert_eq!(low64(word_at(&tx.data, 5)), 1); // revocable
        assert_eq!(word_at(&tx.data, 6), [0u8; 32]); // refUID
        assert_eq!(low64(word_at(&tx.data, 7)), 0xC0); // offset to `bytes data`
        assert_eq!(low64(word_at(&tx.data, 8)), 0); // value
        assert_eq!(low64(word_at(&tx.data, 9)), payload.len() as u64); // data length
        assert_eq!(&tx.data[4 + 10 * 32..], payload.as_slice()); // the blob
    }

    #[test]
    fn on_chain_and_off_chain_carry_the_same_payload() {
        // The invariant that ties the two surfaces together: the `bytes data`
        // the relayer writes on-chain is byte-identical to the off-chain
        // attestation's data, so a verifier reading either recovers the same
        // score, expiry, and Solana anchor.
        let signer = crate::EasAttestationSigner::base_sepolia(
            covenant_identity::Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap(),
        );
        let offchain = signer.attest_reputation(&projection()).unwrap();
        let tx = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        assert_eq!(&tx.data[4 + 10 * 32..], offchain.message.data.as_slice());
        assert_eq!(tx.schema, offchain.message.schema);
        assert_eq!(tx.expiration_time, offchain.message.expiration_time);
    }

    #[test]
    fn mainnet_is_gated() {
        let err = stage_reputation_attestation(&projection(), BASE_MAINNET, RelayPolicy::default())
            .unwrap_err();
        assert!(matches!(err, EvmSignerError::MainnetGated("Base")));
        // Sepolia, the only autonomous chain, stages.
        assert!(
            stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
                .is_ok()
        );
    }

    #[test]
    fn a_mainnet_id_marked_autonomous_is_still_gated() {
        // RelayChain is #[non_exhaustive], so this forgery is impossible outside
        // the crate; the redundant chain-id guard makes even an in-crate mistake
        // (the mainnet id mistakenly marked autonomous) fail closed.
        let forged = RelayChain {
            autonomous: true,
            ..BASE_MAINNET
        };
        assert!(matches!(
            stage_reputation_attestation(&projection(), forged, RelayPolicy::default()),
            Err(EvmSignerError::MainnetGated(_))
        ));
    }

    #[test]
    fn stages_to_the_sepolia_predeploy() {
        let tx = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        assert_eq!(tx.chain_id, 84_532);
        assert_eq!(
            eth::hex_0x(&tx.to),
            "0x4200000000000000000000000000000000000021"
        );
        assert!(tx.revocable);
        assert_eq!(tx.expiration_time, 1_800_000_000);
    }

    #[test]
    fn predeploy_is_shared_across_the_superchain() {
        assert_eq!(BASE_SEPOLIA.eas, BASE_MAINNET.eas);
        assert_eq!(
            eth::hex_0x(&EAS_PREDEPLOY),
            "0x4200000000000000000000000000000000000021"
        );
    }

    #[test]
    fn refuses_an_under_attesting_projection() {
        // The same fail-closed the off-chain path enforces: a score with no
        // expiry (EAS treats 0 as never-expiring) must not reach calldata.
        let mut stale = projection();
        stale.expiry_unix = 0;
        assert!(matches!(
            stage_reputation_attestation(&stale, BASE_SEPOLIA, RelayPolicy::default()),
            Err(EvmSignerError::Reputation(_))
        ));
    }

    #[test]
    fn enforces_the_payload_bound() {
        let tight = RelayPolicy {
            max_data_bytes: 32,
            ..RelayPolicy::default()
        };
        let err = stage_reputation_attestation(&projection(), BASE_SEPOLIA, tight).unwrap_err();
        assert!(matches!(
            err,
            EvmSignerError::PayloadTooLarge { max: 32, .. }
        ));
    }

    #[test]
    fn json_artifact_carries_the_custody_and_expiry_obligations() {
        let tx = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        let v = tx.to_json();
        assert_eq!(v["chainId"], 84_532);
        assert_eq!(v["to"], "0x4200000000000000000000000000000000000021");
        assert_eq!(v["function"], ATTEST_SIGNATURE);
        assert_eq!(v["recipient"], "0x0000000000000000000000000000000000000000");
        assert_eq!(v["expirationTime"], 1_800_000_000);
        assert_eq!(v["revocable"], true);
        assert_eq!(v["value"], "0x0");
        assert_eq!(v["abiSourceCommit"], EAS_ABI_SOURCE_COMMIT);
        assert_eq!(v["policy"]["maxWritesPerDay"], 24);

        let notes = v["notes"].as_array().unwrap();
        let joined = notes.iter().filter_map(Value::as_str).collect::<String>();
        assert!(joined.contains("Unsigned"));
        assert!(joined.contains("operator-custodied"));
        assert!(joined.contains("non-transferable"));
        assert!(joined.contains("InvalidSchema"));
        assert!(joined.contains("writes/day"));
    }

    #[test]
    fn calldata_is_deterministic() {
        let a = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        let b = stage_reputation_attestation(&projection(), BASE_SEPOLIA, RelayPolicy::default())
            .unwrap();
        assert_eq!(a.data, b.data);
        assert_eq!(a.to_json_string(), b.to_json_string());
    }
}
