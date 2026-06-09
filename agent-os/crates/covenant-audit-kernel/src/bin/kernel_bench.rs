//! Fuel-bench entry point. Reads a binary corpus container from stdin:
//! u32le case count, then per case u32le events-len, events bytes, u32le
//! anchors-len, anchors bytes. Runs verify_chain on every case and prints a
//! sha256 digest of all reports so the work cannot be dead-code-eliminated
//! and behavioral drift across kernel variants shows up as a digest change.

use covenant_audit_kernel::{verify_chain, Failure};
use sha2::{Digest, Sha256};
use std::io::Read;

fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().expect("u32"));
    *pos += 4;
    v
}

fn read_blob<'a>(buf: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let len = read_u32(buf, pos) as usize;
    let blob = &buf[*pos..*pos + len];
    *pos += len;
    blob
}

fn main() {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).expect("read corpus");
    let mut pos = 0;
    let cases = read_u32(&input, &mut pos);
    let mut digest = Sha256::new();
    for _ in 0..cases {
        let events = read_blob(&input, &mut pos);
        let anchors = read_blob(&input, &mut pos);
        let report = verify_chain(events, anchors);
        digest.update(report.events.to_le_bytes());
        digest.update(report.anchors.to_le_bytes());
        digest.update([report.valid as u8]);
        digest.update(report.root_hash_hex.as_bytes());
        for failure in &report.failures {
            let (tag, a, b) = match failure {
                Failure::LengthMismatch { events, anchors } => (1u8, *events, *anchors),
                Failure::ParseError { index } => (2, *index, 0),
                Failure::EntryMismatch { index } => (3, *index, 0),
                Failure::EntryMissing { index } => (4, *index, 0),
                Failure::AnchorParseError { index } => (5, *index, 0),
                Failure::DanglingAnchors { count } => (6, *count, 0),
            };
            digest.update([tag]);
            digest.update(a.to_le_bytes());
            digest.update(b.to_le_bytes());
        }
    }
    let out = digest.finalize();
    let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
    println!("DIGEST {hex}");
}
