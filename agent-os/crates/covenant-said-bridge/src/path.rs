use crate::{BridgeError, Result};

const PUBKEY_MIN: usize = 32;
const PUBKEY_MAX: usize = 44;

pub(crate) fn validate_pubkey(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(BridgeError::Invalid(format!("{label} must not be empty")));
    }
    if value.len() < PUBKEY_MIN || value.len() > PUBKEY_MAX {
        return Err(BridgeError::Invalid(format!(
            "{label} length {} is outside base58 range {PUBKEY_MIN}..={PUBKEY_MAX}",
            value.len()
        )));
    }
    if !value.chars().all(is_base58) {
        return Err(BridgeError::Invalid(format!(
            "{label} contains non-base58 characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_chain(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(BridgeError::Invalid("chain must not be empty".into()));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(BridgeError::Invalid(format!(
            "chain {value:?} must be ascii [a-z0-9_-]"
        )));
    }
    Ok(())
}

fn is_base58(c: char) -> bool {
    matches!(
        c,
        '1'..='9'
            | 'A'..='H'
            | 'J'..='N'
            | 'P'..='Z'
            | 'a'..='k'
            | 'm'..='z'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_separators() {
        assert!(validate_pubkey("addr", "AdChc/../etc").is_err());
        assert!(validate_pubkey("addr", "AdChc?admin=1").is_err());
        assert!(validate_pubkey("addr", "AdChc#frag").is_err());
        assert!(validate_chain("solana/../admin").is_err());
        assert!(validate_chain("solana?x=1").is_err());
    }

    #[test]
    fn accepts_real_wallet() {
        validate_pubkey("addr", "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb").unwrap();
    }

    #[test]
    fn rejects_o_zero_l_lowercase_i_and_zero() {
        assert!(validate_pubkey("addr", &"1".repeat(31)).is_err());
        assert!(validate_pubkey("addr", &"1".repeat(45)).is_err());
        assert!(validate_pubkey("addr", &format!("0{}", "1".repeat(31))).is_err());
        assert!(validate_pubkey("addr", &format!("O{}", "1".repeat(31))).is_err());
        assert!(validate_pubkey("addr", &format!("I{}", "1".repeat(31))).is_err());
        assert!(validate_pubkey("addr", &format!("l{}", "1".repeat(31))).is_err());
    }

    #[test]
    fn accepts_known_chains() {
        for c in ["solana", "base", "polygon", "avalanche", "sei", "iotex"] {
            validate_chain(c).unwrap();
        }
    }
}
