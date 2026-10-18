# First Percolator #241 paid attempt: tool error, no proof

Job `ac6a8b30-1003-4b2a-9558-b580aa590812` completed a signed Covenant checker execution, but its Kani outcome was **`tool_error`, accepted false**. No proof or refutation was established. This archive preserves that failed attempt and its charge; it must not be presented as a successful verification or replaced by a later retry.

The order targeted `proof_v16_public_counterparty_backing_withdraw_rejects_lapsed_bucket_at_expiry` on pre-existing Percolator commit `035d4cb25eff1fce25d0a385ab92b700d8da3469`. Its signed history-free snapshot `7cbc0004b0e47b09187471054219fb7fb9cb0184` independently resolves to the identical tree `6de5571b649dcd572a15a73341206e75e24c016b`. The fix and harness were not authored by this compute job, and no patch overlay was used.

The signed result reports exit code 1, `tool_error`, accepted false, with no timeout, OOM or evidence overflow. Dependency compilation attempted to execute build scripts for `quote`, `getrandom` and `libc` under `/scratch/target`; stderr records `Permission denied (os error 13)` and `never executed`. Kani did not produce a JSON proof export. Its report consequently states `export is not a JSON object`. The logs establish the compilation/launch failure; the mount diagnosis and corrective deployment are separate follow-up work.

`jobs/pr241-at-expiry-first-tool-error/kani/` contains the exact signed `result.json`, stdout and stderr: 2,474 raw bytes in total. There is no `kani-export.json`, and the archive does not synthesize one. The signed receipt status `ok` means the checker returned a valid execution report; it does not mean the requested proof passed.

The signed offer and receipt both state **100,000 microUSDC**. The coordinator API also reports **100,000 microUSDC charged** and **90,000 microUSDC payout** for this failed proof attempt. The chain settlement has not been independently verified. Direct native checks pay for valid completed checker execution separately from proof acceptance.

The order binds image `ghcr.io/open-covenant/covenant/compute-node-public@sha256:1ac3419ac2c070b464d753b5b78094725733740c8b136505b3a38cae759345bd`, Kani 0.68.0, CBMC 6.11.0 and `nightly-2026-08-21-x86_64-unknown-linux-gnu`, with 27,648 MiB memory, 6 CPUs, 8,192 MiB scratch, a 1,200-second harness timeout, 1,800-second check timeout and 2,100,000-ms deadline. The source, plan and signed resource fields remain unchanged in this archive.

The prior generic and tiny Kani transport canaries succeeded independently. Their accepted outcomes do not turn this Percolator attempt into a proof. Any corrected retry needs a new explicit job and receipt, and must preserve this original failure in the campaign history.

## Offline verification

The actual offline verifier build is `d997bb6436569d8dc96f963aaf72259b92ccb5ba`; canonical merge `983172ddd04ac2de70136125e2088f22c6edf505` has identical verifier/runtime/Kani input objects in the documented scopes, with five different release/documentation paths. Whole repository trees are not equal. Obtain either source and use Rust 1.98.1:

```sh
python3 build-verifier.py --checkout /path/to/covenant --project /tmp/receipt-verifier
cp verifier.Cargo.lock /tmp/receipt-verifier/Cargo.lock
cargo +1.98.1 build --locked --manifest-path /tmp/receipt-verifier/Cargo.toml
python3 verify-archive.py --archive . --verifier /tmp/receipt-verifier/target/debug/verify-public-receipt
```

Use the configured target directory if `CARGO_TARGET_DIR` is set. Building may download public dependencies; verification is offline and spends nothing. Expected verification output includes `receipt_execution_status: ok`, `acceptance_passed: false` and `kani.outcome: tool_error`.

No paid rerun is part of these instructions. The public `compute-v0.2.0` download lacks this pilot workflow; `pilot-cli-provenance.json` identifies the separate source-built CLI. Operator signatures bind claims and exact bytes but do not establish hardware isolation, independent settlement or a complete coordinator job inventory.

Public export must wait until the signed order expires at **2026-10-18 00:04:36.101 UTC**, followed by the exact inventory/privacy gate. Collection and offline inspection can happen earlier. No live execution capability is to be published.
