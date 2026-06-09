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
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Eight lowercase hex chars from one state word via SWAR: spread the
    /// nibbles into one byte each (already in output order, so no byte swap),
    /// then map nibble -> char branchlessly. The `+6` carry flags nibbles
    /// above 9; per-byte sums stay below 0x100 and the 0x27 multiply cannot
    /// carry across bytes because each flag byte is 0 or 1.
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(always)]
    fn hex8(word: u32) -> u64 {
        const LO: u64 = 0x0101010101010101;
        let x = u64::from(word);
        let v = ((x & 0xFFFF_0000) >> 16) | ((x & 0xFFFF) << 32);
        let v = ((v >> 8) & 0x0000_00FF_0000_00FF) | ((v & 0x0000_00FF_0000_00FF) << 16);
        let v = ((v >> 4) & 0x000F_000F_000F_000F) | ((v & 0x000F_000F_000F_000F) << 8);
        let gap = (v.wrapping_add(LO * 0x06) & (LO * 0x10)) >> 4;
        v.wrapping_add(LO * 0x30)
            .wrapping_add(gap.wrapping_mul(0x27))
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

    /// sha256 of the 129-byte `previous \n event` hex composition. The middle
    /// block's schedule is built directly from `event` words shifted through
    /// a one-byte carry (no 64-byte staging buffer), and the final padding
    /// block is constant except for the carried last hex char, so its
    /// schedule folds at compile time (length 129 * 8 = 1032 bits).
    #[cfg(not(target_arch = "wasm32"))]
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

    /// simd128 lane of the kernel, wasm-only. wasmtime's fuel meter charges
    /// one unit per executed instruction regardless of width, so a v128 op
    /// that does four words of work costs the same as one scalar op. The
    /// build does not enable simd128 globally; `#[target_feature]` functions
    /// are safe to call on wasm because feature support is validated at
    /// module load, and the pinned wasmtime enables SIMD by default. Native
    /// builds (unit tests, differential suite) keep the scalar path.
    #[cfg(target_arch = "wasm32")]
    mod simd {
        use super::H0;
        use std::arch::wasm32::*;

        /// Same round as the scalar `round!`, but `k + w` arrives pre-added
        /// as a lane extracted from the vectorized schedule.
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

        /// Eight rounds: the working-variable rotation has period eight, so
        /// after one expansion the roles line back up with `a..h`. Re-seeding
        /// the Maj cache (`b ^ c`) per oct costs one xor per eight rounds and
        /// keeps the SSA chain local to the macro.
        macro_rules! oct {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $wl:ident, $wh:ident) => {
                let x0 = $b ^ $c;
                roundv!(
                    $a,
                    $b,
                    $c,
                    $d,
                    $e,
                    $f,
                    $g,
                    $h,
                    u32x4_extract_lane::<0>($wl),
                    x0,
                    x1
                );
                roundv!(
                    $h,
                    $a,
                    $b,
                    $c,
                    $d,
                    $e,
                    $f,
                    $g,
                    u32x4_extract_lane::<1>($wl),
                    x1,
                    x2
                );
                roundv!(
                    $g,
                    $h,
                    $a,
                    $b,
                    $c,
                    $d,
                    $e,
                    $f,
                    u32x4_extract_lane::<2>($wl),
                    x2,
                    x3
                );
                roundv!(
                    $f,
                    $g,
                    $h,
                    $a,
                    $b,
                    $c,
                    $d,
                    $e,
                    u32x4_extract_lane::<3>($wl),
                    x3,
                    x4
                );
                roundv!(
                    $e,
                    $f,
                    $g,
                    $h,
                    $a,
                    $b,
                    $c,
                    $d,
                    u32x4_extract_lane::<0>($wh),
                    x4,
                    x5
                );
                roundv!(
                    $d,
                    $e,
                    $f,
                    $g,
                    $h,
                    $a,
                    $b,
                    $c,
                    u32x4_extract_lane::<1>($wh),
                    x5,
                    x6
                );
                roundv!(
                    $c,
                    $d,
                    $e,
                    $f,
                    $g,
                    $h,
                    $a,
                    $b,
                    u32x4_extract_lane::<2>($wh),
                    x6,
                    x7
                );
                roundv!(
                    $b,
                    $c,
                    $d,
                    $e,
                    $f,
                    $g,
                    $h,
                    $a,
                    u32x4_extract_lane::<3>($wh),
                    x7,
                    x8
                );
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

        /// Next four schedule words from the previous sixteen. The σ1 feed
        /// has an in-batch dependency (w16/w17 feed w18/w19), handled in two
        /// half-steps; σ1 of a zero lane is zero, so padding with zeros keeps
        /// the untouched lanes exact.
        #[inline]
        #[target_feature(enable = "simd128")]
        fn sched4(x0: v128, x1: v128, x2: v128, x3: v128) -> v128 {
            let zero = u32x4(0, 0, 0, 0);
            let w1 =
                i8x16_shuffle::<4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19>(x0, x1);
            let w9 =
                i8x16_shuffle::<4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19>(x2, x3);
            let t = u32x4_add(u32x4_add(x0, small_sigma0(w1)), w9);
            let w14 = i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23>(
                x3, zero,
            );
            let t = u32x4_add(t, small_sigma1(w14));
            let w16 =
                i8x16_shuffle::<16, 17, 18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5, 6, 7>(t, zero);
            u32x4_add(t, small_sigma1(w16))
        }

        /// One compression with the message schedule and the `+K` folds done
        /// four lanes at a time; rounds stay scalar (each round depends on
        /// the previous) and read their `k + w` term by lane extraction.
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

        /// 16 message bytes as four big-endian schedule words: one fused
        /// 16-byte load plus a single byte-reversing shuffle.
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

        /// Lowercase hex via nibble table lookup: byte-reverse each word to
        /// big-endian, split nibbles, interleave high/low, then one swizzle
        /// per 16 output chars maps nibble -> ascii.
        #[inline]
        #[target_feature(enable = "simd128")]
        fn hex_half(out: &mut [u8], s0: u32, s1: u32, s2: u32, s3: u32) {
            let table = u8x16(
                b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'a', b'b', b'c', b'd',
                b'e', b'f',
            );
            let v = u32x4(s0, s1, s2, s3);
            let v = i8x16_shuffle::<3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12>(v, v);
            let hi = u8x16_shr(v, 4);
            let lo = v128_and(v, u8x16_splat(0x0f));
            let n0 =
                i8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(hi, lo);
            let n1 = i8x16_shuffle::<8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31>(
                hi, lo,
            );
            let c0 = i8x16_swizzle(table, n0);
            let c1 = i8x16_swizzle(table, n1);
            out[0..8].copy_from_slice(&u64x2_extract_lane::<0>(c0).to_le_bytes());
            out[8..16].copy_from_slice(&u64x2_extract_lane::<1>(c0).to_le_bytes());
            out[16..24].copy_from_slice(&u64x2_extract_lane::<0>(c1).to_le_bytes());
            out[24..32].copy_from_slice(&u64x2_extract_lane::<1>(c1).to_le_bytes());
        }

        #[inline]
        #[target_feature(enable = "simd128")]
        fn hex_state(state: &[u32; 8]) -> [u8; 64] {
            let mut out = [0u8; 64];
            hex_half(&mut out[0..32], state[0], state[1], state[2], state[3]);
            hex_half(&mut out[32..64], state[4], state[5], state[6], state[7]);
            out
        }

        /// Vector rotate-right; wasm SIMD has no rotate, so shift/shift/or.
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

        /// Four-message round: lane L of every vector is message L's state,
        /// so one instruction stream advances four independent digests. Same
        /// Ch/Maj forms as the scalar `round!`.
        macro_rules! roundm {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $k:literal, $w:ident, $xo:ident, $xn:ident) => {
                let t1 = u32x4_add(
                    u32x4_add(
                        u32x4_add(
                            u32x4_add($h, big_sigma1($e)),
                            v128_xor(v128_and(v128_xor($f, $g), $e), $g),
                        ),
                        u32x4($k, $k, $k, $k),
                    ),
                    $w,
                );
                let $xn = v128_xor($a, $b);
                let t2 = u32x4_add(big_sigma0($a), v128_xor($b, v128_and($xn, $xo)));
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

        /// One compression over four messages in transposed lanes.
        #[inline]
        #[target_feature(enable = "simd128")]
        fn compressm(state: &mut [v128; 8], w: [v128; 16]) {
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
            let x0 = v128_xor(b, c);
            roundm!(a, b, c, d, e, f, g, h, 0x428a2f98u32, w0, x0, x1);
            roundm!(h, a, b, c, d, e, f, g, 0x71374491u32, w1, x1, x2);
            roundm!(g, h, a, b, c, d, e, f, 0xb5c0fbcfu32, w2, x2, x3);
            roundm!(f, g, h, a, b, c, d, e, 0xe9b5dba5u32, w3, x3, x4);
            roundm!(e, f, g, h, a, b, c, d, 0x3956c25bu32, w4, x4, x5);
            roundm!(d, e, f, g, h, a, b, c, 0x59f111f1u32, w5, x5, x6);
            roundm!(c, d, e, f, g, h, a, b, 0x923f82a4u32, w6, x6, x7);
            roundm!(b, c, d, e, f, g, h, a, 0xab1c5ed5u32, w7, x7, x8);
            roundm!(a, b, c, d, e, f, g, h, 0xd807aa98u32, w8, x8, x9);
            roundm!(h, a, b, c, d, e, f, g, 0x12835b01u32, w9, x9, x10);
            roundm!(g, h, a, b, c, d, e, f, 0x243185beu32, w10, x10, x11);
            roundm!(f, g, h, a, b, c, d, e, 0x550c7dc3u32, w11, x11, x12);
            roundm!(e, f, g, h, a, b, c, d, 0x72be5d74u32, w12, x12, x13);
            roundm!(d, e, f, g, h, a, b, c, 0x80deb1feu32, w13, x13, x14);
            roundm!(c, d, e, f, g, h, a, b, 0x9bdc06a7u32, w14, x14, x15);
            roundm!(b, c, d, e, f, g, h, a, 0xc19bf174u32, w15, x15, x16);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0xe49b69c1u32, w0, x16, x17);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0xefbe4786u32, w1, x17, x18);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x0fc19dc6u32, w2, x18, x19);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x240ca1ccu32, w3, x19, x20);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x2de92c6fu32, w4, x20, x21);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x4a7484aau32, w5, x21, x22);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x5cb0a9dcu32, w6, x22, x23);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x76f988dau32, w7, x23, x24);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0x983e5152u32, w8, x24, x25);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0xa831c66du32, w9, x25, x26);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0xb00327c8u32, w10, x26, x27);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0xbf597fc7u32, w11, x27, x28);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0xc6e00bf3u32, w12, x28, x29);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xd5a79147u32, w13, x29, x30);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0x06ca6351u32, w14, x30, x31);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0x14292967u32, w15, x31, x32);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0x27b70a85u32, w0, x32, x33);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0x2e1b2138u32, w1, x33, x34);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x4d2c6dfcu32, w2, x34, x35);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x53380d13u32, w3, x35, x36);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x650a7354u32, w4, x36, x37);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x766a0abbu32, w5, x37, x38);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x81c2c92eu32, w6, x38, x39);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x92722c85u32, w7, x39, x40);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0xa2bfe8a1u32, w8, x40, x41);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0xa81a664bu32, w9, x41, x42);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0xc24b8b70u32, w10, x42, x43);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0xc76c51a3u32, w11, x43, x44);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0xd192e819u32, w12, x44, x45);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xd6990624u32, w13, x45, x46);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0xf40e3585u32, w14, x46, x47);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0x106aa070u32, w15, x47, x48);
            schedm!(w0, w1, w9, w14);
            roundm!(a, b, c, d, e, f, g, h, 0x19a4c116u32, w0, x48, x49);
            schedm!(w1, w2, w10, w15);
            roundm!(h, a, b, c, d, e, f, g, 0x1e376c08u32, w1, x49, x50);
            schedm!(w2, w3, w11, w0);
            roundm!(g, h, a, b, c, d, e, f, 0x2748774cu32, w2, x50, x51);
            schedm!(w3, w4, w12, w1);
            roundm!(f, g, h, a, b, c, d, e, 0x34b0bcb5u32, w3, x51, x52);
            schedm!(w4, w5, w13, w2);
            roundm!(e, f, g, h, a, b, c, d, 0x391c0cb3u32, w4, x52, x53);
            schedm!(w5, w6, w14, w3);
            roundm!(d, e, f, g, h, a, b, c, 0x4ed8aa4au32, w5, x53, x54);
            schedm!(w6, w7, w15, w4);
            roundm!(c, d, e, f, g, h, a, b, 0x5b9cca4fu32, w6, x54, x55);
            schedm!(w7, w8, w0, w5);
            roundm!(b, c, d, e, f, g, h, a, 0x682e6ff3u32, w7, x55, x56);
            schedm!(w8, w9, w1, w6);
            roundm!(a, b, c, d, e, f, g, h, 0x748f82eeu32, w8, x56, x57);
            schedm!(w9, w10, w2, w7);
            roundm!(h, a, b, c, d, e, f, g, 0x78a5636fu32, w9, x57, x58);
            schedm!(w10, w11, w3, w8);
            roundm!(g, h, a, b, c, d, e, f, 0x84c87814u32, w10, x58, x59);
            schedm!(w11, w12, w4, w9);
            roundm!(f, g, h, a, b, c, d, e, 0x8cc70208u32, w11, x59, x60);
            schedm!(w12, w13, w5, w10);
            roundm!(e, f, g, h, a, b, c, d, 0x90befffau32, w12, x60, x61);
            schedm!(w13, w14, w6, w11);
            roundm!(d, e, f, g, h, a, b, c, 0xa4506cebu32, w13, x61, x62);
            schedm!(w14, w15, w7, w12);
            roundm!(c, d, e, f, g, h, a, b, 0xbef9a3f7u32, w14, x62, x63);
            schedm!(w15, w0, w8, w13);
            roundm!(b, c, d, e, f, g, h, a, 0xc67178f2u32, w15, x63, _x64);
            state[0] = u32x4_add(state[0], a);
            state[1] = u32x4_add(state[1], b);
            state[2] = u32x4_add(state[2], c);
            state[3] = u32x4_add(state[3], d);
            state[4] = u32x4_add(state[4], e);
            state[5] = u32x4_add(state[5], f);
            state[6] = u32x4_add(state[6], g);
            state[7] = u32x4_add(state[7], h);
        }

        /// 16 bytes from each of four lanes, transposed into four schedule
        /// vectors. The 4x4 word transpose is the usual unpack32/unpack64
        /// ladder, with the big-endian byte swap folded into the first-level
        /// shuffles for free.
        #[inline]
        #[target_feature(enable = "simd128")]
        fn quad<const O: usize>(
            b0: &[u8; 64],
            b1: &[u8; 64],
            b2: &[u8; 64],
            b3: &[u8; 64],
        ) -> [v128; 4] {
            let r0 = load_raw::<O>(b0);
            let r1 = load_raw::<O>(b1);
            let r2 = load_raw::<O>(b2);
            let r3 = load_raw::<O>(b3);
            let p0 =
                i8x16_shuffle::<3, 2, 1, 0, 19, 18, 17, 16, 7, 6, 5, 4, 23, 22, 21, 20>(r0, r1);
            let p1 = i8x16_shuffle::<11, 10, 9, 8, 27, 26, 25, 24, 15, 14, 13, 12, 31, 30, 29, 28>(
                r0, r1,
            );
            let p2 =
                i8x16_shuffle::<3, 2, 1, 0, 19, 18, 17, 16, 7, 6, 5, 4, 23, 22, 21, 20>(r2, r3);
            let p3 = i8x16_shuffle::<11, 10, 9, 8, 27, 26, 25, 24, 15, 14, 13, 12, 31, 30, 29, 28>(
                r2, r3,
            );
            [
                i8x16_shuffle::<0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23>(p0, p2),
                i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 26, 27, 28, 29, 30, 31>(
                    p0, p2,
                ),
                i8x16_shuffle::<0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23>(p1, p3),
                i8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 26, 27, 28, 29, 30, 31>(
                    p1, p3,
                ),
            ]
        }

        /// Block `k` of a padded message: full blocks come from the line,
        /// the one or two final blocks from the prebuilt 128-byte tail.
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

        /// Four equal-block-count messages digested in lockstep.
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
            let mut state = [
                u32x4(H0[0], H0[0], H0[0], H0[0]),
                u32x4(H0[1], H0[1], H0[1], H0[1]),
                u32x4(H0[2], H0[2], H0[2], H0[2]),
                u32x4(H0[3], H0[3], H0[3], H0[3]),
                u32x4(H0[4], H0[4], H0[4], H0[4]),
                u32x4(H0[5], H0[5], H0[5], H0[5]),
                u32x4(H0[6], H0[6], H0[6], H0[6]),
                u32x4(H0[7], H0[7], H0[7], H0[7]),
            ];
            for k in 0..blocks {
                let b0 = block_at(lines[0], &tails[0], full[0], k);
                let b1 = block_at(lines[1], &tails[1], full[1], k);
                let b2 = block_at(lines[2], &tails[2], full[2], k);
                let b3 = block_at(lines[3], &tails[3], full[3], k);
                let q0 = quad::<0>(b0, b1, b2, b3);
                let q1 = quad::<16>(b0, b1, b2, b3);
                let q2 = quad::<32>(b0, b1, b2, b3);
                let q3 = quad::<48>(b0, b1, b2, b3);
                compressm(
                    &mut state,
                    [
                        q0[0], q0[1], q0[2], q0[3], q1[0], q1[1], q1[2], q1[3], q2[0], q2[1],
                        q2[2], q2[3], q3[0], q3[1], q3[2], q3[3],
                    ],
                );
            }
            let mut out = [[0u8; 64]; 4];
            macro_rules! lane_hex {
                ($idx:literal) => {{
                    let o = &mut out[$idx];
                    hex_half(
                        &mut o[0..32],
                        u32x4_extract_lane::<$idx>(state[0]),
                        u32x4_extract_lane::<$idx>(state[1]),
                        u32x4_extract_lane::<$idx>(state[2]),
                        u32x4_extract_lane::<$idx>(state[3]),
                    );
                    hex_half(
                        &mut o[32..64],
                        u32x4_extract_lane::<$idx>(state[4]),
                        u32x4_extract_lane::<$idx>(state[5]),
                        u32x4_extract_lane::<$idx>(state[6]),
                        u32x4_extract_lane::<$idx>(state[7]),
                    );
                }};
            }
            lane_hex!(0);
            lane_hex!(1);
            lane_hex!(2);
            lane_hex!(3);
            out
        }

        /// Digest every line, four at a time. Messages only share a round
        /// instruction stream when their padded block counts match, so lines
        /// are bucketed by block count; leftovers take the single-lane path.
        #[target_feature(enable = "simd128")]
        pub(super) fn digest_all(lines: &[&[u8]]) -> Vec<[u8; 64]> {
            let mut out = vec![[0u8; 64]; lines.len()];
            let mut buckets: Vec<Vec<u32>> = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                let blocks = (line.len() + 72) >> 6;
                if buckets.len() <= blocks {
                    buckets.resize_with(blocks + 1, Vec::new);
                }
                buckets[blocks].push(i as u32);
            }
            for (blocks, bucket) in buckets.iter().enumerate() {
                let mut quads = bucket.chunks_exact(4);
                for q in &mut quads {
                    let hexes = digest4(
                        [
                            lines[q[0] as usize],
                            lines[q[1] as usize],
                            lines[q[2] as usize],
                            lines[q[3] as usize],
                        ],
                        blocks,
                    );
                    for (j, hex) in hexes.iter().enumerate() {
                        out[q[j] as usize] = *hex;
                    }
                }
                for &i in quads.remainder() {
                    out[i as usize] = digest_hex(lines[i as usize]);
                }
            }
            out
        }

        /// sha256(line) as lowercase hex; same block walk as the scalar
        /// `digest_state`, compression vectorized.
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

        /// sha256 of the 129-byte `previous \n event` hex composition. The
        /// middle block is `\n` plus the first 63 event bytes: each schedule
        /// word comes from one shuffle that fuses the one-byte shift and the
        /// big-endian swap. The final block carries only the last event byte
        /// plus padding (129 * 8 = 1032 bits).
        #[target_feature(enable = "simd128")]
        pub(super) fn link_hex(previous: &[u8; 64], event: &[u8; 64]) -> [u8; 64] {
            let mut state = H0;
            compress_block(&mut state, previous);
            let v0 = load_raw::<0>(event);
            let v1 = load_raw::<16>(event);
            let v2 = load_raw::<32>(event);
            let v3 = load_raw::<48>(event);
            let nl = u8x16_splat(b'\n');
            let s0 = i8x16_shuffle::<2, 1, 0, 16, 6, 5, 4, 3, 10, 9, 8, 7, 14, 13, 12, 11>(v0, nl);
            let s1 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(
                v0, v1,
            );
            let s2 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(
                v1, v2,
            );
            let s3 = i8x16_shuffle::<18, 17, 16, 15, 22, 21, 20, 19, 26, 25, 24, 23, 30, 29, 28, 27>(
                v2, v3,
            );
            compress_v(&mut state, s0, s1, s2, s3);
            let zero = u32x4(0, 0, 0, 0);
            let w0 = (u32::from(event[63]) << 24) | 0x0080_0000;
            compress_v(
                &mut state,
                u32x4(w0, 0, 0, 0),
                zero,
                zero,
                u32x4(0, 0, 0, 1032),
            );
            hex_state(&state)
        }
    }

    #[cfg(target_arch = "wasm32")]
    use simd::{digest_all, digest_hex, link_hex};

    #[cfg(not(target_arch = "wasm32"))]
    fn digest_all(lines: &[&[u8]]) -> Vec<[u8; 64]> {
        lines.iter().map(|line| digest_hex(line)).collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn digest_hex(bytes: &[u8]) -> [u8; 64] {
        hex_state(&digest_state(bytes))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn link_hex(previous: &[u8; 64], event: &[u8; 64]) -> [u8; 64] {
        chain_hex(previous, event)
    }

    fn hex_string(hex: &[u8; 64]) -> String {
        String::from_utf8(hex.to_vec()).expect("hex output is ascii")
    }

    fn zero_hash() -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(ZERO_CHAIN_HASH.as_bytes());
        out
    }

    /// 16-byte vector scan: one compare against `\n` and a lane bitmask
    /// replace the SWAR detector at a quarter of the per-byte fuel.
    #[cfg(target_arch = "wasm32")]
    #[target_feature(enable = "simd128")]
    fn find_newline(bytes: &[u8], mut i: usize) -> usize {
        use std::arch::wasm32::*;
        let n = bytes.len();
        while i + 16 <= n {
            let v = u64x2(
                u64::from_le_bytes(bytes[i..i + 8].try_into().expect("8-byte chunk")),
                u64::from_le_bytes(bytes[i + 8..i + 16].try_into().expect("8-byte chunk")),
            );
            let mask = i8x16_bitmask(u8x16_eq(v, u8x16_splat(b'\n')));
            if mask != 0 {
                return i + mask.trailing_zeros() as usize;
            }
            i += 16;
        }
        while i < n {
            if bytes[i] == b'\n' {
                return i;
            }
            i += 1;
        }
        n
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
        /// Leaves pos past the closing quote. Stop lanes are quote,
        /// backslash, control (signed < 0x20 also catches >= 0x80), or DEL;
        /// the first flagged lane is re-checked exactly by the scalar arm.
        #[cfg(target_arch = "wasm32")]
        #[target_feature(enable = "simd128")]
        fn string_body(&mut self) -> Option<&'a [u8]> {
            use std::arch::wasm32::*;
            let start = self.pos;
            let buf = self.buf;
            let mut i = self.pos;
            while i + 16 <= buf.len() {
                let v = u64x2(
                    u64::from_le_bytes(buf[i..i + 8].try_into().expect("8-byte chunk")),
                    u64::from_le_bytes(buf[i + 8..i + 16].try_into().expect("8-byte chunk")),
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
                        let body = &buf[start..i];
                        self.pos = i + 1;
                        return Some(body);
                    }
                    self.pos = i;
                    return None;
                }
                i += 16;
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

        /// SWAR form of the same detector. It drops the usual `& !x`
        /// zero-test masking: every spurious flag it can raise either sits on
        /// a byte with the high bit set (a true hit via the `| x` term) or
        /// lands strictly above a true hit through borrow/carry propagation,
        /// so the lowest flagged byte is always a genuine stop byte and is
        /// re-checked exactly by the scalar arm.
        #[cfg(not(target_arch = "wasm32"))]
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
                let hit = (quote | slash | x.wrapping_sub(LO * 0x20) | x.wrapping_add(LO) | x) & HI;
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

        let event_hexes = digest_all(&event_lines);
        let mut previous = zero_hash();
        for (index, line) in event_lines.iter().enumerate() {
            let event_hex = event_hexes[index];
            let chain = link_hex(&previous, &event_hex);
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
            let event_hex = digest_hex(line);
            let chain = link_hex(&previous, &event_hex);
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
