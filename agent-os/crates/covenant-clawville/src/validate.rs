//! Input validation for everything that crosses the trust boundary into a
//! verdict or a capability grant. ClawVille bounties are agent-authored and
//! agent-submitted, so every identifier and free-text field is untrusted.
//!
//! Mirrors the on-chain-string hardening in `covenant-metaplex`: reject
//! control and bidirectional / zero-width Trojan-Source characters
//! (CVE-2021-42574) that survive `is_control()` but can make a recorded
//! bounty id, action label, or verdict render as something other than its
//! bytes.

/// Max bytes for any free-text identifier we record (bounty id, action
/// label, criterion needle/pointer).
pub const FIELD_MAX_LEN: usize = 256;

pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{2028}'..='\u{2029}' // line + paragraph separators
            | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
            | '\u{2060}'..='\u{2064}' // word joiner + invisible operators
            | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
            | '\u{FEFF}'              // BOM / zero-width no-break space
            | '\u{061C}',             // Arabic letter mark
        )
}

/// A bounded, control-free, bidi-free identifier or label.
pub fn field(name: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if s.len() > FIELD_MAX_LEN {
        return Err(format!(
            "{name} must be at most {FIELD_MAX_LEN} bytes, got {}",
            s.len()
        ));
    }
    if s.chars().any(is_unsafe_char) {
        return Err(format!(
            "{name} must not contain control characters or bidirectional/zero-width formatting"
        ));
    }
    Ok(())
}

/// A 32-byte hash in canonical wire form: exactly 64 lowercase hex chars.
pub fn hash_hex(name: &str, s: &str) -> Result<(), String> {
    if s.len() != 64 {
        return Err(format!("{name} must be 64 hex characters, got {}", s.len()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(format!("{name} must be lowercase ASCII hex only"));
    }
    Ok(())
}

/// Base58 charset + length sanity for a Solana pubkey (agent / wallet id).
/// `solana-sdk`-free, like the daemon-side metaplex validator.
pub fn pubkey(name: &str, s: &str) -> Result<(), String> {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if !(32..=44).contains(&s.len()) {
        return Err(format!(
            "{name} must be a base58 pubkey of 32-44 chars, got {}",
            s.len()
        ));
    }
    if !s.bytes().all(|b| BASE58.contains(&b)) {
        return Err(format!("{name} must be base58 (no 0, O, I, l)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_rejects_trojan_source_and_overlong() {
        field("id", "bounty-42").unwrap();
        assert!(field("id", "").is_err(), "empty");
        assert!(field("id", &"x".repeat(257)).is_err(), "overlong");
        for bad in [
            "a\u{202e}b",
            "a\u{200b}b",
            "a\u{2028}b",
            "a\u{feff}",
            "a\u{0007}",
        ] {
            assert!(field("id", bad).is_err(), "unsafe char in {bad:?}");
        }
    }

    #[test]
    fn hash_hex_is_64_lowercase() {
        hash_hex("root", &"a".repeat(64)).unwrap();
        assert!(hash_hex("root", "deadbeef").is_err(), "short");
        assert!(hash_hex("root", &"A".repeat(64)).is_err(), "uppercase");
        assert!(hash_hex("root", &"g".repeat(64)).is_err(), "non-hex");
    }

    #[test]
    fn pubkey_is_base58_bounded() {
        pubkey("worker", "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH").unwrap();
        assert!(pubkey("w", "deadbeef").is_err(), "short");
        assert!(pubkey("w", &"1".repeat(45)).is_err(), "long");
        assert!(pubkey("w", &"0".repeat(40)).is_err(), "0 not base58");
    }
}
