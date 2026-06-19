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

    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    const fn tail_wk_table() -> [[u32; 64]; 16] {
        let mut tbl = [[0u32; 64]; 16];
        let mut n = 0;
        while n < 16 {
            let c = if n < 10 { 0x30 + n as u32 } else { 0x57 + n as u32 };
            let wk = &mut tbl[n];
            wk[0] = (c << 24) | 0x0080_0000;
            wk[15] = 1032;
            let mut i = 16;
            while i < 64 {
                let w15 = wk[i - 15];
                let w2 = wk[i - 2];
                wk[i] = wk[i - 16]
                    .wrapping_add(w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3))
                    .wrapping_add(wk[i - 7])
                    .wrapping_add(w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10));
                i += 1;
            }
            let mut i = 0;
            while i < 64 {
                wk[i] = wk[i].wrapping_add(K[i]);
                i += 1;
            }
            n += 1;
        }
        tbl
    }

    const TAIL_WK: [[u32; 64]; 16] = tail_wk_table();

    // Lowercase hex char -> 0..15, masked so indexing never bounds-checks.
    #[cfg(target_arch = "wasm32")]
    #[inline(always)]
    fn tail_idx(c: u8) -> usize {
        usize::from((c & 0x0f) + (c >> 6) * 9) & 15
    }

    #[cfg(target_arch = "wasm32")]
    macro_rules! roundt {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:expr, $xo:ident, $xn:ident) => {
            let t1 = $h
                .wrapping_add($e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25))
                .wrapping_add((($f ^ $g) & $e) ^ $g)
                .wrapping_add($wk);
            let $xn = $a ^ $b;
            let t2 = ($a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22))
                .wrapping_add($b ^ ($xn & $xo));
            $d = $d.wrapping_add(t1);
            $h = t1.wrapping_add(t2);
        };
    }

    #[cfg(target_arch = "wasm32")]
    macro_rules! octt {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:ident, $i:literal) => {
            let x0 = $b ^ $c;
            roundt!($a, $b, $c, $d, $e, $f, $g, $h, $wk[$i], x0, x1);
            roundt!($h, $a, $b, $c, $d, $e, $f, $g, $wk[$i + 1], x1, x2);
            roundt!($g, $h, $a, $b, $c, $d, $e, $f, $wk[$i + 2], x2, x3);
            roundt!($f, $g, $h, $a, $b, $c, $d, $e, $wk[$i + 3], x3, x4);
            roundt!($e, $f, $g, $h, $a, $b, $c, $d, $wk[$i + 4], x4, x5);
            roundt!($d, $e, $f, $g, $h, $a, $b, $c, $wk[$i + 5], x5, x6);
            roundt!($c, $d, $e, $f, $g, $h, $a, $b, $wk[$i + 6], x6, x7);
            roundt!($b, $c, $d, $e, $f, $g, $h, $a, $wk[$i + 7], x7, _x8);
        };
    }

    #[cfg(target_arch = "wasm32")]
    fn compress_tail(state: &mut [u32; 8], wk: &[u32; 64]) {
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        octt!(a, b, c, d, e, f, g, h, wk, 0);
        octt!(a, b, c, d, e, f, g, h, wk, 8);
        octt!(a, b, c, d, e, f, g, h, wk, 16);
        octt!(a, b, c, d, e, f, g, h, wk, 24);
        octt!(a, b, c, d, e, f, g, h, wk, 32);
        octt!(a, b, c, d, e, f, g, h, wk, 40);
        octt!(a, b, c, d, e, f, g, h, wk, 48);
        octt!(a, b, c, d, e, f, g, h, wk, 56);
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    #[cfg(target_arch = "wasm32")]
    macro_rules! octtb {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:ident, $i:literal) => {
            macro_rules! wkb {
                ($j:expr) => {
                    u32::from_le_bytes($wk[4 * $j..4 * $j + 4].try_into().expect("4-byte chunk"))
                };
            }
            let x0 = $b ^ $c;
            roundt!($a, $b, $c, $d, $e, $f, $g, $h, wkb!($i), x0, x1);
            roundt!($h, $a, $b, $c, $d, $e, $f, $g, wkb!($i + 1), x1, x2);
            roundt!($g, $h, $a, $b, $c, $d, $e, $f, wkb!($i + 2), x2, x3);
            roundt!($f, $g, $h, $a, $b, $c, $d, $e, wkb!($i + 3), x3, x4);
            roundt!($e, $f, $g, $h, $a, $b, $c, $d, wkb!($i + 4), x4, x5);
            roundt!($d, $e, $f, $g, $h, $a, $b, $c, wkb!($i + 5), x5, x6);
            roundt!($c, $d, $e, $f, $g, $h, $a, $b, wkb!($i + 6), x6, x7);
            roundt!($b, $c, $d, $e, $f, $g, $h, $a, wkb!($i + 7), x7, _x8);
        };
    }

    #[cfg(target_arch = "wasm32")]
    fn compress_tail_b(state: &mut [u32; 8], wk: &[u8; 256]) {
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        octtb!(a, b, c, d, e, f, g, h, wk, 0);
        octtb!(a, b, c, d, e, f, g, h, wk, 8);
        octtb!(a, b, c, d, e, f, g, h, wk, 16);
        octtb!(a, b, c, d, e, f, g, h, wk, 24);
        octtb!(a, b, c, d, e, f, g, h, wk, 32);
        octtb!(a, b, c, d, e, f, g, h, wk, 40);
        octtb!(a, b, c, d, e, f, g, h, wk, 48);
        octtb!(a, b, c, d, e, f, g, h, wk, 56);
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(chunk.try_into().expect("4-byte chunk"));
        }
        for i in 16..64 {
            let w15 = w[i - 15];
            let w2 = w[i - 2];
            w[i] = w[i - 16]
                .wrapping_add(w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3))
                .wrapping_add(w[i - 7])
                .wrapping_add(w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10));
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add(((f ^ g) & e) ^ g)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add(((a | b) & c) | (a & b));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    // SWAR hex: the `+6` carry flags nibbles above 9; the 0x27 multiply
    // cannot carry across bytes because each flag byte is 0 or 1.
    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
    fn hex_state(state: &[u32; 8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 8 {
            out[8 * i..8 * i + 8].copy_from_slice(&hex8(state[i]).to_le_bytes());
            i += 1;
        }
        out
    }

    #[cfg(target_arch = "wasm32")]
    mod simd {
        use super::{H0, K};
        use std::arch::wasm32::*;

        macro_rules! roundv {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:expr, $xo:ident, $xn:ident) => {
                let t1 = $h
                    .wrapping_add($e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25))
                    .wrapping_add((($f ^ $g) & $e) ^ $g)
                    .wrapping_add($wk);
                let $xn = $a ^ $b;
                let t2 = ($a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22))
                    .wrapping_add($b ^ ($xn & $xo));
                $d = $d.wrapping_add(t1);
                $h = t1.wrapping_add(t2);
            };
        }

        macro_rules! oct {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wl:ident, $wh:ident) => {
                let x0 = $b ^ $c;
                roundv!($a, $b, $c, $d, $e, $f, $g, $h, u32x4_extract_lane::<0>($wl), x0, x1);
                roundv!($h, $a, $b, $c, $d, $e, $f, $g, u32x4_extract_lane::<1>($wl), x1, x2);
                roundv!($g, $h, $a, $b, $c, $d, $e, $f, u32x4_extract_lane::<2>($wl), x2, x3);
                roundv!($f, $g, $h, $a, $b, $c, $d, $e, u32x4_extract_lane::<3>($wl), x3, x4);
                roundv!($e, $f, $g, $h, $a, $b, $c, $d, u32x4_extract_lane::<0>($wh), x4, x5);
                roundv!($d, $e, $f, $g, $h, $a, $b, $c, u32x4_extract_lane::<1>($wh), x5, x6);
                roundv!($c, $d, $e, $f, $g, $h, $a, $b, u32x4_extract_lane::<2>($wh), x6, x7);
                roundv!($b, $c, $d, $e, $f, $g, $h, $a, u32x4_extract_lane::<3>($wh), x7, x8);
            };
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn small_sigma0(x: v128) -> v128 {
            v128_xor(
                v128_xor(
                    v128_or(u32x4_shr(x, 7), u32x4_shl(x, 25)),
                    v128_or(u32x4_shr(x, 18), u32x4_shl(x, 14)),
                ),
                u32x4_shr(x, 3),
            )
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn small_sigma1(x: v128) -> v128 {
            v128_xor(
                v128_xor(
                    v128_or(u32x4_shr(x, 17), u32x4_shl(x, 15)),
                    v128_or(u32x4_shr(x, 19), u32x4_shl(x, 13)),
                ),
                u32x4_shr(x, 10),
            )
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn sched4(x0: v128, x1: v128, x2: v128, x3: v128) -> v128 {
            let zero = u32x4(0, 0, 0, 0);
            let w1 = i8x16_shuffle::<4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19>(x0, x1);
            let w9 = i8x16_shuffle::<4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19>(x2, x3);
            let t = u32x4_add(u32x4_add(x0, small_sigma0(w1)), w9);
            let w14 = i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23>(x3, zero);
            let t = u32x4_add(t, small_sigma1(w14));
            let w16 = i8x16_shuffle::<16, 17, 18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5, 6, 7>(t, zero);
            u32x4_add(t, small_sigma1(w16))
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn compress_v(state: &mut [u32; 8], x0: v128, x1: v128, x2: v128, x3: v128) {
            let x4 = sched4(x0, x1, x2, x3);
            let x5 = sched4(x1, x2, x3, x4);
            let x6 = sched4(x2, x3, x4, x5);
            let x7 = sched4(x3, x4, x5, x6);
            let x8 = sched4(x4, x5, x6, x7);
            let x9 = sched4(x5, x6, x7, x8);
            let x10 = sched4(x6, x7, x8, x9);
            let x11 = sched4(x7, x8, x9, x10);
            let x12 = sched4(x8, x9, x10, x11);
            let x13 = sched4(x9, x10, x11, x12);
            let x14 = sched4(x10, x11, x12, x13);
            let x15 = sched4(x11, x12, x13, x14);
            let wk0 = u32x4_add(x0, u32x4(0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5));
            let wk1 = u32x4_add(x1, u32x4(0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5));
            let wk2 = u32x4_add(x2, u32x4(0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3));
            let wk3 = u32x4_add(x3, u32x4(0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174));
            let wk4 = u32x4_add(x4, u32x4(0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc));
            let wk5 = u32x4_add(x5, u32x4(0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da));
            let wk6 = u32x4_add(x6, u32x4(0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7));
            let wk7 = u32x4_add(x7, u32x4(0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967));
            let wk8 = u32x4_add(x8, u32x4(0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13));
            let wk9 = u32x4_add(x9, u32x4(0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85));
            let wk10 = u32x4_add(x10, u32x4(0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3));
            let wk11 = u32x4_add(x11, u32x4(0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070));
            let wk12 = u32x4_add(x12, u32x4(0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5));
            let wk13 = u32x4_add(x13, u32x4(0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3));
            let wk14 = u32x4_add(x14, u32x4(0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208));
            let wk15 = u32x4_add(x15, u32x4(0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2));
            let mut a = state[0];
            let mut b = state[1];
            let mut c = state[2];
            let mut d = state[3];
            let mut e = state[4];
            let mut f = state[5];
            let mut g = state[6];
            let mut h = state[7];
            oct!(a, b, c, d, e, f, g, h, wk0, wk1);
            oct!(a, b, c, d, e, f, g, h, wk2, wk3);
            oct!(a, b, c, d, e, f, g, h, wk4, wk5);
            oct!(a, b, c, d, e, f, g, h, wk6, wk7);
            oct!(a, b, c, d, e, f, g, h, wk8, wk9);
            oct!(a, b, c, d, e, f, g, h, wk10, wk11);
            oct!(a, b, c, d, e, f, g, h, wk12, wk13);
            oct!(a, b, c, d, e, f, g, h, wk14, wk15);
            state[0] = state[0].wrapping_add(a);
            state[1] = state[1].wrapping_add(b);
            state[2] = state[2].wrapping_add(c);
            state[3] = state[3].wrapping_add(d);
            state[4] = state[4].wrapping_add(e);
            state[5] = state[5].wrapping_add(f);
            state[6] = state[6].wrapping_add(g);
            state[7] = state[7].wrapping_add(h);
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn load_be<const O: usize>(b: &[u8; 64]) -> v128 {
            let v = u64x2(
                u64::from_le_bytes(b[O..O + 8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(b[O + 8..O + 16].try_into().expect("8-byte chunk")),
            );
            i8x16_shuffle::<3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12>(v, v)
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn load_raw<const O: usize>(b: &[u8; 64]) -> v128 {
            u64x2(
                u64::from_le_bytes(b[O..O + 8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(b[O + 8..O + 16].try_into().expect("8-byte chunk")),
            )
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn compress_block(state: &mut [u32; 8], block: &[u8; 64]) {
            compress_v(
                state,
                load_be::<0>(block),
                load_be::<16>(block),
                load_be::<32>(block),
                load_be::<48>(block),
            );
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn hex_half<const O: usize>(out: &mut [u8; 64], v: v128) {
            let table = u8x16(
                b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'a', b'b', b'c', b'd',
                b'e', b'f',
            );
            let v = i8x16_shuffle::<3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12>(v, v);
            let hi = u8x16_shr(v, 4);
            let lo = v128_and(v, u8x16_splat(0x0f));
            let n0 = i8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(hi, lo);
            let n1 = i8x16_shuffle::<8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31>(hi, lo);
            let c0 = i8x16_swizzle(table, n0);
            let c1 = i8x16_swizzle(table, n1);
            out[O..O + 8].copy_from_slice(&u64x2_extract_lane::<0>(c0).to_le_bytes());
            out[O + 8..O + 16].copy_from_slice(&u64x2_extract_lane::<1>(c0).to_le_bytes());
            out[O + 16..O + 24].copy_from_slice(&u64x2_extract_lane::<0>(c1).to_le_bytes());
            out[O + 24..O + 32].copy_from_slice(&u64x2_extract_lane::<1>(c1).to_le_bytes());
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn hex_state(state: &[u32; 8]) -> [u8; 64] {
            let mut out = [0u8; 64];
            hex_half::<0>(&mut out, u32x4(state[0], state[1], state[2], state[3]));
            hex_half::<32>(&mut out, u32x4(state[4], state[5], state[6], state[7]));
            out
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn rotrv<const N: u32>(x: v128) -> v128 {
            v128_or(u32x4_shr(x, N), u32x4_shl(x, 32 - N))
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn big_sigma0(x: v128) -> v128 {
            v128_xor(v128_xor(rotrv::<2>(x), rotrv::<13>(x)), rotrv::<22>(x))
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn big_sigma1(x: v128) -> v128 {
            v128_xor(v128_xor(rotrv::<6>(x), rotrv::<11>(x)), rotrv::<25>(x))
        }

        macro_rules! roundm {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $k:literal, $w:ident) => {
                let t1 = u32x4_add(
                    u32x4_add(
                        u32x4_add(u32x4_add($h, big_sigma1($e)), v128_bitselect($f, $g, $e)),
                        u32x4($k, $k, $k, $k),
                    ),
                    $w,
                );
                let t2 = u32x4_add(big_sigma0($a), v128_bitselect($c, $a, v128_xor($a, $b)));
                $d = u32x4_add($d, t1);
                $h = u32x4_add(t1, t2);
            };
        }
        macro_rules! schedm {
            ($w0:ident, $w1:ident, $w9:ident, $w14:ident) => {
                $w0 = u32x4_add(
                    u32x4_add($w0, u32x4_add(small_sigma0($w1), $w9)),
                    small_sigma1($w14),
                );
            };
        }

        macro_rules! compressm_at {
            ($state:expr, $w:expr $(,)?) => {{
                let state: &mut [v128; 8] = $state;
                let w: [v128; 16] = $w;
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
            roundm!(a, b, c, d, e, f, g, h, 0x428a2f98u32, w0);
            roundm!(h, a, b, c, d, e, f, g, 0x71374491u32, w1);
            roundm!(g, h, a, b, c, d, e, f, 0xb5c0fbcfu32, w2);
            roundm!(f, g, h, a, b, c, d, e, 0xe9b5dba5u32, w3);
            roundm!(e, f, g, h, a, b, c, d, 0x3956c25bu32, w4);
            roundm!(d, e, f, g, h, a, b, c, 0x59f111f1u32, w5);
            roundm!(c, d, e, f, g, h, a, b, 0x923f82a4u32, w6);
            roundm!(b, c, d, e, f, g, h, a, 0xab1c5ed5u32, w7);
            roundm!(a, b, c, d, e, f, g, h, 0xd807aa98u32, w8);
            roundm!(h, a, b, c, d, e, f, g, 0x12835b01u32, w9);
            roundm!(g, h, a, b, c, d, e, f, 0x243185beu32, w10);
            roundm!(f, g, h, a, b, c, d, e, 0x550c7dc3u32, w11);
            roundm!(e, f, g, h, a, b, c, d, 0x72be5d74u32, w12);
            roundm!(d, e, f, g, h, a, b, c, 0x80deb1feu32, w13);
            roundm!(c, d, e, f, g, h, a, b, 0x9bdc06a7u32, w14);
            roundm!(b, c, d, e, f, g, h, a, 0xc19bf174u32, w15);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0xe49b69c1u32, w0);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0xefbe4786u32, w1);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x0fc19dc6u32, w2);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x240ca1ccu32, w3);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x2de92c6fu32, w4);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x4a7484aau32, w5);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x5cb0a9dcu32, w6);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x76f988dau32, w7);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0x983e5152u32, w8);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0xa831c66du32, w9);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0xb00327c8u32, w10);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0xbf597fc7u32, w11);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0xc6e00bf3u32, w12);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xd5a79147u32, w13);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0x06ca6351u32, w14);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0x14292967u32, w15);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0x27b70a85u32, w0);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0x2e1b2138u32, w1);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x4d2c6dfcu32, w2);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x53380d13u32, w3);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x650a7354u32, w4);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x766a0abbu32, w5);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x81c2c92eu32, w6);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x92722c85u32, w7);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0xa2bfe8a1u32, w8);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0xa81a664bu32, w9);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0xc24b8b70u32, w10);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0xc76c51a3u32, w11);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0xd192e819u32, w12);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xd6990624u32, w13);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0xf40e3585u32, w14);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0x106aa070u32, w15);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0x19a4c116u32, w0);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0x1e376c08u32, w1);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x2748774cu32, w2);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x34b0bcb5u32, w3);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x391c0cb3u32, w4);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x4ed8aa4au32, w5);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x5b9cca4fu32, w6);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x682e6ff3u32, w7);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0x748f82eeu32, w8);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0x78a5636fu32, w9);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0x84c87814u32, w10);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0x8cc70208u32, w11);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0x90befffau32, w12);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xa4506cebu32, w13);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0xbef9a3f7u32, w14);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0xc67178f2u32, w15);
            state[0] = u32x4_add(state[0], a);
            state[1] = u32x4_add(state[1], b);
            state[2] = u32x4_add(state[2], c);
            state[3] = u32x4_add(state[3], d);
            state[4] = u32x4_add(state[4], e);
            state[5] = u32x4_add(state[5], f);
            state[6] = u32x4_add(state[6], g);
            state[7] = u32x4_add(state[7], h);
                    }};
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn transpose_be(r0: v128, r1: v128, r2: v128, r3: v128) -> [v128; 4] {
            let p0 = i8x16_shuffle::<3, 2, 1, 0, 19, 18, 17, 16, 7, 6, 5, 4, 23, 22, 21, 20>(r0, r1);
            let p1 = i8x16_shuffle::<11, 10, 9, 8, 27, 26, 25, 24, 15, 14, 13, 12, 31, 30, 29, 28>(r0, r1);
            let p2 = i8x16_shuffle::<3, 2, 1, 0, 19, 18, 17, 16, 7, 6, 5, 4, 23, 22, 21, 20>(r2, r3);
            let p3 = i8x16_shuffle::<11, 10, 9, 8, 27, 26, 25, 24, 15, 14, 13, 12, 31, 30, 29, 28>(r2, r3);
            [
                i8x16_shuffle::<0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23>(p0, p2),
                i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 26, 27, 28, 29, 30, 31>(p0, p2),
                i8x16_shuffle::<0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23>(p1, p3),
                i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 26, 27, 28, 29, 30, 31>(p1, p3),
            ]
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn quad<const O: usize>(
            b0: &[u8; 64],
            b1: &[u8; 64],
            b2: &[u8; 64],
            b3: &[u8; 64],
        ) -> [v128; 4] {
            transpose_be(
                load_raw::<O>(b0),
                load_raw::<O>(b1),
                load_raw::<O>(b2),
                load_raw::<O>(b3),
            )
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn shift_cols(hex: &[u8; 64], nl: v128) -> [v128; 4] {
            let v0 = load_raw::<0>(hex);
            let v1 = load_raw::<16>(hex);
            let v2 = load_raw::<32>(hex);
            let v3 = load_raw::<48>(hex);
            [
                i8x16_shuffle::<16, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14>(v0, nl),
                i8x16_shuffle::<15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30>(v0, v1),
                i8x16_shuffle::<15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30>(v1, v2),
                i8x16_shuffle::<15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30>(v2, v3),
            ]
        }

        #[inline]
        fn block_at<'a>(
            line: &'a [u8],
            tail: &'a [u8; 128],
            full: usize,
            k: usize,
        ) -> &'a [u8; 64] {
            if k < full {
                line[64 * k..64 * k + 64].try_into().expect("64-byte block")
            } else {
                let off = 64 * (k - full);
                tail[off..off + 64].try_into().expect("64-byte block")
            }
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn splat_h0() -> [v128; 8] {
            [
                u32x4_splat(H0[0]),
                u32x4_splat(H0[1]),
                u32x4_splat(H0[2]),
                u32x4_splat(H0[3]),
                u32x4_splat(H0[4]),
                u32x4_splat(H0[5]),
                u32x4_splat(H0[6]),
                u32x4_splat(H0[7]),
            ]
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn transpose32(s0: v128, s1: v128, s2: v128, s3: v128) -> [v128; 4] {
            let p0 = i32x4_shuffle::<0, 4, 1, 5>(s0, s1);
            let p1 = i32x4_shuffle::<2, 6, 3, 7>(s0, s1);
            let p2 = i32x4_shuffle::<0, 4, 1, 5>(s2, s3);
            let p3 = i32x4_shuffle::<2, 6, 3, 7>(s2, s3);
            [
                i32x4_shuffle::<0, 1, 4, 5>(p0, p2),
                i32x4_shuffle::<2, 3, 6, 7>(p0, p2),
                i32x4_shuffle::<0, 1, 4, 5>(p1, p3),
                i32x4_shuffle::<2, 3, 6, 7>(p1, p3),
            ]
        }

        #[target_feature(enable = "simd128")]
        fn lanes_hex(state: &[v128; 8]) -> [[u8; 64]; 4] {
            let lo = transpose32(state[0], state[1], state[2], state[3]);
            let hi = transpose32(state[4], state[5], state[6], state[7]);
            let mut out = [[0u8; 64]; 4];
            let mut l = 0;
            while l < 4 {
                hex_half::<0>(&mut out[l], lo[l]);
                hex_half::<32>(&mut out[l], hi[l]);
                l += 1;
            }
            out
        }

        macro_rules! roundw {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wk:expr) => {
                let t1v = u32x4_add(
                    u32x4_add(u32x4_add($h, big_sigma1($e)), v128_bitselect($f, $g, $e)),
                    $wk,
                );
                let t2v = u32x4_add(big_sigma0($a), v128_bitselect($c, $a, v128_xor($a, $b)));
                $d = u32x4_add($d, t1v);
                $h = u32x4_add(t1v, t2v);
            };
        }
        macro_rules! octg {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $i:literal, $t0:ident, $t1:ident, $t2:ident, $t3:ident) => {
                let r0 = transpose32(
                    u32x4($t0[$i], $t0[$i + 1], $t0[$i + 2], $t0[$i + 3]),
                    u32x4($t1[$i], $t1[$i + 1], $t1[$i + 2], $t1[$i + 3]),
                    u32x4($t2[$i], $t2[$i + 1], $t2[$i + 2], $t2[$i + 3]),
                    u32x4($t3[$i], $t3[$i + 1], $t3[$i + 2], $t3[$i + 3]),
                );
                let r1 = transpose32(
                    u32x4($t0[$i + 4], $t0[$i + 5], $t0[$i + 6], $t0[$i + 7]),
                    u32x4($t1[$i + 4], $t1[$i + 5], $t1[$i + 6], $t1[$i + 7]),
                    u32x4($t2[$i + 4], $t2[$i + 5], $t2[$i + 6], $t2[$i + 7]),
                    u32x4($t3[$i + 4], $t3[$i + 5], $t3[$i + 6], $t3[$i + 7]),
                );
                roundw!($a, $b, $c, $d, $e, $f, $g, $h, r0[0]);
                roundw!($h, $a, $b, $c, $d, $e, $f, $g, r0[1]);
                roundw!($g, $h, $a, $b, $c, $d, $e, $f, r0[2]);
                roundw!($f, $g, $h, $a, $b, $c, $d, $e, r0[3]);
                roundw!($e, $f, $g, $h, $a, $b, $c, $d, r1[0]);
                roundw!($d, $e, $f, $g, $h, $a, $b, $c, r1[1]);
                roundw!($c, $d, $e, $f, $g, $h, $a, $b, r1[2]);
                roundw!($b, $c, $d, $e, $f, $g, $h, $a, r1[3]);
            };
        }

        // The tail block's first 16 schedule words are the padding itself:
        // w0 = char<<24 | 0x80<<16, w1..w14 = 0, w15 = 1032. Only round 0
        // differs across lanes, so rounds 1..16 take immediate K constants
        // and skip the table gather entirely; extended rounds still gather.
        #[inline]
        #[target_feature(enable = "simd128")]
        fn compress_tail4(
            state: &mut [v128; 8],
            t0: &[u32; 64],
            t1: &[u32; 64],
            t2: &[u32; 64],
            t3: &[u32; 64],
        ) {
            let mut a = state[0];
            let mut b = state[1];
            let mut c = state[2];
            let mut d = state[3];
            let mut e = state[4];
            let mut f = state[5];
            let mut g = state[6];
            let mut h = state[7];
            let wkv0 = u32x4(t0[0], t1[0], t2[0], t3[0]);
            roundw!(a, b, c, d, e, f, g, h, wkv0);
            roundw!(h, a, b, c, d, e, f, g, u32x4_splat(K[1]));
            roundw!(g, h, a, b, c, d, e, f, u32x4_splat(K[2]));
            roundw!(f, g, h, a, b, c, d, e, u32x4_splat(K[3]));
            roundw!(e, f, g, h, a, b, c, d, u32x4_splat(K[4]));
            roundw!(d, e, f, g, h, a, b, c, u32x4_splat(K[5]));
            roundw!(c, d, e, f, g, h, a, b, u32x4_splat(K[6]));
            roundw!(b, c, d, e, f, g, h, a, u32x4_splat(K[7]));
            roundw!(a, b, c, d, e, f, g, h, u32x4_splat(K[8]));
            roundw!(h, a, b, c, d, e, f, g, u32x4_splat(K[9]));
            roundw!(g, h, a, b, c, d, e, f, u32x4_splat(K[10]));
            roundw!(f, g, h, a, b, c, d, e, u32x4_splat(K[11]));
            roundw!(e, f, g, h, a, b, c, d, u32x4_splat(K[12]));
            roundw!(d, e, f, g, h, a, b, c, u32x4_splat(K[13]));
            roundw!(c, d, e, f, g, h, a, b, u32x4_splat(K[14]));
            roundw!(b, c, d, e, f, g, h, a, u32x4_splat(K[15].wrapping_add(1032)));
            octg!(a, b, c, d, e, f, g, h, 16, t0, t1, t2, t3);
            octg!(a, b, c, d, e, f, g, h, 24, t0, t1, t2, t3);
            octg!(a, b, c, d, e, f, g, h, 32, t0, t1, t2, t3);
            octg!(a, b, c, d, e, f, g, h, 40, t0, t1, t2, t3);
            octg!(a, b, c, d, e, f, g, h, 48, t0, t1, t2, t3);
            octg!(a, b, c, d, e, f, g, h, 56, t0, t1, t2, t3);
            state[0] = u32x4_add(state[0], a);
            state[1] = u32x4_add(state[1], b);
            state[2] = u32x4_add(state[2], c);
            state[3] = u32x4_add(state[3], d);
            state[4] = u32x4_add(state[4], e);
            state[5] = u32x4_add(state[5], f);
            state[6] = u32x4_add(state[6], g);
            state[7] = u32x4_add(state[7], h);
        }

        #[target_feature(enable = "simd128")]
        pub(super) fn link_quad(
            cand: &[Option<&[u8; 64]>; 4],
            hexes: &[[u8; 64]],
            tail: &[[u32; 64]; 16],
            out: &mut [[u8; 64]; 4],
        ) {
            const ZERO_HEX: [u8; 64] = [b'0'; 64];
            let mut prevs: [&[u8; 64]; 4] = [&ZERO_HEX; 4];
            let mut hx: [&[u8; 64]; 4] = [&hexes[0]; 4];
            let mut l = 0;
            while l < 4 {
                if let Some(p) = cand[l] {
                    prevs[l] = p;
                }
                if let Some(h) = hexes.get(l) {
                    hx[l] = h;
                }
                l += 1;
            }
            let mut state = splat_h0();
            let q0 = quad::<0>(prevs[0], prevs[1], prevs[2], prevs[3]);
            let q1 = quad::<16>(prevs[0], prevs[1], prevs[2], prevs[3]);
            let q2 = quad::<32>(prevs[0], prevs[1], prevs[2], prevs[3]);
            let q3 = quad::<48>(prevs[0], prevs[1], prevs[2], prevs[3]);
            compressm_at!(
                &mut state,
                [
                    q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3], q2[0], q2[1], q2[2],
                    q2[3], q3[0], q3[1], q3[2], q3[3],
                ],
            );
            let nl = u8x16_splat(b'\n');
            let s0 = shift_cols(hx[0], nl);
            let s1 = shift_cols(hx[1], nl);
            let s2 = shift_cols(hx[2], nl);
            let s3 = shift_cols(hx[3], nl);
            let q0 = transpose_be(s0[0], s1[0], s2[0], s3[0]);
            let q1 = transpose_be(s0[1], s1[1], s2[1], s3[1]);
            let q2 = transpose_be(s0[2], s1[2], s2[2], s3[2]);
            let q3 = transpose_be(s0[3], s1[3], s2[3], s3[3]);
            compressm_at!(
                &mut state,
                [
                    q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3], q2[0], q2[1], q2[2],
                    q2[3], q3[0], q3[1], q3[2], q3[3],
                ],
            );
            compress_tail4(
                &mut state,
                &tail[super::tail_idx(hx[0][63])],
                &tail[super::tail_idx(hx[1][63])],
                &tail[super::tail_idx(hx[2][63])],
                &tail[super::tail_idx(hx[3][63])],
            );
            *out = lanes_hex(&state);
        }

        #[target_feature(enable = "simd128")]
        pub(super) fn mid_wk_quad(hexes: &[[u8; 64]], out: &mut [[u8; 256]; 4]) {
            let first = &hexes[0];
            let hx: [&[u8; 64]; 4] = [
                first,
                hexes.get(1).unwrap_or(first),
                hexes.get(2).unwrap_or(first),
                hexes.get(3).unwrap_or(first),
            ];
            let nl = u8x16_splat(b'\n');
            let s0 = shift_cols(hx[0], nl);
            let s1 = shift_cols(hx[1], nl);
            let s2 = shift_cols(hx[2], nl);
            let s3 = shift_cols(hx[3], nl);
            let q0 = transpose_be(s0[0], s1[0], s2[0], s3[0]);
            let q1 = transpose_be(s0[1], s1[1], s2[1], s3[1]);
            let q2 = transpose_be(s0[2], s1[2], s2[2], s3[2]);
            let q3 = transpose_be(s0[3], s1[3], s2[3], s3[3]);
            let [mut w0, mut w1, mut w2, mut w3] = q0;
            let [mut w4, mut w5, mut w6, mut w7] = q1;
            let [mut w8, mut w9, mut w10, mut w11] = q2;
            let [mut w12, mut w13, mut w14, mut w15] = q3;
            macro_rules! emit {
                ($i:literal, $a:ident, $b:ident, $c:ident, $d:ident) => {
                    let r = transpose32(
                        u32x4_add($a, u32x4_splat(K[$i])),
                        u32x4_add($b, u32x4_splat(K[$i + 1])),
                        u32x4_add($c, u32x4_splat(K[$i + 2])),
                        u32x4_add($d, u32x4_splat(K[$i + 3])),
                    );
                    out[0][4 * $i..4 * $i + 8]
                        .copy_from_slice(&u64x2_extract_lane::<0>(r[0]).to_le_bytes());
                    out[0][4 * $i + 8..4 * $i + 16]
                        .copy_from_slice(&u64x2_extract_lane::<1>(r[0]).to_le_bytes());
                    out[1][4 * $i..4 * $i + 8]
                        .copy_from_slice(&u64x2_extract_lane::<0>(r[1]).to_le_bytes());
                    out[1][4 * $i + 8..4 * $i + 16]
                        .copy_from_slice(&u64x2_extract_lane::<1>(r[1]).to_le_bytes());
                    out[2][4 * $i..4 * $i + 8]
                        .copy_from_slice(&u64x2_extract_lane::<0>(r[2]).to_le_bytes());
                    out[2][4 * $i + 8..4 * $i + 16]
                        .copy_from_slice(&u64x2_extract_lane::<1>(r[2]).to_le_bytes());
                    out[3][4 * $i..4 * $i + 8]
                        .copy_from_slice(&u64x2_extract_lane::<0>(r[3]).to_le_bytes());
                    out[3][4 * $i + 8..4 * $i + 16]
                        .copy_from_slice(&u64x2_extract_lane::<1>(r[3]).to_le_bytes());
                };
            }
            emit!(0, w0, w1, w2, w3);
            emit!(4, w4, w5, w6, w7);
            emit!(8, w8, w9, w10, w11);
            emit!(12, w12, w13, w14, w15);
            schedm!(w0, w1, w9, w14);
            schedm!(w1, w2, w10, w15);
            schedm!(w2, w3, w11, w0);
            schedm!(w3, w4, w12, w1);
            emit!(16, w0, w1, w2, w3);
            schedm!(w4, w5, w13, w2);
            schedm!(w5, w6, w14, w3);
            schedm!(w6, w7, w15, w4);
            schedm!(w7, w8, w0, w5);
            emit!(20, w4, w5, w6, w7);
            schedm!(w8, w9, w1, w6);
            schedm!(w9, w10, w2, w7);
            schedm!(w10, w11, w3, w8);
            schedm!(w11, w12, w4, w9);
            emit!(24, w8, w9, w10, w11);
            schedm!(w12, w13, w5, w10);
            schedm!(w13, w14, w6, w11);
            schedm!(w14, w15, w7, w12);
            schedm!(w15, w0, w8, w13);
            emit!(28, w12, w13, w14, w15);
            schedm!(w0, w1, w9, w14);
            schedm!(w1, w2, w10, w15);
            schedm!(w2, w3, w11, w0);
            schedm!(w3, w4, w12, w1);
            emit!(32, w0, w1, w2, w3);
            schedm!(w4, w5, w13, w2);
            schedm!(w5, w6, w14, w3);
            schedm!(w6, w7, w15, w4);
            schedm!(w7, w8, w0, w5);
            emit!(36, w4, w5, w6, w7);
            schedm!(w8, w9, w1, w6);
            schedm!(w9, w10, w2, w7);
            schedm!(w10, w11, w3, w8);
            schedm!(w11, w12, w4, w9);
            emit!(40, w8, w9, w10, w11);
            schedm!(w12, w13, w5, w10);
            schedm!(w13, w14, w6, w11);
            schedm!(w14, w15, w7, w12);
            schedm!(w15, w0, w8, w13);
            emit!(44, w12, w13, w14, w15);
            schedm!(w0, w1, w9, w14);
            schedm!(w1, w2, w10, w15);
            schedm!(w2, w3, w11, w0);
            schedm!(w3, w4, w12, w1);
            emit!(48, w0, w1, w2, w3);
            schedm!(w4, w5, w13, w2);
            schedm!(w5, w6, w14, w3);
            schedm!(w6, w7, w15, w4);
            schedm!(w7, w8, w0, w5);
            emit!(52, w4, w5, w6, w7);
            schedm!(w8, w9, w1, w6);
            schedm!(w9, w10, w2, w7);
            schedm!(w10, w11, w3, w8);
            schedm!(w11, w12, w4, w9);
            emit!(56, w8, w9, w10, w11);
            schedm!(w12, w13, w5, w10);
            schedm!(w13, w14, w6, w11);
            schedm!(w14, w15, w7, w12);
            schedm!(w15, w0, w8, w13);
            emit!(60, w12, w13, w14, w15);
        }

        #[target_feature(enable = "simd128")]
        pub(super) fn link_hex_mid(
            previous: &[u8; 64],
            mid_wk: &[u8; 256],
            last: u8,
            tail: &[[u32; 64]; 16],
        ) -> [u8; 64] {
            let mut state = H0;
            compress_block(&mut state, previous);
            super::compress_tail_b(&mut state, mid_wk);
            super::compress_tail(&mut state, &tail[super::tail_idx(last)]);
            hex_state(&state)
        }

        #[target_feature(enable = "simd128")]
        fn digest4(lines: [&[u8]; 4], blocks: usize) -> [[u8; 64]; 4] {
            let mut tails = [[0u8; 128]; 4];
            let mut full = [0usize; 4];
            for l in 0..4 {
                let bytes = lines[l];
                let rem = bytes.len() % 64;
                full[l] = bytes.len() / 64;
                let tail = &mut tails[l];
                tail[..rem].copy_from_slice(&bytes[bytes.len() - rem..]);
                tail[rem] = 0x80;
                let len_at = if rem >= 56 { 120 } else { 56 };
                tail[len_at..len_at + 8]
                    .copy_from_slice(&((bytes.len() as u64).wrapping_mul(8)).to_be_bytes());
            }
            let mut state = splat_h0();
            let shared = full[0].min(full[1]).min(full[2]).min(full[3]);
            let mut it0 = lines[0].chunks_exact(64);
            let mut it1 = lines[1].chunks_exact(64);
            let mut it2 = lines[2].chunks_exact(64);
            let mut it3 = lines[3].chunks_exact(64);
            for _ in 0..shared {
                let (Some(b0), Some(b1), Some(b2), Some(b3)) =
                    (it0.next(), it1.next(), it2.next(), it3.next())
                else {
                    break;
                };
                let b0: &[u8; 64] = b0.try_into().expect("64-byte block");
                let b1: &[u8; 64] = b1.try_into().expect("64-byte block");
                let b2: &[u8; 64] = b2.try_into().expect("64-byte block");
                let b3: &[u8; 64] = b3.try_into().expect("64-byte block");
                let q0 = quad::<0>(b0, b1, b2, b3);
                let q1 = quad::<16>(b0, b1, b2, b3);
                let q2 = quad::<32>(b0, b1, b2, b3);
                let q3 = quad::<48>(b0, b1, b2, b3);
                compressm_at!(
                    &mut state,
                    [
                        q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3], q2[0], q2[1],
                        q2[2], q2[3], q3[0], q3[1], q3[2], q3[3],
                    ],
                );
            }
            for k in shared..blocks {
                let b0 = block_at(lines[0], &tails[0], full[0], k);
                let b1 = block_at(lines[1], &tails[1], full[1], k);
                let b2 = block_at(lines[2], &tails[2], full[2], k);
                let b3 = block_at(lines[3], &tails[3], full[3], k);
                let q0 = quad::<0>(b0, b1, b2, b3);
                let q1 = quad::<16>(b0, b1, b2, b3);
                let q2 = quad::<32>(b0, b1, b2, b3);
                let q3 = quad::<48>(b0, b1, b2, b3);
                compressm_at!(
                    &mut state,
                    [
                        q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3], q2[0], q2[1],
                        q2[2], q2[3], q3[0], q3[1], q3[2], q3[3],
                    ],
                );
            }
            lanes_hex(&state)
        }

            #[target_feature(enable = "simd128")]
            pub(super) fn digest_all(lines: &[&[u8]]) -> Vec<[u8; 64]> {
                let mut out = vec![[0u8; 64]; lines.len()];

                macro_rules! run_quad {
                    ($q:expr, $blocks:expr) => {{
                        let qv: [u32; 4] = $q;
                        let i0 = qv[0] as usize;
                        let i1 = qv[1] as usize;
                        let i2 = qv[2] as usize;
                        let i3 = qv[3] as usize;
                        let l0 = lines[i0];
                        let l1 = lines[i1];
                        let l2 = lines[i2];
                        let l3 = lines[i3];
                        let full: usize = $blocks - 1;

                        let hexes = if (l0.len() >> 6) == full
                            && (l1.len() >> 6) == full
                            && (l2.len() >> 6) == full
                            && (l3.len() >> 6) == full
                        {
                            let off = full << 6;
                            let mut tails = [[0u8; 64]; 4];

                            let r0 = l0.len() - off;
                            tails[0][..r0].copy_from_slice(&l0[off..off + r0]);
                            tails[0][r0] = 0x80;
                            tails[0][56..64]
                                .copy_from_slice(&((l0.len() as u64).wrapping_mul(8)).to_be_bytes());

                            let r1 = l1.len() - off;
                            tails[1][..r1].copy_from_slice(&l1[off..off + r1]);
                            tails[1][r1] = 0x80;
                            tails[1][56..64]
                                .copy_from_slice(&((l1.len() as u64).wrapping_mul(8)).to_be_bytes());

                            let r2 = l2.len() - off;
                            tails[2][..r2].copy_from_slice(&l2[off..off + r2]);
                            tails[2][r2] = 0x80;
                            tails[2][56..64]
                                .copy_from_slice(&((l2.len() as u64).wrapping_mul(8)).to_be_bytes());

                            let r3 = l3.len() - off;
                            tails[3][..r3].copy_from_slice(&l3[off..off + r3]);
                            tails[3][r3] = 0x80;
                            tails[3][56..64]
                                .copy_from_slice(&((l3.len() as u64).wrapping_mul(8)).to_be_bytes());

                            let mut state = splat_h0();
                            let mut k = 0;
                            while k < full {
                                let p = k << 6;
                                let b0: &[u8; 64] =
                                    l0[p..p + 64].try_into().expect("64-byte block");
                                let b1: &[u8; 64] =
                                    l1[p..p + 64].try_into().expect("64-byte block");
                                let b2: &[u8; 64] =
                                    l2[p..p + 64].try_into().expect("64-byte block");
                                let b3: &[u8; 64] =
                                    l3[p..p + 64].try_into().expect("64-byte block");
                                let q0 = quad::<0>(b0, b1, b2, b3);
                                let q1 = quad::<16>(b0, b1, b2, b3);
                                let q2 = quad::<32>(b0, b1, b2, b3);
                                let q3 = quad::<48>(b0, b1, b2, b3);
                                compressm_at!(
                                    &mut state,
                                    [
                                        q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3],
                                        q2[0], q2[1], q2[2], q2[3], q3[0], q3[1], q3[2], q3[3],
                                    ],
                                );
                                k += 1;
                            }

                            let b0: &[u8; 64] = &tails[0];
                            let b1: &[u8; 64] = &tails[1];
                            let b2: &[u8; 64] = &tails[2];
                            let b3: &[u8; 64] = &tails[3];
                            let q0 = quad::<0>(b0, b1, b2, b3);
                            let q1 = quad::<16>(b0, b1, b2, b3);
                            let q2 = quad::<32>(b0, b1, b2, b3);
                            let q3 = quad::<48>(b0, b1, b2, b3);
                            compressm_at!(
                                &mut state,
                                [
                                    q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3],
                                    q2[0], q2[1], q2[2], q2[3], q3[0], q3[1], q3[2], q3[3],
                                ],
                            );
                            lanes_hex(&state)
                        } else {
                            digest4([l0, l1, l2, l3], $blocks)
                        };

                        out[i0] = hexes[0];
                        out[i1] = hexes[1];
                        out[i2] = hexes[2];
                        out[i3] = hexes[3];
                    }};
                }

                let mut p5 = [0u32; 4];
                let mut p6 = [0u32; 4];
                let mut c5 = 0usize;
                let mut c6 = 0usize;
                let mut pending: Vec<([u32; 4], usize)> = Vec::new();

                let mut i = 0usize;
                while i < lines.len() {
                    let blocks = (lines[i].len() + 72) >> 6;
                    if blocks == 6 {
                        p6[c6] = i as u32;
                        c6 += 1;
                        if c6 == 4 {
                            run_quad!(p6, 6usize);
                            c6 = 0;
                        }
                    } else if blocks == 5 {
                        p5[c5] = i as u32;
                        c5 += 1;
                        if c5 == 4 {
                            run_quad!(p5, 5usize);
                            c5 = 0;
                        }
                    } else {
                        if pending.len() <= blocks {
                            pending.resize(blocks + 1, ([0u32; 4], 0usize));
                        }
                        let c;
                        {
                            let slot = &mut pending[blocks];
                            c = slot.1;
                            slot.0[c] = i as u32;
                            if c == 3 {
                                slot.1 = 0;
                            } else {
                                slot.1 = c + 1;
                            }
                        }
                        if c == 3 {
                            let q = pending[blocks].0;
                            run_quad!(q, blocks);
                        }
                    }
                    i += 1;
                }

                let mut r = 0usize;
                while r < c5 {
                    let j = p5[r] as usize;
                    out[j] = digest_hex(lines[j]);
                    r += 1;
                }
                r = 0;
                while r < c6 {
                    let j = p6[r] as usize;
                    out[j] = digest_hex(lines[j]);
                    r += 1;
                }

                let mut b = 0usize;
                while b < pending.len() {
                    let (q, c) = pending[b];
                    let mut k = 0usize;
                    while k < c {
                        let j = q[k] as usize;
                        out[j] = digest_hex(lines[j]);
                        k += 1;
                    }
                    b += 1;
                }

                out
            }

        #[target_feature(enable = "simd128")]
        pub(super) fn digest_hex(bytes: &[u8]) -> [u8; 64] {
            let mut state = H0;
            let mut chunks = bytes.chunks_exact(64);
            for block in &mut chunks {
                compress_block(&mut state, block.try_into().expect("64-byte block"));
            }
            let rem = chunks.remainder();
            let mut tail = [0u8; 64];
            tail[..rem.len()].copy_from_slice(rem);
            tail[rem.len()] = 0x80;
            if rem.len() >= 56 {
                compress_block(&mut state, &tail);
                tail = [0u8; 64];
            }
            tail[56..].copy_from_slice(&((bytes.len() as u64).wrapping_mul(8)).to_be_bytes());
            compress_block(&mut state, &tail);
            hex_state(&state)
        }

        #[target_feature(enable = "simd128")]
        pub(super) fn link_hex(
            previous: &[u8; 64],
            event: &[u8; 64],
            tail: &[[u32; 64]; 16],
        ) -> [u8; 64] {
            let mut state = H0;
            compress_block(&mut state, previous);
            let v0 = load_raw::<0>(event);
            let v1 = load_raw::<16>(event);
            let v2 = load_raw::<32>(event);
            let v3 = load_raw::<48>(event);
            let nl = u8x16_splat(b'\n');
            let s0 = i8x16_shuffle::<2, 1, 0, 16, 6, 5, 4, 3, 10, 9, 8, 7, 14, 13, 12, 11>(v0, nl);
            let s1 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(v0, v1);
            let s2 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(v1, v2);
            let s3 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(v2, v3);
            compress_v(&mut state, s0, s1, s2, s3);
            super::compress_tail(&mut state, &tail[super::tail_idx(event[63])]);
            hex_state(&state)
        }
    }

    #[cfg(target_arch = "wasm32")]
    use simd::{digest_all, link_hex, link_hex_mid, link_quad, mid_wk_quad};

    #[cfg(not(target_arch = "wasm32"))]
    fn digest_all(lines: &[&[u8]]) -> Vec<[u8; 64]> {
        lines.iter().map(|line| digest_hex(line)).collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn digest_hex(bytes: &[u8]) -> [u8; 64] {
        hex_state(&digest_state(bytes))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn link_hex(previous: &[u8; 64], event: &[u8; 64], _tail: &[[u32; 64]; 16]) -> [u8; 64] {
        let mut msg = [0u8; 129];
        msg[..64].copy_from_slice(previous);
        msg[64] = b'\n';
        msg[65..].copy_from_slice(event);
        hex_state(&digest_state(&msg))
    }

    fn hex_string(hex: &[u8; 64]) -> String {
        String::from_utf8(hex.to_vec()).expect("hex output is ascii")
    }

    fn zero_hash() -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(ZERO_CHAIN_HASH.as_bytes());
        out
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
        use std::arch::wasm32::*;

        let n = bytes.len();
        let mut lines = Vec::with_capacity(n / 192 + 4);
        let nl = u8x16_splat(b'\n');
        let mut start = 0usize;
        let mut i = 0usize;

        macro_rules! load16 {
            ($buf:ident, $o:literal) => {{
                u64x2(
                    u64::from_le_bytes($buf[$o..$o + 8].try_into().expect("8-byte chunk")),
                    u64::from_le_bytes($buf[$o + 8..$o + 16].try_into().expect("8-byte chunk")),
                )
            }};
        }

        macro_rules! mask64 {
            ($buf:ident, $a:literal, $b:literal, $c:literal, $d:literal) => {{
                let m0 = u64::from(i8x16_bitmask(u8x16_eq(load16!($buf, $a), nl)) as u16);
                let m1 = u64::from(i8x16_bitmask(u8x16_eq(load16!($buf, $b), nl)) as u16);
                let m2 = u64::from(i8x16_bitmask(u8x16_eq(load16!($buf, $c), nl)) as u16);
                let m3 = u64::from(i8x16_bitmask(u8x16_eq(load16!($buf, $d), nl)) as u16);
                m0 | (m1 << 16) | (m2 << 32) | (m3 << 48)
            }};
        }

        macro_rules! push_line {
            ($end:expr) => {{
                let end = $end;
                if end > start && bytes[end - 1] != b'\r' {
                    lines.push(&bytes[start..end]);
                } else if end > start {
                    let trimmed = end - 1;
                    if trimmed > start {
                        lines.push(&bytes[start..trimmed]);
                    }
                }
                start = end + 1;
            }};
        }

        macro_rules! handle_mask {
            ($mask:expr, $base:expr) => {{
                let base: usize = $base;
                let mut mask: u64 = $mask;
                while mask != 0 {
                    let end = base + mask.trailing_zeros() as usize;
                    mask &= mask - 1;
                    push_line!(end);
                }
            }};
        }

        while i + 1024 <= n {
            let b: &[u8; 1024] = bytes[i..i + 1024].try_into().expect("1024-byte chunk");
            handle_mask!(mask64!(b, 0, 16, 32, 48), i);
            handle_mask!(mask64!(b, 64, 80, 96, 112), i + 64);
            handle_mask!(mask64!(b, 128, 144, 160, 176), i + 128);
            handle_mask!(mask64!(b, 192, 208, 224, 240), i + 192);
            handle_mask!(mask64!(b, 256, 272, 288, 304), i + 256);
            handle_mask!(mask64!(b, 320, 336, 352, 368), i + 320);
            handle_mask!(mask64!(b, 384, 400, 416, 432), i + 384);
            handle_mask!(mask64!(b, 448, 464, 480, 496), i + 448);
            handle_mask!(mask64!(b, 512, 528, 544, 560), i + 512);
            handle_mask!(mask64!(b, 576, 592, 608, 624), i + 576);
            handle_mask!(mask64!(b, 640, 656, 672, 688), i + 640);
            handle_mask!(mask64!(b, 704, 720, 736, 752), i + 704);
            handle_mask!(mask64!(b, 768, 784, 800, 816), i + 768);
            handle_mask!(mask64!(b, 832, 848, 864, 880), i + 832);
            handle_mask!(mask64!(b, 896, 912, 928, 944), i + 896);
            handle_mask!(mask64!(b, 960, 976, 992, 1008), i + 960);
            i += 1024;
        }

        while i + 512 <= n {
            let b: &[u8; 512] = bytes[i..i + 512].try_into().expect("512-byte chunk");
            handle_mask!(mask64!(b, 0, 16, 32, 48), i);
            handle_mask!(mask64!(b, 64, 80, 96, 112), i + 64);
            handle_mask!(mask64!(b, 128, 144, 160, 176), i + 128);
            handle_mask!(mask64!(b, 192, 208, 224, 240), i + 192);
            handle_mask!(mask64!(b, 256, 272, 288, 304), i + 256);
            handle_mask!(mask64!(b, 320, 336, 352, 368), i + 320);
            handle_mask!(mask64!(b, 384, 400, 416, 432), i + 384);
            handle_mask!(mask64!(b, 448, 464, 480, 496), i + 448);
            i += 512;
        }

        while i + 64 <= n {
            let b: &[u8; 64] = bytes[i..i + 64].try_into().expect("64-byte chunk");
            handle_mask!(mask64!(b, 0, 16, 32, 48), i);
            i += 64;
        }

        while i < n {
            if bytes[i] == b'\n' {
                push_line!(i);
            }
            i += 1;
        }

        if start < n {
            if bytes[n - 1] != b'\r' {
                lines.push(&bytes[start..n]);
            } else {
                let end = n - 1;
                if end > start {
                    lines.push(&bytes[start..end]);
                }
            }
        }

        lines
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut acc = 0u64;
        let mut ca = a.chunks_exact(8);
        let mut cb = b.chunks_exact(8);
        for (x, y) in (&mut ca).zip(&mut cb) {
            acc |= u64::from_le_bytes(x.try_into().expect("8-byte chunk"))
                ^ u64::from_le_bytes(y.try_into().expect("8-byte chunk"));
        }
        for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
            acc |= u64::from(x ^ y);
        }
        acc == 0
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn eq64(a: &[u8; 64], b: &[u8; 64]) -> bool {
        use std::arch::wasm32::*;
        macro_rules! lane {
            ($p:ident, $o:literal) => {
                u64x2(
                    u64::from_le_bytes($p[$o..$o + 8].try_into().expect("8-byte chunk")),
                    u64::from_le_bytes($p[$o + 8..$o + 16].try_into().expect("8-byte chunk")),
                )
            };
        }
        let d = v128_or(
            v128_or(
                v128_xor(lane!(a, 0), lane!(b, 0)),
                v128_xor(lane!(a, 16), lane!(b, 16)),
            ),
            v128_or(
                v128_xor(lane!(a, 32), lane!(b, 32)),
                v128_xor(lane!(a, 48), lane!(b, 48)),
            ),
        );
        !v128_any_true(d)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn eq64(a: &[u8; 64], b: &[u8; 64]) -> bool {
        let mut acc = 0u64;
        let mut i = 0;
        while i < 64 {
            acc |= u64::from_le_bytes(a[i..i + 8].try_into().expect("8-byte chunk"))
                ^ u64::from_le_bytes(b[i..i + 8].try_into().expect("8-byte chunk"));
            i += 8;
        }
        acc == 0
    }

    fn fold_digits(digits: &[u8]) -> u64 {
        let mut value = 0u64;
        for &c in digits {
            value = value.wrapping_mul(10).wrapping_add(u64::from(c - b'0'));
        }
        value
    }

    fn uuid_eq(a: &str, b: &str) -> bool {
        bytes_eq(a.as_bytes(), b.as_bytes()) || a.eq_ignore_ascii_case(b)
    }

    fn digit_run(buf: &[u8], mut i: usize) -> usize {
        const LO: u64 = 0x0101010101010101;
        while i + 8 <= buf.len() {
            let x = u64::from_le_bytes(buf[i..i + 8].try_into().expect("8-byte chunk"));
            let bad = ((x & (LO * 0xf0)) ^ (LO * 0x30))
                | (((x & (LO * 0x0f)).wrapping_add(LO * 0x06)) & (LO * 0x10));
            if bad != 0 {
                return i + (bad.trailing_zeros() >> 3) as usize;
            }
            i += 8;
        }
        while buf.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    }

    #[inline(always)]
    fn eq_small<const N: usize>(a: &[u8; N], s: &[u8; N]) -> bool {
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
        acc == 0
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    #[target_feature(enable = "simd128")]
    fn eq_n<const N: usize>(a: &[u8; N], s: &[u8; N]) -> bool {
        use std::arch::wasm32::*;
        if N < 16 {
            return eq_small(a, s);
        }
        let ld = |x: &[u8]| {
            u64x2(
                u64::from_le_bytes(x[..8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(x[8..16].try_into().expect("8-byte chunk")),
            )
        };
        let mut acc = v128_xor(ld(&a[..16]), ld(&s[..16]));
        let mut i = 16;
        while i + 16 <= N {
            acc = v128_or(acc, v128_xor(ld(&a[i..i + 16]), ld(&s[i..i + 16])));
            i += 16;
        }
        if i < N {
            acc = v128_or(acc, v128_xor(ld(&a[N - 16..]), ld(&s[N - 16..])));
        }
        !v128_any_true(acc)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[inline(always)]
    fn eq_n<const N: usize>(a: &[u8; N], s: &[u8; N]) -> bool {
        eq_small(a, s)
    }

    #[inline(always)]
    fn tag<const N: usize>(buf: &[u8], pos: usize, s: &[u8; N]) -> usize {
        let end = pos + N;
        let Some(a) = buf.get(pos..end) else {
            return 0;
        };
        let a: &[u8; N] = a.try_into().expect("length-checked slice");
        if eq_n(a, s) {
            end
        } else {
            0
        }
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    #[target_feature(enable = "simd128")]
    fn str_end(buf: &[u8], start: usize) -> usize {
        use std::arch::wasm32::*;
        let mut i = start;
        while i + 16 <= buf.len() {
            let c: &[u8; 16] = buf[i..i + 16].try_into().expect("16-byte chunk");
            let v = u64x2(
                u64::from_le_bytes(c[0..8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(c[8..16].try_into().expect("8-byte chunk")),
            );
            let stop = v128_or(
                v128_or(
                    u8x16_eq(v, u8x16_splat(b'"')),
                    u8x16_eq(v, u8x16_splat(b'\\')),
                ),
                v128_or(
                    i8x16_lt(v, i8x16_splat(0x20)),
                    u8x16_eq(v, u8x16_splat(0x7f)),
                ),
            );
            let mask = i8x16_bitmask(stop);
            if mask != 0 {
                i += mask.trailing_zeros() as usize;
                if buf[i] == b'"' {
                    return i + 1;
                }
                return 0;
            }
            i += 16;
        }
        while let Some(&c) = buf.get(i) {
            if c == b'"' {
                return i + 1;
            }
            if !(0x20..0x7f).contains(&c) || c == b'\\' {
                return 0;
            }
            i += 1;
        }
        0
    }

    // SWAR stop-byte detector: every spurious flag either sits on a byte
    // with the high bit set or lands strictly above a true hit, so the
    // lowest flagged byte is a genuine stop byte, re-checked exactly.
    #[cfg(not(target_arch = "wasm32"))]
    fn str_end(buf: &[u8], start: usize) -> usize {
        const LO: u64 = 0x0101010101010101;
        const HI: u64 = 0x8080808080808080;
        let mut i = start;
        while i + 8 <= buf.len() {
            let x = u64::from_le_bytes(buf[i..i + 8].try_into().expect("8-byte chunk"));
            let quote = (x ^ (LO * 0x22)).wrapping_sub(LO);
            let slash = (x ^ (LO * 0x5c)).wrapping_sub(LO);
            let hit = (quote | slash | x.wrapping_sub(LO * 0x20) | x.wrapping_add(LO) | x) & HI;
            if hit != 0 {
                i += (hit.trailing_zeros() >> 3) as usize;
                if buf[i] == b'"' {
                    return i + 1;
                }
                return 0;
            }
            i += 8;
        }
        while let Some(&c) = buf.get(i) {
            if c == b'"' {
                return i + 1;
            }
            if !(0x20..0x7f).contains(&c) || c == b'\\' {
                return 0;
            }
            i += 1;
        }
        0
    }

    // Speculative fixed-width string; any miss falls back to the exact scan,
    // so acceptance is identical by construction. N must be >= 16.
    #[cfg(target_arch = "wasm32")]
    #[inline]
    #[target_feature(enable = "simd128")]
    fn qstr<const N: usize>(buf: &[u8], pos: usize) -> usize {
        use std::arch::wasm32::*;
        let end = pos + N;
        if buf.get(end) == Some(&b'"') {
            let body: &[u8; N] = buf[pos..end].try_into().expect("length-checked slice");
            let stops = |c: &[u8]| {
                let v = u64x2(
                    u64::from_le_bytes(c[0..8].try_into().expect("8-byte chunk")),
                    u64::from_le_bytes(c[8..16].try_into().expect("8-byte chunk")),
                );
                v128_or(
                    v128_or(
                        u8x16_eq(v, u8x16_splat(b'"')),
                        u8x16_eq(v, u8x16_splat(b'\\')),
                    ),
                    v128_or(
                        i8x16_lt(v, i8x16_splat(0x20)),
                        u8x16_eq(v, u8x16_splat(0x7f)),
                    ),
                )
            };
            let mut acc = stops(&body[0..16]);
            let mut i = 16;
            while i + 16 <= N {
                acc = v128_or(acc, stops(&body[i..i + 16]));
                i += 16;
            }
            if i < N {
                acc = v128_or(acc, stops(&body[N - 16..N]));
            }
            if !v128_any_true(acc) {
                return end + 1;
            }
        }
        str_end(buf, pos)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[inline]
    fn qstr<const N: usize>(buf: &[u8], pos: usize) -> usize {
        str_end(buf, pos)
    }

    #[inline(always)]
    fn digits_end(buf: &[u8], start: usize) -> usize {
        let end = digit_run(buf, start);
        let len = end - start;
        if len == 0 || len > 19 || (buf[start] == b'0' && len > 1) {
            return 0;
        }
        end
    }

    fn num_end(buf: &[u8], mut pos: usize) -> usize {
        if buf.get(pos) == Some(&b'-') {
            pos += 1;
        }
        let start = pos;
        pos = digit_run(buf, start);
        let len = pos - start;
        if len == 0 || len > 17 || (buf[start] == b'0' && len > 1) {
            return 0;
        }
        if buf.get(pos) == Some(&b'.') {
            let frac = pos + 1;
            pos = digit_run(buf, frac);
            if pos == frac || pos - frac > 17 {
                return 0;
            }
        }
        if matches!(buf.get(pos), Some(b'e' | b'E')) {
            return 0;
        }
        pos
    }

    // Strict scanner: accepts only inputs serde_json provably accepts with
    // identical extracted values; anything unusual bails to serde.
    struct Scan<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Scan<'a> {
        #[inline(always)]
        fn lit<const N: usize>(&mut self, s: &[u8; N]) -> bool {
            let end = self.pos + N;
            let Some(a) = self.buf.get(self.pos..end) else {
                return false;
            };
            let a: &[u8; N] = a.try_into().expect("length-checked slice");
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

        fn string_body(&mut self) -> Option<&'a [u8]> {
            let start = self.pos;
            match str_end(self.buf, start) {
                0 => None,
                p => {
                    self.pos = p;
                    Some(&self.buf[start..p - 1])
                }
            }
        }

        fn number(&mut self) -> bool {
            match num_end(self.buf, self.pos) {
                0 => false,
                p => {
                    self.pos = p;
                    true
                }
            }
        }

        #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
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
    }

    fn ascii_str(bytes: &[u8]) -> Option<&str> {
        std::str::from_utf8(bytes).ok()
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn kind_value(buf: &[u8], pos: usize) -> usize {
        let arm = match (buf.get(pos + 9).copied(), buf.get(pos + 21).copied()) {
            (Some(b'i'), _) => 0u32,
            (Some(b'h'), Some(b'i')) => 1,
            (Some(b'h'), Some(b'c')) => 2,
            _ => 3,
        };
        'fast: {
            if arm == 0 {
                let mut p = tag(buf, pos, b"{\"type\":\"intent_dispatched\",\"intent_id\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<36>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"intent_text\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<17>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"matched_agent\":");
                if p == 0 {
                    break 'fast;
                }
                let q = tag(buf, p, b"null");
                if q != 0 {
                    p = q;
                } else {
                    p = tag(buf, p, b"\"");
                    if p == 0 {
                        break 'fast;
                    }
                    p = str_end(buf, p);
                    if p == 0 {
                        break 'fast;
                    }
                }
                p = tag(buf, p, b",\"result_hash_hex\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<64>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"status\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = str_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b"}");
                if p == 0 {
                    break 'fast;
                }
                return p;
            } else if arm == 1 {
                let mut p = tag(buf, pos, b"{\"type\":\"hermes_tool_invoked\",\"intent_id\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<36>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"run_id\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<16>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"tool\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = str_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"preview_hash_hex\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<64>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b"}");
                if p == 0 {
                    break 'fast;
                }
                return p;
            } else if arm == 2 {
                let mut p = tag(buf, pos, b"{\"type\":\"hermes_tool_completed\",\"intent_id\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<36>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"run_id\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = qstr::<16>(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"tool\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = str_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"duration_ms\":");
                if p == 0 {
                    break 'fast;
                }
                p = num_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"error\":");
                if p == 0 {
                    break 'fast;
                }
                let q = tag(buf, p, b"true");
                if q != 0 {
                    p = q;
                } else {
                    p = tag(buf, p, b"false");
                    if p == 0 {
                        break 'fast;
                    }
                }
                p = tag(buf, p, b"}");
                if p == 0 {
                    break 'fast;
                }
                return p;
            }
        }
        let mut s = Scan { buf, pos };
        if s.value(0) {
            s.pos
        } else {
            0
        }
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn issuer_value(buf: &[u8], pos: usize) -> usize {
        if buf.get(pos) == Some(&b'"') {
            return str_end(buf, pos + 1);
        }
        if buf.get(pos) == Some(&b'{') {
            'fast: {
                let mut p = tag(buf, pos, b"{\"display\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = str_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b",\"pubkey_b58\":\"");
                if p == 0 {
                    break 'fast;
                }
                p = str_end(buf, p);
                if p == 0 {
                    break 'fast;
                }
                p = tag(buf, p, b"}");
                if p != 0 {
                    return p;
                }
            }
        }
        let mut s = Scan { buf, pos };
        if s.value(0) {
            s.pos
        } else {
            0
        }
    }

    #[cfg(target_arch = "wasm32")]
    macro_rules! stops_at {
        ($a:expr, $p:expr) => {{
            let c: &[u8; 16] = $a[$p..$p + 16].try_into().expect("16-byte chunk");
            let v = u64x2(
                u64::from_le_bytes(c[0..8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(c[8..16].try_into().expect("8-byte chunk")),
            );
            v128_or(
                v128_or(
                    u8x16_eq(v, u8x16_splat(b'"')),
                    u8x16_eq(v, u8x16_splat(b'\\')),
                ),
                v128_or(i8x16_lt(v, i8x16_splat(0x20)), u8x16_eq(v, u8x16_splat(0x7f))),
            )
        }};
    }

    #[cfg(target_arch = "wasm32")]
    macro_rules! ldw {
        ($a:expr, $p:expr) => {{
            let c: &[u8; 16] = $a[$p..$p + 16].try_into().expect("16-byte chunk");
            u64x2(
                u64::from_le_bytes(c[0..8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(c[8..16].try_into().expect("8-byte chunk")),
            )
        }};
    }

    #[cfg(target_arch = "wasm32")]
    #[inline(always)]
    fn all_digits8(x: u64) -> bool {
        ((x.wrapping_add(0x4646_4646_4646_4646) | x.wrapping_sub(0x3030_3030_3030_3030))
            & 0x8080_8080_8080_8080)
            == 0
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn tmpl_k0n(a: &[u8; 339]) -> bool {
        use std::arch::wasm32::*;
        let mut bad = v128_and(v128_xor(ldw!(a, 0), u8x16(123, 34, 105, 100, 34, 58, 34, 0, 0, 0, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0));
        bad = v128_or(bad, stops_at!(a, 7));
        bad = v128_or(bad, stops_at!(a, 23));
        bad = v128_or(bad, stops_at!(a, 27));
        bad = v128_or(bad, v128_xor(ldw!(a, 43), u8x16(34, 44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 44), u8x16(44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34, 58)));
        let mut ok = all_digits8(u64::from_le_bytes(a[60..68].try_into().expect("8-byte chunk")));
        ok &= all_digits8(u64::from_le_bytes(a[65..73].try_into().expect("8-byte chunk")));
        ok &= a[60] != b'0';
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 73), u8x16(44, 34, 105, 115, 115, 117, 101, 114, 34, 58, 34, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 84), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 96), u8x16(34, 44, 34, 107, 105, 110, 100, 34, 58, 123, 34, 116, 121, 112, 101, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 112), u8x16(58, 34, 105, 110, 116, 101, 110, 116, 95, 100, 105, 115, 112, 97, 116, 99)));
        bad = v128_or(bad, v128_xor(ldw!(a, 128), u8x16(104, 101, 100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 130), u8x16(100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 146));
        bad = v128_or(bad, stops_at!(a, 162));
        bad = v128_or(bad, stops_at!(a, 166));
        bad = v128_or(bad, v128_xor(ldw!(a, 182), u8x16(34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 116, 101, 120, 116, 34, 58)));
        bad = v128_or(bad, v128_xor(ldw!(a, 183), u8x16(44, 34, 105, 110, 116, 101, 110, 116, 95, 116, 101, 120, 116, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 199));
        bad = v128_or(bad, stops_at!(a, 200));
        bad = v128_or(bad, v128_xor(ldw!(a, 216), u8x16(34, 44, 34, 109, 97, 116, 99, 104, 101, 100, 95, 97, 103, 101, 110, 116)));
        bad = v128_or(bad, v128_xor(ldw!(a, 232), u8x16(34, 58, 110, 117, 108, 108, 44, 34, 114, 101, 115, 117, 108, 116, 95, 104)));
        bad = v128_or(bad, v128_xor(ldw!(a, 242), u8x16(115, 117, 108, 116, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 258));
        bad = v128_or(bad, stops_at!(a, 274));
        bad = v128_or(bad, stops_at!(a, 290));
        bad = v128_or(bad, stops_at!(a, 306));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 322), u8x16(34, 44, 34, 115, 116, 97, 116, 117, 115, 34, 58, 34, 0, 0, 34, 125)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 255)));
        bad = v128_or(bad, v128_and(stops_at!(a, 323), u8x16(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 0)));
        ok &= a[338] == b'}';
        ok && !v128_any_true(bad)
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn tmpl_k0a(a: &[u8; 347]) -> bool {
        use std::arch::wasm32::*;
        let mut bad = v128_and(v128_xor(ldw!(a, 0), u8x16(123, 34, 105, 100, 34, 58, 34, 0, 0, 0, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0));
        bad = v128_or(bad, stops_at!(a, 7));
        bad = v128_or(bad, stops_at!(a, 23));
        bad = v128_or(bad, stops_at!(a, 27));
        bad = v128_or(bad, v128_xor(ldw!(a, 43), u8x16(34, 44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 44), u8x16(44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34, 58)));
        let mut ok = all_digits8(u64::from_le_bytes(a[60..68].try_into().expect("8-byte chunk")));
        ok &= all_digits8(u64::from_le_bytes(a[65..73].try_into().expect("8-byte chunk")));
        ok &= a[60] != b'0';
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 73), u8x16(44, 34, 105, 115, 115, 117, 101, 114, 34, 58, 34, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 84), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 96), u8x16(34, 44, 34, 107, 105, 110, 100, 34, 58, 123, 34, 116, 121, 112, 101, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 112), u8x16(58, 34, 105, 110, 116, 101, 110, 116, 95, 100, 105, 115, 112, 97, 116, 99)));
        bad = v128_or(bad, v128_xor(ldw!(a, 128), u8x16(104, 101, 100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 130), u8x16(100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 146));
        bad = v128_or(bad, stops_at!(a, 162));
        bad = v128_or(bad, stops_at!(a, 166));
        bad = v128_or(bad, v128_xor(ldw!(a, 182), u8x16(34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 116, 101, 120, 116, 34, 58)));
        bad = v128_or(bad, v128_xor(ldw!(a, 183), u8x16(44, 34, 105, 110, 116, 101, 110, 116, 95, 116, 101, 120, 116, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 199));
        bad = v128_or(bad, stops_at!(a, 200));
        bad = v128_or(bad, v128_xor(ldw!(a, 216), u8x16(34, 44, 34, 109, 97, 116, 99, 104, 101, 100, 95, 97, 103, 101, 110, 116)));
        bad = v128_or(bad, v128_xor(ldw!(a, 219), u8x16(109, 97, 116, 99, 104, 101, 100, 95, 97, 103, 101, 110, 116, 34, 58, 34)));
        bad = v128_or(bad, v128_and(stops_at!(a, 235), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 245), u8x16(34, 44, 34, 114, 101, 115, 117, 108, 116, 95, 104, 97, 115, 104, 95, 104)));
        bad = v128_or(bad, v128_xor(ldw!(a, 250), u8x16(115, 117, 108, 116, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 266));
        bad = v128_or(bad, stops_at!(a, 282));
        bad = v128_or(bad, stops_at!(a, 298));
        bad = v128_or(bad, stops_at!(a, 314));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 330), u8x16(34, 44, 34, 115, 116, 97, 116, 117, 115, 34, 58, 34, 0, 0, 34, 125)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 255)));
        bad = v128_or(bad, v128_and(stops_at!(a, 331), u8x16(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 0)));
        ok &= a[346] == b'}';
        ok && !v128_any_true(bad)
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn tmpl_k1(a: &[u8; 322]) -> bool {
        use std::arch::wasm32::*;
        let mut bad = v128_and(v128_xor(ldw!(a, 0), u8x16(123, 34, 105, 100, 34, 58, 34, 0, 0, 0, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0));
        bad = v128_or(bad, stops_at!(a, 7));
        bad = v128_or(bad, stops_at!(a, 23));
        bad = v128_or(bad, stops_at!(a, 27));
        bad = v128_or(bad, v128_xor(ldw!(a, 43), u8x16(34, 44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 44), u8x16(44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34, 58)));
        let mut ok = all_digits8(u64::from_le_bytes(a[60..68].try_into().expect("8-byte chunk")));
        ok &= all_digits8(u64::from_le_bytes(a[65..73].try_into().expect("8-byte chunk")));
        ok &= a[60] != b'0';
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 73), u8x16(44, 34, 105, 115, 115, 117, 101, 114, 34, 58, 34, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 84), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 96), u8x16(34, 44, 34, 107, 105, 110, 100, 34, 58, 123, 34, 116, 121, 112, 101, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 112), u8x16(58, 34, 104, 101, 114, 109, 101, 115, 95, 116, 111, 111, 108, 95, 105, 110)));
        bad = v128_or(bad, v128_xor(ldw!(a, 128), u8x16(118, 111, 107, 101, 100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105)));
        bad = v128_or(bad, v128_xor(ldw!(a, 132), u8x16(100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 148));
        bad = v128_or(bad, stops_at!(a, 164));
        bad = v128_or(bad, stops_at!(a, 168));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 184), u8x16(34, 44, 34, 114, 117, 110, 95, 105, 100, 34, 58, 34, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, stops_at!(a, 196));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 212), u8x16(34, 44, 34, 116, 111, 111, 108, 34, 58, 34, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 222), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 233), u8x16(34, 44, 34, 112, 114, 101, 118, 105, 101, 119, 95, 104, 97, 115, 104, 95)));
        bad = v128_or(bad, v128_xor(ldw!(a, 239), u8x16(118, 105, 101, 119, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 255));
        bad = v128_or(bad, stops_at!(a, 271));
        bad = v128_or(bad, stops_at!(a, 287));
        bad = v128_or(bad, stops_at!(a, 303));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 306), u8x16(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 34, 125, 125)), u8x16(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255)));
        ok && !v128_any_true(bad)
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn tmpl_k2<const D: usize, const ERR: bool, const L: usize>(a: &[u8; L]) -> bool {
        use std::arch::wasm32::*;
        let mut bad = v128_and(v128_xor(ldw!(a, 0), u8x16(123, 34, 105, 100, 34, 58, 34, 0, 0, 0, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0));
        bad = v128_or(bad, stops_at!(a, 7));
        bad = v128_or(bad, stops_at!(a, 23));
        bad = v128_or(bad, stops_at!(a, 27));
        bad = v128_or(bad, v128_xor(ldw!(a, 43), u8x16(34, 44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 44), u8x16(44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34, 58)));
        let mut ok = all_digits8(u64::from_le_bytes(a[60..68].try_into().expect("8-byte chunk")));
        ok &= all_digits8(u64::from_le_bytes(a[65..73].try_into().expect("8-byte chunk")));
        ok &= a[60] != b'0';
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 73), u8x16(44, 34, 105, 115, 115, 117, 101, 114, 34, 58, 34, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 84), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 96), u8x16(34, 44, 34, 107, 105, 110, 100, 34, 58, 123, 34, 116, 121, 112, 101, 34)));
        bad = v128_or(bad, v128_xor(ldw!(a, 112), u8x16(58, 34, 104, 101, 114, 109, 101, 115, 95, 116, 111, 111, 108, 95, 99, 111)));
        bad = v128_or(bad, v128_xor(ldw!(a, 128), u8x16(109, 112, 108, 101, 116, 101, 100, 34, 44, 34, 105, 110, 116, 101, 110, 116)));
        bad = v128_or(bad, v128_xor(ldw!(a, 134), u8x16(100, 34, 44, 34, 105, 110, 116, 101, 110, 116, 95, 105, 100, 34, 58, 34)));
        bad = v128_or(bad, stops_at!(a, 150));
        bad = v128_or(bad, stops_at!(a, 166));
        bad = v128_or(bad, stops_at!(a, 170));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 186), u8x16(34, 44, 34, 114, 117, 110, 95, 105, 100, 34, 58, 34, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0)));
        bad = v128_or(bad, stops_at!(a, 198));
        bad = v128_or(bad, v128_and(v128_xor(ldw!(a, 214), u8x16(34, 44, 34, 116, 111, 111, 108, 34, 58, 34, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_and(stops_at!(a, 224), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0)));
        bad = v128_or(bad, v128_xor(ldw!(a, 235), u8x16(34, 44, 34, 100, 117, 114, 97, 116, 105, 111, 110, 95, 109, 115, 34, 58)));
        let mut q = 251;
        while q < 251 + D {
            ok &= a[q].is_ascii_digit();
            q += 1;
        }
        if D > 1 {
            ok &= a[251] != b'0';
        }
        if ERR {
            bad = v128_or(
                bad,
                v128_and(v128_xor(ldw!(a, 250 + D), u8x16(0, 44, 34, 101, 114, 114, 111, 114, 34, 58, 116, 114, 117, 101, 125, 125)), u8x16(0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255)),
            );
        } else {
            bad = v128_or(bad, v128_xor(ldw!(a, 251 + D), u8x16(44, 34, 101, 114, 114, 111, 114, 34, 58, 102, 97, 108, 115, 101, 125, 125)));
        }
        ok && !v128_any_true(bad)
    }

    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn fast_event_tmpl(line: &[u8]) -> u32 {
        let hit = match line.get(114) {
            Some(b'i') => match line.len() {
                339 => matches!(<&[u8; 339]>::try_from(line), Ok(a) if tmpl_k0n(a)),
                347 => matches!(<&[u8; 347]>::try_from(line), Ok(a) if tmpl_k0a(a)),
                _ => false,
            },
            Some(b'h') => match line.get(126) {
                Some(b'i') => matches!(<&[u8; 322]>::try_from(line), Ok(a) if tmpl_k1(a)),
                Some(b'c') => {
                    let n = line.len();
                    match (n, line.get(n.wrapping_sub(4))) {
                        (267, _) => matches!(<&[u8; 267]>::try_from(line), Ok(a) if tmpl_k2::<1, true, 267>(a)),
                        (268, Some(b'u')) => matches!(<&[u8; 268]>::try_from(line), Ok(a) if tmpl_k2::<2, true, 268>(a)),
                        (268, _) => matches!(<&[u8; 268]>::try_from(line), Ok(a) if tmpl_k2::<1, false, 268>(a)),
                        (269, Some(b'u')) => matches!(<&[u8; 269]>::try_from(line), Ok(a) if tmpl_k2::<3, true, 269>(a)),
                        (269, _) => matches!(<&[u8; 269]>::try_from(line), Ok(a) if tmpl_k2::<2, false, 269>(a)),
                        (270, Some(b'u')) => matches!(<&[u8; 270]>::try_from(line), Ok(a) if tmpl_k2::<4, true, 270>(a)),
                        (270, _) => matches!(<&[u8; 270]>::try_from(line), Ok(a) if tmpl_k2::<3, false, 270>(a)),
                        (271, _) => matches!(<&[u8; 271]>::try_from(line), Ok(a) if tmpl_k2::<4, false, 271>(a)),
                        _ => false,
                    }
                }
                _ => false,
            },
            _ => false,
        };
        if hit {
            36 << 5 | 13
        } else {
            0
        }
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    #[inline]
    fn fast_event(line: &[u8]) -> u32 {
        #[cfg(target_arch = "wasm32")]
        {
            let hit = match line.len() {
                267 => match <&[u8; 267]>::try_from(line) {
                    Ok(a) => tmpl_k2::<1, true, 267>(a),
                    Err(_) => false,
                },
                268 => {
                    if line[264] == b'u' {
                        match <&[u8; 268]>::try_from(line) {
                            Ok(a) => tmpl_k2::<2, true, 268>(a),
                            Err(_) => false,
                        }
                    } else {
                        match <&[u8; 268]>::try_from(line) {
                            Ok(a) => tmpl_k2::<1, false, 268>(a),
                            Err(_) => false,
                        }
                    }
                }
                269 => {
                    if line[265] == b'u' {
                        match <&[u8; 269]>::try_from(line) {
                            Ok(a) => tmpl_k2::<3, true, 269>(a),
                            Err(_) => false,
                        }
                    } else {
                        match <&[u8; 269]>::try_from(line) {
                            Ok(a) => tmpl_k2::<2, false, 269>(a),
                            Err(_) => false,
                        }
                    }
                }
                270 => {
                    if line[266] == b'u' {
                        match <&[u8; 270]>::try_from(line) {
                            Ok(a) => tmpl_k2::<4, true, 270>(a),
                            Err(_) => false,
                        }
                    } else {
                        match <&[u8; 270]>::try_from(line) {
                            Ok(a) => tmpl_k2::<3, false, 270>(a),
                            Err(_) => false,
                        }
                    }
                }
                271 => match <&[u8; 271]>::try_from(line) {
                    Ok(a) => tmpl_k2::<4, false, 271>(a),
                    Err(_) => false,
                },
                322 => match <&[u8; 322]>::try_from(line) {
                    Ok(a) => tmpl_k1(a),
                    Err(_) => false,
                },
                339 => match <&[u8; 339]>::try_from(line) {
                    Ok(a) => tmpl_k0n(a),
                    Err(_) => false,
                },
                347 => match <&[u8; 347]>::try_from(line) {
                    Ok(a) => tmpl_k0a(a),
                    Err(_) => false,
                },
                _ => false,
            };
            if hit {
                return 36 << 5 | 13;
            }
        }
        fast_event_cold(line)
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn fast_event_cold(line: &[u8]) -> u32 {
        let mut p = tag(line, 0, b"{\"id\":\"");
        if p == 0 {
            return 0;
        }
        p = qstr::<36>(line, p);
        if p == 0 {
            return 0;
        }
        let id_len = p - 1 - 7;
        p = tag(line, p, b",\"timestamp_ms\":");
        if p == 0 {
            return 0;
        }
        let ts_start = p;
        p = digits_end(line, ts_start);
        if p == 0 {
            return 0;
        }
        let ts_len = p - ts_start;
        p = tag(line, p, b",\"issuer\":");
        if p == 0 {
            return 0;
        }
        p = issuer_value(line, p);
        if p == 0 {
            return 0;
        }
        p = tag(line, p, b",\"kind\":");
        if p == 0 {
            return 0;
        }
        p = kind_value(line, p);
        if p == 0 {
            return 0;
        }
        p = tag(line, p, b"}");
        if p != 0 && p == line.len() && id_len < 1 << 27 {
            (id_len as u32) << 5 | ts_len as u32
        } else {
            0
        }
    }

    #[inline(always)]
    fn event_spans(line: &[u8], packed: u32) -> (&[u8], &[u8]) {
        let id_len = (packed >> 5) as usize;
        let ts_start = 7 + id_len + 17;
        let ts_len = (packed & 31) as usize;
        (&line[7..7 + id_len], &line[ts_start..ts_start + ts_len])
    }

    fn parse_event(line: &[u8]) -> Result<(Cow<'_, str>, u64), ()> {
        let packed = fast_event(line);
        if packed != 0 {
            let (id, ts) = event_spans(line, packed);
            if let Some(id) = ascii_str(id) {
                return Ok((Cow::Borrowed(id), fold_digits(ts)));
            }
        }
        match serde_json::from_slice::<EventFields>(line) {
            Ok(event) => Ok((event.id, event.timestamp_ms)),
            Err(_) => Err(()),
        }
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn fast_anchor(line: &[u8]) -> Option<AnchorFields<'_>> {
        let mut p = tag(line, 0, b"{\"index\":");
        if p == 0 {
            return None;
        }
        let idx_start = p;
        p = digits_end(line, idx_start);
        if p == 0 {
            return None;
        }
        let index = fold_digits(&line[idx_start..p]);
        p = tag(line, p, b",\"event_id\":\"");
        if p == 0 {
            return None;
        }
        let id_start = p;
        p = qstr::<36>(line, p);
        if p == 0 {
            return None;
        }
        let event_id = &line[id_start..p - 1];
        p = tag(line, p, b",\"timestamp_ms\":");
        if p == 0 {
            return None;
        }
        let ts_start = p;
        p = digits_end(line, ts_start);
        if p == 0 {
            return None;
        }
        let timestamp_ms = fold_digits(&line[ts_start..p]);
        p = tag(line, p, b",\"event_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let ev_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let event_hash_hex = &line[ev_start..p - 1];
        p = tag(line, p, b",\"previous_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let prev_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let previous_hash_hex = &line[prev_start..p - 1];
        p = tag(line, p, b",\"chain_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let chain_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let chain_hash_hex = &line[chain_start..p - 1];
        p = tag(line, p, b"}");
        if p == 0 || p != line.len() {
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

    // Shape-only acceptance test; used when the anchor's trailing chain hash
    // already differs, so a shape-valid anchor is a guaranteed EntryMismatch.
    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn anchor_shape(line: &[u8]) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            use std::arch::wasm32::*;
            let il_end = digits_end(line, 9);
            if il_end != 0 {
                let ts_end = digits_end(line, il_end + 66);
                if ts_end != 0 && line.len() == ts_end + 256 {
                    let id = il_end + 13;
                    let mut bad = v128_and(v128_xor(ldw!(line, 0), u8x16(123, 34, 105, 110, 100, 101, 120, 34, 58, 0, 0, 0, 0, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0));
                    bad = v128_or(
                        bad,
                        v128_and(v128_xor(ldw!(line, il_end), u8x16(44, 34, 101, 118, 101, 110, 116, 95, 105, 100, 34, 58, 34, 0, 0, 0)), u8x16(255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0)),
                    );
                    bad = v128_or(bad, stops_at!(line, id));
                    bad = v128_or(bad, stops_at!(line, id + 16));
                    bad = v128_or(bad, stops_at!(line, id + 20));
                    bad = v128_or(bad, v128_xor(ldw!(line, id + 36), u8x16(34, 44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34)));
                    bad = v128_or(bad, v128_xor(ldw!(line, id + 37), u8x16(44, 34, 116, 105, 109, 101, 115, 116, 97, 109, 112, 95, 109, 115, 34, 58)));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end), u8x16(44, 34, 101, 118, 101, 110, 116, 95, 104, 97, 115, 104, 95, 104, 101, 120)));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end + 3), u8x16(118, 101, 110, 116, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
                    bad = v128_or(bad, stops_at!(line, ts_end + 19));
                    bad = v128_or(bad, stops_at!(line, ts_end + 35));
                    bad = v128_or(bad, stops_at!(line, ts_end + 51));
                    bad = v128_or(bad, stops_at!(line, ts_end + 67));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end + 83), u8x16(34, 44, 34, 112, 114, 101, 118, 105, 111, 117, 115, 95, 104, 97, 115, 104)));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end + 90), u8x16(105, 111, 117, 115, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
                    bad = v128_or(bad, stops_at!(line, ts_end + 106));
                    bad = v128_or(bad, stops_at!(line, ts_end + 122));
                    bad = v128_or(bad, stops_at!(line, ts_end + 138));
                    bad = v128_or(bad, stops_at!(line, ts_end + 154));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end + 170), u8x16(34, 44, 34, 99, 104, 97, 105, 110, 95, 104, 97, 115, 104, 95, 104, 101)));
                    bad = v128_or(bad, v128_xor(ldw!(line, ts_end + 174), u8x16(104, 97, 105, 110, 95, 104, 97, 115, 104, 95, 104, 101, 120, 34, 58, 34)));
                    bad = v128_or(bad, stops_at!(line, ts_end + 190));
                    bad = v128_or(bad, stops_at!(line, ts_end + 206));
                    bad = v128_or(bad, stops_at!(line, ts_end + 222));
                    bad = v128_or(bad, stops_at!(line, ts_end + 238));
                    let tail2 =
                        u32::from(line[ts_end + 254] ^ b'"') | u32::from(line[ts_end + 255] ^ b'}');
                    if tail2 == 0 && !v128_any_true(bad) {
                        return true;
                    }
                }
            }
        }
        anchor_shape_cold(line)
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn anchor_shape_cold(line: &[u8]) -> bool {
        let mut p = tag(line, 0, b"{\"index\":");
        if p == 0 {
            return false;
        }
        p = digits_end(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b",\"event_id\":\"");
        if p == 0 {
            return false;
        }
        p = qstr::<36>(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b",\"timestamp_ms\":");
        if p == 0 {
            return false;
        }
        p = digits_end(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b",\"event_hash_hex\":\"");
        if p == 0 {
            return false;
        }
        p = qstr::<64>(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b",\"previous_hash_hex\":\"");
        if p == 0 {
            return false;
        }
        p = qstr::<64>(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b",\"chain_hash_hex\":\"");
        if p == 0 {
            return false;
        }
        p = qstr::<64>(line, p);
        if p == 0 {
            return false;
        }
        p = tag(line, p, b"}");
        p != 0 && p == line.len()
    }

    fn parse_anchor(line: &[u8]) -> Option<AnchorFields<'_>> {
        if let Some(anchor) = fast_anchor(line) {
            return Some(anchor);
        }
        serde_json::from_slice::<AnchorFields>(line).ok()
    }

    // fast_anchor fused with the expected-entry compare; None defers to the
    // serde fallback, which stays the acceptance authority.
    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn anchor_diff(
        line: &[u8],
        index: u64,
        id: &str,
        timestamp_ms: u64,
        event_hex: &[u8; 64],
        previous: &[u8; 64],
        chain: &[u8; 64],
    ) -> Option<bool> {
        let mut p = tag(line, 0, b"{\"index\":");
        if p == 0 {
            return None;
        }
        let idx_start = p;
        p = digits_end(line, idx_start);
        if p == 0 {
            return None;
        }
        let a_index = fold_digits(&line[idx_start..p]);
        p = tag(line, p, b",\"event_id\":\"");
        if p == 0 {
            return None;
        }
        let id_start = p;
        p = qstr::<36>(line, p);
        if p == 0 {
            return None;
        }
        let a_id = ascii_str(&line[id_start..p - 1])?;
        p = tag(line, p, b",\"timestamp_ms\":");
        if p == 0 {
            return None;
        }
        let ts_start = p;
        p = digits_end(line, ts_start);
        if p == 0 {
            return None;
        }
        let a_ts = fold_digits(&line[ts_start..p]);
        p = tag(line, p, b",\"event_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let ev_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let a_event = &line[ev_start..p - 1];
        p = tag(line, p, b",\"previous_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let prev_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let a_previous = &line[prev_start..p - 1];
        p = tag(line, p, b",\"chain_hash_hex\":\"");
        if p == 0 {
            return None;
        }
        let chain_start = p;
        p = qstr::<64>(line, p);
        if p == 0 {
            return None;
        }
        let a_chain = &line[chain_start..p - 1];
        p = tag(line, p, b"}");
        if p == 0 || p != line.len() {
            return None;
        }
        Some(
            a_index == index
                && uuid_eq(a_id, id)
                && a_ts == timestamp_ms
                && bytes_eq(a_event, event_hex)
                && bytes_eq(a_previous, previous)
                && bytes_eq(a_chain, chain),
        )
    }

    // One-pass byte comparison of an anchor line against the expected entry
    // in canonical order; byte equality implies the anchor parses to exactly
    // the expected field values, any difference falls back to the slow path.
    // The caller has already pinned the 85-byte chain tail via chain_span.
    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn anchor_line_matches(
        line: &[u8],
        prefix: &[u8],
        id: &[u8],
        ts_digits: &[u8],
        event_hex: &[u8; 64],
        previous: &[u8; 64],
    ) -> bool {
        let pl = prefix.len();
        let il = pl - 22;
        let idl = id.len();
        let tl = ts_digits.len();
        if line.len() != 295 + il + idl + tl {
            return false;
        }
        #[cfg(target_arch = "wasm32")]
        if idl == 36 && (8..=16).contains(&tl) && pl <= 32 {
            use std::arch::wasm32::*;
            let ld = |b: &[u8], o: usize| {
                u64x2(
                    u64::from_le_bytes(b[o..o + 8].try_into().expect("8-byte chunk")),
                    u64::from_le_bytes(b[o + 8..o + 16].try_into().expect("8-byte chunk")),
                )
            };
            let lu =
                |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().expect("8-byte chunk"));
            let mut acc = v128_xor(ld(line, 0), ld(prefix, 0));
            acc = v128_or(acc, v128_xor(ld(line, pl - 16), ld(prefix, pl - 16)));
            acc = v128_or(acc, v128_xor(ld(line, pl), ld(id, 0)));
            acc = v128_or(acc, v128_xor(ld(line, pl + 16), ld(id, 16)));
            acc = v128_or(acc, v128_xor(ld(line, pl + 20), ld(id, 20)));
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, pl + 36),
                    u8x16(b'"', b',', b'"', b't', b'i', b'm', b'e', b's', b't', b'a', b'm', b'p', b'_', b'm', b's', b'"'),
                ),
            );
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, pl + 37),
                    u8x16(b',', b'"', b't', b'i', b'm', b'e', b's', b't', b'a', b'm', b'p', b'_', b'm', b's', b'"', b':'),
                ),
            );
            let p_ts = pl + 53;
            let mut acc64 = lu(line, p_ts) ^ lu(ts_digits, 0);
            acc64 |= lu(line, p_ts + tl - 8) ^ lu(ts_digits, tl - 8);
            let p = p_ts + tl;
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, p),
                    u8x16(b',', b'"', b'e', b'v', b'e', b'n', b't', b'_', b'h', b'a', b's', b'h', b'_', b'h', b'e', b'x'),
                ),
            );
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, p + 3),
                    u8x16(b'v', b'e', b'n', b't', b'_', b'h', b'a', b's', b'h', b'_', b'h', b'e', b'x', b'"', b':', b'"'),
                ),
            );
            let p_ev = p + 19;
            acc = v128_or(acc, v128_xor(ld(line, p_ev), ld(event_hex, 0)));
            acc = v128_or(acc, v128_xor(ld(line, p_ev + 16), ld(event_hex, 16)));
            acc = v128_or(acc, v128_xor(ld(line, p_ev + 32), ld(event_hex, 32)));
            acc = v128_or(acc, v128_xor(ld(line, p_ev + 48), ld(event_hex, 48)));
            let q = p_ev + 64;
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, q),
                    u8x16(b'"', b',', b'"', b'p', b'r', b'e', b'v', b'i', b'o', b'u', b's', b'_', b'h', b'a', b's', b'h'),
                ),
            );
            acc = v128_or(
                acc,
                v128_xor(
                    ld(line, q + 7),
                    u8x16(b'i', b'o', b'u', b's', b'_', b'h', b'a', b's', b'h', b'_', b'h', b'e', b'x', b'"', b':', b'"'),
                ),
            );
            let p_pr = q + 23;
            acc = v128_or(acc, v128_xor(ld(line, p_pr), ld(previous, 0)));
            acc = v128_or(acc, v128_xor(ld(line, p_pr + 16), ld(previous, 16)));
            acc = v128_or(acc, v128_xor(ld(line, p_pr + 32), ld(previous, 32)));
            acc = v128_or(acc, v128_xor(ld(line, p_pr + 48), ld(previous, 48)));
            acc64 |= u64::from(line[p_pr + 64] ^ b'"');
            return !v128_any_true(acc) && acc64 == 0;
        }
        let p_id = pl;
        let p_ts = p_id + idl + 17;
        let p_ev = p_ts + tl + 19;
        let p_pr = p_ev + 87;
        let Some(seg_id) = line.get(p_id..p_id + idl) else {
            return false;
        };
        let lit = |pos: usize| -> &[u8] { &line[pos..] };
        let mut ok = bytes_eq(&line[..pl], prefix);
        ok &= match <&[u8; 36]>::try_from(id) {
            Ok(arr) => match seg_id.try_into() {
                Ok(seg) => eq_n::<36>(seg, arr),
                Err(_) => false,
            },
            Err(_) => bytes_eq(seg_id, id),
        };
        ok &= eq_n::<17>(
            lit(p_id + idl)[..17].try_into().expect("17-byte slice"),
            b"\",\"timestamp_ms\":",
        );
        ok &= bytes_eq(&line[p_ts..p_ts + tl], ts_digits);
        ok &= eq_n::<19>(
            lit(p_ts + tl)[..19].try_into().expect("19-byte slice"),
            b",\"event_hash_hex\":\"",
        );
        ok &= eq_n::<64>(
            line[p_ev..p_ev + 64].try_into().expect("64-byte slice"),
            event_hex,
        );
        ok &= eq_n::<23>(
            lit(p_ev + 64)[..23].try_into().expect("23-byte slice"),
            b"\",\"previous_hash_hex\":\"",
        );
        ok &= eq_n::<64>(
            line[p_pr..p_pr + 64].try_into().expect("64-byte slice"),
            previous,
        );
        ok &= line[p_pr + 64] == b'"';
        ok
    }

    #[cfg_attr(target_arch = "wasm32", target_feature(enable = "simd128"))]
    fn chain_span(line: &[u8]) -> Option<&[u8; 64]> {
        let n = line.len();
        if n < 85 {
            return None;
        }
        let t: &[u8; 85] = line[n - 85..].try_into().expect("85-byte tail");
        const TAG: &[u8; 19] = b",\"chain_hash_hex\":\"";
        let acc = (u64::from_le_bytes(t[0..8].try_into().expect("8-byte chunk"))
            ^ u64::from_le_bytes(TAG[0..8].try_into().expect("8-byte chunk")))
            | (u64::from_le_bytes(t[8..16].try_into().expect("8-byte chunk"))
                ^ u64::from_le_bytes(TAG[8..16].try_into().expect("8-byte chunk")))
            | u64::from(
                u32::from_le_bytes(t[15..19].try_into().expect("4-byte chunk"))
                    ^ u32::from_le_bytes(TAG[15..19].try_into().expect("4-byte chunk")),
            )
            | u64::from(t[83] ^ b'"')
            | u64::from(t[84] ^ b'}');
        if acc == 0 {
            t[19..83].try_into().ok()
        } else {
            None
        }
    }

    pub fn verify_chain(events_jsonl: &[u8], anchors_jsonl: &[u8]) -> ChainReport {
        let event_lines = split_lines(events_jsonl);
        let anchor_lines = split_lines(anchors_jsonl);

        let mut anchor_failures = Vec::new();
        let mut failures = Vec::new();

        let event_hexes = digest_all(&event_lines);
        let tail = &TAIL_WK;
        let mut previous = zero_hash();
        #[cfg(target_arch = "wasm32")]
        let zero_prev = zero_hash();
        #[cfg(target_arch = "wasm32")]
        let mut spec_on = true;
        #[cfg(target_arch = "wasm32")]
        let mut spec_batch = false;
        #[cfg(target_arch = "wasm32")]
        let mut spec_miss = 0u32;
        #[cfg(target_arch = "wasm32")]
        let mut spec_span: [Option<&[u8; 64]>; 5] = [None; 5];
        #[cfg(target_arch = "wasm32")]
        let mut spec_buf = [[0u8; 64]; 4];
        #[cfg(target_arch = "wasm32")]
        let mut seq_wk = [[0u8; 256]; 4];
        #[cfg(target_arch = "wasm32")]
        let mut pre_hit: Option<bool> = None;
        const IDX_LIT: &[u8; 13] = b",\"event_id\":\"";
        let mut pre = [0u8; 48];
        pre[..9].copy_from_slice(b"{\"index\":");
        pre[9] = b'0';
        pre[10..23].copy_from_slice(IDX_LIT);
        let mut il = 1usize;
        for (index, line) in event_lines.iter().enumerate() {
            if index > 0 {
                let mut p = 8 + il;
                loop {
                    if pre[p] < b'9' {
                        pre[p] += 1;
                        break;
                    }
                    pre[p] = b'0';
                    if p == 9 {
                        il += 1;
                        pre[9] = b'1';
                        let mut z = 10;
                        while z < 9 + il {
                            pre[z] = b'0';
                            z += 1;
                        }
                        pre[9 + il..22 + il].copy_from_slice(IDX_LIT);
                        break;
                    }
                    p -= 1;
                }
            }
            let event_hex = &event_hexes[index];
            #[cfg(target_arch = "wasm32")]
            let chain_store: [u8; 64];
            #[cfg(target_arch = "wasm32")]
            let chain: &[u8; 64] = {
                if index & 3 == 0 {
                    spec_batch = spec_on;
                    if spec_on {
                        spec_span[0] = if index == 0 {
                            Some(&zero_prev)
                        } else {
                            spec_span[4]
                        };
                        let mut m = 1;
                        while m < 5 {
                            spec_span[m] =
                                anchor_lines.get(index + m - 1).and_then(|a| chain_span(a));
                            m += 1;
                        }
                        let cand: &[Option<&[u8; 64]>; 4] =
                            spec_span[..4].try_into().expect("4-lane window");
                        link_quad(cand, &event_hexes[index..], tail, &mut spec_buf);
                    } else {
                        spec_span = [None; 5];
                        mid_wk_quad(&event_hexes[index..], &mut seq_wk);
                    }
                }
                let cached = pre_hit.take();
                match spec_span[index & 3] {
                    Some(c) if cached.unwrap_or_else(|| eq64(c, &previous)) => {
                        spec_miss = 0;
                        &spec_buf[index & 3]
                    }
                    _ => {
                        spec_miss += 1;
                        if spec_miss >= 8 {
                            spec_on = false;
                        }
                        chain_store = if spec_batch {
                            link_hex(&previous, event_hex, tail)
                        } else {
                            link_hex_mid(&previous, &seq_wk[index & 3], event_hex[63], tail)
                        };
                        &chain_store
                    }
                }
            };
            #[cfg(not(target_arch = "wasm32"))]
            let chain_store = link_hex(&previous, event_hex, tail);
            #[cfg(not(target_arch = "wasm32"))]
            let chain = &chain_store;
            let mut slow: Option<Option<(Cow<'_, str>, u64)>> = None;
            match fast_event(line) {
                packed if packed != 0 => match anchor_lines.get(index) {
                    Some(aline) => {
                        #[cfg(target_arch = "wasm32")]
                        let tail_ok = {
                            let cs = if spec_batch {
                                spec_span[(index & 3) + 1]
                            } else {
                                chain_span(aline)
                            };
                            let ok = matches!(cs, Some(c) if eq64(c, chain));
                            pre_hit = Some(ok);
                            ok
                        };
                        #[cfg(not(target_arch = "wasm32"))]
                        let tail_ok = matches!(chain_span(aline), Some(c) if eq64(c, chain));
                        if tail_ok {
                            let (id, ts) = event_spans(line, packed);
                            if !anchor_line_matches(
                                aline,
                                &pre[..22 + il],
                                id,
                                ts,
                                event_hex,
                                &previous,
                            ) {
                                slow = Some(match ascii_str(id) {
                                    Some(id) => Some((Cow::Borrowed(id), fold_digits(ts))),
                                    None => parse_event(line).ok(),
                                });
                            }
                        } else if anchor_shape(aline) {
                            failures.push(Failure::EntryMismatch {
                                index: index as u64,
                            });
                        } else {
                            let (id, ts) = event_spans(line, packed);
                            slow = Some(match ascii_str(id) {
                                Some(id) => Some((Cow::Borrowed(id), fold_digits(ts))),
                                None => parse_event(line).ok(),
                            });
                        }
                    }
                    None => failures.push(Failure::EntryMissing {
                        index: index as u64,
                    }),
                },
                _ => match serde_json::from_slice::<EventFields>(line) {
                    Ok(event) => slow = Some(Some((event.id, event.timestamp_ms))),
                    Err(_) => slow = Some(None),
                },
            }
            match slow {
                None => {}
                Some(Some((id, timestamp_ms))) => match anchor_lines.get(index) {
                    Some(aline) => match anchor_diff(
                        aline,
                        index as u64,
                        &id,
                        timestamp_ms,
                        event_hex,
                        &previous,
                        chain,
                    )
                    .or_else(|| {
                        serde_json::from_slice::<AnchorFields>(aline).ok().map(|a| {
                            a.index == index as u64
                                && uuid_eq(&a.event_id, &id)
                                && a.timestamp_ms == timestamp_ms
                                && bytes_eq(a.event_hash_hex.as_bytes(), event_hex)
                                && bytes_eq(a.previous_hash_hex.as_bytes(), &previous)
                                && bytes_eq(a.chain_hash_hex.as_bytes(), chain)
                        })
                    }) {
                        Some(true) => {}
                        Some(false) => failures.push(Failure::EntryMismatch {
                            index: index as u64,
                        }),
                        None => {
                            anchor_failures.push(Failure::AnchorParseError {
                                index: index as u64,
                            });
                            failures.push(Failure::EntryMismatch {
                                index: index as u64,
                            });
                        }
                    },
                    None => failures.push(Failure::EntryMissing {
                        index: index as u64,
                    }),
                },
                Some(None) => {
                    failures.push(Failure::ParseError {
                        index: index as u64,
                    });
                    match anchor_lines.get(index) {
                        Some(aline) => match parse_anchor(aline) {
                            Some(actual)
                                if actual.index == index as u64
                                    && bytes_eq(actual.event_hash_hex.as_bytes(), event_hex)
                                    && bytes_eq(actual.previous_hash_hex.as_bytes(), &previous)
                                    && bytes_eq(actual.chain_hash_hex.as_bytes(), chain) => {}
                            Some(_) => failures.push(Failure::EntryMismatch {
                                index: index as u64,
                            }),
                            None => {
                                anchor_failures.push(Failure::AnchorParseError {
                                    index: index as u64,
                                });
                                failures.push(Failure::EntryMismatch {
                                    index: index as u64,
                                });
                            }
                        },
                        None => failures.push(Failure::EntryMissing {
                            index: index as u64,
                        }),
                    }
                }
            }
            previous = *chain;
        }

        for (index, aline) in anchor_lines.iter().enumerate().skip(event_lines.len()) {
            if parse_anchor(aline).is_none() {
                anchor_failures.push(Failure::AnchorParseError {
                    index: index as u64,
                });
            }
        }

        let mut all = anchor_failures;
        if anchor_lines.len() != event_lines.len() {
            all.push(Failure::LengthMismatch {
                events: event_lines.len() as u64,
                anchors: anchor_lines.len() as u64,
            });
        }
        all.extend(failures);
        if anchor_lines.len() > event_lines.len() {
            all.push(Failure::DanglingAnchors {
                count: (anchor_lines.len() - event_lines.len()) as u64,
            });
        }

        ChainReport {
            events: event_lines.len() as u64,
            anchors: anchor_lines.len() as u64,
            valid: all.is_empty(),
            root_hash_hex: hex_string(&previous),
            failures: all,
        }
    }

    pub fn fold_chain(lines: &[&[u8]]) -> Vec<ChainEntry> {
        let tail = &TAIL_WK;
        let event_hexes = digest_all(lines);
        let mut previous = zero_hash();
        let mut entries = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let event_hex = event_hexes[index];
            let chain = link_hex(&previous, &event_hex, tail);
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
