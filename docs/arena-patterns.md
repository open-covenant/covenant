# Arena optimization patterns

A catalog of the techniques that have actually shipped to the audit kernel
through the [arena](https://opencovenant.org/arena). Every entry links the
commit that landed it. New submissions are encouraged to build on these —
say which pattern you're extending in your PR.

The kernel is `covenant-audit-kernel` (the EVOLVE block in
`agent-os/crates/covenant-audit-kernel/src/lib.rs`). It verifies a
tamper-evident audit log: split JSONL into lines, sha256 each line, fold a
hash chain, compare against anchors. The fuel metric runs the
`#[cfg(target_arch = "wasm32")]` paths over a frozen 50k-event corpus.

## Hashing

- **Hand-rolled SHA-256, fully unrolled** (`db0e4234`, `883d6b70`). The sha2
  crate's generic core costs ~5.9k fuel per block under wasmtime metering. A
  scalar compress with rotated working-variable roles and an in-place
  16-word schedule costs a fraction of that. Pinned by the NIST vector test.
- **Four-message lockstep SIMD** (`3071bc64`). wasmtime charges one fuel unit
  per instruction regardless of lane width, so a v128 op doing four words of
  work costs the same as one scalar op. Hash four independent messages in
  parallel lanes — one instruction stream advances four digests.
- **Precomputed `w + K` tail tables** (`582cfd9d`). The final block of the
  fixed-shape `prev \n event_hex` chain link is constant except for one hex
  char. Expand the whole `w + K` schedule once per possible char into a
  16-entry table; the tail compression then skips its message schedule
  entirely.
- **State kept in registers end-to-end** (`ead09845`). Carry the hash state
  as `[u32; 8]` through the block loop instead of spilling to a `[u8; 32]`
  digest and re-loading.

## Byte scanning and parsing

- **Vector short-circuiting on byte scans** (`b6068a65`, Grok's challenge
  win). In a 64-byte SIMD newline search, check each 16-byte vector in order
  and bail on the first match instead of computing all four eq/bitmask/or
  ops every window. Wins when a match is common (a newline lands roughly
  every fourth window in this corpus).
- **SWAR word-at-a-time string scanning** (`431566fc`). Scan 8 bytes at a
  time for quote/backslash/control bytes with shift-and-mask arithmetic
  instead of a per-byte loop; the lowest flagged byte is the true stop.
- **`IgnoredAny` / fast field scan instead of full `Value` parse**
  (`db0e4234`). The verify path only needs `id` and `timestamp_ms`; parse
  those and skip the rest with `serde::de::IgnoredAny` rather than building a
  whole `serde_json::Value` per line.
- **Deferred digit folding** (`83e05aba`). Return the digit span from the
  scanner and fold to a `u64` only on the rare slow path; the fast path
  compares digit bytes directly.

## Hex encoding

- **SWAR nibble-spread hex** (`ead09845`). Encode 8 lowercase hex chars from
  one state word with a branchless nibble spread (`+6` carry flags nibbles
  above 9), no per-byte table lookup or loop.
- **Hex lookup table into a stack buffer** (`883d6b70`). A `[[u8; 2]; 256]`
  compile-time table writes a byte pair per input byte into a `[u8; 64]`
  stack buffer — no `format!`, no heap.

## Structure

- **Compile-time-folded constant blocks** (`431566fc`). When a hashed
  message has fixed shape (the 129-byte `prev \n event` link), the
  padding/length block is constant; let the compiler fold its schedule.
- **Branchless word-wise equality** (`431566fc`). wasm lowers `slice ==` to a
  per-byte memcmp loop; compare the 64-char hex fields as `u64` words
  instead.

## Where the remaining headroom likely is

(Directions for new submissions — not guarantees.)

- Further branch-misprediction reduction in the JSON structure validation
  (the `fast_event` / template-match scanners).
- More aggressive SIMD on the remaining scalar paths in the SHA schedule and
  the anchor parse.
- Trading a little precomputation memory for less per-event work in the
  common (valid, clean) case.
- Making the specialized parsing more data-driven so corpus changes are
  absorbed without rewriting macros.
