# Test fixtures

The `percolator_prog_e2e.rs` test loads Toly's actual deployed
mainnet program (`4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi`)
into an in-process `solana-program-test` bank and submits our
keeper's wire-format instructions against it. This verifies our
instruction builders are byte-for-byte compatible with the
program running on Solana mainnet right now.

## Fetching the fixture

The `.so` is not checked into git (857 KB, dumped from mainnet).
Fetch it with the Solana CLI:

```bash
solana program dump 4m3ipBQDYX6JQ9YSmUXDjESDHMtGWtiXforkWr9Qoxdi \
  agent-os/crates/covenant-percolator/tests/fixtures/percolator-prog.so \
  --url mainnet-beta
```

Then run the e2e suite:

```bash
cargo test -p covenant-percolator --features solana-rpc --test percolator_prog_e2e
```

The test auto-skips with a stderr note if the fixture is absent.
This is intentional — CI without internet shouldn't fail; only
direct e2e runs need the dump.
