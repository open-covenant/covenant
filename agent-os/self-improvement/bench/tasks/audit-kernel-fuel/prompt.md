Reduce the wasmtime fuel cost (deterministic instruction count, wasm32-wasip1 release) of the code between the `// EVOLVE-BLOCK-START` and `// EVOLVE-BLOCK-END` markers in `agent-os/crates/covenant-audit-kernel/src/lib.rs`.

Hard constraints:

- Edit ONLY inside the EVOLVE block of that one file. Any change to other files, to the public API, types, wrapper functions, the `#![forbid(unsafe_code)]` attribute, or the markers themselves scores zero.
- Observable behavior must be bit-identical: same `ChainReport` and `ChainEntry` values, same failure kinds in the same order. Unit tests pin exact lowercase sha256 hex output, the `prev\nhash` chain composition, and the all-zeros root; a held-out differential suite and a frozen 50k-event corpus digest catch any drift.
- Dependencies are frozen: `sha2`, `serde_json`, `serde` only.

Fuel is dominated by per-line work over large JSONL inputs: hashing, hex encoding, JSON parsing, and heap allocation (allocator instructions count). Lower fuel is better; the score is `baseline_fuel / candidate_fuel`.
