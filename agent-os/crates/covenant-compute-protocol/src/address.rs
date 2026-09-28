//! Solana-address shape validation, shared by every path that will
//! eventually hand a string to a transfer. A malformed address caught
//! where it enters — a signed withdrawal, an unbond, a node's own
//! payout config — is a clear refusal to its owner; caught at the
//! transfer push it is work already done for money that can't land,
//! retried forever by a sweep that can't fix a typo.

use crate::sign::ProtocolError;

/// Checks that `address_b58` decodes as base58 to exactly 32 bytes —
/// the shape of every Solana account key (wallets, ATAs and PDAs
/// alike; on-curve-ness is deliberately not checked). `what` names the
/// field for the refusal, so the message reads in the caller's terms:
/// "payout address is not base58", "recipient address decodes to 31
/// bytes, not a 32-byte key".
pub fn validate_address_b58(what: &str, address_b58: &str) -> Result<(), ProtocolError> {
    let decoded = bs58::decode(address_b58)
        .into_vec()
        .map_err(|e| ProtocolError::Invalid(format!("{what} is not base58: {e}")))?;
    if decoded.len() != 32 {
        return Err(ProtocolError::Invalid(format!(
            "{what} decodes to {} bytes, not a 32-byte key",
            decoded.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_key_shape_passes() {
        validate_address_b58(
            "payout address",
            "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
        )
        .unwrap();
    }

    #[test]
    fn refusals_name_the_field_and_the_defect() {
        let not_b58 = validate_address_b58("payout address", "not-base58!")
            .unwrap_err()
            .to_string();
        assert!(
            not_b58.contains("payout address is not base58"),
            "{not_b58}"
        );

        let short = validate_address_b58("payout address", "abc")
            .unwrap_err()
            .to_string();
        assert!(
            short.contains("not a 32-byte key"),
            "a wrong-width decode must say so: {short}"
        );

        // The classic operator typo: a truncated paste of a real key
        // still decodes as base58, just not to 32 bytes.
        let truncated = validate_address_b58(
            "payout address",
            "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRST",
        )
        .unwrap_err()
        .to_string();
        assert!(truncated.contains("not a 32-byte key"), "{truncated}");
    }
}
