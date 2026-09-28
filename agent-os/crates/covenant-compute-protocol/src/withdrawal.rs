//! A buyer's signed request to take unspent deposit balance back out
//! (A3's money-out verb — a deposit book with no exit is a trap, not a
//! balance). Same wrap-don't-embed signing shape as
//! [`crate::dispute::DisputeRequest`], so a relay can neither forge a
//! withdrawal nor re-point one at a different recipient, and the
//! `withdrawal_id` doubles as the idempotency key end to end: request,
//! books debit, and on-chain memo all name the same id.

use covenant_identity::LocalIdentity;
use covenant_types::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sign::{
    sign_domain, to_canonical_json, verify_domain, ProtocolError, WITHDRAWAL_DOMAIN,
};

/// The memo the coordinator stamps on a withdrawal transfer: this
/// prefix, the buyer's compute identity pubkey, and the withdrawal id
/// the buyer chose. The on-chain transaction names the obligation it
/// honors, exactly as a payout names its receipt.
pub const WITHDRAWAL_MEMO_PREFIX: &str = "compute-withdrawal:v1:";

/// How far `requested_at_ms` may sit from the coordinator's clock.
/// Replaying a captured request is already harmless — the withdrawal
/// id dedups — so this only bounds how stale a request can arrive.
pub const WITHDRAWAL_MAX_SKEW_MS: u64 = 120_000;

/// The memo a withdrawal transfer for this buyer under this id must
/// carry.
pub fn withdrawal_memo_for(buyer_pubkey_b58: &str, withdrawal_id: Uuid) -> String {
    format!("{WITHDRAWAL_MEMO_PREFIX}{buyer_pubkey_b58}:{withdrawal_id}")
}

/// Splits a well-formed withdrawal memo back into (buyer pubkey,
/// withdrawal id). `None` for anything else.
pub fn parse_withdrawal_memo(memo: &str) -> Option<(&str, Uuid)> {
    let rest = memo.strip_prefix(WITHDRAWAL_MEMO_PREFIX)?;
    let (buyer, id) = rest.rsplit_once(':')?;
    if buyer.is_empty() {
        return None;
    }
    Some((buyer, Uuid::parse_str(id).ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WithdrawalFields {
    buyer: AgentId,
    withdrawal_id: Uuid,
    amount_micro_usdc: u64,
    recipient_address_b58: String,
    requested_at_ms: u64,
}

/// A signed instruction to move `amount_micro_usdc` of the buyer's
/// available balance to `recipient_address_b58`. The recipient is a
/// wallet address of the buyer's choosing — it is signed, so a relay
/// cannot redirect the money, and it is validated as a real 32-byte
/// key at both ends rather than trusted into the transfer path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalRequest {
    pub buyer: AgentId,
    pub withdrawal_id: Uuid,
    pub amount_micro_usdc: u64,
    pub recipient_address_b58: String,
    pub requested_at_ms: u64,
    pub payload_json: String,
    pub signature_b58: String,
    pub signer_pubkey_b58: String,
}

impl WithdrawalRequest {
    pub fn sign(
        buyer: AgentId,
        withdrawal_id: Uuid,
        amount_micro_usdc: u64,
        recipient_address_b58: String,
        requested_at_ms: u64,
        identity: &LocalIdentity,
    ) -> Result<Self, ProtocolError> {
        check_amount(amount_micro_usdc)?;
        check_recipient(&recipient_address_b58)?;
        let fields = WithdrawalFields {
            buyer: buyer.clone(),
            withdrawal_id,
            amount_micro_usdc,
            recipient_address_b58: recipient_address_b58.clone(),
            requested_at_ms,
        };
        let payload_json = to_canonical_json(&fields)?;
        let (signature_b58, signer_pubkey_b58) =
            sign_domain(identity, WITHDRAWAL_DOMAIN, &payload_json);
        Ok(Self {
            buyer,
            withdrawal_id,
            amount_micro_usdc,
            recipient_address_b58,
            requested_at_ms,
            payload_json,
            signature_b58,
            signer_pubkey_b58,
        })
    }

    pub fn verify(&self) -> Result<(), ProtocolError> {
        if self.signer_pubkey_b58 != self.buyer.pubkey_base58() {
            return Err(ProtocolError::Invalid(
                "signer_pubkey_b58 does not match buyer".into(),
            ));
        }
        check_amount(self.amount_micro_usdc)?;
        check_recipient(&self.recipient_address_b58)?;
        verify_domain(
            WITHDRAWAL_DOMAIN,
            &self.payload_json,
            &self.signature_b58,
            &self.signer_pubkey_b58,
        )?;
        let decoded: WithdrawalFields = serde_json::from_str(&self.payload_json)?;
        let expected = WithdrawalFields {
            buyer: self.buyer.clone(),
            withdrawal_id: self.withdrawal_id,
            amount_micro_usdc: self.amount_micro_usdc,
            recipient_address_b58: self.recipient_address_b58.clone(),
            requested_at_ms: self.requested_at_ms,
        };
        if decoded != expected {
            return Err(ProtocolError::Invalid(
                "payload_json does not match withdrawal fields".into(),
            ));
        }
        Ok(())
    }

    /// The memo the transfer honoring this request must carry.
    pub fn memo(&self) -> String {
        withdrawal_memo_for(&self.buyer.pubkey_base58(), self.withdrawal_id)
    }
}

fn check_amount(amount_micro_usdc: u64) -> Result<(), ProtocolError> {
    if amount_micro_usdc == 0 {
        return Err(ProtocolError::Invalid(
            "withdrawal amount must be positive".into(),
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

    fn signed(buyer: &LocalIdentity) -> WithdrawalRequest {
        WithdrawalRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            25_000,
            RECIPIENT.into(),
            1_000,
            buyer,
        )
        .unwrap()
    }

    #[test]
    fn sign_and_verify_round_trips() {
        let buyer = LocalIdentity::generate("buyer@local");
        signed(&buyer).verify().expect("verify");
    }

    #[test]
    fn a_redirected_or_resized_withdrawal_fails_verification() {
        let buyer = LocalIdentity::generate("buyer@local");

        let mut redirected = signed(&buyer);
        redirected.recipient_address_b58 = "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4T".into();
        assert!(redirected.verify().is_err());

        let mut resized = signed(&buyer);
        resized.amount_micro_usdc = 1;
        assert!(resized.verify().is_err());
    }

    #[test]
    fn someone_elses_signature_is_rejected() {
        let buyer = LocalIdentity::generate("buyer@local");
        let impostor = LocalIdentity::generate("impostor@local");
        let req = WithdrawalRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            25_000,
            RECIPIENT.into(),
            1_000,
            &impostor,
        )
        .unwrap();
        assert!(req.verify().is_err());
    }

    #[test]
    fn zero_amounts_and_bogus_recipients_bounce_at_both_ends() {
        let buyer = LocalIdentity::generate("buyer@local");
        assert!(WithdrawalRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            0,
            RECIPIENT.into(),
            1_000,
            &buyer,
        )
        .is_err());
        assert!(WithdrawalRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            25_000,
            "not-base58!".into(),
            1_000,
            &buyer,
        )
        .is_err());
        assert!(WithdrawalRequest::sign(
            buyer.agent_id(),
            Uuid::new_v4(),
            25_000,
            "abc".into(),
            1_000,
            &buyer,
        )
        .is_err());

        // A patched client that signed a bogus recipient anyway still
        // bounces at verification.
        let mut req = signed(&buyer);
        req.recipient_address_b58 = "abc".into();
        assert!(req.verify().is_err());
    }

    #[test]
    fn memo_round_trips_and_rejects_foreign_shapes() {
        let buyer = LocalIdentity::generate("buyer@local");
        let req = signed(&buyer);
        let memo = req.memo();
        assert!(memo.starts_with(WITHDRAWAL_MEMO_PREFIX));
        let (pubkey, id) = parse_withdrawal_memo(&memo).expect("parse");
        assert_eq!(pubkey, buyer.agent_id().pubkey_base58());
        assert_eq!(id, req.withdrawal_id);

        assert!(parse_withdrawal_memo("compute-payout:v1:x:y").is_none());
        assert!(parse_withdrawal_memo(&format!("{WITHDRAWAL_MEMO_PREFIX}only-one-part")).is_none());
        assert!(
            parse_withdrawal_memo(&format!("{WITHDRAWAL_MEMO_PREFIX}:{}", Uuid::new_v4()))
                .is_none()
        );
        assert!(parse_withdrawal_memo(&format!(
            "{WITHDRAWAL_MEMO_PREFIX}{}:not-a-uuid",
            buyer.agent_id().pubkey_base58()
        ))
        .is_none());
    }
}
