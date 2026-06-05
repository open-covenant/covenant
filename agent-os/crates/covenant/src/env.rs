//! Ecosystem-alignment env-var helpers for the covenant CLI.
//!
//! `NO_DNA=1` is the signal a non-human caller emits to declare "I am
//! an agent or CI script, not a person at a TTY." When set, the CLI
//! flips its `--json` default from `false` to `true` so structured
//! output is the path of least resistance; passing `--json` explicitly
//! still works and is idempotent. Any other value (including unset,
//! "0", "true", or "yes") leaves the human-readable default in place
//! — the contract is exactly `NO_DNA=1`, matching the convention used
//! by the upstream Solana skills.

pub(crate) fn no_dna_active() -> bool {
    no_dna_active_from(std::env::var("NO_DNA").ok().as_deref())
}

fn no_dna_active_from(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dna_active_from_recognises_only_the_canonical_one() {
        assert!(no_dna_active_from(Some("1")));
        assert!(!no_dna_active_from(None));
        assert!(!no_dna_active_from(Some("")));
        assert!(!no_dna_active_from(Some("0")));
        assert!(!no_dna_active_from(Some("true")));
        assert!(!no_dna_active_from(Some("TRUE")));
        assert!(!no_dna_active_from(Some("yes")));
        assert!(!no_dna_active_from(Some(" 1")));
        assert!(!no_dna_active_from(Some("11")));
        assert!(!no_dna_active_from(Some("01")));
    }
}
