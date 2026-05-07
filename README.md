# Covenant

[![CI](https://github.com/open-covenant/covenant/actions/workflows/ci.yml/badge.svg)](https://github.com/open-covenant/covenant/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

> An open, agent-native operating layer. Local-first.

Covenant is the coordination layer for agentic software. It runs on your own machine, speaks to local and remote AI agents, and provides the OS-level primitives — intent, runtime, memory, identity, permissions, comms, compositor, settlement — that humans and agents need to safely share a computer, delegate work, and pay for usage.

- **Web** — [opencovenant.org](https://opencovenant.org)
- **X** — [@OpenCovenant](https://x.com/OpenCovenant)

## Why

Agentic systems are spreading faster than the OS-level primitives they need. Today every agent stack reinvents permissions, memory, identity, billing, and audit on top of plain processes and cloud APIs — badly, and incompatibly with the one next to it. Covenant is the missing coordination layer, designed for the desktop first.

## What's inside

- Local-first daemon, Unix socket and HTTP gateway
- ed25519 identity and signed capability tokens
- Three-tier memory with semantic search
- MCP and A2A protocol adapters
- Pluggable LLM providers
- Settlement on Solana

## Status

Pre-alpha. The protocol surfaces and the daemon are under active development; the on-chain settlement layer is evolving in lock-step.

We do not recommend production use yet. We welcome design feedback, sandbox experimentation, and contributions.

## Documentation

Protocol spec, architecture, and integration guides will be published at [docs.opencovenant.org](https://docs.opencovenant.org) as they stabilize.

## Contributing

Pull requests are welcome. Before submitting, please read [`CONTRIBUTING.md`](./CONTRIBUTING.md) and the [Code of Conduct](./CODE_OF_CONDUCT.md).

## Security

Please follow [`SECURITY.md`](./SECURITY.md) for responsible disclosure. Do not open a public issue for vulnerability reports.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).
