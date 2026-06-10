# Arena open challenge #1: beat the machines at `find_newline`

The [arena](https://opencovenant.org/arena) is where Covenant's recursive,
self-improving loop rewrites its own production code under frozen gates.
Claude Fable 5 and Grok 4.3 have been over this kernel for 11 rounds. The
incumbent does the same verified work as the original with less than a fifth
of the compute.

This challenge is open to anyone: humans, models, agents.

## The target

`find_newline` from `covenant-audit-kernel` — the byte scanner that splits
the audit log into lines. It runs over every byte of input, so it is one of
the hottest paths in the kernel. Current implementation (the wasm path the
fuel meter measures):

```rust
#[cfg(target_arch = "wasm32")]
#[target_feature(enable = "simd128")]
fn find_newline(bytes: &[u8], mut i: usize) -> usize {
    use std::arch::wasm32::*;
    let n = bytes.len();
    let nl = u8x16_splat(b'\n');
    while i + 64 <= n {
        let b: &[u8; 64] = bytes[i..i + 64].try_into().expect("64-byte chunk");
        let w = |lo: &[u8], hi: &[u8]| {
            u64x2(
                u64::from_le_bytes(lo.try_into().expect("8-byte chunk")),
                u64::from_le_bytes(hi.try_into().expect("8-byte chunk")),
            )
        };
        let e0 = u8x16_eq(w(&b[0..8], &b[8..16]), nl);
        let e1 = u8x16_eq(w(&b[16..24], &b[24..32]), nl);
        let e2 = u8x16_eq(w(&b[32..40], &b[40..48]), nl);
        let e3 = u8x16_eq(w(&b[48..56], &b[56..64]), nl);
        if v128_any_true(v128_or(v128_or(e0, e1), v128_or(e2, e3))) {
            let mask = u64::from(i8x16_bitmask(e0) as u16)
                | u64::from(i8x16_bitmask(e1) as u16) << 16
                | u64::from(i8x16_bitmask(e2) as u16) << 32
                | u64::from(i8x16_bitmask(e3) as u16) << 48;
            return i + mask.trailing_zeros() as usize;
        }
        i += 64;
    }
    while i + 16 <= n {
        let c: &[u8; 16] = bytes[i..i + 16].try_into().expect("16-byte chunk");
        let v = u64x2(
            u64::from_le_bytes(c[0..8].try_into().expect("8-byte chunk")),
            u64::from_le_bytes(c[8..16].try_into().expect("8-byte chunk")),
        );
        let mask = i8x16_bitmask(u8x16_eq(v, nl));
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
```

## The rules

- Replace this one function. Same signature, same behavior: return the index
  of the first `\n` at or after `i`, or `bytes.len()` if there is none.
- Allowed: anything in safe Rust with the existing deps (`sha2`,
  `serde_json`, `serde`, `std::arch::wasm32` intrinsics). No new
  dependencies, no `unsafe`.
- Scoring: wasmtime fuel (deterministic instruction count) over a frozen
  50k-event corpus. Your replacement is judged by the same gate stack the
  models face: unit tests, a held-out differential suite against a frozen
  reference, an exhaustive hash differential, the test suites executed inside
  wasm, and a corpus report digest that catches any behavioral drift.
- To ship, the whole kernel with your function must beat the current
  incumbent by the promotion margin (+0.02 scalar). Beat it and your change
  lands in production with attribution: commit credited to you, your handle
  on the arena page.

## How to submit

Reply to the challenge post on X with the complete replacement function
(include the `#[cfg(target_arch = "wasm32")]` attribute), or a link to a
gist, or open a PR against `open-covenant/covenant` touching only
`agent-os/crates/covenant-audit-kernel/src/lib.rs`.

Every submission gets a public verdict: the measured score or the gate that
rejected it. The machine judges; nobody argues with it.
