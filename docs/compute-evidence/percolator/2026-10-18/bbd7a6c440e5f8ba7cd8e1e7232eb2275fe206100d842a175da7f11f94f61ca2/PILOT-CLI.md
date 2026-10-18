# Optional Covenant rerun

Offline receipt verification is described in README.md and spends nothing. A fresh paid execution is a separate opt-in action.

Build the buyer from canonical source `4dd036a4d59612326095768ff4b4f272aa48562d` using its lockfile and Rust 1.98.1. The recorded pilot binary was actually built from `6502c3bb406b757c80410bf30014b08dc448e882`, SHA256 `b2fc00d2dcb5010b316afc907187e95d7d71f68d9cf51205265acc0827c225cf`; exact buyer/protocol inputs are unchanged at the canonical merge. Record your own build's source, compiler, target and binary hash. Released `compute-v0.2.0` downloads are not this pilot CLI.

```sh
git clone https://github.com/open-covenant/covenant.git covenant-pilot
git -C covenant-pilot checkout --detach 4dd036a4d59612326095768ff4b4f272aa48562d
cargo +1.98.1 build --locked --manifest-path covenant-pilot/agent-os/Cargo.toml \
  -p covenant-compute-buyer --bin covenant-compute
PILOT_CLI="$PWD/covenant-pilot/agent-os/target/debug/covenant-compute"
python3 native-rerun-command.py --job /path/to/archive/jobs/pr241-at-expiry \
  --cli "$PILOT_CLI" --source-dir /path/to/new/rerun-source > rerun-template.txt
```

Use your configured target directory if `CARGO_TARGET_DIR` is set. Replace the job path with `jobs/pr241-after-expiry` for the other route. The generator only prints commands; it does not execute source, contact Covenant or spend. It reconstructs the exact signed source bundle and uses the archived plan, image, setup, resources, timeout/deadline and offer.

Review the generated `check --dry-run` preview using your own buyer configuration. It performs local validation without a purchase and does not promise live capacity. To buy an intentional rerun, remove `--dry-run`, add a fresh idempotency key once, and reuse that same key for recovery. A new key is another purchase. Each archived offer is 50,000 microUSDC; completed execution and accepted proof outcome are distinct. No campaign buyer keys or private configuration are included here.

Exact matching capacity must support 27,648 MiB memory, 6 CPUs and 8,192 MiB scratch with the pinned image. The planned smaller retained seat cannot satisfy that exact request. Offline verification needs no such capacity; changing resources creates a separately scoped run.
