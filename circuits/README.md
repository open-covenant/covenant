# Covenant Circuits

Circom targets for Covenant proof generation and proof-backed Solana protocol receipts.

## Layout

```
circuits/
├── package.json                    # circomlib + snarkjs pins
├── catalog/
│   ├── README.md                   # shared naming + manifest conventions
│   └── *.json                      # circuit metadata manifests
├── ceremony/
│   ├── README.md                   # production trusted-setup workspace
│   ├── phase1/
│   │   ├── README.md               # pinned ptau source + verifier attestations
│   │   └── VERIFY.md               # exact commands to verify the ptau + r1cs
│   └── phase2/
│       ├── README.md               # contribution / beacon / export checklist
│       ├── VERIFY.md               # exact commands to verify the final zkey + VK
│       ├── BEACON.md               # final randomness-beacon record
│       └── contributions/
│           └── TEMPLATE.md         # per-contributor transcript template
├── task_completion/
│   ├── task_completion.circom      # top-level
│   ├── components/
│   │   └── merkle_verifier.circom
│   ├── inputs/sample_input.json    # sample input (zeroed); regenerate before proving
│   ├── scripts/
│   │   ├── compile.sh              # circom --r1cs --wasm --sym
│   │   ├── setup.sh                # TRUSTED-SETUP-DEV-ONLY ptau + zkey
│   │   ├── prove.sh
│   │   └── verify.sh
│   └── build/
│       ├── verification_key.json       # committed (small, dev-only)
│       ├── verification_key.meta.json  # dev-only status marker
│       └── (*.ptau, *.zkey, *.wasm, *.r1cs — gitignored)
└── README.md
```

## Compile

Prereqs: Node 20+, Rust toolchain, `circom` 2.1.5+. Install circom via
`cargo install --git https://github.com/iden3/circom.git --tag v2.1.9`.

```bash
cd circuits
pnpm install
pnpm compile          # emits r1cs / wasm / sym to task_completion/build/
pnpm setup            # dev-only ptau + zkey + verification_key.json
pnpm prove            # requires a real sample_input.json first
pnpm verify
```

`compile.sh` pipes `snarkjs r1cs info` into `build/constraints.txt` so CI can assert the
constraint count stays under the 20 000 ceiling.

## Constraint count

**Observed:** not measured yet: `circom` was not installed in the environment used for the
initial commit, so compilation (and `build/constraints.txt`) is deferred until `circom`
is available in CI.
**Estimated** from circomlib Poseidon gate counts:

| Component                            | ~Constraints |
|--------------------------------------|-------------:|
| Poseidon sponge over (salt ∥ task_preimage) | ~240 |
| Poseidon sponge over result_preimage (3 absorb chunks) | ~720 |
| Merkle verify depth-3 (8 leaf hashes + 3 node hashes) | ~1 230 |
| AND over 8 criteria bits             | ~10 |
| Num2Bits(64) × 2 + LessEqThan(64)    | ~195 |
| Plumbing / constants                 | ~100 |
| **Estimated total**                  | **~2 500** |

This is well under the 15 000 target and leaves ample headroom if `N_RESULT` or `K` grow.
The spec's original budget (~11 000) assumed a wider Poseidon2 variant; circomlib's
Poseidon is denser per absorb but runs fewer rounds. Final numbers land in
`reports/05-circuit-zk.md` once circom is available in CI.

## Trusted setup — dev only

`scripts/setup.sh` runs a **single-contributor, local** ceremony:

1. `powersoftau new bn128 15` (supports ≤ 32k constraints — comfortable headroom).
2. One `contribute` pass with time-based entropy. That entropy is knowable to anyone with
   shell access to the build host. Treat the resulting SRS as public.
3. `groth16 setup` + one `zkey contribute` pass.

The exported `verification_key.json` is flagged via `verification_key.meta.json` with
`"status": "dev-only"`. It exists solely so the rest of the stack can run
end-to-end against a development cluster.

**Do not point production settlement at this VK.** The real ceremony must use published transcripts, external randomness, and pinned artifact provenance before any production rollout.

## Known limitations (M1)

- **Poseidon variant:** circomlib ships original Poseidon; spec calls for Poseidon2.
  Marked `// POSEIDON-VARIANT`. Swap before ceremony; constraint count moves ±10 %.
- **Sample input:** `inputs/sample_input.json` is a zeroed sample. A generator script
  that computes real `task_hash` / `result_hash` / Merkle root from JS-side Poseidon lives
  in spec 09 (proof-gen service) and is not duplicated here.
- **Merkle semantic:** the verifier template opens a single leaf and relies on the AND
  check to enforce the remaining bits. If reviewers want "every leaf is true **and** is
  bound to the committed root," swap to a tree-builder (see comment in
  `components/merkle_verifier.circom`).
- **No negative test harness yet.** snarkjs fullprove will reject tampered witnesses by
  construction; the automated cases from spec test plan (§Negative tests) land with the
  proof-gen service work.
- **No CI job yet.** Workflow wiring is tracked separately.

## Artifact policy

Tracked: `task_completion.circom`, components, scripts, `package.json`, this README,
`build/verification_key.json`, `build/verification_key.meta.json`.

Gitignored: `build/*.r1cs`, `build/*.wasm`, `build/*.sym`, `build/*.zkey`,
`build/*.ptau`, `build/task_completion_js/`, `build/proof.json`, `build/public.json`,
`build/constraints.txt`.

For the real production ceremony, use `circuits/ceremony/`. The current
`task_completion/build/verification_key.json` remains dev-only and is not a
drop-in replacement for the production VK.

## Circuit catalog

`circuits/catalog/` is the shared metadata layer for the proof-gen service and future
verification-key management. Each manifest records the circuit slug, version, verifier type,
public-input order, and lifecycle (`live`, `planned`, or `research`).
