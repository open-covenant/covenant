# Percolator #241: Covenant verification receipts

Both public backing-withdrawal route harnesses for [Percolator #241](https://github.com/aeyakovenko/percolator/pull/241) passed paid Covenant Kani checks. These are verification receipts for an existing fix and harnesses; they do not establish Covenant authorship of that Percolator code.

| Boundary | Covenant job | Outcome | Runner duration | Raw artifacts |
| --- | --- | --- | --- | --- |
| Exactly at expiry, slot 10 | `b7a20990-a245-486e-ac4b-44d1331c272a` | proven, accepted=true | 561,736 ms | 5,225,549 bytes |
| After expiry, slot 11 | `0425f25b-83bc-45ee-8fb9-cdcbc94d7b09` | proven, accepted=true | 563,394 ms | 5,225,578 bytes |

Each raw export contains 6,321 successful checks, 163 unreachable checks, zero failed or undetermined checks, and the exact required cover “withdrawal from lapsed backing under a live claim is refused” satisfied. Both runs exited 0 without timeout, OOM or evidence overflow. [The machine-readable summary](proof-summary.json) records their exact harness names, export hashes, prices and expiries.

The harness domain is backing 1–8, withdrawal 1–backing, and claim 0–(backing minus withdrawal). Bucket expiry is slot 10; current slot is 10 or 11 respectively. Each harness asserts `V16Error::Stale` and unchanged header and engine state, with source attributes unwind 48 and CaDiCaL. This is a bounded claim about these selected harnesses, not all Percolator behavior.

Original source commit: `035d4cb25eff1fce25d0a385ab92b700d8da3469`. Signed snapshot: `7cbc0004b0e47b09187471054219fb7fb9cb0184`. Both resolve to tree `6de5571b649dcd572a15a73341206e75e24c016b`. Each job retains the original commit object, exact source bundle (SHA256 `4c3120a9604e62af97155a0becfd035112547a63a72ce1d48ddee02b569e5709`), readable source and an independently checked tree mapping.

The signed plans use tests=true, `features=["fuzz"]`, expected outcome proven, the required live-claim cover, and a 1,200-second harness timeout. Orders request 27,648 MiB memory, 6 CPUs and 8,192 MiB scratch, with a 1,800-second check timeout and 2,100,000 ms deadline. Setup is `cargo fetch --locked`. The feature correction is recorded in [plan-correction.json](plan-correction.json); it did not change source, harness assertions or covers.

Image: `ghcr.io/open-covenant/covenant/compute-node-public@sha256:5648c867341b054aebf3570266200cd49766caea7f5119d2735dc437fcfe3f7d`, also the resolved image digest. Tool pin: Kani 0.68.0, CBMC 6.11.0, nightly-2026-08-21-x86_64-unknown-linux-gnu. Node source is `4dd036a4d59612326095768ff4b4f272aa48562d`; coordinator remains `983172ddd04ac2de70136125e2088f22c6edf505`. Actual buyer binary build remains `6502c3bb406b757c80410bf30014b08dc448e882`; the offline verifier remains the actual `d997bb6436569d8dc96f963aaf72259b92ccb5ba` build. [Scoped equivalence](verifier-merge-equivalence-4dd036a.json) covers buyer/protocol/verifier inputs, excluding the changed node executor. Image and deployment observations are recorded separately in [pilot-image-pin.json](pilot-image-pin.json).

Each accepted order and receipt is priced at 50,000 microUSDC (0.05 USDC). The coordinator reports charge 50,000 and payout 45,000 for each, with transaction references retained in the job metadata. The two accepted checks therefore have a combined reported charge of 100,000 microUSDC. Settlement is not independently verified by these signatures.

Two earlier paid failures remain separate, unchanged evidence:

- [First attempt](../e45e484093bd11eda6784f3b13b73b4d352bdf77263a9512f457ea25cface48d/README.md), job `ac6a8b30-1003-4b2a-9558-b580aa590812`: build scripts could not execute; tool_error, charge 100,000 microUSDC.
- [Second attempt](../97e7fd29b7390e08298e6ec24d86e4e6ef4e3cb93f1dd189835d21fd41d3196d/README.md), job `360eba3e-43a0-43e7-a0d7-aa3ca57e4174`: all-tests selection required `fuzz`; tool_error, charge 50,000 microUSDC.

Neither failure produced a proof export. The four #241 attempts together have a reported charge of 250,000 microUSDC. Successful proof receipts do not replace either failure or alter its original feature-free plan.

All **10,451,127 bytes** of result/stdout/stderr/Kani-export artifacts are retained byte-for-byte in the two `jobs/` directories, alongside exact signed envelopes, receipts and outputs. No buyer identity, private configuration or active service access is needed for verification. The archive must remain private until the latest included envelope expiry, 2026-10-18 00:46:34.322 UTC, and pass the exact-inventory privacy gate.

## Verify offline without spending

Check out Covenant `d997bb6436569d8dc96f963aaf72259b92ccb5ba`, then build the standalone helper with its recorded lockfile:

```sh
python3 build-verifier.py --checkout /path/to/covenant --project /path/to/helper
cp verifier.Cargo.lock /path/to/helper/Cargo.lock
cargo +1.98.1 build --locked --manifest-path /path/to/helper/Cargo.toml
python3 verify-archive.py --archive /path/to/this/archive \
  --verifier /path/to/helper/target/debug/verify-public-receipt
```

Building may fetch public Rust dependencies. Verification itself makes no network request, requires no buyer credentials or live provider, and buys nothing. It checks inventory hashes, buyer/operator signatures, exact envelope/output commitments, original/snapshot source equality, plans, image/tool/resource bindings, required covers and complete raw artifact bytes.

An optional paid rerun is separate; [PILOT-CLI.md](PILOT-CLI.md) gives the source-build and exact-field command generator. Released `compute-v0.2.0` downloads at `93f0924154f4f4e818fb9c90e8e52669043c2c83` do not contain these pilot commands despite the same version number. The 32 GB host used a 27 GiB proof limit. Exact reruns need the recorded 27,648 MiB / 6 CPU / 8,192 MiB capability; the planned 12,288 MiB / 6 CPU / 4,096 MiB retained seat does not satisfy that larger request. Historical receipts do not guarantee live capacity.

## Product validation and trust limits

Initial local validation used Rust 1.98.1: buyer 464 passed / 13 explicit ignores; coordinator/node/protocol 1,163 passed / 28 ignores, plus 223 script guards. Historical PR #322 full Linux Rust CI used 1.99.1 and passed 4,078 tests / 431 ignores per job. Later PR #328, PR #331 and merged-source CI used pinned 1.98.1 and passed 4,108 / 431 per job. [The current merge CI](https://github.com/open-covenant/covenant/actions/runs/39080516786/job/117536330385) and [the scoped compiler audit](product-validation-pr328-pr331-merges.json) keep these runs distinct. Repeated jobs are not additional unique coverage; these product checks are separate from the paid Kani proofs.

Operator signatures authenticate claims and bind the evidence. They do not independently establish hardware isolation, reproducible image construction, blockchain settlement or complete coordinator job inventory. Any change to Percolator source or proof bounds requires a new verification claim. Publishing receipt links does not change the verified source tree.
