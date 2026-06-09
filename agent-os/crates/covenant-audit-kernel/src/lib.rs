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
    use serde::de::IgnoredAny;
    use serde::Deserialize;
    use std::borrow::Cow;

    const HEX_PAIRS: [[u8; 2]; 256] = {
        let mut table = [[0u8; 2]; 256];
        let digits = *b"0123456789abcdef";
        let mut i = 0;
        while i < 256 {
            table[i] = [digits[i >> 4], digits[i & 15]];
            i += 1;
        }
        table
    };

    #[derive(Deserialize)]
    struct EventFields<'a> {
        #[serde(borrow)]
        id: Cow<'a, str>,
        timestamp_ms: u64,
        #[allow(dead_code)]
        issuer: IgnoredAny,
        #[allow(dead_code)]
        kind: IgnoredAny,
    }

    /// Borrowed mirror of `ChainEntry`: same field names and value shapes, so
    /// serde accepts and rejects exactly the same lines, without owning five
    /// strings per anchor.
    #[derive(Deserialize)]
    struct AnchorFields<'a> {
        index: u64,
        #[serde(borrow)]
        event_id: Cow<'a, str>,
        timestamp_ms: u64,
        #[serde(borrow)]
        event_hash_hex: Cow<'a, str>,
        #[serde(borrow)]
        previous_hash_hex: Cow<'a, str>,
        #[serde(borrow)]
        chain_hash_hex: Cow<'a, str>,
    }

    /// SHA-256, hand-rolled: the sha2 crate's generic core costs ~5.9k fuel
    /// per block under wasmtime metering; this scalar compress with a 16-word
    /// ring schedule costs a fraction of that. Pinned by the NIST vector unit
    /// test and the frozen corpus digest.
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    /// One SHA-256 compression, fully unrolled with rotated working-variable
    /// roles and an in-place 16-word schedule: wasm has no register renaming,
    /// so a looped round with `h = g; g = f; ...` shuffles pay per-instruction
    /// fuel. Pinned by the NIST vector unit test and the frozen corpus digest.
    #[allow(clippy::too_many_lines)]
    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w0 = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        let mut w1 = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
        let mut w2 = u32::from_be_bytes([block[8], block[9], block[10], block[11]]);
        let mut w3 = u32::from_be_bytes([block[12], block[13], block[14], block[15]]);
        let mut w4 = u32::from_be_bytes([block[16], block[17], block[18], block[19]]);
        let mut w5 = u32::from_be_bytes([block[20], block[21], block[22], block[23]]);
        let mut w6 = u32::from_be_bytes([block[24], block[25], block[26], block[27]]);
        let mut w7 = u32::from_be_bytes([block[28], block[29], block[30], block[31]]);
        let mut w8 = u32::from_be_bytes([block[32], block[33], block[34], block[35]]);
        let mut w9 = u32::from_be_bytes([block[36], block[37], block[38], block[39]]);
        let mut w10 = u32::from_be_bytes([block[40], block[41], block[42], block[43]]);
        let mut w11 = u32::from_be_bytes([block[44], block[45], block[46], block[47]]);
        let mut w12 = u32::from_be_bytes([block[48], block[49], block[50], block[51]]);
        let mut w13 = u32::from_be_bytes([block[52], block[53], block[54], block[55]]);
        let mut w14 = u32::from_be_bytes([block[56], block[57], block[58], block[59]]);
        let mut w15 = u32::from_be_bytes([block[60], block[61], block[62], block[63]]);
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0x428a2f98)
            .wrapping_add(w0);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0x71374491)
            .wrapping_add(w1);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0xb5c0fbcf)
            .wrapping_add(w2);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0xe9b5dba5)
            .wrapping_add(w3);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x3956c25b)
            .wrapping_add(w4);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0x59f111f1)
            .wrapping_add(w5);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x923f82a4)
            .wrapping_add(w6);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0xab1c5ed5)
            .wrapping_add(w7);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0xd807aa98)
            .wrapping_add(w8);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0x12835b01)
            .wrapping_add(w9);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0x243185be)
            .wrapping_add(w10);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0x550c7dc3)
            .wrapping_add(w11);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x72be5d74)
            .wrapping_add(w12);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0x80deb1fe)
            .wrapping_add(w13);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x9bdc06a7)
            .wrapping_add(w14);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0xc19bf174)
            .wrapping_add(w15);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w0 = w0
            .wrapping_add(w1.rotate_right(7) ^ w1.rotate_right(18) ^ (w1 >> 3))
            .wrapping_add(w9)
            .wrapping_add(w14.rotate_right(17) ^ w14.rotate_right(19) ^ (w14 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0xe49b69c1)
            .wrapping_add(w0);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w1 = w1
            .wrapping_add(w2.rotate_right(7) ^ w2.rotate_right(18) ^ (w2 >> 3))
            .wrapping_add(w10)
            .wrapping_add(w15.rotate_right(17) ^ w15.rotate_right(19) ^ (w15 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0xefbe4786)
            .wrapping_add(w1);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w2 = w2
            .wrapping_add(w3.rotate_right(7) ^ w3.rotate_right(18) ^ (w3 >> 3))
            .wrapping_add(w11)
            .wrapping_add(w0.rotate_right(17) ^ w0.rotate_right(19) ^ (w0 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0x0fc19dc6)
            .wrapping_add(w2);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w3 = w3
            .wrapping_add(w4.rotate_right(7) ^ w4.rotate_right(18) ^ (w4 >> 3))
            .wrapping_add(w12)
            .wrapping_add(w1.rotate_right(17) ^ w1.rotate_right(19) ^ (w1 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0x240ca1cc)
            .wrapping_add(w3);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w4 = w4
            .wrapping_add(w5.rotate_right(7) ^ w5.rotate_right(18) ^ (w5 >> 3))
            .wrapping_add(w13)
            .wrapping_add(w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x2de92c6f)
            .wrapping_add(w4);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w5 = w5
            .wrapping_add(w6.rotate_right(7) ^ w6.rotate_right(18) ^ (w6 >> 3))
            .wrapping_add(w14)
            .wrapping_add(w3.rotate_right(17) ^ w3.rotate_right(19) ^ (w3 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0x4a7484aa)
            .wrapping_add(w5);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w6 = w6
            .wrapping_add(w7.rotate_right(7) ^ w7.rotate_right(18) ^ (w7 >> 3))
            .wrapping_add(w15)
            .wrapping_add(w4.rotate_right(17) ^ w4.rotate_right(19) ^ (w4 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x5cb0a9dc)
            .wrapping_add(w6);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w7 = w7
            .wrapping_add(w8.rotate_right(7) ^ w8.rotate_right(18) ^ (w8 >> 3))
            .wrapping_add(w0)
            .wrapping_add(w5.rotate_right(17) ^ w5.rotate_right(19) ^ (w5 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0x76f988da)
            .wrapping_add(w7);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w8 = w8
            .wrapping_add(w9.rotate_right(7) ^ w9.rotate_right(18) ^ (w9 >> 3))
            .wrapping_add(w1)
            .wrapping_add(w6.rotate_right(17) ^ w6.rotate_right(19) ^ (w6 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0x983e5152)
            .wrapping_add(w8);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w9 = w9
            .wrapping_add(w10.rotate_right(7) ^ w10.rotate_right(18) ^ (w10 >> 3))
            .wrapping_add(w2)
            .wrapping_add(w7.rotate_right(17) ^ w7.rotate_right(19) ^ (w7 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0xa831c66d)
            .wrapping_add(w9);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w10 = w10
            .wrapping_add(w11.rotate_right(7) ^ w11.rotate_right(18) ^ (w11 >> 3))
            .wrapping_add(w3)
            .wrapping_add(w8.rotate_right(17) ^ w8.rotate_right(19) ^ (w8 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0xb00327c8)
            .wrapping_add(w10);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w11 = w11
            .wrapping_add(w12.rotate_right(7) ^ w12.rotate_right(18) ^ (w12 >> 3))
            .wrapping_add(w4)
            .wrapping_add(w9.rotate_right(17) ^ w9.rotate_right(19) ^ (w9 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0xbf597fc7)
            .wrapping_add(w11);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w12 = w12
            .wrapping_add(w13.rotate_right(7) ^ w13.rotate_right(18) ^ (w13 >> 3))
            .wrapping_add(w5)
            .wrapping_add(w10.rotate_right(17) ^ w10.rotate_right(19) ^ (w10 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0xc6e00bf3)
            .wrapping_add(w12);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w13 = w13
            .wrapping_add(w14.rotate_right(7) ^ w14.rotate_right(18) ^ (w14 >> 3))
            .wrapping_add(w6)
            .wrapping_add(w11.rotate_right(17) ^ w11.rotate_right(19) ^ (w11 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0xd5a79147)
            .wrapping_add(w13);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w14 = w14
            .wrapping_add(w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3))
            .wrapping_add(w7)
            .wrapping_add(w12.rotate_right(17) ^ w12.rotate_right(19) ^ (w12 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x06ca6351)
            .wrapping_add(w14);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w15 = w15
            .wrapping_add(w0.rotate_right(7) ^ w0.rotate_right(18) ^ (w0 >> 3))
            .wrapping_add(w8)
            .wrapping_add(w13.rotate_right(17) ^ w13.rotate_right(19) ^ (w13 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0x14292967)
            .wrapping_add(w15);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w0 = w0
            .wrapping_add(w1.rotate_right(7) ^ w1.rotate_right(18) ^ (w1 >> 3))
            .wrapping_add(w9)
            .wrapping_add(w14.rotate_right(17) ^ w14.rotate_right(19) ^ (w14 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0x27b70a85)
            .wrapping_add(w0);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w1 = w1
            .wrapping_add(w2.rotate_right(7) ^ w2.rotate_right(18) ^ (w2 >> 3))
            .wrapping_add(w10)
            .wrapping_add(w15.rotate_right(17) ^ w15.rotate_right(19) ^ (w15 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0x2e1b2138)
            .wrapping_add(w1);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w2 = w2
            .wrapping_add(w3.rotate_right(7) ^ w3.rotate_right(18) ^ (w3 >> 3))
            .wrapping_add(w11)
            .wrapping_add(w0.rotate_right(17) ^ w0.rotate_right(19) ^ (w0 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0x4d2c6dfc)
            .wrapping_add(w2);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w3 = w3
            .wrapping_add(w4.rotate_right(7) ^ w4.rotate_right(18) ^ (w4 >> 3))
            .wrapping_add(w12)
            .wrapping_add(w1.rotate_right(17) ^ w1.rotate_right(19) ^ (w1 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0x53380d13)
            .wrapping_add(w3);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w4 = w4
            .wrapping_add(w5.rotate_right(7) ^ w5.rotate_right(18) ^ (w5 >> 3))
            .wrapping_add(w13)
            .wrapping_add(w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x650a7354)
            .wrapping_add(w4);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w5 = w5
            .wrapping_add(w6.rotate_right(7) ^ w6.rotate_right(18) ^ (w6 >> 3))
            .wrapping_add(w14)
            .wrapping_add(w3.rotate_right(17) ^ w3.rotate_right(19) ^ (w3 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0x766a0abb)
            .wrapping_add(w5);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w6 = w6
            .wrapping_add(w7.rotate_right(7) ^ w7.rotate_right(18) ^ (w7 >> 3))
            .wrapping_add(w15)
            .wrapping_add(w4.rotate_right(17) ^ w4.rotate_right(19) ^ (w4 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x81c2c92e)
            .wrapping_add(w6);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w7 = w7
            .wrapping_add(w8.rotate_right(7) ^ w8.rotate_right(18) ^ (w8 >> 3))
            .wrapping_add(w0)
            .wrapping_add(w5.rotate_right(17) ^ w5.rotate_right(19) ^ (w5 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0x92722c85)
            .wrapping_add(w7);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w8 = w8
            .wrapping_add(w9.rotate_right(7) ^ w9.rotate_right(18) ^ (w9 >> 3))
            .wrapping_add(w1)
            .wrapping_add(w6.rotate_right(17) ^ w6.rotate_right(19) ^ (w6 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0xa2bfe8a1)
            .wrapping_add(w8);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w9 = w9
            .wrapping_add(w10.rotate_right(7) ^ w10.rotate_right(18) ^ (w10 >> 3))
            .wrapping_add(w2)
            .wrapping_add(w7.rotate_right(17) ^ w7.rotate_right(19) ^ (w7 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0xa81a664b)
            .wrapping_add(w9);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w10 = w10
            .wrapping_add(w11.rotate_right(7) ^ w11.rotate_right(18) ^ (w11 >> 3))
            .wrapping_add(w3)
            .wrapping_add(w8.rotate_right(17) ^ w8.rotate_right(19) ^ (w8 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0xc24b8b70)
            .wrapping_add(w10);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w11 = w11
            .wrapping_add(w12.rotate_right(7) ^ w12.rotate_right(18) ^ (w12 >> 3))
            .wrapping_add(w4)
            .wrapping_add(w9.rotate_right(17) ^ w9.rotate_right(19) ^ (w9 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0xc76c51a3)
            .wrapping_add(w11);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w12 = w12
            .wrapping_add(w13.rotate_right(7) ^ w13.rotate_right(18) ^ (w13 >> 3))
            .wrapping_add(w5)
            .wrapping_add(w10.rotate_right(17) ^ w10.rotate_right(19) ^ (w10 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0xd192e819)
            .wrapping_add(w12);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w13 = w13
            .wrapping_add(w14.rotate_right(7) ^ w14.rotate_right(18) ^ (w14 >> 3))
            .wrapping_add(w6)
            .wrapping_add(w11.rotate_right(17) ^ w11.rotate_right(19) ^ (w11 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0xd6990624)
            .wrapping_add(w13);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w14 = w14
            .wrapping_add(w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3))
            .wrapping_add(w7)
            .wrapping_add(w12.rotate_right(17) ^ w12.rotate_right(19) ^ (w12 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0xf40e3585)
            .wrapping_add(w14);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w15 = w15
            .wrapping_add(w0.rotate_right(7) ^ w0.rotate_right(18) ^ (w0 >> 3))
            .wrapping_add(w8)
            .wrapping_add(w13.rotate_right(17) ^ w13.rotate_right(19) ^ (w13 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0x106aa070)
            .wrapping_add(w15);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w0 = w0
            .wrapping_add(w1.rotate_right(7) ^ w1.rotate_right(18) ^ (w1 >> 3))
            .wrapping_add(w9)
            .wrapping_add(w14.rotate_right(17) ^ w14.rotate_right(19) ^ (w14 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0x19a4c116)
            .wrapping_add(w0);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w1 = w1
            .wrapping_add(w2.rotate_right(7) ^ w2.rotate_right(18) ^ (w2 >> 3))
            .wrapping_add(w10)
            .wrapping_add(w15.rotate_right(17) ^ w15.rotate_right(19) ^ (w15 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0x1e376c08)
            .wrapping_add(w1);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w2 = w2
            .wrapping_add(w3.rotate_right(7) ^ w3.rotate_right(18) ^ (w3 >> 3))
            .wrapping_add(w11)
            .wrapping_add(w0.rotate_right(17) ^ w0.rotate_right(19) ^ (w0 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0x2748774c)
            .wrapping_add(w2);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w3 = w3
            .wrapping_add(w4.rotate_right(7) ^ w4.rotate_right(18) ^ (w4 >> 3))
            .wrapping_add(w12)
            .wrapping_add(w1.rotate_right(17) ^ w1.rotate_right(19) ^ (w1 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0x34b0bcb5)
            .wrapping_add(w3);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w4 = w4
            .wrapping_add(w5.rotate_right(7) ^ w5.rotate_right(18) ^ (w5 >> 3))
            .wrapping_add(w13)
            .wrapping_add(w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x391c0cb3)
            .wrapping_add(w4);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w5 = w5
            .wrapping_add(w6.rotate_right(7) ^ w6.rotate_right(18) ^ (w6 >> 3))
            .wrapping_add(w14)
            .wrapping_add(w3.rotate_right(17) ^ w3.rotate_right(19) ^ (w3 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0x4ed8aa4a)
            .wrapping_add(w5);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w6 = w6
            .wrapping_add(w7.rotate_right(7) ^ w7.rotate_right(18) ^ (w7 >> 3))
            .wrapping_add(w15)
            .wrapping_add(w4.rotate_right(17) ^ w4.rotate_right(19) ^ (w4 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0x5b9cca4f)
            .wrapping_add(w6);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w7 = w7
            .wrapping_add(w8.rotate_right(7) ^ w8.rotate_right(18) ^ (w8 >> 3))
            .wrapping_add(w0)
            .wrapping_add(w5.rotate_right(17) ^ w5.rotate_right(19) ^ (w5 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0x682e6ff3)
            .wrapping_add(w7);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        w8 = w8
            .wrapping_add(w9.rotate_right(7) ^ w9.rotate_right(18) ^ (w9 >> 3))
            .wrapping_add(w1)
            .wrapping_add(w6.rotate_right(17) ^ w6.rotate_right(19) ^ (w6 >> 10));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(0x748f82ee)
            .wrapping_add(w8);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        d = d.wrapping_add(t1);
        h = t1.wrapping_add(t2);
        w9 = w9
            .wrapping_add(w10.rotate_right(7) ^ w10.rotate_right(18) ^ (w10 >> 3))
            .wrapping_add(w2)
            .wrapping_add(w7.rotate_right(17) ^ w7.rotate_right(19) ^ (w7 >> 10));
        let t1 = g
            .wrapping_add(d.rotate_right(6) ^ d.rotate_right(11) ^ d.rotate_right(25))
            .wrapping_add((d & e) ^ (!d & f))
            .wrapping_add(0x78a5636f)
            .wrapping_add(w9);
        let t2 = (h.rotate_right(2) ^ h.rotate_right(13) ^ h.rotate_right(22))
            .wrapping_add((h & a) ^ (h & b) ^ (a & b));
        c = c.wrapping_add(t1);
        g = t1.wrapping_add(t2);
        w10 = w10
            .wrapping_add(w11.rotate_right(7) ^ w11.rotate_right(18) ^ (w11 >> 3))
            .wrapping_add(w3)
            .wrapping_add(w8.rotate_right(17) ^ w8.rotate_right(19) ^ (w8 >> 10));
        let t1 = f
            .wrapping_add(c.rotate_right(6) ^ c.rotate_right(11) ^ c.rotate_right(25))
            .wrapping_add((c & d) ^ (!c & e))
            .wrapping_add(0x84c87814)
            .wrapping_add(w10);
        let t2 = (g.rotate_right(2) ^ g.rotate_right(13) ^ g.rotate_right(22))
            .wrapping_add((g & h) ^ (g & a) ^ (h & a));
        b = b.wrapping_add(t1);
        f = t1.wrapping_add(t2);
        w11 = w11
            .wrapping_add(w12.rotate_right(7) ^ w12.rotate_right(18) ^ (w12 >> 3))
            .wrapping_add(w4)
            .wrapping_add(w9.rotate_right(17) ^ w9.rotate_right(19) ^ (w9 >> 10));
        let t1 = e
            .wrapping_add(b.rotate_right(6) ^ b.rotate_right(11) ^ b.rotate_right(25))
            .wrapping_add((b & c) ^ (!b & d))
            .wrapping_add(0x8cc70208)
            .wrapping_add(w11);
        let t2 = (f.rotate_right(2) ^ f.rotate_right(13) ^ f.rotate_right(22))
            .wrapping_add((f & g) ^ (f & h) ^ (g & h));
        a = a.wrapping_add(t1);
        e = t1.wrapping_add(t2);
        w12 = w12
            .wrapping_add(w13.rotate_right(7) ^ w13.rotate_right(18) ^ (w13 >> 3))
            .wrapping_add(w5)
            .wrapping_add(w10.rotate_right(17) ^ w10.rotate_right(19) ^ (w10 >> 10));
        let t1 = d
            .wrapping_add(a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25))
            .wrapping_add((a & b) ^ (!a & c))
            .wrapping_add(0x90befffa)
            .wrapping_add(w12);
        let t2 = (e.rotate_right(2) ^ e.rotate_right(13) ^ e.rotate_right(22))
            .wrapping_add((e & f) ^ (e & g) ^ (f & g));
        h = h.wrapping_add(t1);
        d = t1.wrapping_add(t2);
        w13 = w13
            .wrapping_add(w14.rotate_right(7) ^ w14.rotate_right(18) ^ (w14 >> 3))
            .wrapping_add(w6)
            .wrapping_add(w11.rotate_right(17) ^ w11.rotate_right(19) ^ (w11 >> 10));
        let t1 = c
            .wrapping_add(h.rotate_right(6) ^ h.rotate_right(11) ^ h.rotate_right(25))
            .wrapping_add((h & a) ^ (!h & b))
            .wrapping_add(0xa4506ceb)
            .wrapping_add(w13);
        let t2 = (d.rotate_right(2) ^ d.rotate_right(13) ^ d.rotate_right(22))
            .wrapping_add((d & e) ^ (d & f) ^ (e & f));
        g = g.wrapping_add(t1);
        c = t1.wrapping_add(t2);
        w14 = w14
            .wrapping_add(w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3))
            .wrapping_add(w7)
            .wrapping_add(w12.rotate_right(17) ^ w12.rotate_right(19) ^ (w12 >> 10));
        let t1 = b
            .wrapping_add(g.rotate_right(6) ^ g.rotate_right(11) ^ g.rotate_right(25))
            .wrapping_add((g & h) ^ (!g & a))
            .wrapping_add(0xbef9a3f7)
            .wrapping_add(w14);
        let t2 = (c.rotate_right(2) ^ c.rotate_right(13) ^ c.rotate_right(22))
            .wrapping_add((c & d) ^ (c & e) ^ (d & e));
        f = f.wrapping_add(t1);
        b = t1.wrapping_add(t2);
        w15 = w15
            .wrapping_add(w0.rotate_right(7) ^ w0.rotate_right(18) ^ (w0 >> 3))
            .wrapping_add(w8)
            .wrapping_add(w13.rotate_right(17) ^ w13.rotate_right(19) ^ (w13 >> 10));
        let t1 = a
            .wrapping_add(f.rotate_right(6) ^ f.rotate_right(11) ^ f.rotate_right(25))
            .wrapping_add((f & g) ^ (!f & h))
            .wrapping_add(0xc67178f2)
            .wrapping_add(w15);
        let t2 = (b.rotate_right(2) ^ b.rotate_right(13) ^ b.rotate_right(22))
            .wrapping_add((b & c) ^ (b & d) ^ (c & d));
        e = e.wrapping_add(t1);
        a = t1.wrapping_add(t2);
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    fn digest32(bytes: &[u8]) -> [u8; 32] {
        let mut state = H0;
        let mut chunks = bytes.chunks_exact(64);
        for block in &mut chunks {
            compress(&mut state, block.try_into().expect("64-byte block"));
        }
        let rem = chunks.remainder();
        let mut tail = [0u8; 64];
        tail[..rem.len()].copy_from_slice(rem);
        tail[rem.len()] = 0x80;
        if rem.len() >= 56 {
            compress(&mut state, &tail);
            tail = [0u8; 64];
        }
        tail[56..].copy_from_slice(&((bytes.len() as u64) * 8).to_be_bytes());
        compress(&mut state, &tail);
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 8 {
            out[4 * i..4 * i + 4].copy_from_slice(&state[i].to_be_bytes());
            i += 1;
        }
        out
    }

    fn hex64(digest: &[u8; 32]) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 32 {
            let pair = HEX_PAIRS[digest[i] as usize];
            out[2 * i] = pair[0];
            out[2 * i + 1] = pair[1];
            i += 1;
        }
        out
    }

    fn chain_hex(previous: &[u8; 64], event: &[u8; 64]) -> [u8; 64] {
        let mut material = [0u8; 129];
        material[..64].copy_from_slice(previous);
        material[64] = b'\n';
        material[65..].copy_from_slice(event);
        hex64(&digest32(&material))
    }

    fn hex_string(hex: &[u8; 64]) -> String {
        String::from_utf8(hex.to_vec()).expect("hex output is ascii")
    }

    fn zero_hash() -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(ZERO_CHAIN_HASH.as_bytes());
        out
    }

    fn find_newline(bytes: &[u8], mut i: usize) -> usize {
        const NL: u64 = 0x0a0a0a0a0a0a0a0a;
        const LO: u64 = 0x0101010101010101;
        const HI: u64 = 0x8080808080808080;
        let n = bytes.len();
        while i + 8 <= n {
            let word = u64::from_le_bytes(bytes[i..i + 8].try_into().expect("8-byte chunk"));
            let x = word ^ NL;
            let hit = x.wrapping_sub(LO) & !x & HI;
            if hit != 0 {
                return i + (hit.trailing_zeros() >> 3) as usize;
            }
            i += 8;
        }
        while i < n {
            if bytes[i] == b'\n' {
                return i;
            }
            i += 1;
        }
        n
    }

    fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
        let mut lines = Vec::with_capacity(bytes.len() / 192 + 4);
        let mut start = 0;
        while start < bytes.len() {
            let end = find_newline(bytes, start);
            let mut line = &bytes[start..end];
            if let [head @ .., b'\r'] = line {
                line = head;
            }
            if !line.is_empty() {
                lines.push(line);
            }
            start = end + 1;
        }
        lines
    }

    fn uuid_eq(a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }

    /// Strict scanner over the compact JSON this chain emits. It accepts only
    /// inputs serde_json provably accepts with identical extracted values;
    /// anything unusual (whitespace, escapes, non-ASCII, exponents, deep
    /// nesting, reordered fields) bails out to the serde fallback, which
    /// remains the behavioral authority.
    struct Scan<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Scan<'a> {
        fn lit(&mut self, s: &[u8]) -> bool {
            let end = self.pos + s.len();
            if self.buf.len() >= end && &self.buf[self.pos..end] == s {
                self.pos = end;
                true
            } else {
                false
            }
        }

        /// String body after the opening quote: printable ASCII, no escapes.
        /// Leaves pos past the closing quote.
        fn string_body(&mut self) -> Option<&'a [u8]> {
            let start = self.pos;
            while let Some(&c) = self.buf.get(self.pos) {
                if c == b'"' {
                    let body = &self.buf[start..self.pos];
                    self.pos += 1;
                    return Some(body);
                }
                if !(0x20..0x7f).contains(&c) || c == b'\\' {
                    return None;
                }
                self.pos += 1;
            }
            None
        }

        fn uint(&mut self) -> Option<u64> {
            let start = self.pos;
            let mut value: u64 = 0;
            while let Some(&c) = self.buf.get(self.pos) {
                if !c.is_ascii_digit() {
                    break;
                }
                value = value.checked_mul(10)?.checked_add(u64::from(c - b'0'))?;
                self.pos += 1;
            }
            let len = self.pos - start;
            if len == 0 || (self.buf[start] == b'0' && len > 1) {
                return None;
            }
            Some(value)
        }

        fn number(&mut self) -> bool {
            if self.buf.get(self.pos) == Some(&b'-') {
                self.pos += 1;
            }
            let start = self.pos;
            while self.buf.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            let len = self.pos - start;
            if len == 0 || len > 17 || (self.buf[start] == b'0' && len > 1) {
                return false;
            }
            if self.buf.get(self.pos) == Some(&b'.') {
                self.pos += 1;
                let frac = self.pos;
                while self.buf.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    self.pos += 1;
                }
                if self.pos == frac || self.pos - frac > 17 {
                    return false;
                }
            }
            !matches!(self.buf.get(self.pos), Some(b'e' | b'E'))
        }

        fn value(&mut self, depth: u32) -> bool {
            if depth > 16 {
                return false;
            }
            match self.buf.get(self.pos) {
                Some(b'"') => {
                    self.pos += 1;
                    self.string_body().is_some()
                }
                Some(b'{') => {
                    self.pos += 1;
                    if self.lit(b"}") {
                        return true;
                    }
                    loop {
                        if !self.lit(b"\"")
                            || self.string_body().is_none()
                            || !self.lit(b":")
                            || !self.value(depth + 1)
                        {
                            return false;
                        }
                        if self.lit(b",") {
                            continue;
                        }
                        return self.lit(b"}");
                    }
                }
                Some(b'[') => {
                    self.pos += 1;
                    if self.lit(b"]") {
                        return true;
                    }
                    loop {
                        if !self.value(depth + 1) {
                            return false;
                        }
                        if self.lit(b",") {
                            continue;
                        }
                        return self.lit(b"]");
                    }
                }
                Some(b't') => self.lit(b"true"),
                Some(b'f') => self.lit(b"false"),
                Some(b'n') => self.lit(b"null"),
                Some(b'-' | b'0'..=b'9') => self.number(),
                _ => false,
            }
        }

        fn done(&self) -> bool {
            self.pos == self.buf.len()
        }
    }

    fn ascii_str(bytes: &[u8]) -> Option<&str> {
        std::str::from_utf8(bytes).ok()
    }

    fn fast_event(line: &[u8]) -> Option<(&str, u64)> {
        let mut s = Scan { buf: line, pos: 0 };
        if !s.lit(b"{\"id\":\"") {
            return None;
        }
        let id = s.string_body()?;
        if !s.lit(b",\"timestamp_ms\":") {
            return None;
        }
        let ts = s.uint()?;
        if s.lit(b",\"issuer\":")
            && s.value(0)
            && s.lit(b",\"kind\":")
            && s.value(0)
            && s.lit(b"}")
            && s.done()
        {
            Some((ascii_str(id)?, ts))
        } else {
            None
        }
    }

    fn parse_event(line: &[u8]) -> Result<(Cow<'_, str>, u64), ()> {
        if let Some((id, ts)) = fast_event(line) {
            return Ok((Cow::Borrowed(id), ts));
        }
        match serde_json::from_slice::<EventFields>(line) {
            Ok(event) => Ok((event.id, event.timestamp_ms)),
            Err(_) => Err(()),
        }
    }

    fn fast_anchor(line: &[u8]) -> Option<AnchorFields<'_>> {
        let mut s = Scan { buf: line, pos: 0 };
        if !s.lit(b"{\"index\":") {
            return None;
        }
        let index = s.uint()?;
        if !s.lit(b",\"event_id\":\"") {
            return None;
        }
        let event_id = s.string_body()?;
        if !s.lit(b",\"timestamp_ms\":") {
            return None;
        }
        let timestamp_ms = s.uint()?;
        if !s.lit(b",\"event_hash_hex\":\"") {
            return None;
        }
        let event_hash_hex = s.string_body()?;
        if !s.lit(b",\"previous_hash_hex\":\"") {
            return None;
        }
        let previous_hash_hex = s.string_body()?;
        if !s.lit(b",\"chain_hash_hex\":\"") {
            return None;
        }
        let chain_hash_hex = s.string_body()?;
        if !(s.lit(b"}") && s.done()) {
            return None;
        }
        Some(AnchorFields {
            index,
            event_id: Cow::Borrowed(ascii_str(event_id)?),
            timestamp_ms,
            event_hash_hex: Cow::Borrowed(ascii_str(event_hash_hex)?),
            previous_hash_hex: Cow::Borrowed(ascii_str(previous_hash_hex)?),
            chain_hash_hex: Cow::Borrowed(ascii_str(chain_hash_hex)?),
        })
    }

    fn parse_anchor(line: &[u8]) -> Option<AnchorFields<'_>> {
        if let Some(anchor) = fast_anchor(line) {
            return Some(anchor);
        }
        serde_json::from_slice::<AnchorFields>(line).ok()
    }

    pub fn verify_chain(events_jsonl: &[u8], anchors_jsonl: &[u8]) -> ChainReport {
        let event_lines = split_lines(events_jsonl);
        let anchor_lines = split_lines(anchors_jsonl);
        let mut failures = Vec::new();

        let mut anchors: Vec<Option<AnchorFields>> = Vec::with_capacity(anchor_lines.len());
        for (index, line) in anchor_lines.iter().enumerate() {
            match parse_anchor(line) {
                Some(entry) => anchors.push(Some(entry)),
                None => {
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

        let mut previous = zero_hash();
        for (index, line) in event_lines.iter().enumerate() {
            let event_hex = hex64(&digest32(line));
            let chain = chain_hex(&previous, &event_hex);
            match parse_event(line) {
                Ok((id, timestamp_ms)) => match anchors.get(index) {
                    Some(Some(actual))
                        if actual.index == index as u64
                            && uuid_eq(&actual.event_id, &id)
                            && actual.timestamp_ms == timestamp_ms
                            && actual.event_hash_hex.as_bytes() == &event_hex[..]
                            && actual.previous_hash_hex.as_bytes() == &previous[..]
                            && actual.chain_hash_hex.as_bytes() == &chain[..] => {}
                    Some(_) => failures.push(Failure::EntryMismatch {
                        index: index as u64,
                    }),
                    None => failures.push(Failure::EntryMissing {
                        index: index as u64,
                    }),
                },
                Err(()) => {
                    failures.push(Failure::ParseError {
                        index: index as u64,
                    });
                    match anchors.get(index) {
                        Some(Some(actual))
                            if actual.index == index as u64
                                && actual.event_hash_hex.as_bytes() == &event_hex[..]
                                && actual.previous_hash_hex.as_bytes() == &previous[..]
                                && actual.chain_hash_hex.as_bytes() == &chain[..] => {}
                        Some(_) => failures.push(Failure::EntryMismatch {
                            index: index as u64,
                        }),
                        None => failures.push(Failure::EntryMissing {
                            index: index as u64,
                        }),
                    }
                }
            }
            previous = chain;
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
            root_hash_hex: hex_string(&previous),
            failures,
        }
    }

    pub fn fold_chain(lines: &[&[u8]]) -> Vec<ChainEntry> {
        let mut previous = zero_hash();
        let mut entries = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let event_hex = hex64(&digest32(line));
            let chain = chain_hex(&previous, &event_hex);
            let (event_id, timestamp_ms) = match parse_event(line) {
                Ok((id, ts)) => (id.into_owned(), ts),
                Err(()) => (String::new(), 0),
            };
            entries.push(ChainEntry {
                index: index as u64,
                event_id,
                timestamp_ms,
                event_hash_hex: hex_string(&event_hex),
                previous_hash_hex: hex_string(&previous),
                chain_hash_hex: hex_string(&chain),
            });
            previous = chain;
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
