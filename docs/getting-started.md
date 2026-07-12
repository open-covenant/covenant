# Getting started

From zero to your first task in five steps.

## Prerequisites

| Tool | Version |
|------|---------|
| Rust | stable |
| Node.js | 22+ |
| pnpm | 10+ |
| Anchor (optional, for the on-chain program) | 0.31+ |
| solana-cli (optional, for the on-chain program) | latest |

## 1. Clone

```bash
git clone https://github.com/open-covenant/covenant.git
cd covenant
```

## 2. Build the daemon and CLI

```bash
cd agent-os
cargo build --workspace --exclude covenant-settlement-program --release --locked
```

The two binaries land at `target/release/covenantd` (daemon) and `target/release/covenant` (CLI). State lives under `$COVENANT_HOME` (default `$HOME/.covenant`).

## 3. Install the example agent

```bash
mkdir -p ~/.covenant/agents
cp -R ../examples/hello-agent ~/.covenant/agents/hello
```

## 4. Start the daemon

In one terminal:

```bash
./target/release/covenantd
```

## 5. Bootstrap and run your first task

In another terminal:

```bash
./target/release/covenant bootstrap
./target/release/covenant intent "say hello"
```

`bootstrap` grants the capabilities every loaded agent needs to handle a task. After it runs, `intent` dispatches through the agent and prints the result.

## Verify what happened

```bash
./target/release/covenant audit recent -n 5
./target/release/covenant audit verify
```

The audit log is a locally hash-chained record of every step. `audit verify` walks the chain and reports any tampering.

## Run the test suite

```bash
bash agent-os/scripts/validate.sh --scripts
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/validate.sh
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

- [`docs/demo.md`](./demo.md): one task end-to-end with every audit artifact called out
- [`docs/agent-quickstart.md`](./agent-quickstart.md): from zero to a running, capability-scoped agent with the SDK
- [`docs/agent-sdk.md`](./agent-sdk.md): build an agent in Rust with the `covenant-sdk` client
- [`README.md`](../README.md): project overview
- [`CONTRIBUTING.md`](../CONTRIBUTING.md): contribution guide
- [`SECURITY.md`](../SECURITY.md): responsible disclosure
- [`ROADMAP.md`](../ROADMAP.md): what's in flight
