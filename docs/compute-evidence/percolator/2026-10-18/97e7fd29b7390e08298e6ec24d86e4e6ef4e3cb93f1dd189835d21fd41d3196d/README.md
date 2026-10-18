# Percolator #241: second paid execution failure

Covenant job `360eba3e-43a0-43e7-a0d7-aa3ca57e4174` is a completed signed execution with **accepted=false / tool_error**, not a proven property. The existing Percolator fix and harness were authored before this paid run; this receipt establishes a verification attempt, not Covenant authorship.

The corrected node scratch mount allowed dependency build scripts and several Percolator test compilations to finish. Cargo then stopped because the all-tests selection included `reference_model_conformance`, which requires the `fuzz` feature. The signed plan requests tests with an empty feature list. The full stderr records that failure. No `kani-export.json` was produced, so there is no solver proof or refutation to report. The runner exited 1; timeout, OOM and evidence-overflow flags are all false.

The signed offer and receipt price are each **50,000 microUSDC (0.05 USDC)**. The coordinator reports a charge of 50,000; its payout field is unavailable (`null`). These records do not independently verify settlement. Successful receipt execution status (`ok`) must be read separately from the rejected proof outcome.

The earlier failed paid attempt `ac6a8b30-1003-4b2a-9558-b580aa590812` remains a separate immutable archive with its own 100,000 microUSDC receipt and scratch-execution failure. This archive neither replaces that attempt nor claims a successful retry.

Original Percolator commit: `035d4cb25eff1fce25d0a385ab92b700d8da3469`. Signed snapshot: `7cbc0004b0e47b09187471054219fb7fb9cb0184`. Both resolve to tree `6de5571b649dcd572a15a73341206e75e24c016b`. The exact source bundle SHA256 is `4c3120a9604e62af97155a0becfd035112547a63a72ce1d48ddee02b569e5709`; the original commit object and readable source files are preserved for independent comparison.

Harness: `proof_v16_public_counterparty_backing_withdraw_rejects_lapsed_bucket_at_expiry`. The signed plan requests an expected proven outcome, the live-claim cover, tests=true, features=[], and a 1,200-second harness timeout. The order requests 27,648 MiB memory, 6 CPUs, 8,192 MiB scratch, 1,800-second check timeout and 2,100,000 ms deadline. Setup is `cargo fetch --locked`.

Image: `ghcr.io/open-covenant/covenant/compute-node-public@sha256:5648c867341b054aebf3570266200cd49766caea7f5119d2735dc437fcfe3f7d`. Resolved image ID: `sha256:5648c867341b054aebf3570266200cd49766caea7f5119d2735dc437fcfe3f7d`. The tool pin is Kani 0.68.0, CBMC 6.11.0, nightly 2026-08-21-x86_64-unknown-linux-gnu. Runtime source records identify node 4dd036a and coordinator 983172d; the operator signature binds the image identifiers but does not independently prove reproducible image construction.

Actual buyer binary build remains 6502c3b, SHA256 `b2fc00d2dcb5010b316afc907187e95d7d71f68d9cf51205265acc0827c225cf`. The offline verifier remains the actual d997 build. `verifier-merge-equivalence-4dd036a.json` records exact unchanged buyer/protocol/verifier inputs; it does not claim all-agent-os or whole-runtime equality. The published `compute-v0.2.0` release at 93f0924 lacks these pilot commands despite the same version string.

The directory under `jobs/` contains the complete signed envelope, receipt, output, exact source bundle and **15,152 bytes** of raw result/stdout/stderr artifacts. All bytes are retained without redaction or log trimming. The signed execution window expires at `2026-10-18T00:33:06.006000+00:00`; public staging must wait until then and pass exact-inventory privacy review.

To verify offline, check out Covenant commit `d997bb6436569d8dc96f963aaf72259b92ccb5ba`; run `python3 build-verifier.py --checkout /path/to/covenant --project /path/to/helper`, copy `verifier.Cargo.lock` to the helper's `Cargo.lock`, and build it using `cargo +1.98.1 build --locked --manifest-path /path/to/helper/Cargo.toml`. Then run `python3 verify-archive.py --archive /path/to/this/archive --verifier /path/to/helper/target/debug/verify-public-receipt`. Building may fetch public dependencies; verification itself uses no network or buyer credentials and spends nothing.

Signatures authenticate operator claims and bind source, plan, output and raw evidence. They do not independently establish hardware isolation, blockchain settlement or complete coordinator job inventory. Future attempts need their own signed plan and job ID; no further purchase is performed by this archive.
