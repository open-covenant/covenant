# HANDOVER

_Last refreshed: 2026-05-07 — Sprints 22–37 landed; long autonomous run continuing._

The previous autonomous session paused itself to start fresh. This file is the canonical entry point for the next session.

## What just happened

Sprint 22 — New `covenant-mcp` crate with MCP-aligned `Tool` trait + registry; daemon dispatches `tools/list` and `tools/call` through IPC + HTTP + CLI; two native tools (`echo`, `clock`) ship.

Sprint 23 — External MCP server transport. `covenant_mcp::{transport, external, config}` add JSON-RPC 2.0 over stdio (subprocess + `kill_on_drop(true)`, request-id correlation, in-process `MockMcpClient` for tests), `bootstrap_remote_tools` running the `initialize` → `notifications/initialized` → `tools/list` handshake, and `[[mcp.server]]` blocks in `~/.covenant/secrets.toml`. Daemon main merges native + remote tools into one registry, fail-soft on per-server errors.

Sprint 24 — Tool capability gating. `CallTool` now requires `tool.call.<name>`; reused the agent dispatch's `CapabilityCheck` audit event (with `agent_id="tool:<name>"`). Refactored the audit helper to take a generic (scope_id, required) tuple. Closes the security gap left by Sprint 22.

Sprint 25 — Web UI tool surface (Iko Rane). New "tools" section in the Next.js app: polled `/tools` list, dropdown + JSON textarea call form, inline "grant tool.call.\*" on capability denial. `pnpm typecheck` + `pnpm build` clean.

Sprint 26 — First `live_` test. `covenant-mcp-fake-server` bin + `live_stdio_mcp_initialize_lists_and_calls` (`#[ignore]`'d) drives `StdioMcpClient` against a real subprocess end-to-end. Closes Sprint 23's untested-subprocess gap. Run with `cargo test -p covenant-mcp -- --ignored live_`.

Sprint 27 — Live Ollama coverage. Three more `#[ignore]`'d tests in `crates/covenant-llm/tests/live_ollama.rs` exercising `OllamaEmbedder::embed` (1k-dim real vectors) and `OllamaProvider::complete` (real `qwen2.5:7b` inference). Live ratio now 3.0%. Run with `cargo test -p covenant-llm -- --ignored live_`.

Sprint 28 — Live research-agent subprocess test. `agents/research/tests/live_subprocess.rs::live_research_agent_returns_result_via_stdio` (`#[ignore]`'d) spawns the `research` binary with tempdir-isolated `COVENANT_HOME`+`HOME`, pipes a JSON Intent to stdin, reads the `AgentResult` from stdout. Run with `cargo test -p research-agent -- --ignored live_`.

Sprint 29 — Live covenantd full-loop test. `crates/covenantd/tests/live_daemon.rs::live_covenantd_ping_intent_echo_loop` (`#[ignore]`'d) spawns the actual `covenantd` binary against a tempdir HOME with an ephemeral HTTP port, polls for the socket, drives `Ping → Pong` and `SubmitIntent → echo`. Run with `cargo test -p covenantd --test live_daemon -- --ignored live_`.

Sprint 30 — Audit feed end-to-end (split: Achille / Iko). `Request::RecentAudit` + `Response::AuditEvents` + `GET /audit/recent`; web UI gains an "audit feed" section that polls on the existing 3 s timer and renders per-variant (dispatch / capability_check / capability_granted / intent_ignored).

Sprint 31 — Web UI settlement receipts feed (Iko). The receipt stream from Sprint 6 finally has a UI surface. The web app now reflects every primitive the daemon ships.

Sprint 32 — covenant-a2a scaffolding. New crate with `A2ATask` / `A2ATaskResult` / `A2ATaskStatus` + async `Mailbox` trait + `InMemoryMailbox`. No daemon wiring this sprint — mirrors Sprint 22:23 ratio (types first, transport in a follow-up).

Sprint 33 — Live agent-dispatch test. `crates/covenantd/tests/live_agent_dispatch.rs::live_covenantd_dispatches_to_research_agent` (`#[ignore]`'d) spawns the real daemon, registers a research agent manifest pointing at `target/debug/research`, grants the cap, dispatches, asserts real agent output + memory + receipt land. Most thorough live test in the repo.

Sprint 34 — Web UI memory tier filter (Iko). Small polish: select above the recent-memory list filters by working / episodic / longterm.

Sprint 35 — Phase 0 §9 acceptance live test. `crates/covenantd/tests/live_full_acceptance.rs::live_covenantd_full_acceptance_with_ollama` (`#[ignore]`'d). Configures the tempdir's `secrets.toml` for real Ollama + `qwen2.5:7b`, runs the full intent loop, asserts the response isn't any of the canned-fallback paths. 5.03s runtime against real model inference.

Sprint 36 — A2A daemon wiring (task flow). `Mailbox` trait gets `try_recv_*` non-blocking variants; `Server` holds `Arc<dyn Mailbox>`; new IPC + HTTP for `SendA2ATask` and `TryRecvA2ATask`. Result channel + capability gating + live test scoped out to follow-ups.

Sprint 37 — A2A result-channel wiring. Closes the duplex: `Request::PostA2AResult` / `TryRecvA2AResult` + `Response::A2AResultPosted` / `A2AResultOpt`; `POST /a2a/results` + `GET /a2a/results/next`. `Request` drops `Eq` to symmetric-match `Response` (since `A2ATaskResult` carries a `serde_json::Value`).

Totals across all sixteen: **16 crates, 144 mock tests + 8 live tests**, clippy + fmt + pnpm build clean. `scripts/test-stats.sh` reports `mock: 140 · live: 8 (5.4%)`. Per AGENTS.md framework rule (commit 0bb3f80), phase rollups remain operator-only.

**Note for next session:** the `~/.gitconfig` `includeIf` for this repo points at lowercase `~/projects/covenant/`. Some shell invocations land you in uppercase `~/Projects/covenant/` and `git rcommit` then errors with "not a git command". Workaround: `cd ~/projects/covenant` (lowercase) before running rcommit.

Earlier sessions: 30+ commits across all three identities. Phase 0/1/2 substantively complete. Phase 4 v0 web UI scaffolded (Iko Rane). Phase 5 Solana program scaffolded (Noam Rook). Three `00_spec.md` §11 pins closed: Sprint 19 working-tier GC, Sprint 20 `covenant verify` drift scan, Sprint 21 `.covenantignore`. One §11 pin remains: per-resource budget mid-task graceful save.

## Read order (do this first)

1. `00_spec.md` — re-anchor on the product (8 primitives, 6 phases, locked decisions).
2. `AGENTS.md` — workflow loop, identity rotation, validation rules, **and the handover protocol** at the bottom.
3. `PROJECT_STATE.md` — current snapshot + queued sprint candidates.
4. **The tail of `SPRINT_LOG.md`** — the latest `Resume from here` block tells you exactly what to pick up.
5. `BLOCKERS.md` — anything new the operator must do.

## How to verify the build is still green

    cargo build --workspace --exclude covenant-settlement-program
    cargo test  --workspace --exclude covenant-settlement-program
    cargo clippy --workspace --all-targets -- -D warnings
    anchor build              # only when changing the Solana program

The `--exclude covenant-settlement-program` is intentional: Anchor builds it via `cargo-build-sbf` for the BPF target; `anchor build` is the validation step there.

## Live local environment

- Daemon binary: `target/debug/covenantd`. Unix socket at `$COVENANT_HOME/sock` (default `~/.covenant/sock`); HTTP gateway at `127.0.0.1:8421`.
- Live LLM via Ollama + `qwen2.5:7b` (configured in `~/.covenant/secrets.toml`).
- Live embeddings via `nomic-embed-text` (same secrets file).
- ed25519 local identity at `$COVENANT_HOME/identity/local.key` (mode 0600).
- Web UI: `cd covenant-web && pnpm install && pnpm dev` → http://localhost:3000.

## Continuation rules

- **Don't stop unless a true blocker appears.** Use the `BLOCKERS.md` format.
- **After a substantial commit, refresh this file's "What just happened" section.** A one-line update is enough.
- **Use `git rcommit` for every commit.** The rotation routes by file path: web → Iko, Solana → Noam, default → Achille.
- **When this session gets long** (heuristic: ≈25+ commits, or you sense the model spreading thin):
  1. Refresh this `HANDOVER.md`.
  2. Run `scripts/handover.sh`.
  3. The next clean session will read `HANDOVER.md` and continue.

## Quick links

- Spec: `00_spec.md`
- Workflow: `AGENTS.md`
- Live state: `PROJECT_STATE.md`
- Sprint history (read tail for resume): `SPRINT_LOG.md`
- Operator-only blockers: `BLOCKERS.md`
- Production-readiness scorecard: `PRODUCTION_READINESS.md`
