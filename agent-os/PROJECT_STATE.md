# Project State

_Last updated: 2026-05-07 — end of Sprint 37. Sprints 22–25 shipped MCP types + transport + capability gating + UI; Sprints 26–29 added six `live_` tests; Sprint 30 added the audit feed; Sprint 31 surfaced settlement receipts; Sprint 32 scaffolded A2A wire types; Sprints 33–35 added live agent dispatch + tier filter + §9 acceptance against real Ollama; Sprint 36 wired A2A task flow into the daemon; Sprint 37 closed the duplex with the result channel. Test-stats: `mock: 140 · live: 8 (5.4%)`. Phase rollups are operator-only._

## What this is
Covenant — an open agent-native operating layer. Sits above Linux/macOS, replaces the desktop/file/app metaphor with primitives for agentic work: intent, runtime, memory, identity, permissions, comms, compositor, settlement. Local-first; Solana for settlement; token `$covnt` (manual pump.fun launch by operator, not in build scope).

Authoritative spec: `00_spec.md`. 8 primitives, 6 phases, ~36 weeks.

## Goals (extracted from spec)
1. Ship Phase 0 in 4 weeks: daemon, intent router v0, first agent, settlement trait stub.
2. Hit the Phase 0 acceptance test (spec §9): NL intent → daemon → research agent → result, end-to-end < 5 s, no files outside `$COVENANT_HOME`, result in working memory tier only.
3. Build through Phase 5 (settlement on-chain + ecosystem) over 36 weeks.

## Stack (detected / planned)
| Layer | Tech | Status |
|---|---|---|
| Core daemon, runtime, memory | Rust workspace | 16 covenant-* crates + `agents/research` + `programs/settlement` (Anchor) + `covenant-web/` (Next.js 15) live. Daemon offers Unix socket and HTTP gateway (`127.0.0.1:8421`). On-disk artefacts under `$COVENANT_HOME/`: `memory.db`, `receipts/working.jsonl`, `identity/local.key` (0600), `audit/events.jsonl`, `capabilities/{granted,revoked}.jsonl`, `agents/<id>/`, `secrets.toml`, `sock`. |
| Agent isolation | subprocess → gVisor → Firecracker | Phase 1 / Phase 5 |
| Memory store | LanceDB + SQLite | Phase 1 |
| Protocols | MCP + A2A | Phase 3 |
| LLM | OpenAI-compatible + Ollama | Phase 0 (blocked on keys) |
| Web UI | Next.js + React | Phase 4 |
| TUI | Ratatui | Phase 4 |
| Compositor (optional) | Smithay/Wayland | Phase 4 |
| Settlement | Solana (Anchor) + Pyth oracle | Phase 5 |
| Build | cargo workspace | live |

## Completed
- Sprint 0 — discovery, state capture, workflow files. (2026-05-05)
- Sprint 1 — Cargo workspace, `covenant-types`, `covenant-manifest`, round-trip + validation tests. (2026-05-05)
- Sprint 2 — `covenant-ipc`, `covenantd` (lib+bin), `covenant` CLI; length-prefixed JSON IPC; in-process end-to-end + real-binary smoke test green. (2026-05-05)
- Sprint 3 — `covenant-router` (keyword-overlap matcher); daemon loads `$COVENANT_HOME/agents/*.toml` on startup and routes `SubmitIntent`; smoke test confirms routed vs. unmatched paths. (2026-05-05)
- Sprint 4 — `covenant-runtime` (subprocess `Runner`, `SubprocessRunner`, `MockRunner`, wall-clock timeout); `agents/research` Rust stub binary; daemon refactored to a `Server` holding `Arc<Router>` + `Arc<dyn Runner>`; Phase 0 acceptance loop runs end-to-end with a real subprocess. (2026-05-05)
- Sprint 5 — `covenant-memory` (`MemoryStore` trait, `InMemoryStore`, SQLite-backed `SqliteStore`); daemon writes working-tier records on every intent completion; `Request::RecentMemory` + `covenant memory recent` CLI subcommand; persists at `$COVENANT_HOME/memory.db`. Closes the last Phase 0 acceptance criterion. (2026-05-05)
- Sprint 6 — `covenant-settlement` (`Settlement` trait, `JsonlReceiptStore`, `InMemorySettlement`, `NoopSettlement`, `memory_write_credits()`); daemon records a paired `SettlementReceipt { resource: Memory }` for every memory write; `Request::RecentReceipts` + `covenant receipts recent` CLI subcommand; first burn surface live (off-chain). (2026-05-05)
- Sprint 7 — `covenant-llm` (Provider trait + Mock + Ollama + Anthropic + OpenAI/DeepSeek; `~/.covenant/secrets.toml` parser; `pick_provider` auto-fallback). HTTP via `reqwest` (rustls). Not yet wired into the research agent — Sprint 8. (2026-05-05)
- Sprint 8 — `covenant-tools` (SearchProvider + Mock + Brave + SerpAPI). `agents/research` rewired to be `#[tokio::main]` async, loading both providers from secrets and running a live search → summarise pipeline (canned-text fallback when no provider is configured). Phase 0 acceptance §9 unblocked at the code level. (2026-05-05)
- Sprint 9 — `covenant-identity` (ed25519 keypair, on-disk persistence at mode 0600, sign/verify helpers); daemon loads or creates `local.key` on startup; placeholder zero-pubkey replaced with the real ed25519 issuer; same key reusable for Solana settlement (Phase 5). (2026-05-05)
- Sprint 10 — `covenant-audit` (`AuditEvent`, `AuditKind::IntentDispatched`, async `AuditLog` trait, `JsonlAuditLog`, `InMemoryAuditLog`); daemon writes an audit entry for every dispatch with issuer pubkey, intent id, matched agent, and a hash of the result text. Persists at `$COVENANT_HOME/audit/events.jsonl`. (2026-05-05)
- Live LLM verified — `qwen2.5:7b` via Ollama returns coherent summaries end-to-end. Set `[llm] provider = "ollama"` + `model = "qwen2.5:7b"` in `~/.covenant/secrets.toml`. Web search still mock; Brave / SerpAPI keys are the only remaining external dependency for full Phase 0 acceptance. (2026-05-05)
- Sprint 11 — `covenant-permissions` (`SignedCapability`, `canonical_message`, `sign`/`verify`/`verify_with_clock`, `CapabilityStore` trait, `JsonlCapabilityStore`, `InMemoryCapabilityStore`); daemon opens `$COVENANT_HOME/capabilities/granted.jsonl` and exposes `Request::RecentCapabilities`; CLI `covenant capabilities recent`. **Read-only**: no enforcement at dispatch yet. (2026-05-05)
- Sprint 12 — capability enforcement (audit-only) + grant flow. Daemon now `Server` holds `Arc<LocalIdentity>`; every dispatch audit-logs a `CapabilityCheck { passed, required, missing }` event against the matched agent's required actions; `Request::GrantCapability` signs and persists; CLI `covenant capabilities grant <action>`. Revoke + hard rejection deferred to Sprint 13. (2026-05-05)
- Sprint 13 — capability revocation + hard enforcement. `Revocation` tombstones in `$COVENANT_HOME/capabilities/revoked.jsonl`; `granted ⊝ revoked` is the live set; dispatch rejects intents whose matched agent has missing actions (`Response::Error`); CLI `covenant capabilities revoke <signature-b58>`. **Phase 2 substantially complete.** (2026-05-05)
- Sprint 14 — memory vector search. New `Embedder` trait + `OllamaEmbedder` + `MockEmbedder` + secrets-config; `MemoryStore::search_similar` via cosine; daemon embeds on every memory write and on every search query; CLI `covenant memory search <query>`. Real-binary smoke: 3 stored intents retrieved by 3 semantically distinct queries with no keyword overlap. **Phase 1 substantively complete.** (2026-05-05)
- Sprint 15 — HTTP gateway on the daemon. New `covenantd::http` module (axum 0.7 router); 8 endpoints (`/health`, `/intent`, `/memory/{recent,search}`, `/receipts/recent`, `/capabilities/{recent,grant,revoke}`); binds `127.0.0.1:$COVENANT_HTTP_PORT` (default 8421) alongside the Unix socket. Curl smoke green. (2026-05-05)
- Sprint 16 — `covenant-web` Next.js 15 + React 19 UI (Iko Rane's first commit). Single page hits the HTTP gateway: intent submit + capability grant/revoke + semantic memory search + live recent-memory tail (3s polling). 10 files, all routed to Iko via the rotation. (2026-05-05)
- Sprint 17 — Solana settlement program scaffold (Noam Rook's first commit). `programs/settlement/` Anchor 0.31.1 program with `Config` PDA + 3 ix + 3 events; `Anchor.toml` at root; `anchor build` + `cargo clippy -D warnings` both green. (2026-05-05)
- Sprint 18 — HTTP gateway integration tests. `crates/covenantd/tests/http_gateway.rs` covers `/health`, capability-rejection, and the full grant → dispatch → memory/receipt/cap → semantic search → revoke → re-reject lifecycle. **90 tests** total. (2026-05-05)
- Sprint 19 — Working-tier memory GC. `MemoryStore::purge_older_than(tier, before_ms)` on the trait + in-memory + sqlite impls; `Request::PurgeMemory`; `POST /memory/purge`; CLI `covenant memory purge [--tier T] (--before-ms M | --older-than-ms D)`. Closes one of the spec §11 pins. **92 tests** total. (2026-05-05)
- Sprint 20 — `covenant verify` drift scan. New `Request::Verify { window }` + `Response::VerifyReport { checks, orphans_total }` (3 cross-checks: memory↔audit, capability↔audit, memory↔receipts); `GET /verify?window=N`; CLI `covenant verify [--window N]` (exit non-zero on drift). Closes another §11 pin. (2026-05-05)
- Sprint 21 — `.covenantignore` allow/deny list. New `covenant_memory::ignore` module (gitignore-style: `*` `**` `?` globs, `!` negation, `/` anchoring, last-rule-wins, custom no-dep matcher); daemon loads `$COVENANT_HOME/.covenantignore` (seeds default credentials list if missing); `dispatch_intent` short-circuits matching intents with a new `AuditKind::IntentIgnored` and skips the memory write + receipt entirely; `Request::IgnoreCheck`/`Response::IgnoreReport` IPC; CLI `covenant ignore check <text>`. Closes the third §11 pin. **106 tests** total. (2026-05-05)
- Sprint 22 — MCP adapter scaffolding. New `covenant-mcp` crate (`ToolSpec`, `Content`, `ToolCallResult`, `Tool` trait, `ToolRegistry`); MCP-shaped wire format (camelCase `inputSchema` / `isError`). Two native tools: `EchoTool` (validates required `text` arg) and `ClockTool` (no-arg, returns `{"epoch_ms": u64}`). `Server` gains `Arc<ToolRegistry>`; new `Request::ListTools`/`Request::CallTool` + `Response::ToolList`/`Response::ToolResult`; HTTP `GET /tools` + `POST /tools/call`; CLI `covenant tools list` / `covenant tools call <name> [--args <json>]`. **119 tests** total. External MCP server transport (stdio JSON-RPC 2.0) lands next. (2026-05-06)
- Sprint 23 — External MCP server transport. New `covenant_mcp::transport` (`McpClient` async trait, `StdioMcpClient` with subprocess + line-delimited JSON-RPC 2.0 + request-id correlation + `kill_on_drop(true)`, `MockMcpClient` for tests), `covenant_mcp::external` (`bootstrap_remote_tools` runs `initialize` + `notifications/initialized` + `tools/list`; `RemoteTool` impls `Tool` over JSON-RPC `tools/call`), `covenant_mcp::config` (`[[mcp.server]]` blocks in `secrets.toml`). Daemon main spawns each configured server, merges remote + native tools in one `ToolRegistry`, fail-soft on individual server errors. **131 tests** total. (2026-05-06)
- Sprint 24 — Tool capability gating. `CallTool` requires capability `tool.call.<name>`, audited via the same `CapabilityCheck` event as agent dispatch (`agent_id` set to `tool:<name>` for distinguishability). `ListTools` stays open. Refactored `audit_capability_check(card)` into a generic `check_capabilities(scope_id, required)` shared by both paths. **133 tests** total. (2026-05-06)
- Sprint 25 — Web UI tool surface (Iko Rane). `covenant-web/lib/api.ts` adds `listTools()` / `callTool(name, args)` + types (`ToolSpec`, `ContentBlock`, `ToolCallResponse`). `app/page.tsx` adds a "tools" section: polled `/tools` list, native `<select>` + JSON args textarea + call button, content rendered as `<pre>` blocks, inline "grant" button on capability denial. Bundle: 5.36 kB. (2026-05-06)
- Sprint 26 — First `live_` test. New `[[bin]] covenant-mcp-fake-server` (~80 line stdin→stdout JSON-RPC stand-in) + `crates/covenant-mcp/tests/live_stdio.rs::live_stdio_mcp_initialize_lists_and_calls` (`#[ignore]`'d). Run with `cargo test -p covenant-mcp -- --ignored live_`. Exercises real `tokio::process` spawn, line-delimited JSON-RPC framing, request-id correlation, `kill_on_drop`. Closes Sprint 23's untested-subprocess gap. **133 mock tests + 1 live test**. (2026-05-06)
- Sprint 27 — Live Ollama coverage. New `crates/covenant-llm/tests/live_ollama.rs` with three `#[ignore]`'d tests: `live_ollama_embeds_real_text`, `live_ollama_semantic_similarity_holds`, `live_ollama_chat_completes`. Real network calls against `nomic-embed-text` + `qwen2.5:7b` on local Ollama. Run with `cargo test -p covenant-llm -- --ignored live_`. **133 mock + 4 live**, ratio `3.0%`. (2026-05-06)
- Sprint 28 — Live research-agent subprocess test. New `agents/research/tests/live_subprocess.rs::live_research_agent_returns_result_via_stdio` (`#[ignore]`'d). Spawns the `research` binary, pipes a JSON `Intent` to stdin (close-on-EOF), reads `AgentResult` from stdout. Hermetic via tempdir-isolated `COVENANT_HOME` + `HOME`. **133 mock + 5 live**, ratio `3.7%`. (2026-05-06)
- Sprint 29 — Live covenantd full-loop test. New `crates/covenantd/tests/live_daemon.rs::live_covenantd_ping_intent_echo_loop` (`#[ignore]`'d). Spawns the real binary, picks a free TCP port for the HTTP gateway, polls for the Unix socket to appear, drives `Ping → Pong` and `SubmitIntent → echo fallback` over real IPC. Highest-value live test on the board — exercises the binary the operator actually runs. **133 mock + 6 live**, ratio `4.4%`. (2026-05-06)
- Sprint 30 — Audit feed end-to-end (Achille + Iko split). `Request::RecentAudit` + `Response::AuditEvents` + `GET /audit/recent` + Web UI "audit feed" section polling on the existing 3 s timer with per-variant rendering. Particularly useful for surfacing the `tool:<name>` capability-check rows from Sprint 24. **134 mock + 6 live**, ratio still `4.4%`. (2026-05-06)
- Sprint 31 — Web UI settlement receipts feed (Iko Rane). New `recentReceipts()` API helper + "settlement receipts" section in `page.tsx` showing `[time] resource credits · onchain_sig | (local-only)`. Closes the visibility gap for the receipt stream that Sprint 6 started writing. Web bundle: 5.79 kB. (2026-05-06)
- Sprint 32 — covenant-a2a scaffolding. New crate with `A2ATask` / `A2ATaskResult` / `A2ATaskStatus` wire types + async `Mailbox` trait + `InMemoryMailbox` (Mutex+VecDeque+Notify-backed). No daemon wiring this sprint — pure types + in-memory impl. **141 mock + 6 live**, ratio `4.2%`. (2026-05-06)
- Sprint 33 — Live agent-dispatch test. New `crates/covenantd/tests/live_agent_dispatch.rs::live_covenantd_dispatches_to_research_agent` (`#[ignore]`'d). Spawns covenantd with a tempdir HOME, registers a research agent manifest pointing at `target/debug/research`, grants the cap, dispatches a matching intent, asserts real agent output (not echo) + memory + receipt land. Closes the last big live-coverage gap. **141 mock + 7 live**, ratio `4.9%`. (2026-05-06)
- Sprint 34 — Web UI memory tier filter (Iko Rane). `recentMemory()` API helper takes an optional `tier`, page.tsx adds an `all tiers / working / episodic / longterm` select above the recent-memory list. (2026-05-06)
- Sprint 35 — Phase 0 §9 acceptance live test (real Ollama). New `crates/covenantd/tests/live_full_acceptance.rs::live_covenantd_full_acceptance_with_ollama` (`#[ignore]`'d). Configures `secrets.toml` in tempdir HOME with `[llm] provider="ollama", model="qwen2.5:7b"`; runs the full intent loop end-to-end against real model inference; asserts the response is none of the canned-fallback paths. 5.03s runtime. **141 mock + 8 live**, ratio `5.5%`. (2026-05-06)
- Sprint 36 — A2A daemon wiring (task flow). `Mailbox` trait gains `try_recv_task`/`try_recv_result`; `Server` holds `Arc<dyn Mailbox>`; `Request::SendA2ATask`+`TryRecvA2ATask`; `POST /a2a/tasks` + `GET /a2a/tasks/next`. Result channel + capability gating + live test deferred to follow-up sprints. **143 mock + 8 live**. (2026-05-07)
- Sprint 37 — A2A result-channel wiring. `Request` drops `Eq` (so `A2ATaskResult` can ride it); new `Request::PostA2AResult`/`TryRecvA2AResult` + `Response::A2AResultPosted`/`A2AResultOpt`; daemon delegates to `Mailbox::send_result`/`try_recv_result`; `POST /a2a/results` + `GET /a2a/results/next`. Closes the duplex Sprint 36 left half-open. **144 mock + 8 live**. (2026-05-07)

## Current sprint
None active. Open tracks queued; pick from below.

## Next sprint candidates (operator chooses)
- **Sprint 19a — MCP / A2A adapter scaffolding** (Phase 3, Achille). Substantial spec interpretation; defines the wire types and the `Tool` trait for the agent runtime.
- **Sprint 19b — Solana SPL CPIs + Pyth oracle wiring** (Phase 5, Noam). Replaces the v0 event stubs with real token burns/mints; Pyth price account; DEX router selection. Devnet deploy is operator action.
- **Sprint 19c — Web UI polish** (Phase 4, Iko). Live audit feed, intent stream, settlement dashboard. Tailwind CSS adoption.
- **Sprint 19d — Polish bundle** (Achille). Working-tier memory GC + `covenant verify` drift scan; addresses two `00_spec.md` §11 pins.

## Next sprint candidates (pick one to resume)
- **Memory vector search** (Phase 1+ polish): pull `nomic-embed-text` via Ollama, wire embeddings into `MemoryRecord.embedding`, add cosine search to `MemoryStore`. Single-sprint scope.
- **MCP adapter scaffolding** (Phase 3): define MCP wire types + daemon-side tool registry; runtime grows a `Tool` trait. Single-sprint scope but partially specs-bound.
- **A2A adapter scaffolding** (Phase 3): agent-to-agent task envelopes + an orchestrator agent stub.
- **First TUI screen** (Phase 4 — routed to Iko Rane): Ratatui dashboard with intent stream + memory tail + audit tail.
- **Settlement program scaffold** (Phase 5 — routed to Noam Rook): `programs/settlement/` Anchor crate with credit-mint + buyback PDAs; deploy to devnet.

## Backlog (rough order)
- Sprint 3 — intent router v0 (regex + cosine-stub) + first registered agent (research stub). Phase 0 acceptance test passes against mocks.
- Sprint 4 — agent runtime (subprocess) + agent registry on disk + settlement trait + no-op impl with disk receipts.
- Sprint 5 — live LLM + web search wiring (unblocks the spec §9 acceptance test once API keys land).
- Sprint 6+ — Phase 1 work begins (memory layer, sandbox, first burn surface).

## Known risks
| Risk | Severity | Mitigation |
|---|---|---|
| LLM + web-search API keys not yet provided | Medium | All upstream work proceeds with mocks; see BLOCKERS.md. Live verification deferred to Sprint 5. |
| Subprocess agent runtime in Phase 0 has zero isolation | Accepted by spec | gVisor lands Phase 1; Firecracker Phase 5. |
| Pump.fun graduation timing affects on-chain settlement testing | Low (Phase 5) | Operator launches token off the build path; Sprint 6+ unaffected. |
| Spec phase-map shifted from source plan (settlement seam, Phase 2 split) | Tracked | `00_spec.md` §3 supersedes the source plan. |

## Test status
| Crate | Tests | Status |
|---|---|---|
| `covenant-types` | 4 | passing |
| `covenant-manifest` | 7 | passing |
| `covenant-router` | 9 | passing |
| `covenant-runtime` | 4 | passing |
| `covenant-memory` | 22 | passing |
| `covenant-settlement` | 5 | passing |
| `covenant-llm` | 9 | passing |
| `covenant-tools` | 7 | passing |
| `covenant-identity` | 5 | passing |
| `covenant-audit` | 5 | passing |
| `covenant-permissions` | 9 | passing |
| `covenant-mcp` (mock) | 21 | passing |
| `covenant-mcp` (live, `#[ignore]`d) | 1 | passing — `cargo test -p covenant-mcp -- --ignored live_` |
| `covenant-a2a` | 8 | passing |
| `covenant-llm` (live, `#[ignore]`d) | 3 | passing — `cargo test -p covenant-llm -- --ignored live_` |
| `research-agent` (mock) | 2 | passing |
| `research-agent` (live, `#[ignore]`d) | 1 | passing — `cargo test -p research-agent -- --ignored live_` |
| `covenant-ipc` | 5 | passing |
| `covenantd` (lib unit) | 17 | passing |
| `covenantd` (end-to-end) | 1 | passing |
| `covenantd` (http-gateway) | 4 | passing |
| `covenantd` (live, `#[ignore]`d) | 3 | passing — `cargo test -p covenantd --tests -- --ignored live_` |

Total 144 mock + 8 live (`#[ignore]`'d). `scripts/test-stats.sh` → `mock: 140 · live: 8 (5.4%)`. `cargo test --workspace --exclude covenant-settlement-program` and `cargo clippy --workspace --all-targets -- -D warnings` both green at end of Sprint 37. (Anchor program is excluded from cargo-test because Anchor builds it via `cargo-build-sbf` for the BPF target; `anchor build` is the validation step there.)

## Production readiness
See PRODUCTION_READINESS.md. Build/test moved 🟥 → 🟨 in Sprint 1. Other columns red.
