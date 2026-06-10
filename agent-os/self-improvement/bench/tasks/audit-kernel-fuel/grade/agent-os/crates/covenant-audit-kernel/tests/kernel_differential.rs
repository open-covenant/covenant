//! Held-out differential gate: the candidate kernel must structurally match
//! a frozen copy of the original implementation over adversarial corpora.
//! Vendored reference below — never edit it; it IS the behavioral contract.

#![allow(clippy::all)]

mod reference {
    use covenant_audit_kernel::{ChainEntry, ChainReport, Failure, ZERO_CHAIN_HASH};
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

use covenant_audit_kernel as candidate;
use covenant_audit_kernel::{ChainEntry, ZERO_CHAIN_HASH};

fn anchors_json(entries: &[ChainEntry]) -> Vec<u8> {
    let mut out = String::new();
    for e in entries {
        out.push_str(&serde_json::to_string(e).unwrap());
        out.push('\n');
    }
    out.into_bytes()
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn hex(&mut self, n: usize) -> String {
        (0..n).map(|_| char::from_digit((self.next() % 16) as u32, 16).unwrap()).collect()
    }
}

fn event(rng: &mut Lcg) -> String {
    format!(
        r#"{{"id":"{}-{}-4{}-a{}-{}","timestamp_ms":{},"issuer":"agent-{}","kind":{{"type":"intent_dispatched","intent_id":"00000000-0000-4000-a000-000000000000","intent_text":"t{}","matched_agent":null,"result_hash_hex":"{}","status":"ok"}}}}"#,
        rng.hex(8), rng.hex(4), rng.hex(3), rng.hex(3), rng.hex(12),
        1700000000000u64 + rng.next() % 100000,
        rng.hex(6), rng.hex(8), rng.hex(64),
    )
}

fn clean(n: usize, seed: u64) -> (Vec<u8>, Vec<u8>) {
    let mut rng = Lcg(seed);
    let lines: Vec<String> = (0..n).map(|_| event(&mut rng)).collect();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_bytes()).collect();
    let entries = reference::fold_chain(&refs);
    let mut events = lines.join("\n").into_bytes();
    events.push(b'\n');
    (events, anchors_json(&entries))
}

fn assert_same(name: &str, events: &[u8], anchors: &[u8]) {
    let c = candidate::verify_chain(events, anchors);
    let r = reference::verify_chain(events, anchors);
    assert_eq!(c, r, "verify_chain diverged from reference on corpus: {name}");
}

#[test]
fn differential_clean_chains() {
    for (n, seed) in [(0usize, 1u64), (1, 2), (3, 3), (50, 4), (250, 5)] {
        let (ev, an) = clean(n, seed);
        let report = candidate::verify_chain(&ev, &an);
        assert!(report.valid, "clean chain n={n} must verify");
        assert_same(&format!("clean-{n}"), &ev, &an);
    }
}

#[test]
fn differential_tampered_events() {
    let (ev, an) = clean(20, 10);
    let text = String::from_utf8(ev.clone()).unwrap();
    for (name, tampered) in [
        ("tamper-middle", text.replacen("\"status\":\"ok\"", "\"status\":\"forged\"", 1)),
        ("tamper-case", text.replacen("intent_dispatched", "INTENT_DISPATCHED", 1)),
        ("dup-first-line", {
            let first = text.lines().next().unwrap().to_string();
            format!("{first}\n{text}")
        }),
        ("drop-first-line", text.lines().skip(1).collect::<Vec<_>>().join("\n")),
    ] {
        assert_same(name, tampered.as_bytes(), &an);
        let report = candidate::verify_chain(tampered.as_bytes(), &an);
        assert!(!report.valid, "{name} must fail verification");
    }
}

#[test]
fn differential_tampered_anchors() {
    let (ev, an) = clean(20, 11);
    let text = String::from_utf8(an.clone()).unwrap();
    let cases = [
        ("anchor-chain-flip", text.replacen("\"chain_hash_hex\":\"", "\"chain_hash_hex\":\"f", 1)),
        ("anchor-index-shift", text.replacen("\"index\":3", "\"index\":13", 1)),
        ("anchor-ts-shift", text.replacen("\"timestamp_ms\":17", "\"timestamp_ms\":99", 1)),
        ("anchor-truncated", text.lines().take(10).collect::<Vec<_>>().join("\n")),
        ("anchor-dangling", format!("{text}{}", text.lines().last().unwrap())),
        ("anchor-garbage-line", text.replacen("{\"index\":5", "{garbage", 1)),
        ("anchor-wrong-schema", {
            let mut lines: Vec<&str> = text.lines().collect();
            lines[7] = r#"{"index":7,"event_id":"x"}"#;
            lines.join("\n")
        }),
    ];
    for (name, tampered) in cases {
        assert_same(name, &ev, tampered.as_bytes());
    }
}

#[test]
fn differential_uuid_case_insensitive() {
    let (ev, an) = clean(5, 12);
    let upper = String::from_utf8(an).unwrap();
    let mut lines: Vec<String> = upper.lines().map(str::to_string).collect();
    let m = lines[2].find("\"event_id\":\"").unwrap() + 12;
    let id: String = lines[2][m..m + 36].to_uppercase();
    lines[2].replace_range(m..m + 36, &id);
    let an2 = lines.join("\n");
    assert_same("uuid-upper-anchor", &ev, an2.as_bytes());
    assert!(candidate::verify_chain(&ev, an2.as_bytes()).valid);
}

#[test]
fn differential_malformed_and_binary() {
    let (ev, an) = clean(10, 13);
    let text = String::from_utf8(ev).unwrap();
    let mut with_garbage: Vec<String> = text.lines().map(str::to_string).collect();
    with_garbage[4] = "{broken json".into();
    with_garbage[7] = "   ".into();
    let ev2 = with_garbage.join("\n");
    assert_same("malformed-mixed", ev2.as_bytes(), &an);

    let mut binary = ev2.into_bytes();
    binary.extend_from_slice(b"\n\xff\xfe\x80 binary tail\n");
    assert_same("binary-tail", &binary, &an);

    let all_garbage = b"not json\n\xff\xff\n{\n".to_vec();
    assert_same("all-garbage-no-anchors", &all_garbage, b"");

    let garbage_lines: Vec<&[u8]> = vec![b"not json", &[0xff, 0xff], b"{"];
    let entries = reference::fold_chain(&garbage_lines);
    let an3 = anchors_json(&entries);
    assert_same("garbage-with-matching-anchors", &all_garbage, &an3);
    let report = candidate::verify_chain(&all_garbage, &an3);
    assert_eq!(report.root_hash_hex, entries[2].chain_hash_hex);
}

#[test]
fn differential_field_requirements_and_shapes() {
    let (_, an) = clean(1, 14);
    for (name, line) in [
        ("missing-id", r#"{"timestamp_ms":1,"issuer":"a","kind":{}}"#),
        ("missing-kind", r#"{"id":"00000000-0000-4000-a000-000000000000","timestamp_ms":1,"issuer":"a"}"#),
        ("ts-not-number", r#"{"id":"00000000-0000-4000-a000-000000000000","timestamp_ms":"1","issuer":"a","kind":{}}"#),
        ("kind-any-shape", r#"{"id":"00000000-0000-4000-a000-000000000000","timestamp_ms":1,"issuer":{"deep":[1,2]},"kind":"weird"}"#),
    ] {
        let ev = format!("{line}\n");
        assert_same(name, ev.as_bytes(), &an);
    }
}

#[test]
fn differential_line_endings_and_huge_lines() {
    let (ev, an) = clean(8, 15);
    let crlf = String::from_utf8(ev.clone()).unwrap().replace('\n', "\r\n");
    assert_same("crlf", crlf.as_bytes(), &an);
    assert!(candidate::verify_chain(crlf.as_bytes(), &an).valid);

    let blanks = String::from_utf8(ev).unwrap().replace('\n', "\n\n\n");
    assert_same("blank-lines", blanks.as_bytes(), &an);

    let mut rng = Lcg(16);
    let huge = format!(
        r#"{{"id":"{}-{}-4{}-a{}-{}","timestamp_ms":7,"issuer":"a","kind":{{"type":"intent_dispatched","intent_id":"00000000-0000-4000-a000-000000000000","intent_text":"{}","matched_agent":null,"result_hash_hex":"{}","status":"ok"}}}}"#,
        rng.hex(8), rng.hex(4), rng.hex(3), rng.hex(3), rng.hex(12),
        "x".repeat(100_000), rng.hex(64),
    );
    let refs: Vec<&[u8]> = vec![huge.as_bytes()];
    let entries = reference::fold_chain(&refs);
    let ev2 = format!("{huge}\n");
    assert_same("huge-line", ev2.as_bytes(), &anchors_json(&entries));
    assert!(candidate::verify_chain(ev2.as_bytes(), &anchors_json(&entries)).valid);
}

#[test]
fn differential_fold_chain() {
    let mut rng = Lcg(17);
    let lines: Vec<String> = (0..30).map(|_| event(&mut rng)).collect();
    let mut refs: Vec<&[u8]> = lines.iter().map(|l| l.as_bytes()).collect();
    refs.push(b"not an event");
    let c = candidate::fold_chain(&refs);
    let r = reference::fold_chain(&refs);
    assert_eq!(c, r, "fold_chain diverged from reference");
    assert_eq!(c[0].previous_hash_hex, ZERO_CHAIN_HASH);
    assert_eq!(c.last().unwrap().event_id, "");
}

#[test]
fn differential_escaped_strings() {
    // JSON escapes force the fast scanners to bail to serde; candidate and
    // reference must classify and extract identically either way.
    let esc = r#"{"id":"00000000-0000-4000-a000-000000000000","timestamp_ms":5,"issuer":"agent-\"q\\u00e9\"","kind":{"type":"intent_dispatched","intent_id":"00000000-0000-4000-a000-000000000000","intent_text":"say \"hi\"\nback\\slash A","matched_agent":null,"result_hash_hex":"00","status":"ok"}}"#;
    let esc_id = r#"{"id":"66f9619-8b86-4011-a42d-00cf4fc964ff","timestamp_ms":6,"issuer":"a","kind":{}}"#;
    for (name, line) in [("escaped-kind", esc), ("escaped-id", esc_id)] {
        let refs: Vec<&[u8]> = vec![line.as_bytes()];
        let entries = reference::fold_chain(&refs);
        let ev = format!("{line}\n");
        let an = anchors_json(&entries);
        assert_same(name, ev.as_bytes(), &an);
        let c = candidate::fold_chain(&refs);
        assert_eq!(c, entries, "fold_chain diverged on {name}");
    }
}
