# Covenant Compute

Covenant Compute is the installable client for launching bounded GPU workloads.
The first supported workload is a CUDA and Jupyter workspace. ComfyUI and Open
WebUI stay disabled until their images, service routing, cancellation, and
receipt paths pass the same release gate.

## Status

This directory contains a buildable product alpha, not a hosted production
service:

- The React interface and native Tauri command layer are implemented.
- Catalog images must be pinned to an immutable SHA-256 digest.
- Duration, trust, and a USDC-denominated beta allowance are checked before
  launch.
- Launch retries carry a caller-stable idempotency key.
- Capability URLs remain in native memory and are opened by the operating
  system only after scheme and host validation.
- The `covenant-compute-control` service implements authenticated ownership,
  durable SQLite reservations, restart recovery, deadline cancellation, and a
  Vast Jupyter workspace adapter. `covenant-compute-authority` carries the
  reusable authority contract.
- The current private-beta control path accounts against an operator-funded
  allowance. It does not connect a consumer wallet or move user USDC.
- The allowance caps beta-account usage, not the provider invoice. Vast has no
  per-instance billing deadline; the control plane requests deletion at the
  selected duration and retries until the provider confirms it.
- A consumer funding flow and signed public installers are still required
  before a production launch.

The browser demo is deliberately labeled as a simulation. It creates no GPU
workload and spends no funds.

## Development

Install the workspace dependencies from the repository root:

```bash
pnpm install
```

Run the interface against the explicit simulation:

```bash
VITE_COMPUTE_DEMO=true pnpm --dir apps/covenant-compute dev
```

Run the native application against a control plane:

```bash
COVENANT_COMPUTE_API_BASE=https://compute.example \
  pnpm --dir apps/covenant-compute exec tauri dev
```

The native client accepts plain HTTP only for the IP loopback address
`127.0.0.1`. Provider credentials belong on the control plane and must never be
compiled into the application. An operator may supply a consumer session token
through `COVENANT_COMPUTE_API_TOKEN`; the native layer marks it as a sensitive
authorization header and never returns it to the renderer.

An installed app can also accept a private-beta token in the access form. The
renderer clears its input immediately after submission; the native layer keeps
the active token in process memory only. It is never persisted and is discarded
when the app exits or the user clears the session.

## Validation

```bash
pnpm --dir apps/covenant-compute test
pnpm --dir apps/covenant-compute build
cargo test --manifest-path agent-os/Cargo.toml -p covenant-compute
cargo test --manifest-path agent-os/Cargo.toml -p covenant-compute-authority
cargo test --manifest-path apps/covenant-compute/src-tauri/Cargo.toml
```

## Distribution

The `compute desktop` workflow builds:

- a universal macOS `.dmg` for Intel and Apple Silicon;
- a Linux x86_64 AppImage, `.deb`, and `.rpm` on Ubuntu 22.04.

Tag builds use Apple Developer ID signing and notarization credentials, then
publish checksums and keyless artifact signatures. Non-tag macOS bundles use a
local ad-hoc signature so bundle integrity can be tested, but Gatekeeper does
not treat them as consumer releases. A tag is also blocked unless the default
control-plane health endpoint and an authenticated offers request succeed.

The standalone, non-auto-deploying backend blueprint is
[`deploy/compute-render.yaml`](../../deploy/compute-render.yaml). It still
requires operator-supplied beta credentials, a funded Vast account, a
persistent disk, and custom-domain configuration.

Vast direct Jupyter uses a provider root certificate. A first-time macOS user
must install and trust it before the operating system will reliably open a
workspace; the app links to Vast's certificate setup guide. This remains part of
the clean-machine release canary.

The production release gate remains a clean-machine install that can launch a
real GPU workspace, open it, cancel it, and display a transaction-backed charge
and refund after wallet settlement is integrated. The current beta displays
USDC-denominated allowance usage evidence instead. Packaging alone does not
satisfy that gate.
