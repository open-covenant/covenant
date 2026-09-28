//! An operator's stake at risk (C5 phase 2): the memo shapes that let
//! the rail attribute an on-chain bond post and its eventual refund,
//! plus the operator's signed request to take unslashed bond back out.
//! Bonds ride the exact machinery deposits and withdrawals proved —
//! posting mirrors [`crate::escrow::DEPOSIT_MEMO_PREFIX`] (the rail
//! answers who a payment funds from the transaction's own memo), the
//! refund mirrors [`crate::withdrawal`] (a signed, idempotent
//! instruction a relay can neither forge nor re-point).

use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError, UNBOND_DOMAIN};

/// The memo an on-chain bond post must carry to attribute itself: this
/// prefix followed by the operator's compute identity pubkey (base58
/// ed25519 — the key that signs receipts, not a wallet address). Same
/// contract as the buyer deposit memo: a payment that doesn't say whose
/// stake it is never credits anyone.
pub const BOND_MEMO_PREFIX: &str = "compute-bond:v1:";

/// The memo the coordinator stamps on a bond-refund transfer: this
/// prefix, the operator's pubkey, and the unbond id the operator chose
/// — the on-chain transaction names the obligation it honors, exactly
/// as a withdrawal names its debit.
pub const BOND_REFUND_MEMO_PREFIX: &str = "compute-bond-refund:v1:";

/// How far `requested_at_ms` may sit from the coordinator's clock.
/// Replaying a captured request is already harmless — the unbond id
/// dedups — so this only bounds how stale a request can arrive.
pub const UNBOND_MAX_SKEW_MS: u64 = 120_000;

/// The memo an on-chain bond post for this operator must carry.
pub fn bond_memo_for(operator_pubkey_b58: &str) -> String {
    format!("{BOND_MEMO_PREFIX}{operator_pubkey_b58}")
}

/// The memo a bond-refund transfer for this operator under this unbond
/// id must carry.
pub fn bond_refund_memo_for(operator_pubkey_b58: &str, unbond_id: Uuid) -> String {
    format!("{BOND_REFUND_MEMO_PREFIX}{operator_pubkey_b58}:{unbond_id}")
}

/// Splits a well-formed bond-refund memo back into (operator pubkey,
/// unbond id). `None` for anything else.
pub fn parse_bond_refund_memo(memo: &str) -> Option<(&str, Uuid)> {
    let rest = memo.strip_prefix(BOND_REFUND_MEMO_PREFIX)?;
    let (operator, id) = rest.rsplit_once(':')?;
    if operator.is_empty() {
        return None;
    }
    Some((operator, Uuid::parse_str(id).ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UnbondFields {
    operator: AgentId,
    unbond_id: Uuid,
    amount_micro_usdc: u64,
    recipient_address_b58: String,
    requested_at_ms: u64,
}

/// A signed instruction to move `amount_micro_usdc` of the operator's
/// unslashed, uncommitted bond to `recipient_address_b58` once the
/// unbonding window matures. The recipient is signed, so a relay
/// cannot redirect the refund, and it is validated as a real 32-byte
/// key at both ends rather than trusted into the transfer path. The
/// amount stays slashable until the transfer actually leaves — an
/// operator cannot outrun a fault by unbonding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnbondRequest {
    pub operator: AgentId,
    pub unbond_id: Uuid,
    pub amount_micro_usdc: u64,
    pub recipient_address_b58: String,
    pub requested_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl UnbondRequest {
    pub fn sign(
        operator: AgentId,
        unbond_id: Uuid,
        amount_micro_usdc: u64,
        recipient_address_b58: String,
        requested_at_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        check_amount(amount_micro_usdc)?;
        check_recipient(&recipient_address_b58)?;
        let fields = UnbondFields {
            operator: operator.clone(),
            unbond_id,
            amount_micro_usdc,
            recipient_address_b58: recipient_address_b58.clone(),
            requested_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, UNBOND_DOMAIN, &payload_json);
        Ok(Self {
            operator,
            unbond_id,
            amount_micro_usdc,
            recipient_address_b58,
            requested_at_ms,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.operator.pubkey_base58() {
            return Err(ProtocolError::Invalid(
                "signer_pubkey_b58 does not match operator".into(),
            ));
        }
        check_amount(self.amount_micro_usdc)?;
        check_recipient(&self.recipient_address_b58)?;
        verify_domain(
            UNBOND_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: UnbondFields = serde_json::from_str(&self.payload_json)?;
        let expected = UnbondFields {
            operator: self.operator.clone(),
            unbond_id: self.unbond_id,
            amount_micro_usdc: self.amount_micro_usdc,
            recipient_address_b58: self.recipient_address_b58.clone(),
            requested_at_ms: self.requested_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match unbond fields".into(),
            ));
        }
        Ok(())
    }

    /// The memo the refund transfer honoring this request must carry.
    pub fn memo(&self) -> String {
        bond_refund_memo_for(&self.operator.pubkey_base58(), self.unbond_id)
    }
}

fn check_amount(amount_micro_usdc: u64) -> Result<(), ProtocolError> {
    if amount_micro_usdc == 0 {
        return Err(ProtocolError::Invalid(
            "unbond amount must be positive".into(),
        ));
    }
    Ok(())
}

fn check_recipient(recipient_address_b58: &str) -> Result<(), ProtocolError> {
    crate::validate_address_b58("recipient address", recipient_address_b58)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPIENT: &str = "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA";

    fn signed(operator: &LocalIdentity) -> UnbondRequest {
        UnbondRequest::sign(
            operator.agent_id(),
            Uuid::new_v4(),
            50_000,
            RECIPIENT.into(),
            1_000,
            operator,
        )
        .unwrap()
    }

    #[test]
    fn sign_and_verify_round_trips() {
        let operator = LocalIdentity::generate("operator@local");
        signed(&operator).verify().expect("verify");
    }

    #[test]
    fn a_redirected_or_resized_unbond_fails_verification() {
        let operator = LocalIdentity::generate("operator@local");

        let mut redirected = signed(&operator);
        redirected.recipient_address_b58 = "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4T".into();
        assert!(redirected.verify().is_err());

        let mut resized = signed(&operator);
        resized.amount_micro_usdc = 1;
        assert!(resized.verify().is_err());
    }

    #[test]
    fn someone_elses_signature_is_rejected() {
        let operator = LocalIdentity::generate("operator@local");
        let impostor = LocalIdentity::generate("impostor@local");
        let req = UnbondRequest::sign(
            operator.agent_id(),
            Uuid::new_v4(),
            50_000,
            RECIPIENT.into(),
            1_000,
            &impostor,
        )
        .unwrap();
        assert!(req.verify().is_err());
    }

    #[test]
    fn zero_amounts_and_bogus_recipients_bounce_at_both_ends() {
        let operator = LocalIdentity::generate("operator@local");
        assert!(UnbondRequest::sign(
            operator.agent_id(),
            Uuid::new_v4(),
            0,
            RECIPIENT.into(),
            1_000,
            &operator,
        )
        .is_err());
        assert!(UnbondRequest::sign(
            operator.agent_id(),
            Uuid::new_v4(),
            50_000,
            "not-base58!".into(),
            1_000,
            &operator,
        )
        .is_err());

        // A patched client that signed a bogus recipient anyway still
        // bounces at verification.
        let mut req = signed(&operator);
        req.recipient_address_b58 = "abc".into();
        assert!(req.verify().is_err());
    }

    #[test]
    fn bond_memo_names_the_operator() {
        let memo = bond_memo_for("op-key");
        assert_eq!(memo, "compute-bond:v1:op-key");
        assert!(memo.starts_with(BOND_MEMO_PREFIX));
    }

    #[test]
    fn refund_memo_round_trips_and_rejects_foreign_shapes() {
        let operator = LocalIdentity::generate("operator@local");
        let req = signed(&operator);
        let memo = req.memo();
        assert!(memo.starts_with(BOND_REFUND_MEMO_PREFIX));
        let (pubkey, id) = parse_bond_refund_memo(&memo).expect("parse");
        assert_eq!(pubkey, operator.agent_id().pubkey_base58());
        assert_eq!(id, req.unbond_id);

        assert!(parse_bond_refund_memo("compute-withdrawal:v1:x:y").is_none());
        assert!(
            parse_bond_refund_memo(&format!("{BOND_REFUND_MEMO_PREFIX}only-one-part")).is_none()
        );
        assert!(
            parse_bond_refund_memo(&format!("{BOND_REFUND_MEMO_PREFIX}:{}", Uuid::new_v4()))
                .is_none()
        );
        assert!(parse_bond_refund_memo(&format!(
            "{BOND_REFUND_MEMO_PREFIX}{}:not-a-uuid",
            operator.agent_id().pubkey_base58()
        ))
        .is_none());
    }
}
