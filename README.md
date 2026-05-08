# Covenant

[![CI](https://github.com/open-covenant/covenant/actions/workflows/ci.yml/badge.svg)](https://github.com/open-covenant/covenant/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

> Open source, agent-native operating layer.

Covenant is the coordination layer for agentic software. It runs on your own machine, speaks to local and remote AI agents, and provides the OS-level primitives — intent, runtime, memory, identity, permissions, comms, compositor, settlement — that humans and agents need to safely share a computer, delegate work, and pay for usage.

- **Web** — [opencovenant.org](https://opencovenant.org)
- **Docs** — [opencovenant.org/docs](https://opencovenant.org/docs)
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

Concepts, architecture, reference, protocols, and operations docs are published at [opencovenant.org/docs](https://opencovenant.org/docs).

- [Getting started](https://opencovenant.org/docs/getting-started) — install, run the daemon, submit your first intent.
- [Concepts](https://opencovenant.org/docs/concepts) — the eight-primitive vocabulary.
- [System architecture](https://opencovenant.org/docs/architecture) — components, request lifecycle, on-disk state.
- [HTTP API](https://opencovenant.org/docs/http-api) — every gateway route.
- [Capability tokens](https://opencovenant.org/docs/capabilities) — the permission model.
- [Security model](https://opencovenant.org/docs/security) — trust boundaries and threat model.

## Contributing

Pull requests are welcome. Before submitting, please read [`CONTRIBUTING.md`](./CONTRIBUTING.md) and the [Code of Conduct](./CODE_OF_CONDUCT.md).

## Security

Please follow [`SECURITY.md`](./SECURITY.md) for responsible disclosure. Do not open a public issue for vulnerability reports.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).
