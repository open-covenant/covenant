# Getting started

From zero to a working Covenant development environment.

## Prerequisites

| Tool | Version |
|------|---------|
| Rust | stable |
| Node.js | 22+ |
| pnpm | 10+ |
| Anchor (optional, for the on-chain program) | 0.31+ |
| solana-cli (optional, for the on-chain program) | latest |

## Clone

```bash
git clone https://github.com/open-covenant/covenant.git
cd covenant
```

## Build the daemon and crates

```bash
cd agent-os
cargo build --workspace --exclude covenant-settlement-program
```

The built binary lands at `target/debug/covenantd`. Configuration lives under `$COVENANT_HOME` (default `$HOME/.covenant`).

## Run the test suite

```bash
bash scripts/validate.sh --quick
bash scripts/validate.sh
```

Tests prefixed `live_` exercise real backends (real network, real subprocesses, real model). They are `#[ignore]`'d to keep the default run fast. Opt in with:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

## Build the on-chain program (optional)

```bash
anchor build
```

Anchor builds the Solana settlement program for the BPF target. This is a separate toolchain from `cargo build` because of the SBF compiler.

## Run the landing site locally

```bash
cd ..
pnpm --dir landing install --frozen-lockfile --ignore-workspace
pnpm --dir landing dev
```

Visit http://localhost:3001.

## Where to look next

- [`README.md`](../README.md): project overview
- [`CONTRIBUTING.md`](../CONTRIBUTING.md): contribution guide
- [`SECURITY.md`](../SECURITY.md): responsible disclosure
- [`ROADMAP.md`](../ROADMAP.md): what's in flight
