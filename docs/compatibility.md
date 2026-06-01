# Compatibility

Covenant runs as an unprivileged daemon on stock Linux and macOS. The matrix below tracks where each runtime backend is supported and which host prerequisites a live boundary check requires.

## Host operating system

| OS                    | Daemon | CLI | `trusted-local` runtime | `linux-gvisor` runtime |
|-----------------------|:------:|:---:|:------------------------:|:----------------------:|
| Linux (x86_64)        | ✓      | ✓   | ✓                        | ✓ (opt-in; see below) |
| Linux (aarch64)       | ✓      | ✓   | ✓                        | ✓ (opt-in; see below) |
| macOS (arm64, x86_64) | ✓      | ✓   | ✓                        | — (not applicable)    |
| Windows               | —      | —   | —                        | —                     |

macOS is `trusted-local` only. The opt-in gVisor backend (`COVENANT_RUNTIME_BACKEND=linux-gvisor`) requires a Linux host.

## Rust toolchain

The repository pins `stable` via [`rust-toolchain.toml`](../rust-toolchain.toml). A fresh stable toolchain (released within the last six months) is the supported configuration. The workspace edition is 2021.

## Node and pnpm

Required only for the landing site, documentation builds, and the provenance verifier.

| Tool   | Version    |
|--------|------------|
| Node   | ≥ 22       |
| pnpm   | ≥ 10       |

## gVisor opt-in prerequisites

The Linux gVisor runner is gated behind explicit configuration; live coverage requires:

- `runsc` on `PATH` (or pointed at via `COVENANT_RUNSC`),
- a rootfs at `COVENANT_GVISOR_ROOTFS`,
- a scratch directory at `COVENANT_GVISOR_SCRATCH` (defaults under `$COVENANT_HOME`),
- kernel features required by `runsc` on the host.

Sandbox-required manifests fail closed when these are missing. See [`docs/runtime-sandbox-security.md`](./runtime-sandbox-security.md) for the contract.

## Anchor (optional, on-chain settlement)

The settlement program under `agent-os/programs/settlement` is built with Anchor. It is not part of the default `cargo build`.

| Tool         | Version |
|--------------|---------|
| Anchor       | ≥ 0.31  |
| solana-cli   | latest  |

## Default `cargo build`

```bash
cd agent-os && cargo build --workspace --exclude covenant-settlement-program
```

The `--exclude` is intentional — the settlement program uses the SBF compiler shipped with the Anchor toolchain, which is a separate build path from the workspace's native target.
