//! Pure audit chain kernel: per-line sha256 hashing, chain linking, and
//! anchor verification over raw JSONL bytes. Extracted from covenant-audit
//! so the hot path is sync, IO-free, and wasm-compilable for deterministic
//! fuel metering.
//!
//! Behavioral notes vs the original `JsonlAuditLog::verify_integrity`:
//! - Operates on bytes, so a non-UTF8 or schema-invalid event line becomes a
//!   per-line `ParseError` instead of failing the whole read. Anchor lines
//!   that fail to parse become `AnchorParseError` (the original aborted with
//!   a serde error).
//! - Event lines are checked for the four required `AuditEvent` fields
//!   (`id`, `timestamp_ms`, `issuer`, `kind`) but `kind`'s enum tag is not
//!   validated; covenant-audit's typed parse stays authoritative at the API
//!   boundary.
//! - Failures are structured kinds; message formatting stays in
//!   covenant-audit.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

pub const ZERO_CHAIN_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainEntry {
    pub index: u64,
    pub event_id: String,
    pub timestamp_ms: u64,
    pub event_hash_hex: String,
    pub previous_hash_hex: String,
    pub chain_hash_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    LengthMismatch { events: u64, anchors: u64 },
    ParseError { index: u64 },
    EntryMismatch { index: u64 },
    EntryMissing { index: u64 },
    AnchorParseError { index: u64 },
    DanglingAnchors { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainReport {
    pub events: u64,
    pub anchors: u64,
    pub valid: bool,
    pub root_hash_hex: String,
    pub failures: Vec<Failure>,
}

/// Verify an events JSONL byte stream against its anchors JSONL byte stream.
/// Lines split on `\n` with one trailing `\r` stripped, empty lines skipped,
/// mirroring `str::lines` in the original.
pub fn verify_chain(events_jsonl: &[u8], anchors_jsonl: &[u8]) -> ChainReport {
    imp::verify_chain(events_jsonl, anchors_jsonl)
}

/// Fold pre-serialized event lines into chain entries. Lines that do not
/// parse as events yield entries with an empty `event_id` and zero
/// `timestamp_ms`; production serializes events immediately before folding,
/// so that path is unreachable there.
pub fn fold_chain(lines: &[&[u8]]) -> Vec<ChainEntry> {
    imp::fold_chain(lines)
}

// EVOLVE-BLOCK-START
mod imp {
    use super::{ChainEntry, ChainReport, Failure, ZERO_CHAIN_HASH};
    use serde::Deserialize;
    use sha2::{Digest, Sha256};

    #[derive(Deserialize)]
    struct EventFields {
        id: String,
        timestamp_ms: u64,
        #[allow(dead_code)]
        issuer: serde_json::Value,
        #[allow(dead_code)]
        kind: serde_json::Value,
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut out, "{byte:02x}").expect("write to string");
        }
        out
    }

    fn chain_hash(previous_hash_hex: &str, event_hash_hex: &str) -> String {
        let material = format!("{previous_hash_hex}\n{event_hash_hex}");
        sha256_hex(material.as_bytes())
    }

    fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
        bytes
            .split(|b| *b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .filter(|line| !line.is_empty())
            .collect()
    }

    fn uuid_eq(a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }

    fn entries_match(actual: &ChainEntry, expected: &ChainEntry) -> bool {
        actual.index == expected.index
            && uuid_eq(&actual.event_id, &expected.event_id)
            && actual.timestamp_ms == expected.timestamp_ms
            && actual.event_hash_hex == expected.event_hash_hex
            && actual.previous_hash_hex == expected.previous_hash_hex
            && actual.chain_hash_hex == expected.chain_hash_hex
    }

    pub fn verify_chain(events_jsonl: &[u8], anchors_jsonl: &[u8]) -> ChainReport {
        let event_lines = split_lines(events_jsonl);
        let anchor_lines = split_lines(anchors_jsonl);
        let mut failures = Vec::new();

        let mut anchors: Vec<Option<ChainEntry>> = Vec::with_capacity(anchor_lines.len());
        for (index, line) in anchor_lines.iter().enumerate() {
            match serde_json::from_slice::<ChainEntry>(line) {
                Ok(entry) => anchors.push(Some(entry)),
                Err(_) => {
                    failures.push(Failure::AnchorParseError {
                        index: index as u64,
                    });
                    anchors.push(None);
                }
            }
        }

        if anchors.len() != event_lines.len() {
            failures.push(Failure::LengthMismatch {
                events: event_lines.len() as u64,
                anchors: anchors.len() as u64,
            });
        }

        let mut previous_hash_hex = ZERO_CHAIN_HASH.to_string();
        for (index, line) in event_lines.iter().enumerate() {
            let event_hash_hex = sha256_hex(line);
            let chain_hash_hex = chain_hash(&previous_hash_hex, &event_hash_hex);
            match serde_json::from_slice::<EventFields>(line) {
                Ok(event) => {
                    let expected = ChainEntry {
                        index: index as u64,
                        event_id: event.id,
                        timestamp_ms: event.timestamp_ms,
                        event_hash_hex,
                        previous_hash_hex: previous_hash_hex.clone(),
                        chain_hash_hex: chain_hash_hex.clone(),
                    };
                    match anchors.get(index) {
                        Some(Some(actual)) if entries_match(actual, &expected) => {}
                        Some(_) => failures.push(Failure::EntryMismatch {
                            index: index as u64,
                        }),
                        None => failures.push(Failure::EntryMissing {
                            index: index as u64,
                        }),
                    }
                }
                Err(_) => {
                    failures.push(Failure::ParseError {
                        index: index as u64,
                    });
                    match anchors.get(index) {
                        Some(Some(actual))
                            if actual.index == index as u64
                                && actual.event_hash_hex == event_hash_hex
                                && actual.previous_hash_hex == previous_hash_hex
                                && actual.chain_hash_hex == chain_hash_hex => {}
                        Some(_) => failures.push(Failure::EntryMismatch {
                            index: index as u64,
                        }),
                        None => failures.push(Failure::EntryMissing {
                            index: index as u64,
                        }),
                    }
                }
            }
            previous_hash_hex = chain_hash_hex;
        }

        if anchors.len() > event_lines.len() {
            failures.push(Failure::DanglingAnchors {
                count: (anchors.len() - event_lines.len()) as u64,
            });
        }

        ChainReport {
            events: event_lines.len() as u64,
            anchors: anchors.len() as u64,
            valid: failures.is_empty(),
            root_hash_hex: previous_hash_hex,
            failures,
        }
    }

    pub fn fold_chain(lines: &[&[u8]]) -> Vec<ChainEntry> {
        let mut previous = ZERO_CHAIN_HASH.to_string();
        let mut entries = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let event_hash_hex = sha256_hex(line);
            let chain_hash_hex = chain_hash(&previous, &event_hash_hex);
            let (event_id, timestamp_ms) = match serde_json::from_slice::<EventFields>(line) {
                Ok(event) => (event.id, event.timestamp_ms),
                Err(_) => (String::new(), 0),
            };
            entries.push(ChainEntry {
                index: index as u64,
                event_id,
                timestamp_ms,
                event_hash_hex,
                previous_hash_hex: previous.clone(),
                chain_hash_hex: chain_hash_hex.clone(),
            });
            previous = chain_hash_hex;
        }
        entries
    }
}
// EVOLVE-BLOCK-END

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = r#"{"id":"6f9619ff-8b86-d011-b42d-00cf4fc964ff","timestamp_ms":42,"issuer":"agent-a","kind":{"type":"intent_dispatched","intent_id":"6f9619ff-8b86-d011-b42d-00cf4fc964ff","intent_text":"t","matched_agent":null,"result_hash_hex":"00","status":"ok"}}"#;

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn nist_vector_abc() {
        // pinned by covenant-audit too; the chain hash composition depends on it
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn empty_inputs_valid_zero_root() {
        let report = verify_chain(b"", b"");
        assert!(report.valid);
        assert_eq!(report.root_hash_hex, ZERO_CHAIN_HASH);
        assert_eq!(report.events, 0);
        assert_eq!(report.anchors, 0);
    }

    #[test]
    fn fold_then_verify_round_trips() {
        let lines: Vec<&[u8]> = vec![EVENT.as_bytes(), EVENT.as_bytes()];
        let entries = fold_chain(&lines);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].previous_hash_hex, ZERO_CHAIN_HASH);
        assert_eq!(entries[1].previous_hash_hex, entries[0].chain_hash_hex);

        let events = format!("{EVENT}\n{EVENT}\n");
        let anchors = entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let report = verify_chain(events.as_bytes(), anchors.as_bytes());
        assert!(report.valid, "failures: {:?}", report.failures);
        assert_eq!(report.root_hash_hex, entries[1].chain_hash_hex);
    }

    #[test]
    fn chain_link_uses_newline_separator() {
        let lines: Vec<&[u8]> = vec![EVENT.as_bytes()];
        let entries = fold_chain(&lines);
        let event_hash = sha256_hex(EVENT.as_bytes());
        let material = format!("{ZERO_CHAIN_HASH}\n{event_hash}");
        assert_eq!(entries[0].event_hash_hex, event_hash);
        assert_eq!(entries[0].chain_hash_hex, sha256_hex(material.as_bytes()));
    }

    #[test]
    fn tampered_event_detected() {
        let lines: Vec<&[u8]> = vec![EVENT.as_bytes()];
        let entries = fold_chain(&lines);
        let anchors = serde_json::to_string(&entries[0]).unwrap();
        let tampered = EVENT.replace("\"timestamp_ms\":42", "\"timestamp_ms\":43");
        let report = verify_chain(tampered.as_bytes(), anchors.as_bytes());
        assert!(!report.valid);
        assert_eq!(report.failures, vec![Failure::EntryMismatch { index: 0 }]);
    }

    #[test]
    fn malformed_event_line_with_matching_hashes_passes_hash_check() {
        let garbage = b"not json at all".as_slice();
        let entries = fold_chain(&[garbage]);
        let anchors = serde_json::to_string(&entries[0]).unwrap();
        let report = verify_chain(garbage, anchors.as_bytes());
        assert!(!report.valid);
        assert_eq!(report.failures, vec![Failure::ParseError { index: 0 }]);
        assert_eq!(report.root_hash_hex, entries[0].chain_hash_hex);
    }

    #[test]
    fn non_utf8_event_line_is_parse_error_not_abort() {
        let line: &[u8] = &[0xff, 0xfe, 0x01];
        let entries = fold_chain(&[line]);
        let anchors = serde_json::to_string(&entries[0]).unwrap();
        let mut events = line.to_vec();
        events.push(b'\n');
        let report = verify_chain(&events, anchors.as_bytes());
        assert_eq!(report.failures, vec![Failure::ParseError { index: 0 }]);
    }

    #[test]
    fn missing_and_dangling_anchors() {
        let events = format!("{EVENT}\n");
        let report = verify_chain(events.as_bytes(), b"");
        assert!(report
            .failures
            .contains(&Failure::LengthMismatch { events: 1, anchors: 0 }));
        assert!(report.failures.contains(&Failure::EntryMissing { index: 0 }));

        let lines: Vec<&[u8]> = vec![EVENT.as_bytes()];
        let entries = fold_chain(&lines);
        let anchors = serde_json::to_string(&entries[0]).unwrap();
        let report = verify_chain(b"", anchors.as_bytes());
        assert!(report.failures.contains(&Failure::DanglingAnchors { count: 1 }));
    }
}
