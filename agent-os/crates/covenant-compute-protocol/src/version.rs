//! The wire version every compute party declares — the deploy-skew
//! defense. Once the coordinator runs as a long-lived service and node
//! binaries ship to operators who upgrade on their own schedule, the
//! two sides of every call can be built from different commits. A
//! breaking wire change must then be refusable BY NAME ("your node
//! speaks v1, this coordinator requires v2 — upgrade") instead of
//! surfacing as a deserialization error three layers deep. That
//! refusal is only possible if clients already declare a version
//! before the first breaking change lands, which is why this exists
//! while there is nothing yet to refuse.
//!
//! Clients stamp [`PROTOCOL_VERSION_HEADER`] on every coordinator
//! call; the coordinator stamps its own version on every response and
//! refuses `/federation/*` requests declaring less than its configured
//! floor (426, both numbers in the body). A request with no header
//! counts as version 0 — the reserved "predates versioning" value no
//! client ever declares — so bare curl and pre-versioning binaries
//! keep working until a deployment raises the floor.

/// The wire version this build speaks. Bumped ONLY on a breaking wire
/// change (a field removed or re-typed, a signing domain changed, a
/// route's semantics altered) — additive, serde-defaulted growth keeps
/// the version. History: 1 = the launch protocol.
pub const PROTOCOL_VERSION: u32 = 1;

/// The header carrying [`PROTOCOL_VERSION`] on requests and responses.
pub const PROTOCOL_VERSION_HEADER: &str = "x-compute-protocol";

// The coordinator reads "no header" as 0; a build that shipped as
// version 0 would be indistinguishable from one that predates
// versioning entirely.
const _: () = assert!(PROTOCOL_VERSION >= 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_name_is_a_valid_lowercase_http_token() {
        // http's HeaderName::from_static panics on uppercase or
        // non-token bytes; every party builds the name from this
        // constant, so pin the invariant where the constant lives.
        assert!(!PROTOCOL_VERSION_HEADER.is_empty());
        assert!(PROTOCOL_VERSION_HEADER
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
    }
}
