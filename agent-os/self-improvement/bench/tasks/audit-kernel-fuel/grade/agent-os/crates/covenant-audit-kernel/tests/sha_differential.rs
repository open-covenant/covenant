//! The kernel's hash output must equal the sha2 crate bit-for-bit on every
//! input length. covenant-audit's record path hashes with sha2 while verify
//! delegates to the kernel: any divergence means false tamper alarms on
//! legitimate logs, so this pins all padding residues, block boundaries, and
//! multi-block inputs through the public API.

use covenant_audit_kernel::fold_chain;
use sha2::{Digest, Sha256};

fn reference_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn assert_hash_matches(line: &[u8]) {
    let entries = fold_chain(&[line]);
    assert_eq!(
        entries[0].event_hash_hex,
        reference_hex(line),
        "kernel sha256 diverged from the sha2 crate on a {}-byte input",
        line.len()
    );
    let material = format!(
        "{}\n{}",
        entries[0].previous_hash_hex, entries[0].event_hash_hex
    );
    assert_eq!(
        entries[0].chain_hash_hex,
        reference_hex(material.as_bytes()),
        "kernel chain hash diverged on the link material"
    );
}

#[test]
fn matches_sha2_crate_on_every_length_through_nine_blocks() {
    let mut state = 0x243f6a8885a308d3u64;
    let mut byte = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) & 0xff) as u8
    };
    for len in 0..=576 {
        let line: Vec<u8> = (0..len).map(|_| byte()).collect();
        assert_hash_matches(&line);
    }
}

#[test]
fn matches_sha2_crate_on_large_inputs() {
    for len in [4096usize, 65_536, 1_000_000, 1_000_037] {
        let line: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        assert_hash_matches(&line);
    }
}

#[test]
fn matches_sha2_crate_on_all_byte_values() {
    let line: Vec<u8> = (0..=255u8).collect();
    assert_hash_matches(&line);
    let line: Vec<u8> = (0..=255u8).rev().cycle().take(8192).collect();
    assert_hash_matches(&line);
}
