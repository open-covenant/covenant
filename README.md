# Covenant

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

Open agent-native operating layer. Sits above Linux/macOS, replaces the
desktop/file/app metaphor with primitives for agentic work — the
coordination layer that lets humans and agents safely share a computer,
delegate work, remember context, enforce permissions, and settle usage.

Local-first. Solana for settlement. Token: `$covnt` (manual pump.fun launch
by the operator; not in the build path).

**Status:** pre-alpha, Phase 0 substantively complete; Phase 1+ in progress.

- Site: [opencovenant.org](https://opencovenant.org)
- X: [x.com/OpenCovenant](https://x.com/OpenCovenant)

## Repo layout

Two parallel tracks live in this repo:

- **[`agent-os/`](./agent-os)** — the active build. Rust workspace
  (16 crates), agent binaries, Solana settlement program (Anchor),
  operator web UI. See
  [`agent-os/00_spec.md`](./agent-os/00_spec.md) for the build spec and
  [`agent-os/PROJECT_STATE.md`](./agent-os/PROJECT_STATE.md) for the live
  snapshot.
- **[`landing/`](./landing)** — public teaser site (Next.js 15 +
  Tailwind 4), deployed via Render.
- **`apps/`, `contracts/`, `packages/`, `services/`, `circuits/`,
  `infra/`** — earlier OSS baseline targeting Base/EVM (x402 payments,
  task markets, proof verification, EVM settlement). Preserved alongside
  the active build for potential reuse; not on the active path.

## Spec — 8 primitives

Per [`agent-os/00_spec.md`](./agent-os/00_spec.md):

| Primitive | Role |
|---|---|
| Intent | Natural-language requests routed to agents |
| Runtime | Agent isolation: subprocess → gVisor → Firecracker |
| Memory | Three-tier (working, episodic, long-term) with vector search |
| Identity | ed25519 local key, reusable for Solana settlement |
| Permissions | Signed capability tokens, hard enforcement at dispatch |
| Comms | Unix socket + HTTP gateway; MCP + A2A adapters |
| Compositor | TUI (Ratatui), web UI (Next.js), optional Wayland |
| Settlement | Credits + buyback, off-chain receipts, on-chain via Solana |

## Phase status

Per [`agent-os/PROJECT_STATE.md`](./agent-os/PROJECT_STATE.md):

- **Phase 0** — substantively complete. Daemon, intent router, agent
  runtime, settlement trait stub, memory, identity, audit, capability
  enforcement, ignore list. End-to-end §9 acceptance test passes against
  real Ollama.
- **Phase 1** — substantively complete. Memory tiers, vector search via
  Ollama embeddings, working-tier GC, `.covenantignore` allow/deny list.
- **Phase 2** — substantively complete. Capability tokens
  (signed, granted, revoked); hard enforcement at dispatch; tool-call
  capability gating.
- **Phase 3** — in progress. MCP adapter (types, transport, capability
  gating, native + remote tools, web UI surface); A2A adapter (task +
  result channels wired through daemon; capability gating + per-recipient
  routing pending).
- **Phase 4** — partial. Operator web UI scaffolded
  (`agent-os/covenant-web/`); landing teaser at `landing/`. TUI not
  started.
- **Phase 5** — scaffolded. Anchor program at
  `agent-os/programs/settlement/`; SPL CPIs + Pyth oracle wiring pending.

> Per [`agent-os/WORKFLOW.md`](./agent-os/WORKFLOW.md), only the human
> operator promotes a phase from open → substantively complete. The
> autonomous loop ships sprints; the operator audits the rollups.

## Build (active build)

    cd agent-os
    cargo check  --workspace --exclude covenant-settlement-program
    cargo test   --workspace --exclude covenant-settlement-program
    cargo clippy --workspace --all-targets -- -D warnings

The Solana settlement program is excluded from `cargo test` because
Anchor builds it via `cargo-build-sbf` for the BPF target. Run `anchor
build` from `agent-os/` to validate it.

### Test stats

    144 mock tests + 8 live tests (5.4% live coverage)

Tests prefixed `live_` exercise real backends (real Ollama, real spawned
subprocesses, the real daemon binary). They are `#[ignore]`'d to keep
CI fast — run with `cargo test -- --ignored live_`.

## Live local environment

- Daemon binary: `agent-os/target/debug/covenantd`. Unix socket at
  `$COVENANT_HOME/sock` (default `~/.covenant/sock`); HTTP gateway at
  `127.0.0.1:8421`.
- LLM via Ollama + `qwen2.5:7b` (configured in
  `~/.covenant/secrets.toml`).
- Embeddings via `nomic-embed-text` (same secrets file).
- ed25519 local identity at `$COVENANT_HOME/identity/local.key` (mode
  0600).
- Operator web UI: `cd agent-os/covenant-web && pnpm install && pnpm dev`
  → http://localhost:3000.
- Landing teaser: `cd landing && pnpm install && pnpm dev` →
  http://localhost:3001.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).
