# Native Covenant checker and Kani receipt canaries

These two paid canaries verified the deployed native checker path and the transport of complete Kani evidence. They do not establish a Percolator proof or Covenant authorship of their source. The exact original source, signed input snapshot, signed receipt, output and offline verifier are archived here.

| Canary | Job | Result | Signed offer / API-reported charge |
| --- | --- | --- | --- |
| Generic native check | `001980a8-6442-49c0-9081-983def87331d` | Accepted: two source files exist | 100,000 / 100,000 microUSDC |
| Tiny Kani native check | `97fbf4a1-a86c-4ca7-ab63-f44e3a7ffeed` | `receipt_smoke` proven; required cover satisfied | 100,000 / 100,000 microUSDC |

## What each result means

The generic check used existing Percolator commit `035d4cb25eff1fce25d0a385ab92b700d8da3469`. Its signed history-free snapshot `7cbc0004b0e47b09187471054219fb7fb9cb0184` has the same tree, `6de5571b649dcd572a15a73341206e75e24c016b`. It ran only `test -f Cargo.toml && test -f src/v16.rs`, using the mutable image reference `covenant-compute-check:rust`. This checks routing, source transport, empty-patch admission and signed execution. It does not test Percolator behavior or invoke Kani.

The Kani check used the locally authored smoke fixture at commit `64b353b47a9549259c14ddb00e2e81bb91fc1936`. Signed snapshot `c441cb62c506ef4e2921ab4944a87149320977f5` has the same tree, `8c2ba88ee1f50b53462940d3f9f5e5457b319c6b`. Harness `receipt_smoke` selects an arbitrary `u8`, assumes it is less than 10, requires a reachable `value == 4` cover, and checks `value < 10`. It is deliberately a small transport canary. The source also contains `receipt_refute`; this job did not execute that harness.

The Kani order and result bind image `ghcr.io/open-covenant/covenant/compute-node-public@sha256:1ac3419ac2c070b464d753b5b78094725733740c8b136505b3a38cae759345bd`, Kani 0.68.0, CBMC 6.11.0 and `nightly-2026-08-21-x86_64-unknown-linux-gnu`. They request 27,648 MiB memory, 6 CPUs and 8,192 MiB scratch, a 30-second harness timeout, a 300-second check timeout and a 600,000-ms order deadline. The exact signed capability is `kani-v1:e9b92565e08b926e6d036d7f25ef9ca9e2f6f0ce8d80f76d4a45aacc82501d13`.

The signed Kani result reports `proven`, `accepted: true`, exit code 0, with neither timeout, OOM nor evidence overflow. The archived export contains one successful assertion and the satisfied required cover. `result.json`, `stdout.log`, `stderr.log` and `kani-export.json` preserve all 7,340 raw bytes from the signed compressed package; no logs were trimmed. This is evidence of the reported execution under these exact inputs, not a performance benchmark or a claim that every Percolator harness fits the same profile.

## Provenance and limits

Both original source trees were prepared before these paid jobs. Each `source.bundle` contains a single history-free snapshot; `original-commit.raw` and `source-mapping.json` independently bind it to the original tree. The archived file copies are checked against the signed bundles. No patch overlay was used.

The 32 GB host used a 27 GiB proof limit. Before purchase, a proposed 28 GiB request failed the available-memory reserve check and was never purchased. `pilot-resource-adjustment.json` records the measurement and adjustment. The future 12,288 MiB / 6 CPU / 4,096 MiB profile cannot satisfy these exact larger Kani resource requirements. Historical receipts do not guarantee live capacity; offline verification needs none.

Each signed offer and receipt is 100,000 microUSDC; the coordinator API also reports 100,000 charged and 90,000 paid to its checker for each job. The combined reported charge is 200,000 microUSDC (0.20 USDC). `accounting.json` separates these records. Settlement transactions were not independently reconciled. Signatures authenticate operator claims and input/output hashes; they do not prove independent infrastructure ownership, hardware isolation or a complete inventory of coordinator activity.

The actual offline verifier was built at `d997bb6436569d8dc96f963aaf72259b92ccb5ba`. Canonical merge `983172ddd04ac2de70136125e2088f22c6edf505` has identical verifier/runtime/Kani inputs in the documented scopes, while its complete repository tree differs in five release/documentation paths. See `verifier-merge-equivalence-983172d.json`; no build is relabelled as having occurred at the merge.

## Verify offline without spending

Obtain the public Covenant source at canonical commit `983172ddd04ac2de70136125e2088f22c6edf505`, then build the separate offline verifier with Rust 1.98.1:

```sh
python3 build-verifier.py --checkout /path/to/covenant --project /tmp/receipt-verifier
cp verifier.Cargo.lock /tmp/receipt-verifier/Cargo.lock
cargo +1.98.1 build --locked --manifest-path /tmp/receipt-verifier/Cargo.toml
python3 verify-archive.py --archive . --verifier /tmp/receipt-verifier/target/debug/verify-public-receipt
```

If `CARGO_TARGET_DIR` is set, use that directory for the verifier binary. Building may fetch public Cargo dependencies. Verification itself is offline: no buyer credentials, coordinator access, provider capacity or purchase is required. It checks the inventory, source mappings, exact signed envelope/output hashes, receipt signatures, generic command order, Kani plan/tool/image/resources, classified raw export and full artifacts.

## Optional new paid execution

The released `compute-v0.2.0` download at commit `93f0924154f4f4e818fb9c90e8e52669043c2c83` lacks native check/Kani/evidence support. Its `0.2.0` version string alone does not identify the pilot CLI. Build `covenant-compute-buyer --bin covenant-compute` from canonical source `983172ddd04ac2de70136125e2088f22c6edf505` with its lockfile, and record your binary hash. `pilot-cli-provenance.json` preserves the actual local pilot build at `6502c3bb406b757c80410bf30014b08dc448e882` and its platform-specific SHA-256; it is not a released binary.

After offline verification, `python3 native-rerun-command.py --job jobs/native-kani-proven-canary --cli /path/to/covenant-compute --source-dir /path/to/new/source` prints source reconstruction and a no-spend `check --dry-run` from the exact signed fields. It does not execute these commands. A separately opted-in paid rerun removes `--dry-run` and uses one fresh `--idempotency-key`, retained for recovery of that same purchase. It needs your own funded buyer identity and live matching capacity, and pays for completed checker execution even if the requested proof is not accepted. The generated paid command is commented out by default.

Public staging occurs only after both signed order windows expire and the exact inventory passes privacy review. `publication-review.json` is that local review record; it is not another operator signature.
