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

    /// One round with rotated working-variable roles: wasm has no register
    /// renaming, so a looped round with `h = g; g = f; ...` shuffles pay
    /// per-instruction fuel. Ch uses the 3-op masked-select form; Maj reuses
    /// the previous round's `a ^ b` through an SSA chain of `$xo`/`$xn`
    /// bindings so no per-round shuffle instruction is paid.
    macro_rules! round {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $k:literal, $w:ident, $xo:ident, $xn:ident) => {
            let t1 = $h
                .wrapping_add($e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25))
                .wrapping_add((($f ^ $g) & $e) ^ $g)
                .wrapping_add($k)
                .wrapping_add($w);
            let $xn = $a ^ $b;
            let t2 = ($a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22))
                .wrapping_add($b ^ ($xn & $xo));
            $d = $d.wrapping_add(t1);
            $h = t1.wrapping_add(t2);
        };
    }
    macro_rules! sched {
        ($w0:ident, $w1:ident, $w9:ident, $w14:ident) => {
            $w0 = $w0
                .wrapping_add($w1.rotate_right(7) ^ $w1.rotate_right(18) ^ ($w1 >> 3))
                .wrapping_add($w9)
                .wrapping_add($w14.rotate_right(17) ^ $w14.rotate_right(19) ^ ($w14 >> 10));
        };
    }

    /// One SHA-256 compression over a pre-loaded schedule head. Taking `w`
    /// by value and forcing inlining lets the mostly-constant final block of
    /// `chain_hex` fold its zero words at compile time. Pinned by the NIST
    /// vector unit test and the frozen corpus digest.
    #[inline(always)]
    fn compress_w(state: &mut [u32; 8], w: [u32; 16]) {
        let [mut w0, mut w1, mut w2, mut w3, mut w4, mut w5, mut w6, mut w7, mut w8, mut w9, mut w10, mut w11, mut w12, mut w13, mut w14, mut w15] =
            w;
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        let x0 = b ^ c;
        round!(a, b, c, d, e, f, g, h, 0x428a2f98u32, w0, x0, x1);
        round!(h, a, b, c, d, e, f, g, 0x71374491u32, w1, x1, x2);
        round!(g, h, a, b, c, d, e, f, 0xb5c0fbcfu32, w2, x2, x3);
        round!(f, g, h, a, b, c, d, e, 0xe9b5dba5u32, w3, x3, x4);
        round!(e, f, g, h, a, b, c, d, 0x3956c25bu32, w4, x4, x5);
        round!(d, e, f, g, h, a, b, c, 0x59f111f1u32, w5, x5, x6);
        round!(c, d, e, f, g, h, a, b, 0x923f82a4u32, w6, x6, x7);
        round!(b, c, d, e, f, g, h, a, 0xab1c5ed5u32, w7, x7, x8);
        round!(a, b, c, d, e, f, g, h, 0xd807aa98u32, w8, x8, x9);
        round!(h, a, b, c, d, e, f, g, 0x12835b01u32, w9, x9, x10);
        round!(g, h, a, b, c, d, e, f, 0x243185beu32, w10, x10, x11);
        round!(f, g, h, a, b, c, d, e, 0x550c7dc3u32, w11, x11, x12);
        round!(e, f, g, h, a, b, c, d, 0x72be5d74u32, w12, x12, x13);
        round!(d, e, f, g, h, a, b, c, 0x80deb1feu32, w13, x13, x14);
        round!(c, d, e, f, g, h, a, b, 0x9bdc06a7u32, w14, x14, x15);
        round!(b, c, d, e, f, g, h, a, 0xc19bf174u32, w15, x15, x16);
        sched!(w0, w1, w9, w14);
        round!(a, b, c, d, e, f, g, h, 0xe49b69c1u32, w0, x16, x17);
        sched!(w1, w2, w10, w15);
        round!(h, a, b, c, d, e, f, g, 0xefbe4786u32, w1, x17, x18);
        sched!(w2, w3, w11, w0);
        round!(g, h, a, b, c, d, e, f, 0x0fc19dc6u32, w2, x18, x19);
        sched!(w3, w4, w12, w1);
        round!(f, g, h, a, b, c, d, e, 0x240ca1ccu32, w3, x19, x20);
        sched!(w4, w5, w13, w2);
        round!(e, f, g, h, a, b, c, d, 0x2de92c6fu32, w4, x20, x21);
        sched!(w5, w6, w14, w3);
        round!(d, e, f, g, h, a, b, c, 0x4a7484aau32, w5, x21, x22);
        sched!(w6, w7, w15, w4);
        round!(c, d, e, f, g, h, a, b, 0x5cb0a9dcu32, w6, x22, x23);
        sched!(w7, w8, w0, w5);
        round!(b, c, d, e, f, g, h, a, 0x76f988dau32, w7, x23, x24);
        sched!(w8, w9, w1, w6);
        round!(a, b, c, d, e, f, g, h, 0x983e5152u32, w8, x24, x25);
        sched!(w9, w10, w2, w7);
        round!(h, a, b, c, d, e, f, g, 0xa831c66du32, w9, x25, x26);
        sched!(w10, w11, w3, w8);
        round!(g, h, a, b, c, d, e, f, 0xb00327c8u32, w10, x26, x27);
        sched!(w11, w12, w4, w9);
        round!(f, g, h, a, b, c, d, e, 0xbf597fc7u32, w11, x27, x28);
        sched!(w12, w13, w5, w10);
        round!(e, f, g, h, a, b, c, d, 0xc6e00bf3u32, w12, x28, x29);
        sched!(w13, w14, w6, w11);
        round!(d, e, f, g, h, a, b, c, 0xd5a79147u32, w13, x29, x30);
        sched!(w14, w15, w7, w12);
        round!(c, d, e, f, g, h, a, b, 0x06ca6351u32, w14, x30, x31);
        sched!(w15, w0, w8, w13);
        round!(b, c, d, e, f, g, h, a, 0x14292967u32, w15, x31, x32);
        sched!(w0, w1, w9, w14);
        round!(a, b, c, d, e, f, g, h, 0x27b70a85u32, w0, x32, x33);
        sched!(w1, w2, w10, w15);
        round!(h, a, b, c, d, e, f, g, 0x2e1b2138u32, w1, x33, x34);
        sched!(w2, w3, w11, w0);
        round!(g, h, a, b, c, d, e, f, 0x4d2c6dfcu32, w2, x34, x35);
        sched!(w3, w4, w12, w1);
        round!(f, g, h, a, b, c, d, e, 0x53380d13u32, w3, x35, x36);
        sched!(w4, w5, w13, w2);
        round!(e, f, g, h, a, b, c, d, 0x650a7354u32, w4, x36, x37);
        sched!(w5, w6, w14, w3);
        round!(d, e, f, g, h, a, b, c, 0x766a0abbu32, w5, x37, x38);
        sched!(w6, w7, w15, w4);
        round!(c, d, e, f, g, h, a, b, 0x81c2c92eu32, w6, x38, x39);
        sched!(w7, w8, w0, w5);
        round!(b, c, d, e, f, g, h, a, 0x92722c85u32, w7, x39, x40);
        sched!(w8, w9, w1, w6);
        round!(a, b, c, d, e, f, g, h, 0xa2bfe8a1u32, w8, x40, x41);
        sched!(w9, w10, w2, w7);
        round!(h, a, b, c, d, e, f, g, 0xa81a664bu32, w9, x41, x42);
        sched!(w10, w11, w3, w8);
        round!(g, h, a, b, c, d, e, f, 0xc24b8b70u32, w10, x42, x43);
        sched!(w11, w12, w4, w9);
        round!(f, g, h, a, b, c, d, e, 0xc76c51a3u32, w11, x43, x44);
        sched!(w12, w13, w5, w10);
        round!(e, f, g, h, a, b, c, d, 0xd192e819u32, w12, x44, x45);
        sched!(w13, w14, w6, w11);
        round!(d, e, f, g, h, a, b, c, 0xd6990624u32, w13, x45, x46);
        sched!(w14, w15, w7, w12);
        round!(c, d, e, f, g, h, a, b, 0xf40e3585u32, w14, x46, x47);
        sched!(w15, w0, w8, w13);
        round!(b, c, d, e, f, g, h, a, 0x106aa070u32, w15, x47, x48);
        sched!(w0, w1, w9, w14);
        round!(a, b, c, d, e, f, g, h, 0x19a4c116u32, w0, x48, x49);
        sched!(w1, w2, w10, w15);
        round!(h, a, b, c, d, e, f, g, 0x1e376c08u32, w1, x49, x50);
        sched!(w2, w3, w11, w0);
        round!(g, h, a, b, c, d, e, f, 0x2748774cu32, w2, x50, x51);
        sched!(w3, w4, w12, w1);
        round!(f, g, h, a, b, c, d, e, 0x34b0bcb5u32, w3, x51, x52);
        sched!(w4, w5, w13, w2);
        round!(e, f, g, h, a, b, c, d, 0x391c0cb3u32, w4, x52, x53);
        sched!(w5, w6, w14, w3);
        round!(d, e, f, g, h, a, b, c, 0x4ed8aa4au32, w5, x53, x54);
        sched!(w6, w7, w15, w4);
        round!(c, d, e, f, g, h, a, b, 0x5b9cca4fu32, w6, x54, x55);
        sched!(w7, w8, w0, w5);
        round!(b, c, d, e, f, g, h, a, 0x682e6ff3u32, w7, x55, x56);
        sched!(w8, w9, w1, w6);
        round!(a, b, c, d, e, f, g, h, 0x748f82eeu32, w8, x56, x57);
        sched!(w9, w10, w2, w7);
        round!(h, a, b, c, d, e, f, g, 0x78a5636fu32, w9, x57, x58);
        sched!(w10, w11, w3, w8);
        round!(g, h, a, b, c, d, e, f, 0x84c87814u32, w10, x58, x59);
        sched!(w11, w12, w4, w9);
        round!(f, g, h, a, b, c, d, e, 0x8cc70208u32, w11, x59, x60);
        sched!(w12, w13, w5, w10);
        round!(e, f, g, h, a, b, c, d, 0x90befffau32, w12, x60, x61);
        sched!(w13, w14, w6, w11);
        round!(d, e, f, g, h, a, b, c, 0xa4506cebu32, w13, x61, x62);
        sched!(w14, w15, w7, w12);
        round!(c, d, e, f, g, h, a, b, 0xbef9a3f7u32, w14, x62, x63);
        sched!(w15, w0, w8, w13);
        round!(b, c, d, e, f, g, h, a, 0xc67178f2u32, w15, x63, _x64);
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    /// Inlined so the block loop in `digest_state` carries the state in
    /// locals instead of spilling eight words to memory per block.
    #[inline(always)]
    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let w = [
            u32::from_be_bytes([block[0], block[1], block[2], block[3]]),
            u32::from_be_bytes([block[4], block[5], block[6], block[7]]),
            u32::from_be_bytes([block[8], block[9], block[10], block[11]]),
            u32::from_be_bytes([block[12], block[13], block[14], block[15]]),
            u32::from_be_bytes([block[16], block[17], block[18], block[19]]),
            u32::from_be_bytes([block[20], block[21], block[22], block[23]]),
            u32::from_be_bytes([block[24], block[25], block[26], block[27]]),
            u32::from_be_bytes([block[28], block[29], block[30], block[31]]),
            u32::from_be_bytes([block[32], block[33], block[34], block[35]]),
            u32::from_be_bytes([block[36], block[37], block[38], block[39]]),
            u32::from_be_bytes([block[40], block[41], block[42], block[43]]),
            u32::from_be_bytes([block[44], block[45], block[46], block[47]]),
            u32::from_be_bytes([block[48], block[49], block[50], block[51]]),
            u32::from_be_bytes([block[52], block[53], block[54], block[55]]),
            u32::from_be_bytes([block[56], block[57], block[58], block[59]]),
            u32::from_be_bytes([block[60], block[61], block[62], block[63]]),
        ];
        compress_w(state, w);
    }

    fn digest_state(bytes: &[u8]) -> [u32; 8] {
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
        tail[56..].copy_from_slice(&((bytes.len() as u64).wrapping_mul(8)).to_be_bytes());
        compress(&mut state, &tail);
        state
    }

    /// Eight lowercase hex chars from one state word via SWAR: spread the
    /// nibbles into one byte each (already in output order, so no byte swap),
    /// then map nibble -> char branchlessly. The `+6` carry flags nibbles
    /// above 9; per-byte sums stay below 0x100 and the 0x27 multiply cannot
    /// carry across bytes because each flag byte is 0 or 1.
    #[inline(always)]
    fn hex8(word: u32) -> u64 {
        const LO: u64 = 0x0101010101010101;
        let x = u64::from(word);
        let v = ((x & 0xFFFF_0000) >> 16) | ((x & 0xFFFF) << 32);
        let v = ((v >> 8) & 0x0000_00FF_0000_00FF) | ((v & 0x0000_00FF_0000_00FF) << 16);
        let v = ((v >> 4) & 0x000F_000F_000F_000F) | ((v & 0x000F_000F_000F_000F) << 8);
        let gap = (v.wrapping_add(LO * 0x06) & (LO * 0x10)) >> 4;
        v.wrapping_add(LO * 0x30).wrapping_add(gap.wrapping_mul(0x27))
    }

    fn hex_state(state: &[u32; 8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 8 {
            out[8 * i..8 * i + 8].copy_from_slice(&hex8(state[i]).to_le_bytes());
            i += 1;
        }
        out
    }

    /// sha256 of the 129-byte `previous \n event` hex composition. The middle
    /// block's schedule is built directly from `event` words shifted through
    /// a one-byte carry (no 64-byte staging buffer), and the final padding
    /// block is constant except for the carried last hex char, so its
    /// schedule folds at compile time (length 129 * 8 = 1032 bits).
    fn chain_hex(previous: &[u8; 64], event: &[u8; 64]) -> [u8; 64] {
        let mut state = H0;
        compress(&mut state, previous);
        let mut w = [0u32; 16];
        let mut carry = u32::from(b'\n');
        let mut i = 0;
        while i < 16 {
            let v = u32::from_be_bytes(event[4 * i..4 * i + 4].try_into().expect("4-byte chunk"));
            w[i] = (carry << 24) | (v >> 8);
            carry = v & 0xFF;
            i += 1;
        }
        compress_w(&mut state, w);
        let mut w = [0u32; 16];
        w[0] = (carry << 24) | 0x0080_0000;
        w[15] = 1032;
        compress_w(&mut state, w);
        hex_state(&state)
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

    /// Branchless word-wise equality: wasm lowers slice == to a per-byte
    /// memcmp loop, which dominates on the 64-char hex fields.
    fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut acc = 0u64;
        let mut i = 0;
        while i + 8 <= a.len() {
            acc |= u64::from_le_bytes(a[i..i + 8].try_into().expect("8-byte chunk"))
                ^ u64::from_le_bytes(b[i..i + 8].try_into().expect("8-byte chunk"));
            i += 8;
        }
        while i < a.len() {
            acc |= u64::from(a[i] ^ b[i]);
            i += 1;
        }
        acc == 0
    }

    fn uuid_eq(a: &str, b: &str) -> bool {
        bytes_eq(a.as_bytes(), b.as_bytes()) || a.eq_ignore_ascii_case(b)
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
        /// Const-size literal match decomposed into 8/4/2/1-byte loads, so
        /// short prefixes like `{"id":"` cost a couple of wide compares
        /// against folded constants instead of a per-byte tail loop.
        #[inline(always)]
        fn lit<const N: usize>(&mut self, s: &[u8; N]) -> bool {
            let end = self.pos + N;
            let Some(a) = self.buf.get(self.pos..end) else {
                return false;
            };
            let mut acc = 0u64;
            let mut i = 0;
            while i + 8 <= N {
                acc |= u64::from_le_bytes(a[i..i + 8].try_into().expect("8-byte chunk"))
                    ^ u64::from_le_bytes(s[i..i + 8].try_into().expect("8-byte chunk"));
                i += 8;
            }
            if i + 4 <= N {
                acc |= u64::from(
                    u32::from_le_bytes(a[i..i + 4].try_into().expect("4-byte chunk"))
                        ^ u32::from_le_bytes(s[i..i + 4].try_into().expect("4-byte chunk")),
                );
                i += 4;
            }
            if i + 2 <= N {
                acc |= u64::from(
                    u16::from_le_bytes(a[i..i + 2].try_into().expect("2-byte chunk"))
                        ^ u16::from_le_bytes(s[i..i + 2].try_into().expect("2-byte chunk")),
                );
                i += 2;
            }
            if i < N {
                acc |= u64::from(a[i] ^ s[i]);
            }
            if acc == 0 {
                self.pos = end;
                true
            } else {
                false
            }
        }

        /// String body after the opening quote: printable ASCII, no escapes.
        /// Leaves pos past the closing quote. The detector drops the usual
        /// `& !x` zero-test masking: every spurious flag it can raise either
        /// sits on a byte with the high bit set (a true hit via the `| x`
        /// term) or lands strictly above a true hit through borrow/carry
        /// propagation, so the lowest flagged byte is always a genuine
        /// quote/backslash/non-printable and is re-checked exactly by the
        /// scalar arm.
        fn string_body(&mut self) -> Option<&'a [u8]> {
            const LO: u64 = 0x0101010101010101;
            const HI: u64 = 0x8080808080808080;
            let start = self.pos;
            let buf = self.buf;
            let mut i = self.pos;
            while i + 8 <= buf.len() {
                let x = u64::from_le_bytes(buf[i..i + 8].try_into().expect("8-byte chunk"));
                let quote = (x ^ (LO * 0x22)).wrapping_sub(LO);
                let slash = (x ^ (LO * 0x5c)).wrapping_sub(LO);
                let hit =
                    (quote | slash | x.wrapping_sub(LO * 0x20) | x.wrapping_add(LO) | x) & HI;
                if hit != 0 {
                    i += (hit.trailing_zeros() >> 3) as usize;
                    let c = buf[i];
                    if c == b'"' {
                        let body = &buf[start..i];
                        self.pos = i + 1;
                        return Some(body);
                    }
                    self.pos = i;
                    return None;
                }
                i += 8;
            }
            while let Some(&c) = buf.get(i) {
                if c == b'"' {
                    let body = &buf[start..i];
                    self.pos = i + 1;
                    return Some(body);
                }
                if !(0x20..0x7f).contains(&c) || c == b'\\' {
                    self.pos = i;
                    return None;
                }
                i += 1;
            }
            self.pos = i;
            None
        }

        /// Up to 19 digits cannot overflow u64, so the accumulate is
        /// unchecked; 20+ digit runs bail to the serde fallback, which parses
        /// in-range values identically and rejects the rest.
        fn uint(&mut self) -> Option<u64> {
            let start = self.pos;
            let mut value: u64 = 0;
            while let Some(&c) = self.buf.get(self.pos) {
                if !c.is_ascii_digit() {
                    break;
                }
                value = value.wrapping_mul(10).wrapping_add(u64::from(c - b'0'));
                self.pos += 1;
            }
            let len = self.pos - start;
            if len == 0 || len > 19 || (self.buf[start] == b'0' && len > 1) {
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
            let event_hex = hex_state(&digest_state(line));
            let chain = chain_hex(&previous, &event_hex);
            match parse_event(line) {
                Ok((id, timestamp_ms)) => match anchors.get(index) {
                    Some(Some(actual))
                        if actual.index == index as u64
                            && uuid_eq(&actual.event_id, &id)
                            && actual.timestamp_ms == timestamp_ms
                            && bytes_eq(actual.event_hash_hex.as_bytes(), &event_hex)
                            && bytes_eq(actual.previous_hash_hex.as_bytes(), &previous)
                            && bytes_eq(actual.chain_hash_hex.as_bytes(), &chain) => {}
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
                                && bytes_eq(actual.event_hash_hex.as_bytes(), &event_hex)
                                && bytes_eq(actual.previous_hash_hex.as_bytes(), &previous)
                                && bytes_eq(actual.chain_hash_hex.as_bytes(), &chain) => {}
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
            let event_hex = hex_state(&digest_state(line));
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
