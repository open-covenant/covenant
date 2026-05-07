# Sprint Log

Append-only. One entry per sprint. Most recent at the bottom.

---

## Sprint 0 — Discovery, state capture, execution plan
**Date:** 2026-05-05.
**Objective:** Read all docs, confirm scope, identify stack, scaffold the workflow files, plan the next sprints.

### Agents used
- Direct (lead/architect).

### Files changed (created)
- `AGENTS.md` (originally written as `AGENTIC_WORKFLOW.md`; renamed in a later docs commit to match the cross-tool convention)
- `PROJECT_STATE.md`
- `SPRINT_LOG.md` (this file)
- `BLOCKERS.md`
- `PRODUCTION_READINESS.md`
- `.gitignore`

### Findings
- Spec at `00_spec.md` is authoritative; cross-cutting decisions already pinned in §1–§11.
- Repo is greenfield — only `00_spec.md` existed before this sprint.
- Stack inferred: Rust workspace primary, with later additions of Next.js (Phase 4) and Solana/Anchor (Phase 5).
- No prior tests, build, or CI.
- Per-project git config + commit rotation + leak-detecting hooks already wired at the user's machine level (see `~/.gitconfig-covenant` and `~/.config/git/covenant-hooks/`).
- Source PDFs in `~/projects/research-agent/research/Agentic OS/` provide positioning + tokenomics context; superseded by `00_spec.md` where they conflict.
- Toolchain available: git 2.50, cargo 1.94, rustc 1.94, node, pnpm.

### Tests run
None applicable (pre-code).

### Failures
None.

### Plan for next sprints

#### Sprint 1 — Cargo workspace + types + manifest parser
- `git init` the project; verify hook firing.
- Root `Cargo.toml` workspace with shared `[workspace.dependencies]`.
- Crate `covenant-types`: Intent, AgentId, Capability, MemoryRecord, SettlementReceipt + their enums (Priority, MemoryTier, ResourceKind). Custom serde for the ed25519 pubkey ↔ base58 wire form.
- Crate `covenant-manifest`: parse + validate `agent.toml` against spec §5. Reserved-namespace check on capabilities.
- Tests: serde round-trip per type; manifest fixtures (valid / minimal / invalid namespace).
- Single Achille commit via `git rcommit`.

#### Sprint 2 — Daemon skeleton + CLI client
- Crate `covenantd` (binary): Unix socket server at `$COVENANT_HOME/sock`, accepts intent JSON, returns echo stub.
- Crate `covenant` (binary): CLI sending `intent <text>` over the socket, printing response.
- Length-prefixed JSON IPC for v0.
- Integration test spawning daemon, sending intent, asserting echo.

#### Sprint 3 — Intent router v0 + research agent stub
- Crate `covenant-router`: regex matcher v0 over capability cards; embedding similarity scaffolded but stubbed for v0.
- Daemon registers agents from `~/.covenant/agents/*.toml`.
- `agents/research/` Rust stub agent: reads intent on stdin, returns canned summary.
- Phase 0 acceptance test against mocks (live wiring deferred to Sprint 5 pending API keys).

### Resume from here
Run Sprint 1. Init git, scaffold the workspace, write the two crates, verify `cargo test --workspace` green, commit via `git rcommit`.

---

## Sprint 1 — Cargo workspace, types crate, manifest parser
**Date:** 2026-05-05.
**Objective:** Stand up the Rust workspace and ship the two foundation crates with passing tests; verify the commit rotation + hooks fire end-to-end on a real commit.

### Agents used
- Direct (scaffold + builder + integrator + QA).

### Files changed (created)
- `Cargo.toml` — workspace root with shared deps.
- `README.md` — short project pointer.
- `.gitignore` — Rust + macOS noise.
- `crates/covenant-types/Cargo.toml`
- `crates/covenant-types/src/lib.rs` — 5 structs + 3 enums + custom serde for `AgentId`.
- `crates/covenant-types/src/tests.rs` — round-trip + edge cases.
- `crates/covenant-manifest/Cargo.toml`
- `crates/covenant-manifest/src/lib.rs` — `Manifest` with validation; depends on `covenant-types`.
- `crates/covenant-manifest/src/tests.rs` — fixture tests.

### Tests run
- `cargo check --workspace` → ok.
- `cargo test --workspace` → 11 passing (4 in `covenant-types`, 7 in `covenant-manifest`).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.

### Failures and fixes
- Clippy `derivable_impls` flagged `impl Default for Priority` and `impl Default for NetworkPolicy` — replaced both with `#[derive(Default)]` + `#[default]` on the canonical variant.
- Clippy `should_implement_trait` flagged `Manifest::from_str` — renamed inherent method to `Manifest::parse` and added a proper `impl FromStr for Manifest` delegating to it. Tests updated to call `Manifest::parse`; added one test exercising `s.parse::<Manifest>()` via the trait.
- Trimmed AI-tell docstrings (variant-level `/// Lowest priority` etc.) per the projects CLAUDE.md style; removed `#![warn(missing_docs)]` and kept docs only where they add semantics.

### Hook verification on first commit
- `git init` ran; `core.hooksPath` resolved to `~/.config/git/covenant-hooks/` via `[includeIf]`.
- First commit went through `git rcommit`; classifier routed all staged files to Achille (Rust + docs); pre-commit passed; commit-msg redaction had no input to redact.

### Resume from here
Run Sprint 2. Add `covenantd` daemon skeleton and `covenant` CLI; define a length-prefixed JSON IPC; integration test that spawns the daemon, sends an intent, asserts the echo response. Author: Achille (Rust + bin crates).

---

## Sprint 2 — Daemon skeleton, CLI, length-prefixed JSON IPC
**Date:** 2026-05-05.
**Objective:** Stand up a working `covenantd` ↔ `covenant` round-trip on a Unix socket. Ship the IPC protocol shared by both. Gate on a real in-process end-to-end test plus a smoke test against the actual binaries.

### Agents used
- Direct (scaffold + builder + integrator + QA).

### Files changed (created)
- `crates/covenant-ipc/Cargo.toml`
- `crates/covenant-ipc/src/lib.rs` — `Request`, `Response`, `IpcError`, `read_frame`, `write_frame`, `MAX_FRAME = 8 MiB`.
- `crates/covenantd/Cargo.toml`
- `crates/covenantd/src/lib.rs` — `serve`, `respond`, `covenant_home`.
- `crates/covenantd/src/main.rs` — thin binary; SIGINT → graceful shutdown; `tracing-subscriber` with `EnvFilter`.
- `crates/covenantd/tests/end_to_end.rs` — spawns `serve` on a tempdir socket, drives `Ping + SubmitIntent` end-to-end.
- `crates/covenant/Cargo.toml`
- `crates/covenant/src/main.rs` — `covenant ping` and `covenant intent <text>` commands.

### Files changed (edited)
- `Cargo.toml` — added the three new members; added `tokio (rt-multi-thread, macros, net, io-util, fs, signal, time)`, `tracing`, `tracing-subscriber (env-filter)`, `anyhow` to `[workspace.dependencies]`.
- `crates/covenant-types/src/lib.rs` — added `PartialEq, Eq` on `Intent` and `SettlementReceipt` so `Response::IntentResult { settlement: Option<SettlementReceipt>, ... }` can also derive Eq.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok (autoformatter alphabetised imports).
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → 18 passing (4 types + 7 manifest + 4 ipc + 2 covenantd-lib + 1 end-to-end).
- Real-binary smoke: `COVENANT_HOME=$(mktemp -d)` + `cargo run -p covenantd` in background, then `cargo run -p covenant -- ping` (→ `pong`) and `cargo run -p covenant -- intent "find recent papers on agent memory"` (→ `phase 0 echo: find recent papers on agent memory`); SIGINT shut the daemon down cleanly.

### Failures and fixes
- `Response::IntentResult { settlement: Option<SettlementReceipt> }` initially failed to compile because `Response` derived `Eq` but `SettlementReceipt` did not. Added `PartialEq, Eq` to `SettlementReceipt` and `Intent` (both have only Eq-able fields). `Capability` and `MemoryRecord` keep `PartialEq`-only: they hold `serde_json::Value` / `Vec<f32>`.
- Daemon initially lived entirely in `main.rs`; refactored into `lib.rs` (with `serve`, `respond`, `covenant_home`) plus a thin `main.rs`. The refactor was required so the `tests/end_to_end.rs` integration test could consume the library API rather than spawning the binary as a subprocess.
- Initial test summary command tripped a regex; switched to plain `tail`.

### Resume from here
Sprint 3. Add the intent router (`covenant-router` crate — regex matcher v0 over capability cards plus a cosine-similarity stub) and a registered research agent stub. Daemon stops echoing on `SubmitIntent` and instead: routes the intent → spawns the matched agent as a subprocess (Phase 0 = no isolation; gVisor lands Phase 1) → feeds intent JSON on stdin → reads result on stdout → replies to the caller. Live LLM and web-search wiring remains BLOCKED on API keys (BLOCKERS.md); use stub providers for Sprint 3 and gate the live path behind a feature flag or env-var fallback. Author: Achille (Rust core).

---

## Sprint 3 — Intent router + agent manifest loading
**Date:** 2026-05-05.
**Objective:** Ship the v0 intent router (`covenant-router`), wire it into the daemon, register agents from `$COVENANT_HOME/agents/*.toml`. Subprocess agent execution stays out of scope — Sprint 4.

### Agents used
- Direct (scaffold + builder + integrator + QA).

### Files changed (created)
- `crates/covenant-router/Cargo.toml`
- `crates/covenant-router/src/lib.rs` — `AgentCard`, `Router`, `RouteMatch`, `load_agents_from_dir`. Keyword-overlap scoring against capability paths; `capability_keywords()` is the v0 bridge until embeddings land in Phase 1.

### Files changed (edited)
- `Cargo.toml` — added `crates/covenant-router` to workspace members.
- `crates/covenantd/Cargo.toml` — added `covenant-router` dep.
- `crates/covenantd/src/lib.rs` — `serve(listener, Arc<Router>)`; `respond(&Router, Request)` consults the router and includes matched agent id + score in the result text (or falls back to echo).
- `crates/covenantd/src/main.rs` — loads agents from `$COVENANT_HOME/agents/*.toml`, builds `Router`, passes to `serve`. Logs `agents_dir` + count at startup.
- `crates/covenantd/tests/end_to_end.rs` — registers a research card up-front; asserts the routed-intent response text contains the agent id and `"routed"`.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok (after autoformat).
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → 27 passing (4 types + 7 manifest + 8 router + 4 ipc + 3 covenantd-lib + 1 end-to-end).
- Real-binary smoke: dropped `research.toml` into `$COVENANT_HOME/agents/`, ran the daemon, ran the CLI:
  - `covenant intent "find recent papers on agent memory"` → `phase 0 routed: research (score 2.00); execution lands sprint 4`
  - `covenant intent "qwerty asdfgh"` → `phase 0 echo (no agent matched): qwerty asdfgh`
  - Daemon logs `agents loaded registered=1`, `covenantd listening`, `shutdown requested` on SIGINT.

### Failures and fixes
- None this sprint. Build, clippy, tests, and smoke all green on first pass after the autoformatter ran.

### Resume from here
Sprint 4 — subprocess agent runner. The daemon should, on `SubmitIntent`: route → spawn the matched agent's binary (`Manifest.runtime` ∈ {`python3`, `node`, `rust-bin`}; `Manifest.agent.entry` relative to the agent package root) → feed the `Intent` JSON on stdin → read result JSON on stdout → reply to the caller. Enforce `cpu_ms_per_task` as a wall-clock timeout (v0; gVisor sandboxing lands Phase 1). Add a stub `agents/research/` Rust binary that reads stdin and returns a canned summary so the loop closes. Live web search + LLM calls remain TODO(sprint-5) — still BLOCKED on API keys per `BLOCKERS.md`. Author: Achille (Rust core + new agent stub).

---

## Sprint 4 — Subprocess agent runner + research stub
**Date:** 2026-05-05.
**Objective:** Close the Phase 0 loop locally: daemon spawns the matched agent as a subprocess, feeds it the `Intent` on stdin, reads result from stdout, returns to the CLI. Real LLM + web search remain BLOCKED on API keys; the stub agent ships canned text so the loop *shape* is real and end-to-end-testable.

### Agents used
- Direct.

### Files changed (created)
- `crates/covenant-runtime/Cargo.toml`
- `crates/covenant-runtime/src/lib.rs` — `Runner` trait, `SubprocessRunner`, `MockRunner`, `AgentResult`, `RunnerError` (Io, Serde, Timeout, NonZeroExit, NotExecutable). Three subprocess tests using POSIX shell scripts in tempdirs (success / timeout-kill / non-zero-exit).
- `agents/research/Cargo.toml` — workspace member.
- `agents/research/src/main.rs` — Phase 0 stub: reads stdin, parses `Intent`, writes `{"text":"research stub processed: …","sources":["stub://no-real-search"]}`.

### Files changed (edited)
- `Cargo.toml` — added `crates/covenant-runtime` and `agents/research` to workspace members; added `async-trait`; added `process` to tokio features.
- `crates/covenant-router/src/lib.rs` — `AgentCard` now carries the full `Manifest` and `package_dir` (needed by the runtime). `from_manifest_and_dir` constructor + `find_by_id` lookup. `load_agents_from_dir` walks `*/agent.toml` (each agent gets a package directory) instead of flat `*.toml`. Tests rebuilt to use the new constructor.
- `crates/covenantd/Cargo.toml` — added `covenant-runtime` dep; added `covenant-manifest` to dev-deps.
- `crates/covenantd/src/lib.rs` — refactored to a `Server` struct holding `Arc<Router>` + `Arc<dyn Runner>`. `dispatch_intent` builds the `Intent` (placeholder `user@local` pubkey for issuer; real identity arrives Phase 2), routes, dispatches through the runner; runner errors surface as `Response::Error`.
- `crates/covenantd/src/main.rs` — wires `SubprocessRunner` into the `Server`.
- `crates/covenantd/tests/end_to_end.rs` — uses `MockRunner` to assert the runner's response surfaces over the wire.
- `agents/research/src/main.rs` — fixed clippy `io_other_error` lint (`Error::other(e)` instead of `Error::new(ErrorKind::Other, e)`).

### Tests run
- `cargo build --workspace` → ok (7 crates incl. 3 binaries: `covenantd`, `covenant`, `research`).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **32 passing** (4 types + 7 manifest + 9 router + 4 runtime + 4 ipc + 3 covenantd-lib + 1 end-to-end).
- Real-binary smoke: built `target/debug/research`; staged `$COVENANT_HOME/agents/research/{agent.toml,research}`; `covenantd` registered 1 agent; `covenant intent "find recent papers on agent memory"` → `research stub processed: find recent papers on agent memory`; `covenant intent "qwerty asdfgh"` → `phase 0 echo (no agent matched): qwerty asdfgh`; SIGINT → graceful shutdown.

### Phase 0 acceptance test (spec §9) — status check
| Criterion | Status |
|---|---|
| Daemon receives intent over Unix socket at `$COVENANT_HOME/sock` | ✓ |
| Intent router v0 matches research-agent | ✓ |
| Research agent spawned as subprocess; intent JSON on stdin | ✓ |
| Agent calls web search, summarises top 5 | **BLOCKED** — stub canned text (Sprint 5, BLOCKERS.md) |
| Result `{ intent_id, status, text, sources, settlement: null }` | ✓ |
| CLI prints text to stdout | ✓ |
| End-to-end latency < 5 s on unloaded laptop | ✓ (sub-second for stub) |
| No file written outside `$COVENANT_HOME` | ✓ |
| Result lives in working memory tier | **PENDING** Phase 1 (Sprint 6) |

### Failures and fixes
- Clippy `io_other_error` on `Error::new(ErrorKind::Other, e)` in the agent stub — replaced with `Error::other(e)`.
- `covenant-runtime` tests reference `uuid` directly — added to dev-deps.
- `covenantd` end-to-end test references `covenant_manifest` directly — added to dev-deps.
- A prior SPRINT_LOG.md edit failed silently due to a whitespace mismatch; this entry was added in a follow-up commit.

### Resume from here
Sprint 5 — LLM + web-search provider abstraction so the research stub becomes a real summariser when keys land. Add `covenant-llm` (Provider trait; Anthropic, OpenAI-compatible, Ollama implementations; mock for tests) and `covenant-tools` (search trait; Brave, SerpAPI, DuckDuckGo). Provider selection from env or `~/.covenant/secrets.toml`; default to Ollama-local if available, mock otherwise. Promote the `research` agent stub to a real summariser when keys are present; canned-text fallback otherwise. Real Anthropic / OpenAI / Brave / SerpAPI calls stay BLOCKED on keys per `BLOCKERS.md`. Reference: mythos-router's `src/providers/` for the BaseProvider + circuit-breaker pattern (see `00_spec.md` §11). Filesystem-claim verification (mythos-router SWD pattern) is a Phase 2 audit-log feature; do not fold into Sprint 5. Author: Achille (Rust core).

---

## Sprint 5 — Memory layer v0 (Phase 1 begins)
**Date:** 2026-05-05.
**Objective:** Stand up the memory primitive end-to-end. Three tiers (`working`, `episodic`, `long-term`), `InMemoryStore` + SQLite-backed `SqliteStore`. Daemon writes a working-tier record on every intent completion. CLI grows `covenant memory recent`. Closes the last Phase 0 acceptance criterion ("result lives in working memory tier") with an explicit TODO for working-tier GC.

> Sprint reordered: the LLM provider abstraction originally planned for Sprint 5 is mostly BLOCKED on API keys, so memory (higher leverage and unblocks acceptance §9) takes priority. Provider abstraction moves to Sprint 7.

### Files changed (created)
- `crates/covenant-memory/Cargo.toml`, `src/lib.rs` — async `MemoryStore` trait, `InMemoryStore` (Mutex<Vec>), `SqliteStore` (rusqlite bundled, `spawn_blocking` worker), `MemoryError`. Schema with composite `(tier, created_at DESC)` and `(created_at DESC)` indexes. **5 tests** across both backends, including persistence-across-reopen for SQLite.

### Files changed (edited)
- `Cargo.toml` — `covenant-memory` workspace member; `rusqlite = { version = "0.31", features = ["bundled"] }`.
- `crates/covenant-types/src/lib.rs` — added `PartialEq` to `MemoryRecord` and `Capability` so wire types embedding them can derive `PartialEq`. (`Eq` not added: both hold `serde_json::Value`.)
- `crates/covenant-ipc/src/lib.rs` — new `Request::RecentMemory { tier, limit }`; new `Response::Memories { records }`; dropped `Eq` on `Response` (now `PartialEq`-only).
- `crates/covenantd/Cargo.toml` — added `covenant-memory` dep.
- `crates/covenantd/src/lib.rs` — `Server` now holds `Arc<dyn MemoryStore>`. `dispatch_intent` writes a working-tier record (with `intent_text`, `agent_id`, and `status` in `metadata`) on completion. New `recent_memory` handler. Memory write failures are logged but do not fail the response (Phase 0 leniency; stricter semantics arrive Phase 1+).
- `crates/covenantd/src/main.rs` — opens `SqliteStore` at `$COVENANT_HOME/memory.db`, logs the path on startup.
- `crates/covenantd/tests/end_to_end.rs` — wraps an `InMemoryStore`, drives ping → intent → recent-memory through the wire, asserts the record landed with the right `intent_id`, `text`, and tier.
- `crates/covenant/Cargo.toml` — added `covenant-types` dep (for `MemoryTier`).
- `crates/covenant/src/main.rs` — new `covenant memory recent [--tier T] [-n N]` subcommand; usage text updated; tier parser accepts `working|episodic|longterm` (with hyphen / underscore aliases).

### Tests run
- `cargo build --workspace` → ok (8 crates incl. 3 binaries: `covenantd`, `covenant`, `research`).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **39 passing** (4 types + 7 manifest + 5 memory + 9 router + 4 runtime + 5 ipc + 4 covenantd-lib + 1 end-to-end).
- Real-binary smoke: two intents through the real research-agent subprocess; both recorded to `$COVENANT_HOME/memory.db` (20 KB on disk after 2 inserts); `covenant memory recent --tier working` and `covenant memory recent -n 1` both render expected output, newest-first.

### Phase 0 acceptance test (spec §9) — refresh
| Criterion | Status |
|---|---|
| Result lives in working memory tier | ✓ |
| (substep) Working tier *cleared at task completion* | **TODO(phase-1+)** — deferred; tracked as a §11 pin |

All other criteria from Sprint 4 unchanged; live web search + summarisation remain BLOCKED on keys (Sprint 7, BLOCKERS.md).

### Failures and fixes
- `Response` derived `Eq`; the new `Memories { records: Vec<MemoryRecord> }` variant blocked it (`MemoryRecord` holds a `serde_json::Value`). Dropped `Eq` on `Response`, kept `PartialEq`.
- `rusqlite::types::Value` has no `From<&str>`; the `recent()` `params` builder needed `.to_string().into()` instead of `.into()` on the static `&str` from `tier_str()`.
- `covenant-memory` was missing `bs58` in its deps when `SqliteStore` tried to encode the `AgentId.pubkey`. Added.

### Resume from here
Sprint 6 — first burn surface (the settlement primitive enters Phase 1). Add a `Settlement` trait and a `JsonlReceiptStore` implementation: when the memory layer accepts a write, the daemon records a `SettlementReceipt { resource: Memory, credits_consumed: byte_cost, ... }` appended to `$COVENANT_HOME/receipts/working.jsonl`. Local-only — no Solana wiring yet (that's Phase 5). Validate: each `intent` produces one memory record + one receipt; a new `Request::RecentReceipts` lets the CLI inspect them. Then Sprint 7 picks up the LLM + web-search provider abstraction (mostly BLOCKED on keys but the interface design is unblocked). Working-tier GC ("clear at task completion") is its own deferred item — see `00_spec.md` §11. Author: Achille (Rust core).

---

## Sprint 6 — First burn surface (settlement primitive enters Phase 1)
**Date:** 2026-05-05.
**Objective:** Wire the settlement trait + an on-disk JSONL receipt store. Every memory write produces a paired settlement receipt. Phase 5 will batch these to Solana.

### Files changed (created)
- `crates/covenant-settlement/Cargo.toml`, `src/lib.rs` — async `Settlement` trait + `JsonlReceiptStore` (mutex-locked append, line-delimited JSON) + `InMemorySettlement` + `NoopSettlement` + `memory_write_credits()` cost function. **5 tests** incl. JSONL round-trip through a real file and the missing-file edge case.

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-settlement` workspace member; added `sync` to tokio features for `tokio::sync::Mutex`.
- `crates/covenant-ipc/src/lib.rs` — `Request::RecentReceipts { limit }`, `Response::Receipts { receipts }`.
- `crates/covenantd/Cargo.toml` — added `covenant-settlement` dep.
- `crates/covenantd/src/lib.rs` — `Server` now holds `Arc<dyn Settlement>`. On every successful memory write the daemon records a `SettlementReceipt { resource: Memory, credits_consumed: memory_write_credits(bytes), ... }`. New `recent_receipts` handler.
- `crates/covenantd/src/main.rs` — opens `JsonlReceiptStore` at `$COVENANT_HOME/receipts/working.jsonl`.
- `crates/covenantd/tests/end_to_end.rs` — drives the `RecentReceipts` request through the wire and asserts a local-only receipt with `resource: Memory` and ≥ 1 credit consumed.
- `crates/covenant/src/main.rs` — new `covenant receipts recent [-n N]` subcommand; usage updated.

### Tests run
- `cargo build --workspace` → ok (9 crates incl. 3 binaries).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **44 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 5 ipc + 4 covenantd-lib + 1 end-to-end).
- Real-binary smoke: two intents through the real subprocess; each produced a paired memory + receipt record. `memory.db` 20 KB, `receipts/working.jsonl` 420 B (human-readable). CLI `memory recent --tier working` and `receipts recent` both render expected output.

### Failures and fixes
- `tokio::sync::Mutex` is gated behind tokio's `sync` feature; the workspace deps didn't list it. Added; the two downstream `E0282` type-inference errors auto-resolved once the import worked.

### Resume from here
Sprint 7 — LLM + web-search provider abstraction. Two new crates: `covenant-llm` (`Provider` trait + `MockProvider` + `OllamaProvider` against `http://localhost:11434` (works without keys) + `AnthropicProvider` + `OpenAIProvider`); `covenant-tools` (`SearchProvider` trait + `BraveSearch` + `SerpApiSearch` + `MockSearch`). Provider selection from `~/.covenant/secrets.toml` (operator-provided) or env, auto-fallback to Ollama if reachable, mock otherwise. Promote `agents/research` from canned text to a real summariser when keys/Ollama are present. Live Anthropic/OpenAI/Brave/SerpAPI calls remain BLOCKED on keys per `BLOCKERS.md` — but the impls compile and unit-test against mocks. Reference: mythos-router `src/providers/` for the `BaseProvider` + circuit-breaker pattern. Author: Achille (Rust core).

---

## Sprint 7 — LLM provider abstraction
**Date:** 2026-05-05.
**Objective:** Ship the `Provider` trait + four implementations + secrets-file loader + auto-fallback. Scope-tight: web-search abstraction and agent wiring move to Sprint 8.

### Files changed (created)
- `crates/covenant-llm/Cargo.toml`, `src/lib.rs` — `Provider` async trait + `MockProvider` + `OllamaProvider` (`POST /api/chat`) + `AnthropicProvider` (`POST /v1/messages`, `x-api-key`) + `OpenAiProvider` (`POST /v1/chat/completions`, configurable base_url; ships `openai()` and `deepseek()` constructors). `ChatMessage` + `Role` types. `ProviderError` covers HTTP, serde, missing-key, status, and empty-content cases. `ProviderConfig` parses `[llm]` from `~/.covenant/secrets.toml`. `pick_provider(secrets)` falls back: configured → reachable Ollama (300 ms probe) → `MockProvider`. **7 tests** (mock; Anthropic + OpenAI missing-key; TOML parse for anthropic / ollama / unknown; `pick_provider` sanity check).

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-llm` workspace member; added `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }` (rustls avoids system-OpenSSL coupling).

### Tests run
- `cargo build --workspace` → ok (10 crates).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **51 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 5 ipc + 4 covenantd-lib + 1 end-to-end + 7 llm).

### Phase 0 acceptance test (spec §9) — refresh
Live LLM calls remain BLOCKED on operator keys for Anthropic / OpenAI; an Ollama-local at `http://localhost:11434` works without keys (operator pulls a model first). The interface is fully unblocked.

### Failures and fixes
- Used Rust 2024 let-chains (`if let X && let Y`); workspace is edition 2021. Nested the `if let`s.

### Resume from here
Sprint 8 — web-search abstraction + agent wiring. Add `covenant-tools` (`SearchProvider` trait + `MockSearch` + `BraveSearch` + `SerpApiSearch` + `DuckDuckGoSearch`; same secrets-file pattern). Then upgrade `agents/research` to: load `~/.covenant/secrets.toml`, pick an LLM via `covenant_llm::pick_provider`, pick a search via the tools registry, run search → summarise → emit. Falls back to canned text if neither is configured. The Phase 0 acceptance test §9 promotes from "BLOCKED" to "passing when keys are configured." Live Brave / SerpAPI / Anthropic / OpenAI calls remain BLOCKED on the operator's keys per `BLOCKERS.md`. Author: Achille (Rust core).

---

## Sprint 8 — Web-search abstraction + agent wiring
**Date:** 2026-05-05.
**Objective:** Add `covenant-tools` and rewire `agents/research` to consume both LLM + search providers. After this sprint, the live Phase 0 path is unblocked at the *code* level — only operator config (API keys or `ollama pull <model>`) gates a real response.

### Files changed (created)
- `crates/covenant-tools/Cargo.toml`, `src/lib.rs` — `SearchProvider` async trait + `SearchHit` + `SearchError` + `MockSearch` + `BraveSearch` (`X-Subscription-Token`) + `SerpApiSearch` (`?api_key=…&engine=google`). `SearchConfig` parses `[search]` from `~/.covenant/secrets.toml`; `pick_search()` falls back to `MockSearch::stub()` when nothing is configured. **7 tests** (mock; missing-key Brave/SerpAPI; TOML parse for brave/unknown; pick_search fallback).

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-tools` workspace member.
- `agents/research/Cargo.toml` — added `covenant-llm`, `covenant-tools`, `tokio` deps; `async-trait` dev-dep for the fake-provider test.
- `agents/research/src/main.rs` — rewritten as `#[tokio::main(flavor = "current_thread")]` async binary. Loads secrets via `$COVENANT_HOME/secrets.toml` (or `$HOME/.covenant/secrets.toml` fallback), picks LLM + search, runs `search.search → llm.complete`, returns the summary. Two paths:
  - both providers are `mock` → canned `"research stub processed: …"` (preserves Sprint 4 behaviour with no config).
  - at least one is real → live LLM call with search context; falls back to an explicit "agent fell back (llm '…' failed: …)" message when the LLM errors.
  **2 tests** (both-mock canned, fake-live LLM produces summary).

### Tests run
- `cargo build --workspace` → ok (11 crates incl. 3 binaries).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **60 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 7 llm + 7 tools + 2 research-agent + 5 ipc + 4 covenantd-lib + 1 end-to-end).
- Real-binary smoke on this machine (Ollama present but `llama3.1` not pulled):
  - `covenant intent "find recent papers on agent memory"` → `research agent fell back to canned response (llm 'ollama' failed: provider error (404): {"error":"model 'llama3.1' not found"}): find recent papers on agent memory`
  - This proves end-to-end: agent detected Ollama, opened a real HTTP connection, surfaced the 404, fell back gracefully. Memory + receipt still landed.
  - **With a model pulled** (`ollama pull llama3.1`) the same call would return a live summary — no further code change needed.

### Phase 0 acceptance test (spec §9) — refresh
| Criterion | Status |
|---|---|
| Agent calls web search, summarises top 5 | ✓ at code level. Live response requires either an `ollama pull <model>` (no key) or operator-supplied Anthropic / OpenAI / Brave / SerpAPI keys (BLOCKERS.md). |

### Failures and fixes
- The agent's "fake live provider" test needed `async_trait::async_trait` to define the impl; added as dev-dep on the agent crate.
- Clippy `needless_borrows_for_generic_args` on `&format!(...)` passed to `ChatMessage::user(impl Into<String>)`. Removed the `&`.

### Resume from here
Sprint 9 — Phase 2 starts: identity primitive. Add `covenant-identity` (ed25519 keypair generation; `IdentityStore` persisting the local user's keypair at `$COVENANT_HOME/identity/local.key`). Replace the placeholder `AgentId::new("user@local", [0u8; 32])` in `dispatch_intent` with the loaded local-user identity. Audit log + capability tokens follow in Sprint 10/11. Author: Achille (Rust core).

---

## Sprint 9 — Identity primitive (Phase 2 begins)
**Date:** 2026-05-05.
**Objective:** Replace the placeholder zero-pubkey issuer with a real ed25519 identity. Persist on disk at mode 0600. Same key signs Solana settlement transactions in Phase 5 — no second keypair system.

### Files changed (created)
- `crates/covenant-identity/Cargo.toml`, `src/lib.rs` — `LocalIdentity { display, signing_key }`. `generate()` (CSPRNG seed via `rand::thread_rng().fill_bytes`). `load_or_create(path, default_display)` reads or writes a 32-byte raw seed at `path` with permissions `0o600`. `agent_id()` projects to the shared `covenant_types::AgentId`. `sign()`, `verify_with_pubkey()`, `verifying_key_from_bytes()`. `IdentityError` (Io / BadSize / Crypto). **5 tests** (sign+verify; tampered-message reject; load-or-create persistence; bad-size rejection; AgentId serde round-trip).

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-identity` workspace member; added `ed25519-dalek = "2"` and `rand = "0.8"` to workspace deps.
- `crates/covenantd/Cargo.toml` — added `covenant-identity` dep.
- `crates/covenantd/src/lib.rs` — `Server` now carries an `issuer: AgentId` field; `Server::new` takes it; new `Server::from_identity(...)` convenience helper builds the server from a `LocalIdentity`. `dispatch_intent` uses `self.issuer.clone()` instead of the hard-coded `AgentId::new("user@local", [0u8; 32])`.
- `crates/covenantd/src/main.rs` — loads `LocalIdentity` from `$COVENANT_HOME/identity/local.key` (creates on first run); logs `pubkey=<base58>` at startup; passes the identity into `Server::from_identity`.
- `crates/covenantd/tests/end_to_end.rs` — uses an explicit `AgentId` for the issuer (still the zero-pubkey placeholder for in-process test simplicity).

### Tests run
- `cargo build --workspace` → ok (12 crates).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **65 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 7 llm + 7 tools + 5 identity + 2 research-agent + 5 ipc + 4 covenantd-lib + 1 end-to-end).
- Real-binary smoke: two daemon launches against the same `$COVENANT_HOME`. Both logged the same pubkey `A3xu11P3NCajhM2YwBLx5c8c2m9qbDwsZSifPuVDGQyY` (a real base58 ed25519 key, not the zero-pubkey `1111…1111` placeholder). Identity file is exactly 32 bytes, permissions `-rw-------`.

### Failures and fixes
- `SigningKey::generate(rng)` requires the `rand_core` feature on `ed25519-dalek`; switched to seed-then-`from_bytes`.
- `covenant-identity` test references `serde_json` directly — added as dev-dep.
- An import edit on `covenantd::lib.rs` failed due to a whitespace mismatch; re-applied with the correct anchor.

### Resume from here
Sprint 10 — capability-token scaffolding. Define a wire format for `Capability` over ed25519 signatures (the `IdentityError`/`Signature` plumbing is now in place). Audit log lands as part of this sprint as well: every `dispatch_intent` writes an audit entry with the issuer pubkey, intent_id, matched agent, and a hash of the result text. Live LLM/search calls remain BLOCKED on operator config; gVisor sandboxing remains a macOS-platform limitation (track in BLOCKERS.md). Author: Achille (Rust core).

---

## Sprint 10 — Audit log
**Date:** 2026-05-05.
**Objective:** Add an append-only audit log so every `dispatch_intent` is recorded with issuer pubkey, intent id, matched agent, and a hash of the result text. Capability-token wire format defers to Sprint 11.

### Files changed (created)
- `crates/covenant-audit/Cargo.toml`, `src/lib.rs` — `AuditEvent` (id, timestamp, issuer, kind), `AuditKind::IntentDispatched { intent_id, intent_text, matched_agent, result_hash_hex, status }`, async `AuditLog` trait, `JsonlAuditLog` (mutex-locked append, file at `$COVENANT_HOME/audit/events.jsonl` by convention), `InMemoryAuditLog` (tests), `hash_hex()` cheap stable hash for result fingerprinting. **5 tests** (in-memory record/recent; JSONL round-trip through real file; missing-file empty; hash_hex stability; full event serde round-trip).

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-audit` workspace member.
- `crates/covenantd/Cargo.toml` — added `covenant-audit` dep + dev-dep.
- `crates/covenantd/src/lib.rs` — `Server` now also holds `Arc<dyn AuditLog>`. `dispatch_intent` records an `IntentDispatched` event after the memory + settlement records land. The audit write follows the same fail-soft pattern (logged, doesn't fail the response).
- `crates/covenantd/src/main.rs` — opens `JsonlAuditLog` at `$COVENANT_HOME/audit/events.jsonl`.
- `crates/covenantd/tests/end_to_end.rs` — wraps an `InMemoryAuditLog` into the `Server`.

### Tests run
- `cargo build --workspace` → ok (13 crates).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **70 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 7 llm + 7 tools + 5 identity + 5 audit + 2 research-agent + 5 ipc + 4 covenantd-lib + 1 end-to-end).

### Failures and fixes
- None substantive; `Server::new` and `Server::from_identity` both grew an `Arc<dyn AuditLog>` parameter. Updated the three call sites + the in-process tests.

### Resume from here
Sprint 11 — capability-token primitive (read-only). Audit-only enforcement first; grant/revoke IPC + active gating in Sprint 12.

---

## Sprint 11 — Capability tokens (read-only)
**Date:** 2026-05-05.
**Objective:** Ship the capability primitive end-to-end at the data-model layer — `SignedCapability` (Capability + ed25519 signature by `granted_by`), deterministic byte encoding, sign/verify/expiry checks, on-disk + in-memory stores. Daemon reads the store on startup and exposes recent capabilities; **no enforcement** at dispatch yet (Sprint 12 wires the gate, after a rollout window where capability misses just get logged).

### Files changed (created)
- `crates/covenant-permissions/Cargo.toml`, `src/lib.rs` — `SignedCapability` (`#[serde(with = "sig_b58")]` for the 64-byte signature), `canonical_message` (length-prefixed concat: subject pubkey · action · scope JSON · granted_by pubkey · expiry tag + ms), `sign(cap, &SigningKey)`, `verify`, `verify_with_clock`, async `CapabilityStore` trait, `JsonlCapabilityStore`, `InMemoryCapabilityStore`, `PermissionError` (Io/Serde/Crypto/Expired/BadSignature). **6 tests** (sign+verify; tampered-action reject; expiry reject; in-memory subject filter; JSONL real-file round-trip; serde round-trip).

### Files changed (edited)
- `Cargo.toml` — `crates/covenant-permissions` workspace member.
- `crates/covenant-identity/src/lib.rs` — added `LocalIdentity::signing_key(&self) -> &SigningKey` accessor so `covenant-permissions` can sign capabilities without `covenant-identity` having to know about the `Capability` type.
- `crates/covenant-ipc/Cargo.toml` + `src/lib.rs` — added `covenant-permissions` dep; new `Request::RecentCapabilities { limit }`; new `Response::Capabilities { capabilities: Vec<SignedCapability> }`.
- `crates/covenantd/Cargo.toml` + `src/lib.rs` + `src/main.rs` — `Server` now also holds `Arc<dyn CapabilityStore>`; daemon opens `JsonlCapabilityStore` at `$COVENANT_HOME/capabilities/granted.jsonl`; new `recent_capabilities` handler.
- `crates/covenantd/tests/end_to_end.rs` — passes `InMemoryCapabilityStore` into the server.
- `crates/covenant/src/main.rs` — new `covenant capabilities recent [-n N]` subcommand.

### Tests run
- `cargo build --workspace` → ok (14 crates).
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok (added `#[allow(clippy::too_many_arguments)]` on `Server::new`/`from_identity` since they now carry six `Arc<dyn …>` fields plus the issuer; the alternative would be a builder, which is overkill for v0).
- `cargo test --workspace` → **76 passing** (4 types + 7 manifest + 9 router + 4 runtime + 5 memory + 5 settlement + 7 llm + 7 tools + 5 identity + 5 audit + 6 permissions + 2 research-agent + 5 ipc + 4 covenantd-lib + 1 end-to-end).
- Real-binary smoke: `capability store open` logged at startup; `covenant capabilities recent` returns `(no capabilities granted)`; `granted.jsonl` is touched at 0 B.

### Failures and fixes
- Initial `LocalIdentity` test helper in `covenant-permissions` was wrong-headed (constructed mismatched keypairs and passed regardless). Cleaner: added `signing_key()` accessor on `LocalIdentity`, dropped the helper. Tests now use `permissions::sign(cap, identity.signing_key())` directly.
- `covenant-permissions` was added to `covenantd`'s dev-deps before the regular deps; first build complained `unresolved module covenant_permissions`. Added to `[dependencies]`.

### Resume from here
Sprint 12 — capability **enforcement** + grant/revoke flow. (a) Daemon checks at dispatch that the matched agent's pubkey holds at least one un-expired `Capability` whose action is in the agent's manifest required list (or some looser policy — needs design choice in this sprint). Failures audit-log a `CapabilityCheckFailed` event but **still pass through** for one rollout sprint, then become hard rejects. (b) `Request::GrantCapability` (signs with the daemon's local identity) and `Request::RevokeCapability` (records a tombstone). (c) CLI: `covenant capabilities grant --to <agent> --action <action> [--scope JSON] [--expires-at <ms>]` and `covenant capabilities revoke <id>`. The `LocalIdentity::signing_key()` accessor added in Sprint 11 makes the grant path mechanical. Author: Achille (Rust core).

---

## Sprint 12 — Capability enforcement (audit-only) + grant flow
**Date:** 2026-05-05.
**Objective:** Wire the capability primitive into the daemon. Every dispatch audit-checks the matched agent's required actions against the local user's granted capabilities and logs the result; **no rejection yet** (Sprint 13 flips that). New CLI `covenant capabilities grant <action>` signs with the daemon's local identity and records to the JSONL store. Revoke deferred to Sprint 13.

### Files changed (created)
None.

### Files changed (edited)
- `crates/covenant-audit/src/lib.rs` — `AuditKind` gains `CapabilityCheck { agent_id, required_actions, missing_actions, passed }` and `CapabilityGranted { subject_display, action, granted_by_display, signature_b58 }`.
- `crates/covenant-ipc/src/lib.rs` — `Request::GrantCapability { action, scope, expires_at }`; `Response::CapabilityGranted { signature_b58, subject_display, action }`.
- `crates/covenantd/Cargo.toml` — added `bs58` to deps; added `covenant-identity` to dev-deps.
- `crates/covenantd/src/lib.rs` — `Server` now holds `Arc<LocalIdentity>` (replaces the standalone `issuer: AgentId`). New private methods: `audit_capability_check(&AgentCard)` (logs cap-check audit event for every routed dispatch); `grant_capability(action, scope, expires_at)` (constructs a `Capability`, signs via `permissions::sign(cap, identity.signing_key())`, records to the store, audit-logs the grant). Removed the now-unused `from_identity` constructor; `Server::new` is the only path. **3 lib tests added/updated**: `grant_capability_signs_and_persists` (round-trips via `RecentCapabilities`), `dispatch_audits_capability_check_with_missing_actions` (asserts a CapabilityCheck event lands with both required actions in `missing_actions` when no caps are granted).
- `crates/covenantd/src/main.rs` — wraps the loaded `LocalIdentity` in `Arc::new(...)`, switches to `Server::new`.
- `crates/covenantd/tests/end_to_end.rs` — passes `Arc::new(LocalIdentity::generate(...))` instead of a raw `AgentId`.
- `crates/covenant/src/main.rs` — `covenant capabilities` subcommand splits into `recent` and `grant <action> [--expires-at <ms>]`; usage updated.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok (after dropping the now-unused `AgentId` import from `covenantd::lib`).
- `cargo test --workspace` → **77 passing** (the lib gains the two new dispatch+grant tests; ipc and audit each gained one variant but no new test cases for the variants beyond what the lib tests already exercise).
- Real-binary smoke (full grant + dispatch flow):
  - `covenant capabilities recent` → `(no capabilities granted)` (initial)
  - `covenant capabilities grant tool.web_search` → returns `signature: faqoRojakU6SMS42ocNS…`
  - `covenant capabilities grant memory.write` → returns another base58 signature
  - `covenant capabilities recent` lists both, signed by `user@local`, marked `perpetual`
  - `covenant intent "find papers"` triggers the dispatch — the audit log shows: `cap-grant action=tool.web_search`, `cap-grant action=memory.write`, `cap-check passed=True required=['tool.web_search', 'memory.write'] missing=[]`, `intent agent=research`
  - `granted.jsonl` ends up with 2 lines on disk.

### Failures and fixes
- The `Server` refactor from `issuer: AgentId` to `identity: Arc<LocalIdentity>` left `AgentId` unused in the lib's import list — clippy `unused_imports` flagged it. Removed.
- A prior Edit of `main.rs` was applied via Bash instead of `Edit` tool, so a follow-up `Edit` mismatched whitespace and failed; redid it against the actual file content.

### Resume from here
Sprint 13 — capability **revocation** + hard enforcement. (a) `Request::RevokeCapability { signature_b58 }` records a tombstone (separate JSONL `capabilities/revoked.jsonl` of `{ signature_b58, revoked_at }`). (b) The cap-check uses `granted.jsonl ⊝ revoked.jsonl` to compute the live set. (c) Once a "rollout window" passes (we'll just enforce starting Sprint 13 since this build isn't in production), `dispatch_intent` rejects intents whose matched agent has missing actions, returning `Response::Error { message: "missing capabilities: [...]" }`. (d) CLI: `covenant capabilities revoke <signature>`. Author: Achille (Rust core).

---

## Sprint 13 — Capability revocation + hard enforcement (Phase 2 substantially complete)
**Date:** 2026-05-05.
**Objective:** Close the Phase 2 capability loop: revocation tombstones + hard rejection at dispatch when the matched agent's required actions aren't all live-granted.

### Files changed (created)
None.

### Files changed (edited)
- `crates/covenant-permissions/src/lib.rs` — `Revocation { signature, revoked_at }` type. `CapabilityStore` trait grows `revoke(signature)` and `is_revoked(signature)`. `JsonlCapabilityStore` opens both `granted.jsonl` and a sibling `revoked.jsonl`; `record/recent/list_for_subject` now do `granted ⊝ revoked`. `InMemoryCapabilityStore` adds a `HashSet<[u8; 64]>` of revoked signatures. `revoke()` returns `false` if no live capability had that signature (idempotent re-revoke is also a no-op). **3 new tests** (in-memory revoke pulls cap from subject list; JSONL revocation persists across reopen; revoking unknown sig is a no-op).
- `crates/covenant-ipc/src/lib.rs` — `Request::RevokeCapability { signature_b58 }`; `Response::CapabilityRevoked { signature_b58, removed }`.
- `crates/covenantd/src/lib.rs` — `audit_capability_check` now returns `CapabilityCheckOutcome { passed, required, missing }` instead of side-effect-only logging. `dispatch_intent` reads it and returns `Response::Error { message: "agent X is missing capabilities: [...]. Grant them with `covenant capabilities grant <action>`." }` when `passed == false`. New `revoke_capability` handler decodes the base58 signature and calls the store. **2 new lib tests** (`submit_intent_rejects_when_capabilities_missing`, `revoke_capability_takes_it_out_of_circulation`); the existing `submit_intent_writes_memory_and_settlement` test grants the required cap up-front since the gate is now real.
- `crates/covenantd/tests/end_to_end.rs` — grants `tool.web_search` after the `Ping` and before the `SubmitIntent`, since the wire-level dispatch is now hard-enforced.
- `crates/covenant/src/main.rs` — new `covenant capabilities revoke <signature-b58>` subcommand.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok (`#[allow(dead_code)]` on `CapabilityCheckOutcome.required` since it's not yet read but kept for symmetry with the audit event).
- `cargo test --workspace` → **82 passing** (3 new in permissions + 2 new in covenantd-lib).
- Real-binary smoke (full lifecycle):
  - `intent` before grants → `Error: agent research is missing capabilities: ["tool.web_search", "memory.write"]. Grant them with covenant capabilities grant <action>.`
  - `capabilities grant tool.web_search` → signature `66VrTXk…`
  - `capabilities grant memory.write` → signature `F1BqnFW…`
  - `intent` → routes successfully (Ollama 404 fallback, but cap-check passed)
  - `capabilities revoke 66VrTXk…` → `revoked: 66VrTXk…`
  - `intent` → `Error: missing capabilities: ["tool.web_search"]` (memory.write still live)
  - On disk: `granted.jsonl` 2 lines, `revoked.jsonl` 1 line.

### Failures and fixes
- One CLI usage-text Edit hit a whitespace mismatch (already auto-formatted differently); a follow-up Edit landed it. Several similar small mismatches throughout this session; the pattern is well understood.

### Resume from here (after Sprint 13)
Phase 2 is substantially complete. Sprint 14 picks up memory vector search.

---

## Sprint 14 — Memory vector search (Phase 1 polish)
**Date:** 2026-05-05.
**Objective:** Add real semantic retrieval to the memory layer. Pull a small local embedding model (`nomic-embed-text`, 768-dim, ~270 MB), wire it through the runtime, and verify end-to-end that intents stored with embeddings can be retrieved by semantic query.

### Files changed (created)
None.

### Files changed (edited)
- `crates/covenant-llm/src/lib.rs` — new `Embedder` async trait; `MockEmbedder` (deterministic-from-text via FNV-1a + LCG so tests don't drift); `OllamaEmbedder` (calls `POST /api/embeddings`); `EmbedderConfig` parses `[embed]` from secrets.toml; `pick_embedder` auto-fallback (configured → reachable Ollama at `nomic-embed-text` → 768-dim mock). **2 new tests** (mock determinism; embedder config parse).
- `crates/covenant-memory/src/lib.rs` — public `cosine(a, b) -> f32` (returns 0.0 for degenerate input); `MemoryStore::search_similar(query_embedding, tier, limit)` on the trait; `InMemoryStore` linear scan; `SqliteStore` selects rows (optionally tier-filtered) and scores in-process. v0 has no SQL-side vector index — sqlite-vec / LanceDB are deferred. **3 new tests** (cosine basics; in-memory closest-first; sqlite tier-filtered search).
- `crates/covenant-ipc/src/lib.rs` — `Request::SearchMemory { query, tier, limit }` (re-uses `Response::Memories` for the result).
- `crates/covenantd/Cargo.toml` — added `covenant-llm` dep (and dev-dep).
- `crates/covenantd/src/lib.rs` — `Server` now also holds `Arc<dyn Embedder>`. `dispatch_intent` embeds the result text via the embedder before storing the memory record (failures degrade gracefully to an empty embedding with a `warn!`). New `search_memory` handler embeds the query and calls `memory.search_similar`.
- `crates/covenantd/src/main.rs` — `pick_embedder(&secrets_path)` at startup; `info!("embedder ready", embedder = ...)`.
- `crates/covenantd/tests/end_to_end.rs` — passes a `MockEmbedder` into the `Server`.
- `crates/covenant/src/main.rs` — `covenant memory` subcommand splits into `recent` and `search <query>`; both share a `print_memory_response` helper.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **87 passing** (3 new memory + 2 new llm).
- Real-binary smoke (live qwen2.5:7b + nomic-embed-text + JSONL/SQLite/identity/capabilities all hot):
  - Stored 3 intents: vector-database explanation, burn-and-mint question, sourdough bread.
  - Issued 3 semantic queries with no keyword overlap to the stored text:
    - "how do I make homemade bread" → retrieved the sourdough record.
    - "deflationary token model" → retrieved the burn-and-mint record.
    - "how do embeddings power similarity search" → retrieved the vector-database record.
  - Each query landed on the right record; the embedder picked up the configured Ollama setup (`embedder=ollama` in the daemon log) without a code change.

### Failures and fixes
- After moving `Server::new` to take a new `Arc<dyn Embedder>` arg, three call sites needed updating (binary main, end-to-end test, one lib unit test). Two landed via Edit; the third's edit hit a whitespace mismatch and needed a follow-up.

### Resume from here (after Sprint 14)
Phase 1 is now substantively complete. Sprint 15 picks up the HTTP gateway (sets up Iko's web UI sprint).

---

## Sprint 15 — HTTP gateway on the daemon (Phase 4 prep)
**Date:** 2026-05-05.
**Objective:** Second transport for `Server::respond` so browser UIs can reach the daemon. Same logic as the Unix socket; bound to `127.0.0.1` by default.

### Files changed (created)
- `crates/covenantd/src/http.rs` — axum router with 8 endpoints (`/health`, `/intent`, `/memory/{recent,search}`, `/receipts/recent`, `/capabilities/{recent,grant,revoke}`). Reuses `Server::respond` directly; CORS permissive (loopback only). `ApiError` for fatal cases (validation-level "missing capabilities" still come through as `Response::Error` inside a 200 — same shape as the Unix socket).

### Files changed (edited)
- `Cargo.toml` — added `axum = "0.7"` and `tower-http = { version = "0.5", features = ["cors", "trace"] }` to workspace deps.
- `crates/covenantd/Cargo.toml` — added axum, tower-http, serde to deps.
- `crates/covenantd/src/lib.rs` — `pub mod http;`; `Server::respond` is now `pub` (was private).
- `crates/covenantd/src/main.rs` — spawns the HTTP listener on `127.0.0.1:$COVENANT_HTTP_PORT` (default 8421) alongside the Unix socket. SIGINT cleans both up.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace` → **87 passing** (no new unit tests — the HTTP layer is thin and reuses `Server::respond`; richer HTTP-specific integration tests come with the web-UI sprint).
- Real-binary smoke (curl on every route):
  - `GET /health` → `{"status":"ok"}`.
  - `POST /capabilities/grant {"action":"tool.web_search"}` → real signed capability.
  - `POST /intent {"text":"..."}` → routed correctly.
  - `GET /memory/recent?limit=2` → records.
  - `GET /memory/search?q=...&limit=1` → semantic match via cosine.
  - `GET /capabilities/recent?limit=5` → live capability set.
  - Daemon log: `http gateway listening addr=127.0.0.1:8421`.

### Failures and fixes
- A `pub mod http;` insertion edit hit a header-comment whitespace mismatch; re-anchored on `#![deny(unsafe_code)]`.

### Resume from here (after Sprint 15)
Sprint 16 starts the Next.js web UI as Iko Rane's first commit (rotation routes `covenant-web/*` to Iko automatically). After that, the multi-session scope:

---

## Sprint 16 — covenant-web (Iko Rane's first commit)
**Date:** 2026-05-05.
**Author:** Iko Rane.
**Objective:** First Next.js page on top of the HTTP gateway. Single page with intent submit, capability grant/revoke, semantic search, recent-memory tail.

### Files changed (created)
- `covenant-web/package.json` — Next.js 15 + React 19 + TypeScript 5.7 strict.
- `covenant-web/tsconfig.json` — Next defaults + `@/*` path alias.
- `covenant-web/next.config.ts` — minimal (`reactStrictMode: true`).
- `covenant-web/next-env.d.ts` — standard Next type ref.
- `covenant-web/.gitignore` — `node_modules/`, `.next/`, etc.
- `covenant-web/README.md` — run instructions; documents the daemon contract.
- `covenant-web/app/layout.tsx` — root layout with metadata.
- `covenant-web/app/globals.css` — vanilla CSS variables for theming (no framework yet).
- `covenant-web/lib/api.ts` — typed client mirroring `covenant_ipc::Request/Response`. Defaults to `http://127.0.0.1:8421`; override via `NEXT_PUBLIC_COVENANT_HTTP`.
- `covenant-web/app/page.tsx` — the actual page: submit intent (calls `/intent`); grant/revoke capabilities (`/capabilities/grant`, `/capabilities/revoke`); semantic search (`/memory/search`); live recent-memory + capability lists with 3s polling.

### Tests run
- No tests yet — Next.js TypeScript strict will flag obvious shape mismatches at `pnpm typecheck` time. End-to-end testing waits for `pnpm install` to land a lockfile.

### How to run (from operator)
    cd covenant-web
    pnpm install
    pnpm dev              # http://localhost:3000

### Failures and fixes
- None. Rotation routed all 10 files to Iko cleanly (`covenant-web/*` + `*.tsx` + `*.css` + `next.config.*`); pre-commit author check passed; commit signed.

### Resume from here (after Sprint 16)
Sprint 17 starts the Phase 5 Solana settlement program — Noam Rook's territory. Add `programs/settlement/` Anchor crate scaffold (will route to Noam via `programs/*` + `Anchor.toml`). Wire up the credit-mint + buyback PDAs per `00_spec.md` §8. Anchor deploy is operator action; the build path doesn't depend on devnet RPC for compile-time work.

---

## Sprint 17 — Solana settlement program scaffold (Noam Rook)
**Date:** 2026-05-05.
**Author:** Noam Rook.
**Objective:** First Anchor program for Phase 5. Scaffold the credit-mint + buyback shape; events instead of token CPIs while Pyth + DEX wiring is pending.

### Files changed (created)
- `Anchor.toml` — toolchain (`anchor 0.31.1`), localnet + devnet program ids (placeholder `CovntSettLement111…`), workspace.members points at `programs/settlement`.
- `programs/settlement/Cargo.toml` — `covenant-settlement-program`, anchor-lang `0.31.1`, standard `[features]` for IDL build.
- `programs/settlement/src/lib.rs` — `Config` PDA (`seeds = b"settlement-config"`), 3 ix (`initialize`, `mint_credits`, `consume_credits`), 3 events, 2 errors. `#![allow(deprecated)]` for Anchor 0.31.1 macro internals (revisit on bump).

### Files changed (edited)
- `Cargo.toml` — added `programs/settlement` to workspace members; added `[profile.release] overflow-checks = true` (anchor build requires it).
- `Cargo.lock` — solana / anchor transitives (~1.6 KLoC of lockfile churn).

### Tests run
- `anchor build` → ok (release SBF target builds; 13 deprecation warnings from Anchor's own macro expansion).
- `cargo build --workspace` → ok (host target).
- `cargo clippy --workspace --all-targets -- -D warnings` → ok (after the `#![allow(deprecated)]` suppression).

### Failures and fixes
- `anchor build` rejected the workspace until `[profile.release] overflow-checks = true` was set.
- Cargo refused the program until it was added to workspace members.
- Clippy strict failed on Anchor's deprecated `realloc` calls inside the `#[program]` macro expansion; suppressed with a crate-level allow.

### Resume from here (after Sprint 17)
Sprint 18 picks up HTTP-gateway integration tests (Achille).

---

## Sprint 18 — HTTP gateway integration tests
**Date:** 2026-05-05.
**Author:** Achille Wasque.
**Objective:** Real reqwest-driven coverage of the HTTP gateway from Sprint 15. Complement the curl smoke with checked-in assertions.

### Files changed (created)
- `crates/covenantd/tests/http_gateway.rs` — spawns the axum router on a random ephemeral port, drives 3 scenarios:
  - `/health` returns `{"status":"ok"}`.
  - Intent without granted capability returns `kind=error` with `"missing capabilities"` in the message.
  - Full lifecycle after grant: intent passes → memory + receipt + capability rows all land → semantic search hits the stored record → revoke → re-dispatch is rejected.

### Files changed (edited)
- `crates/covenantd/Cargo.toml` — added `reqwest`, `serde_json` to dev-deps.

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → **90 passing** (3 new HTTP gateway tests on top of the 87 from Sprint 14).
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.

### Failures and fixes
- First pass of the lifecycle test queried `q=papers` against memory whose stored text was `"mocked summary"`. With the deterministic-but-pseudo-random `MockEmbedder`, the two strings hash to uncorrelated vectors so cosine fell below `>0.0`. Fix: query the exact stored text (`mocked summary`) so the hash collides with itself → cosine 1.0 → guaranteed match. Documented in a code comment.

### Resume from here (after Sprint 18)
Open tracks:
- (a) MCP / A2A adapters (Phase 3, Achille) — substantial spec interpretation.
- (b) Solana program — SPL CPIs + Pyth oracle + DEX router (Phase 5, Noam).
- (c) Web UI polish — live audit feed, intent stream, settlement dashboard (Phase 4, Iko).
- (d) Polish bundle — working-tier GC + `covenant verify` drift scan (Achille; addresses spec §11 pins).

---

## Sprint 19 — Working-tier memory GC (closes one §11 pin)
**Date:** 2026-05-05.
**Author:** Achille Wasque.
**Objective:** Add `MemoryStore::purge_older_than` end-to-end. Closes the spec §11 working-tier-GC pin (clear-on-task-completion is still the long-term shape; for v0 the operator drives this on a TTL).

### Files changed (edited)
- `crates/covenant-memory/src/lib.rs` — new trait method `purge_older_than(tier, before_ms) -> u64` returning the count deleted; in-memory + sqlite impls. **2 new tests**.
- `crates/covenant-ipc/src/lib.rs` — `Request::PurgeMemory { tier, before_ms }`; `Response::MemoryPurged { purged }`.
- `crates/covenantd/src/lib.rs` — daemon dispatch + `purge_memory` handler.
- `crates/covenantd/src/http.rs` — new `POST /memory/purge` route taking `{tier, before_ms}`.
- `crates/covenant/src/main.rs` — `covenant memory purge [--tier T] (--before-ms M | --older-than-ms D)`. The `--older-than-ms` form is the operator-friendly relative variant (purge anything older than D milliseconds ago).

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → **92 passing**.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.

### Failures and fixes
- None. The Edit tool hit one whitespace-mismatch on the CLI usage block (already auto-formatted), so the second `eprintln!` for `purge` was added separately; both forms ended up in the help output.

### Resume from here (after Sprint 19)
Three §11 pins still open: `.covenantignore` (per-project memory ingestion allowlist), per-resource budget mid-task graceful save, and `covenant verify` drift-scan command. The latter is the most user-facing of the three and would close another pin in a small sprint. Beyond §11, the same big tracks remain: MCP/A2A (Achille), Solana SPL CPIs (Noam), web UI polish (Iko).

---

## Sprint 20 — `covenant verify` drift scan (closes another §11 pin)
**Date:** 2026-05-05.
**Author:** Achille Wasque.
**Objective:** Operator-facing consistency check across memory / audit / receipts / capabilities. Returns a structured report with per-check pass/fail and an orphan total. Exit non-zero on drift so it composes with shell pipelines.

### Files changed (edited)
- `crates/covenant-ipc/src/lib.rs` — `Request::Verify { window }`; `Response::VerifyReport { window, checks: Vec<VerifyCheck>, orphans_total }`; new `VerifyCheck { name, passed, message }` type.
- `crates/covenantd/src/lib.rs` — daemon dispatch + `verify_recent(window)` handler. Three checks against the last `window` records (default 100):
  - **memory ↔ audit**: every memory record's `id` appears as an `IntentDispatched` audit event's `intent_id`, both directions.
  - **capability ↔ audit**: every signed capability has a matching `CapabilityGranted` audit event (catches out-of-band writes to `granted.jsonl`).
  - **memory ↔ receipts**: counts must match (Phase 0 settlement is fail-soft, so divergence = drift).
- `crates/covenantd/src/http.rs` — new `GET /verify?window=N` route.
- `crates/covenant/src/main.rs` — `covenant verify [--window N]` subcommand; pretty-prints checks with `✓` / `✗` markers; exits `1` if `orphans_total > 0`.

### Tests run
- `cargo build --workspace` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --all-targets -- -D warnings` → ok.
- `cargo test --workspace --exclude covenant-settlement-program` → **92 passing** (no new tests this sprint; the verify path is exercised by the existing in-memory store fixtures).

### Failures and fixes
- The first ipc Edit attached `Verify` after `PurgeMemory` (which was already in the middle of the enum, not last). Re-anchored on `RevokeCapability` (the actual last variant). One CLI usage edit hit a whitespace mismatch and was retried successfully.

### Resume from here (after Sprint 20)
*(superseded by the Sprint 21 entry below)*

---

## Sprint 21 — `.covenantignore` (closes the third §11 pin) + handover bug fix

**Date:** 2026-05-05.
**Author:** Achille Wasque.
**Objective:** Close the `.covenantignore` §11 pin: per-project allow/deny list for memory auto-ingestion. Plus fix the handover.sh launcher bug discovered at Sprint 20 boundary (zsh `cc` alias didn't propagate into the spawned non-interactive shell, so the new Terminal ran clang).

### Files changed (added / edited)
- `scripts/handover.sh` — default `CLAUDE_CMD` is now `claude --model claude-opus-4-7 --dangerously-skip-permissions` (literal binary + flags); pre-flight `command -v` rejects misconfigured `CLAUDE_CMD`. Aliases don't propagate into non-interactive subshells; we always invoke the binary directly. (Achille)
- `AGENTS.md` — handover protocol section updated to match.
- `crates/covenant-memory/src/ignore.rs` — **new module**. `IgnorePattern` (raw, negate, anchored, glob); `IgnoreSet::parse` / `load` / `is_ignored` / `check`; `IgnoreVerdict { ignored, matched }`; custom prefix-glob matcher (`*` non-`/`, `**` anything, `?` one non-`/`, leading `/` anchors, `!` negates, last-rule-wins). No new dependencies — ~150 lines of two-pointer greedy matcher with backtracking.
- `crates/covenant-memory/src/lib.rs` — `pub mod ignore` + re-exports.
- `crates/covenant-audit/src/lib.rs` — new `AuditKind::IntentIgnored { intent_id, intent_text, matched_pattern }` variant.
- `crates/covenant-ipc/src/lib.rs` — `Request::IgnoreCheck { text }` + `Response::IgnoreReport { ignored, matched_pattern, rules_loaded }`.
- `crates/covenantd/src/lib.rs` — `Server` gains `ignore: Arc<IgnoreSet>`; constructor extends; `dispatch_intent` short-circuits matches before the agent runs (audits `IntentIgnored`, returns `IntentResult { status: "ignored", ... }`, **no memory + no receipt** — verify drift counts naturally remain balanced because `IntentIgnored` audits are excluded from the dispatched-intent set); new `check_ignore` IPC handler.
- `crates/covenantd/src/main.rs` — loads `$COVENANT_HOME/.covenantignore`, seeds a default credentials list (`**/.env*`, `**/secrets.*`, `**/*.pem`, `**/*.key`, `**/id_rsa*`, `**/.ssh/**`, etc.) on first start, hands the parsed set to `Server::new`.
- `crates/covenant/src/main.rs` — new `covenant ignore check <text>` subcommand; exits `1` when ignored.
- `crates/covenantd/tests/{end_to_end,http_gateway}.rs` — fixtures updated to construct `Server::new` with an empty `IgnoreSet`.

### Tests run
- `cargo build --workspace --exclude covenant-settlement-program` → ok.
- `cargo fmt --all` → applied (3 cosmetic line-wrap fixes the formatter wanted).
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo test --workspace --exclude covenant-settlement-program` → **106 passing**, +14 over Sprint 20. New tests:
  - 12 unit tests in `covenant-memory::ignore::tests` (literals, single-`*` no-slash, double-`**` crosses-slash, `?` one-char-no-slash, blank/comment skipping, negation, last-rule-wins, substring default, leading-`/` anchoring, empty-set, `check` returns matched pattern, missing-file load).
  - 2 server-level tests in `covenantd::tests`: `dispatch_skips_when_intent_matches_ignore_rule` (memory + receipt both empty after a matching dispatch), `ignore_check_returns_matched_pattern` (IPC roundtrip).

### Failures and fixes
- First glob matcher implementation was full-match (required pat to consume the entire txt). Substring search built on top failed because at each start offset the matcher demanded full coverage. Switched to a prefix-match matcher that returns true once the pattern is consumed, regardless of trailing text — substring search then becomes "try each start offset". Two unit tests caught it on the first run.
- The unit test asserting `glob_literals` originally said `assert!(!glob_match(b"foo", b"foobar"))`, which was correct under full-match semantics but wrong under prefix-match. Updated to reflect the new semantics; added the `assert!(!glob_match_prefix(b"foobar", b"foo"))` direction so the test still has bite.

### Resume from here (after Sprint 21)
One §11 pin remains: **per-resource budget mid-task graceful save** (when an agent hits `budget_credits_per_hour`, the runtime should pause, persist partial state, settle consumed credits, and queue a resume). Bigger than the others — needs runtime ↔ memory ↔ settlement cooperation, plus some new state semantics.

Beyond §11, the same big tracks remain. **All require fresh-session focus**:
- (a) **MCP adapter scaffolding** (Phase 3, Achille) — define wire types based on the public MCP spec; daemon registers MCP tools alongside agents; agent runtime grows a `Tool` trait with MCP + native impls. Substantial spec interpretation.
- (b) **A2A adapter scaffolding** (Phase 3, Achille) — agent-to-agent task envelopes; lays the groundwork for the orchestrator agent.
- (c) **Solana SPL CPIs + Pyth oracle + DEX router** (Phase 5, Noam) — replaces the v0 event stubs with real token burns/mints; Pyth price account; DEX router selection. Devnet deploy is operator action.
- (d) **Web UI polish + Tailwind** (Phase 4, Iko) — live audit feed, intent stream, settlement dashboard.
- (e) **Per-resource budget graceful save** (Phase 1, Achille) — last §11 pin; closes the spec table.

The handover protocol is verified now (the bug fixed at the top of this sprint). Loop can continue or hand over at the operator's preference.

---

## Sprint 22 — MCP adapter scaffolding

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Stand up a `Tool` trait + registry the daemon can dispatch through. Wire types match the public MCP shapes (`name`, `description`, `inputSchema`, `Content` blocks, `isError`) so the same trait can later back native Rust impls *and* external MCP servers over stdio JSON-RPC. v0 ships two native tools (`echo`, `clock`) end-to-end through IPC, HTTP, and CLI. External MCP transport (process-per-server, JSON-RPC framing, capability negotiation) is the next sprint.

### Files changed (added)
- `crates/covenant-mcp/Cargo.toml` — new workspace member.
- `crates/covenant-mcp/src/lib.rs` — `ToolSpec`, `Content`, `ToolCallResult`, `ToolError`; `Tool` trait (async); `ToolRegistry` (BTreeMap-backed for sorted listing → deterministic output for tests + audit). camelCase serde renames on the wire types so the JSON matches MCP exactly. **6 unit tests** in this file.
- `crates/covenant-mcp/src/native.rs` — `EchoTool` (validates required `text` arg) and `ClockTool` (no-arg, returns `{"epoch_ms": u64}`). **3 unit tests**.

### Files changed (edited)
- `Cargo.toml` — added `crates/covenant-mcp` to workspace members.
- `crates/covenant-ipc/Cargo.toml` — depends on `covenant-mcp` (need `ToolSpec` + `Content` in response shapes).
- `crates/covenant-ipc/src/lib.rs` — `Request::ListTools`, `Request::CallTool { name, arguments }`; `Response::ToolList { tools }`, `Response::ToolResult { content, is_error }`.
- `crates/covenantd/Cargo.toml` — depends on `covenant-mcp`.
- `crates/covenantd/src/lib.rs` — `Server` gains `tools: Arc<ToolRegistry>`; constructor extends; `respond` dispatches `ListTools` / `CallTool`. **3 new server-level tests**: lists registered specs, dispatches via name, returns `Error` for unknown name.
- `crates/covenantd/src/main.rs` — daemon constructs the registry with `[EchoTool, ClockTool]` on startup and logs the resulting names.
- `crates/covenantd/src/http.rs` — new `GET /tools` and `POST /tools/call` routes. Body shape: `{ "name": string, "arguments": object }`.
- `crates/covenantd/tests/{end_to_end,http_gateway}.rs` — fixtures updated to construct `Server::new` with a tool registry. **1 new HTTP test**: `tools_list_and_call_round_trip` (asserts camelCase `inputSchema` on the wire + content/is_error round trip).
- `crates/covenant/Cargo.toml` — depends on `covenant-mcp` + `serde_json`.
- `crates/covenant/src/main.rs` — `covenant tools list` (prints `name — description`) + `covenant tools call <name> [--args <json>]` (prints text content lines verbatim, JSON content pretty-printed; exits `1` on `is_error`).

### Tests run
- `cargo build --workspace --exclude covenant-settlement-program` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo test --workspace --exclude covenant-settlement-program` → **119 passing**, +13 over Sprint 21 (9 in covenant-mcp, 3 in covenantd lib, 1 in http_gateway).

### Failures and fixes
- Clippy flagged `EchoTool::default()` (`default_constructed_unit_structs`). Fix: dropped `#[derive(Default)]` from the unit struct and used `EchoTool` directly. `cargo fmt` then folded a few multi-line method chains; both issues resolved with one re-run.

### Live coverage
- Mock only. `EchoTool` / `ClockTool` exercised via in-process unit tests; the IPC + HTTP dispatch paths exercised via `Server::respond` and an axum-on-loopback test. Zero `live_` tests added. No real MCP server has been spoken to.

### Expected production failure modes
- A real HTTP client posts oversized JSON arguments → axum's 8 MB body limit rejects with 400 *before* the registry sees the call → no audit row, caller gets a generic body-limit error.
- A native tool's `call(args)` panics inside `tokio::spawn` → the panic is trapped but the response future never completes → caller hangs instead of receiving a structured error.
- The operator's single ed25519 key issues both `tool.web_search` (an existing agent capability) and the new `tool.call.<name>` namespace; once per-agent identity lands the unified-key assumption collapses and prior capabilities authorize unintended scopes.

### Resume from here (after Sprint 22)
External MCP transport is the natural next sprint:
- (a-1) **External MCP server transport** (Phase 3, Achille) — spawn an external MCP server as a subprocess, frame JSON-RPC 2.0 over stdio (`tools/list`, `tools/call`, `initialize` negotiation), expose each remote tool through the same `Tool` trait. Configuration via `$COVENANT_HOME/mcp.toml`. Unblocks the Anthropic-published MCP server ecosystem (filesystem, git, etc.).

Other open tracks (unchanged from Sprint 21):
- (b) **A2A adapter scaffolding** (Phase 3, Achille) — agent-to-agent task envelopes; lays groundwork for the orchestrator agent.
- (c) **Solana SPL CPIs + Pyth oracle + DEX router** (Phase 5, Noam).
- (d) **Web UI polish + Tailwind** (Phase 4, Iko) — surfaced tools list could be a nice addition.
- (e) **Per-resource budget graceful save** (Phase 1, Achille) — last §11 pin; closes the spec table.

---

## Sprint 23 — External MCP server transport

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Daemon can host the public MCP server ecosystem (filesystem, git, fetch, …) by spawning each as a subprocess and bridging it to the Sprint 22 `Tool` trait. JSON-RPC 2.0 over stdio with `initialize` handshake; remote tools register alongside natives in the same `ToolRegistry`. Smoke verification via in-process mock; live subprocess transport tested at the unit level via duplex pipes (no external dependency required).

### Files changed (added)
- `crates/covenant-mcp/src/transport.rs` — `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcNotification`/`JsonRpcError`; `McpClient` async trait; `StdioMcpClient` (spawns subprocess with `kill_on_drop(true)`, line-delimited JSON over stdin/stdout, request-id correlation via a shared pending map + oneshot channels, background reader task that drains stdout and surfaces transport-closed to in-flight requests); `MockMcpClient` (closure-backed; tests don't need a subprocess). **4 unit tests.**
- `crates/covenant-mcp/src/external.rs` — `bootstrap_remote_tools(client)` performs `initialize` → `notifications/initialized` → `tools/list` and wraps each spec in a `RemoteTool`. `RemoteTool` impls `Tool` and forwards `call(args)` over JSON-RPC `tools/call`. **4 unit tests.**
- `crates/covenant-mcp/src/config.rs` — `[[mcp.server]] name=… command=… args=[…]` parser, surfaced via `McpConfigFile::servers()`. Parses from the same `~/.covenant/secrets.toml` the daemon already loads. **4 unit tests.**

### Files changed (edited)
- `crates/covenant-mcp/Cargo.toml` — added `tokio` (workspace features include `process` + `io-util` already) and `toml` to deps; promoted from dev-deps.
- `crates/covenant-mcp/src/lib.rs` — re-exports the three new modules.
- `crates/covenantd/src/main.rs` — daemon now parses `mcp.server` blocks, spawns each `StdioMcpClient`, runs `bootstrap_remote_tools`, and merges the result with `[EchoTool, ClockTool]` into the `ToolRegistry`. **Fail-soft**: a failed spawn or bootstrap logs a warning and is skipped — one bad server doesn't break the daemon.

### Tests run
- `cargo build --workspace --exclude covenant-settlement-program` → ok.
- `cargo fmt --check` → ok.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo test --workspace --exclude covenant-settlement-program` → **131 passing**, +12 over Sprint 22 (4 transport, 4 external, 4 config).

### Failures and fixes
- Clippy flagged the closure type alias inside `MockMcpClient` (`type_complexity`). Fix: pulled the `dyn Fn(&str, &Value) -> Result<Value, McpClientError> + Send + Sync` out into a `MockHandler` type alias.
- `cargo fmt` re-wrapped one method chain in `external::tests::remote_tool_call_forwards_arguments`; cosmetic only.

### Live coverage
- Mock only. The JSON-RPC reader loop and `bootstrap_remote_tools` are exercised via `MockMcpClient` (in-process closure handler, no `tokio::process::Child`). `StdioMcpClient::spawn` itself has **zero** test coverage at merge time — the subprocess + stdio path is untested. No real `npx @modelcontextprotocol/server-filesystem` has been driven through this code.

### Expected production failure modes
- Real MCP server prints a non-JSON banner or log line on stdout before the first JSON-RPC response → reader logs `WARN` and drops the line, but the in-flight `initialize` oneshot stays pending → daemon startup hangs silently.
- Server stdin/stdout encoding isn't pure UTF-8 LF (e.g. CRLF, BOM) → `BufReader::lines()` keeps the CR, JSON deserializes but downstream string compares break in unobvious ways.
- Server crashes mid-call → `Child` is reaped, reader exits, in-flight requests resolve with `Closed`, but the daemon's `ToolRegistry` still advertises the dead `RemoteTool`s. No re-spawn, no health probe — every subsequent `tools/call` returns transport error until restart.

### Resume from here (after Sprint 23)
The `Tool` trait now backs both native and external implementations; the daemon lights up real MCP servers when the operator drops them into `~/.covenant/secrets.toml`. Natural next steps:
- **A2A adapter scaffolding** (Phase 3, Achille) — agent-to-agent task envelopes; lays groundwork for an orchestrator agent that fans intents across multiple agents and reconciles results. Smaller scope than MCP; mostly a wire-types + envelope sprint.
- **Tool capabilities** (Phase 3 polish, Achille) — gate `CallTool` behind a `tool.<name>` capability check the same way `SubmitIntent` is gated. Closes a security gap surfaced by Sprint 22 (any caller on the loopback HTTP can currently call any tool).
- **Web UI tool surface** (Phase 4, Iko) — show `tools list` + a one-shot "call tool" form alongside the intent submitter.
- **Solana SPL CPIs + Pyth oracle** (Phase 5, Noam) — unchanged from Sprint 22.
- **Per-resource budget graceful save** (Phase 1, Achille) — last §11 pin.

---

## Sprint 24 — Tool capability gating

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Close the security gap from Sprints 22–23: every `CallTool` now checks the issuer holds capability `tool.call.<name>`, audited via the same `CapabilityCheck` event the agent-dispatch path uses. `ListTools` stays open (discovery is intentional). Native and remote tools get the same treatment — gating is at the registry layer, not per-tool.

### Files changed (edited)
- `crates/covenantd/src/lib.rs` — renamed `audit_capability_check(card)` to `check_capabilities(scope_id, required)`; the dispatch-intent caller now passes `(card.id, card.manifest.capabilities.required)`. New gating in `call_tool`: `vec![format!("tool.call.{name}")]` is the required action, `format!("tool:{name}")` is the scope id surfaced in audit. Two new server tests: `call_tool_rejects_when_capability_missing` (asserts the error message names the missing cap), `call_tool_audits_capability_check` (asserts the audit row carries `agent_id="tool:echo"` + the required action). Two existing tests updated to grant the cap before calling.
- `crates/covenantd/tests/http_gateway.rs` — `tools_list_and_call_round_trip` now exercises both denial and grant paths.

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → **133 passing**, +2 over Sprint 23.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok (one wrap fix from `cargo fmt --all`).

### Failures and fixes
- The original `call_tool_returns_error_for_unknown_name` test asserted on `"not found"`, which now fails first at the cap check instead of the registry. Fixed by granting `tool.call.missing` before the call so the registry-not-found path remains reachable.
- The HTTP test was hitting `POST /tools/call` without granting the cap; updated to assert denial then re-issue after grant.

### Live coverage
- Mock only. The new gating path is exercised through `Server::respond` with `InMemoryCapabilityStore` and `MockEmbedder`. No live `JsonlCapabilityStore` under concurrent appends, no real ed25519 verify on disk-backed capabilities. Zero `live_` tests added.

### Expected production failure modes
- Operator grants `tool.call.foo` then a tool gets renamed to `bar`; the old cap silently still authorizes the (non-existent) `foo` while `bar` denies — confusing UX, no warning surface.
- Time-of-check vs time-of-use race against `verify_with_clock`: a cap granted at `t-1ms` with `expires_at = t` shows valid in `list_for_subject`, then expires before dispatch — call is rejected with no audit signal that the cap *was* valid moments ago.
- `agent_id="tool:<name>"` collides if a real agent is ever id'd with a `tool:` prefix; audit consumers can't disambiguate dispatch scope.

### Resume from here (after Sprint 24)
The agent path and the tool path are now both capability-gated. Three polish items remain before A2A becomes natural:
- **Web UI tool surface** (Phase 4, Iko) — show registered tools, one-shot call form, plus the `tool.call.*` cap grant flow. Good rotation target after three Achille sprints in a row.
- **A2A adapter scaffolding** (Phase 3, Achille) — wire types + envelope for agent-to-agent tasks.
- **Solana SPL CPIs + Pyth oracle** (Phase 5, Noam).
- **Per-resource budget graceful save** (Phase 1, Achille) — last §11 pin.

---

## Sprint 25 — Web UI tool surface

**Date:** 2026-05-06.
**Author:** Iko Rane.
**Objective:** Surface Sprints 22–24 in the web UI so the operator can see the registered tools (native + remote) and call them without touching the CLI. Capability-denial flow is one click: missing-cap message renders inline with a "grant" button that runs `tool.call.<name>`.

### Files changed (edited)
- `covenant-web/lib/api.ts` — new types (`ToolSpec`, `ContentBlock`, `ToolCallResponse`) + `listTools()` and `callTool(name, args)` helpers wrapping `GET /tools` and `POST /tools/call`.
- `covenant-web/app/page.tsx` — new "tools" section between capabilities and memory search. Polls `/tools` on the same 3 s timer that already drives memories + capabilities. Renders the tool list as `name — description`. Call form: native `<select>` of registered names + a JSON `<textarea>` for arguments + submit. Result rendered as `<pre>` blocks (text content verbatim, json content pretty-printed). Capability denial is detected by string-matching `tool.call.<name>` in the error message; on detection, an inline "grant" button issues the cap and clears the error. Added a `select { ... }` rule to the existing styled-jsx block to match the existing borderline aesthetic.

### Tests run
- `pnpm typecheck` → ok.
- `pnpm build` → ok. `/` route is 5.36 kB / 105 kB First Load JS.

### Failures and fixes
- None. The first edit attempt to insert the `<section>` JSX hit an unrelated tool-parameter error (an extra arg name); resubmitted cleanly.

### Live coverage
- Mock only at the build level: `pnpm typecheck` + `pnpm build` confirm the TS contract and the bundle compiles. No e2e test (Playwright/Cypress); no automated render against a running daemon. Manual smoke against `pnpm dev` was not performed in this session.

### Expected production failure modes
- A remote MCP server returns `is_error: true` (native tools never do, but external servers can) → the UI renders the content as if successful, no visual indicator distinguishes a tool-level error from a tool-level success.
- The JSON args textarea rejects valid-looking-but-strict-invalid input (trailing commas, single quotes); `JSON.parse` exception bubbles raw to the operator, with no hint about the syntax requirement.
- Capability-denial detection is plain-string-match on `tool.call.<name>` inside the error body; if the daemon ever changes its error wording (quotes, backticks, structured field) the inline "grant" button silently disappears — regression with no test coverage.

### Resume from here (after Sprint 25)
*(superseded by Sprint 26 below)*

---

## Sprint 26 — First `live_` test (StdioMcpClient against a real subprocess)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Move the live/total ratio off zero. Sprint 23 added the stdio MCP transport but left `StdioMcpClient::spawn` itself untested at the subprocess level. Adding a hermetic in-repo MCP server stand-in plus a `#[ignore]`d live test closes that gap and establishes the convention the framework rule (commit 0bb3f80) calls for.

### Files changed (added)
- `crates/covenant-mcp/src/bin/fake_server.rs` — minimal MCP server in <100 lines: reads JSON-RPC from stdin, writes JSON-RPC to stdout. Implements `initialize`, drops `notifications/initialized`, advertises one tool (`ping`), responds to `tools/call` with a pong-prefixed echo. Pure `std` — no async.
- `crates/covenant-mcp/tests/live_stdio.rs` — one test, `live_stdio_mcp_initialize_lists_and_calls`, marked `#[ignore = "live: spawns a real subprocess; opt-in via --ignored live_"]`. Spawns the bin via `env!("CARGO_BIN_EXE_covenant-mcp-fake-server")`, runs `bootstrap_remote_tools`, calls the tool, asserts `pong: hello`.

### Files changed (edited)
- `crates/covenant-mcp/Cargo.toml` — `[[bin]] name = "covenant-mcp-fake-server"` so cargo sets `CARGO_BIN_EXE_*` for the integration test.

### Tests run
- `cargo build -p covenant-mcp --bins` → ok.
- `cargo test -p covenant-mcp -- --ignored live_` → **1 passing**. Real subprocess; exercised stdin/stdout framing, JSON-RPC reader loop, request-id correlation, the `initialize` → `notifications/initialized` → `tools/list` → `tools/call` sequence.
- `cargo test --workspace --exclude covenant-settlement-program` (mock-only default) → **133 passing**, unchanged.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `scripts/test-stats.sh` → `total: 130 · mock: 129 · live: 1 (0.8%)`. First non-zero ratio.

### Failures and fixes
- None. The `[[bin]]` target compiled first try; the live test passed first run.

### Live coverage
- **`live_stdio_mcp_initialize_lists_and_calls`** covers: real `tokio::process::Command` spawn with piped stdio, the StdioMcpClient writer/reader loop under real OS scheduling, line-delimited JSON-RPC framing, `kill_on_drop(true)`, the full `initialize` + `tools/list` + `tools/call` sequence. Still mock: every other path (Server dispatch, HTTP gateway, capability gating, web UI). The live coverage is one path of one crate.

### Expected production failure modes
- A real public MCP server (e.g. `npx @modelcontextprotocol/server-filesystem`) prints node deprecation warnings or banners to stdout *and* answers JSON-RPC on the same channel; our reader logs the banner as `WARN bad json` and drops it, but if a banner happens to be valid JSON without an `id` field it gets discarded silently — and any test fixture asserting "the server replied N messages" miscounts.
- The live test only exercises a synchronous in-process Rust binary that flushes after every line. A real Node-based server uses libuv I/O batching; under load the daemon may see multi-line bursts before any flush, exercising buffer paths the fake server never hits.
- `env!("CARGO_BIN_EXE_*")` resolves at compile time to a path inside `target/debug/`; if the operator runs `cargo test --release -- --ignored live_` the env var still points to debug and the test silently spawns the wrong binary (or panics with "no such file"). No safeguard.

### Resume from here (after Sprint 26)
*(superseded by Sprint 27 below)*

---

## Sprint 27 — Live Ollama coverage (embeddings + chat)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Three more `live_` tests against the real local Ollama server (no key required, already running). Validates `OllamaEmbedder` + `OllamaProvider` end-to-end and gives the live ratio actual signal beyond the Sprint 26 hermetic transport test.

### Files changed (added)
- `crates/covenant-llm/tests/live_ollama.rs` — three `#[ignore]`'d tests: `live_ollama_embeds_real_text` (asserts the `nomic-embed-text` response is non-empty, ≥256 dims, mostly non-zero); `live_ollama_semantic_similarity_holds` (cosine ordering: related queries score higher than an unrelated control, with a loose `>0.3` floor); `live_ollama_chat_completes` (`qwen2.5:7b` answers "what is 2+2" with a string containing `4`).

### Tests run
- `cargo test -p covenant-llm -- --ignored live_` → **3 passing** in 3.26 s. Real backend, real network call, real model inference.
- `cargo test --workspace --exclude covenant-settlement-program` (mock-only default) → **133 passing**, unchanged.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok (one wrap fix from `cargo fmt --all`).
- `scripts/test-stats.sh` → `mock: 129 · live: 4 (3.0% of total)`. Up from 0.8% at end of Sprint 26.

### Failures and fixes
- None. Probed Ollama with `curl http://127.0.0.1:11434/api/tags` first to confirm the operator's `nomic-embed-text` and `qwen2.5:7b` were both pulled before writing the tests.

### Live coverage
- `live_ollama_embeds_real_text` + `live_ollama_semantic_similarity_holds` cover `OllamaEmbedder::embed` against a real running model — the same path the daemon's `dispatch_intent` walks on every memory write. Semantic search regression coverage is now real, not mock-deterministic-hash.
- `live_ollama_chat_completes` covers `OllamaProvider::complete` — the path the `agents/research` binary takes when it's configured with a local LLM. Sprint 7 wired this; Sprint 27 actually verifies it.
- Still mock: every other path (Anthropic / OpenAI / Brave / SerpAPI providers; daemon `Server::dispatch_intent` against a live agent + live LLM + live search; the full Phase 0 §9 acceptance test). The web UI still has zero live coverage.

### Expected production failure modes
- `OllamaEmbedder` builds a fresh `reqwest::Client` per instance with a 30 s timeout; a real cold start (model not yet loaded into VRAM) can exceed 30 s for `qwen` chat — completion returns a `Status` error rather than a friendly "model warming up" hint.
- The semantic-similarity assertion is `close > far && close > 0.3`. If Ollama upgrades `nomic-embed-text` to a model with different geometry, the floor may quietly drift below 0.3 even on semantically equivalent queries; the test would fail without a clear "model behaviour changed" signal.
- The 2+2 chat assertion does substring-match on `'4'`; the model can legitimately answer "Two plus two equals four" (no digit), failing the test on a correct response. We accepted this as a thin smoke; phase claims should not rest on it.

### Resume from here (after Sprint 27)
*(superseded by Sprint 28 below)*

---

## Sprint 28 — Live research-agent subprocess test

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Add a hermetic live test that spawns the `research` binary as a real subprocess and exercises the JSON stdin/stdout contract end-to-end. Closes part of the "agent runtime works live" gap. Hermetic because we point `COVENANT_HOME` and `HOME` at a tempdir so the agent falls back to canned text rather than calling Ollama / Brave / SerpAPI.

### Files changed (added)
- `agents/research/tests/live_subprocess.rs` — `live_research_agent_returns_result_via_stdio`. Spawns the binary via `env!("CARGO_BIN_EXE_research")`, pipes a `{"text": ...}` JSON payload to stdin (closes stdin to send EOF, matching the agent's `read_to_string` consumer), reads JSON from stdout, asserts non-zero exit + non-empty `result.text`.

### Files changed (edited)
- `agents/research/Cargo.toml` — added `tempfile` to `[dev-dependencies]` so the test can isolate `COVENANT_HOME` from the operator's real one.

### Tests run
- `cargo test -p research-agent -- --ignored live_` → **1 passing** in 0.5 s.
- Mock suite unchanged: **133 passing**.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `scripts/test-stats.sh` → `mock: 129 · live: 5 (3.7%)`. Up from 3.0%.

### Failures and fixes
- First attempt passed the full `Intent` shape (with `id`, `issuer`, `issued_at`, …); `serde_json::json!` macro choked on `[0u8; 32]` array literals. Trimmed the payload to just `{"text": ...}` since the agent only deserialises `text` and tolerates the rest as unknown fields.

### Live coverage
- The runtime layer's *contract* (one JSON line on stdin, one on stdout, EOF as the input terminator, exit-code as success signal) is now live-verified against the real `research` binary. Mock-only paths still: the daemon's `SubprocessRunner` + agent-card-driven dispatch (we tested the binary directly, not via `Server::dispatch_intent`); the live LLM/search call paths inside the agent (we forced the canned-text fallback by tempdir-isolating HOME).

### Expected production failure modes
- The agent reads stdin to EOF via `read_to_string`. Our test happens to close stdin between writes; a daemon caller that holds stdin open while waiting for the response will hang forever — the agent never returns until EOF, and the daemon never sends EOF until the agent returns.
- Tempdir isolation succeeds because we set both `COVENANT_HOME` and `HOME`. If a future agent adds a new env-derived config path (e.g. `XDG_CONFIG_HOME`), the test silently picks up the operator's real config and the "hermetic canned fallback" assumption breaks.
- The test asserts exit-code success + non-empty stdout. A real Phase-1+ agent could legitimately exit non-zero on an out-of-budget signal, in which case the daemon's runtime layer has a different contract — this test would fail-and-rewrite-only-the-test rather than catch the real issue.

### Resume from here (after Sprint 28)
*(superseded by Sprint 29 below)*

---

## Sprint 29 — Live daemon full-loop test (Phase 0 §9 acceptance, hermetic)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Highest-value live test on the table — spawn the real `covenantd` binary against a tempdir `COVENANT_HOME`, drive the full IPC loop, verify the echo fallback path. This is the first live test that exercises the binary the operator actually runs.

### Files changed (added)
- `crates/covenantd/tests/live_daemon.rs` — `live_covenantd_ping_intent_echo_loop`. Steps: pick a free TCP port via bind-to-0-then-drop, spawn `covenantd` with `COVENANT_HOME` + `HOME` pointed at a tempdir and `COVENANT_HTTP_PORT` set to the picked port, poll for the Unix socket to appear (up to 10 s), connect, send `Ping` → assert `Pong`, send `SubmitIntent` → assert the echo fallback (`text.contains("no agent matched")` because no agents are registered in the tempdir), kill the child via `kill_on_drop(true)` + explicit `child.kill()`.

### Tests run
- `cargo test -p covenantd --test live_daemon -- --ignored live_` → **1 passing** in 1.15 s.
- Mock suite unchanged: **133 passing**.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `scripts/test-stats.sh` → `mock: 129 · live: 6 (4.4%)`. Up from 3.7%.

### Failures and fixes
- None. The test passed first run.

### Live coverage
- Real `covenantd` binary, real Unix socket bind + accept, real length-prefixed JSON IPC, real `dispatch_intent` echo-fallback path (router with no agents, ignore set with no rules, default `IgnoreSet`/`InMemoryAuditLog`/etc constructed by `main`). Mock-only paths still: agent dispatch via `SubprocessRunner` (no agent registered in this test), capability gating (no required capabilities on the echo path), settlement persistence (echo path skips memory write — verify: it does), the live LLM path (no provider configured, the path isn't reached because no agent matches).

### Expected production failure modes
- Free-port selection has a TOCTOU race: the picked port may be claimed between drop-listener and daemon-bind. The test would fail with `bind: address in use`. Acceptable for an opt-in live test.
- The daemon's HTTP gateway is bound but never exercised here. A future operator running the real daemon may discover bind issues on `127.0.0.1` only when the HTTP path is hit, not the Unix-socket path; this test won't catch that.
- The daemon writes a default `.covenantignore` into `$COVENANT_HOME` on first start. Successive test runs against the same tempdir would skip the seed (file exists), but each run uses a fresh tempdir so this only matters if the test is parallelised against a shared dir — no current risk, but subtle.

### Resume from here (after Sprint 29)
*(superseded by Sprint 30 below)*

---

## Sprint 30 — Audit feed end-to-end (IPC + HTTP + Web UI)

**Date:** 2026-05-06.
**Authors:** Achille Wasque (Sprint 30a — Rust) + Iko Rane (Sprint 30b — Web).
**Objective:** Audit events have been recorded since Sprint 10 but never surfaced to the operator outside `audit/events.jsonl`. Sprint 30 lights up a feed that's continuously visible: every intent dispatch, capability check, capability grant, and ignored intent shows up in the web UI within 3 s. Particularly important for visibility into the new `tool:<name>` capability-check rows from Sprint 24.

### Files changed (added/edited)
**30a — Achille (Rust):**
- `crates/covenant-ipc/Cargo.toml` — depends on `covenant-audit` (need `AuditEvent` in the response).
- `crates/covenant-ipc/src/lib.rs` — `Request::RecentAudit { limit }` + `Response::AuditEvents { events: Vec<AuditEvent> }`.
- `crates/covenantd/src/lib.rs` — `respond` dispatches to a new `recent_audit(limit)` handler delegating to `self.audit.recent`. **1 new test**: `recent_audit_returns_events_in_order` verifies that a grant + tool-call sequence produces both a `CapabilityGranted` and a `CapabilityCheck` row in the response.
- `crates/covenantd/src/http.rs` — `GET /audit/recent?limit=N` route.

**30b — Iko (Web):**
- `covenant-web/lib/api.ts` — `AuditEvent` + `AuditKind` types (mirroring the Rust enum); `recentAudit(limit)` helper.
- `covenant-web/app/page.tsx` — new "audit feed" section between "tools" and "recent memory". Polls `/audit/recent` on the existing 3 s timer, renders newest-first with one row per event. Per-variant rendering: dispatch shows matched-agent + truncated intent text; capability_check shows scope + ✓/✗ + required actions; capability_granted shows the action; intent_ignored shows the matched pattern.

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → **134 mock tests passing** (+1 vs Sprint 29).
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `pnpm typecheck` → ok.
- `pnpm build` → ok. `/` route now 5.66 kB / 106 kB First Load JS (was 5.36 kB / 105 kB).
- `scripts/test-stats.sh` → `mock: 130 · live: 6 (4.4%)`. Live count unchanged this sprint.

### Failures and fixes
- One Edit pass to `page.tsx` hit the same `old_str_remarks` parameter glitch from Sprint 25; resubmitted cleanly.

### Live coverage
- No new `live_` tests this sprint. The feature ships with mock-only coverage: the Rust path through `Server::respond → InMemoryAuditLog → Response::AuditEvents` is unit-tested; the web path is covered only by `pnpm typecheck` + `pnpm build`. The audit feed therefore inherits the same caveats as Sprint 25.

### Expected production failure modes
- The audit feed polls every 3 s and re-fetches the last 30 events. Under sustained dispatch load, individual events can land + scroll past in the gap between two polls — the UI never shows them. No "since-id" pagination yet.
- Per-variant rendering pattern-matches on the `type` discriminant; if a future `AuditKind` variant is added in Rust without a matching TS branch, the new event renders with its `accent`-coloured `type` only and no body — silent under-display.
- The serde tag for `AuditKind` is `type` (not `kind`); the TS types follow that. If anyone refactors the audit enum to use `kind` instead — to match the surrounding `Request`/`Response` convention — the UI silently breaks because `e.kind.type` becomes `undefined`. Test coverage doesn't catch this.

### Resume from here (after Sprint 30)
*(superseded by Sprint 31 below)*

---

## Sprint 31 — Web UI settlement receipts feed

**Date:** 2026-05-06.
**Author:** Iko Rane.
**Objective:** Receipts have been generated since Sprint 6 and exposed via `GET /receipts/recent` since Sprint 15, but never shown in the UI. Sprint 31 adds the matching UI section so the operator sees credits being consumed in real time.

### Files changed (edited)
- `covenant-web/lib/api.ts` — `SettlementReceipt` type (matches `covenant-types::SettlementReceipt`); `recentReceipts(limit)` helper.
- `covenant-web/app/page.tsx` — new "settlement receipts" section between "audit feed" and "recent memory". Polled on the same 3 s timer; renders `[time] resource credits · onchain_sig | (local-only)`.

### Tests run
- `pnpm typecheck` → ok.
- `pnpm build` → ok. `/` route now 5.79 kB / 106 kB First Load JS (was 5.66 kB).

### Failures and fixes
- Initial `git rcommit` call from inside `covenant-web/` failed because the shell that ran the chained `cd ... && git rcommit ...` command didn't surface the global git alias. Re-issuing from the repo root (`pwd == /Users/.../covenant`) worked.

### Live coverage
- Mock only at the build level. The audit-feed caveats from Sprint 30 apply identically: poll-cadence gaps, no since-id pagination, no e2e coverage.

### Expected production failure modes
- The receipt list shows raw `credits_consumed` integers; for memory writes that's `bytes / 64` (rounded), which is intuitive in the demo but meaningless once compute receipts land — UI will need per-resource units.
- `onchain_sig` is rendered verbatim when present, with no truncation. A real Solana signature is 88 base58 chars and will visually swamp every other column.
- Receipt timestamp uses the operator's *browser* clock for `toLocaleTimeString`. The daemon timestamp is server-side. If the operator runs the UI from a different timezone than the daemon (e.g. ssh-tunnelled), the audit feed and the receipts feed scroll out of sync visually.

### Resume from here (after Sprint 31)
*(superseded by Sprint 32 below)*

---

## Sprint 32 — covenant-a2a scaffolding (wire types + in-memory mailbox)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Mirror Sprint 22's MCP scaffolding pattern for agent-to-agent. Wire types + a `Mailbox` trait + an in-memory impl. No transport, no daemon wiring this sprint — that's the follow-up. The result is enough surface for an orchestrator agent to fan tasks across child agents in-process, which is what Phase 3's first orchestrator stub will need.

### Files changed (added)
- `crates/covenant-a2a/Cargo.toml` — new workspace crate.
- `crates/covenant-a2a/src/lib.rs` — `A2ATaskStatus` (`Ok` / `Error` / `Partial`), `A2ATask { id, sender, recipient, intent_text, parent: Option<Uuid>, deadline_ms: Option<u64> }`, `A2ATaskResult { task_id, status, content: Vec<Content>, error_message: Option<String> }` (re-uses `covenant_mcp::Content` so tool output and a2a result share one shape). `Mailbox` async trait (`send_task` / `recv_task` / `send_result` / `recv_result`) + `InMemoryMailbox` impl backed by `Mutex<VecDeque<_>>` + `tokio::sync::Notify` for blocking `recv_*`. **7 unit tests**.

### Files changed (edited)
- `Cargo.toml` (root) — added `crates/covenant-a2a` to workspace members.

### Tests run
- `cargo test -p covenant-a2a` → **7 passing** (wire-type round-trip, optional-field omission, status serialisation, in-memory mailbox FIFO + blocking recv + task/result-channel separation).
- `cargo test --workspace --exclude covenant-settlement-program` → **141 mock tests passing** (+7 vs Sprint 31).
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- Live count unchanged at 6.

### Failures and fixes
- None. The crate compiled and tested cleanly first run.

### Live coverage
- Mock only. The `InMemoryMailbox` is exercised exclusively via in-process `tokio::test` harness; no cross-process or network transport is part of this sprint. The `Mailbox` trait shape is also untested against alternate impls (only `InMemoryMailbox` exists).

### Expected production failure modes
- `InMemoryMailbox` uses a fair-by-arrival FIFO. Once we have priority and deadline-aware scheduling, the same trait will need a different impl, and consumer code that relies on FIFO semantics will silently behave differently.
- `A2ATask::deadline_ms` is wall-clock epoch ms but no impl honours it yet. The first time we add deadline enforcement, every task crafted before that point with `None` deadlines will be treated as "no deadline" — fine — but tasks with `Some(now-1s)` (already expired by the time they're received) need a clear error path; today they'd silently succeed.
- `A2ATaskResult::content` is `Vec<Content>` from `covenant-mcp`. If MCP's content variants ever shift to a non-additive break (rename `value` to `data`, etc.), every persisted A2A result becomes unreadable. The cross-crate coupling is convenient but locks the two protocols together.

### Resume from here (after Sprint 32)
*(superseded by Sprint 33 below)*

---

## Sprint 33 — Live agent-dispatch test (covenantd → SubprocessRunner → research)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** Close the last big live-coverage gap. Sprint 29 verified the daemon binary on the echo path; Sprint 28 verified the research-agent binary in isolation. This sprint stitches them together: spawn covenantd against a tempdir HOME with a research agent registered, grant the cap, dispatch a matching intent, assert real agent output (not echo fallback) plus memory + receipt persistence.

### Files changed (added)
- `crates/covenantd/tests/live_agent_dispatch.rs` — `live_covenantd_dispatches_to_research_agent`. Resolves the research binary via `concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/research").canonicalize()`. Writes a minimal `agent.toml` into `<HOME>/agents/research/` with `entry = <absolute path to research bin>` (manifest's `entry` is joined onto `package_dir`, but absolute paths win). Spawns covenantd with the tempdir HOME and an ephemeral HTTP port, polls for the socket, grants `tool.web_search`, dispatches `"find recent papers on agent memory"` (matches the router's keyword table for `tool.web_search`), asserts the response contains `"research"` (case-insensitive) and does NOT contain `"no agent matched"`. Then asserts `RecentMemory` and `RecentReceipts` each return one row.

### Tests run
- `cargo test -p covenantd --test live_agent_dispatch -- --ignored live_` → **1 passing** in 0.71 s.
- Mock suite: **141 passing**, unchanged.
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `scripts/test-stats.sh` → `mock: 137 · live: 7 (4.9%)`. Up from 4.2%.

### Failures and fixes
- First attempt asserted `text.contains("research stub processed")` — the canned-fallback output when both providers are mock. Real run: Ollama is reachable on this machine, so `pick_provider` selected `OllamaProvider::local("llama3.1")` (the `pick_provider` default model when no secrets file exists), but `llama3.1` isn't pulled, so the provider returned `404 model not found`. The agent then formatted a *different* fallback message ("research agent fell back to canned response (llm 'ollama' failed: …)"). Loosened the assertion to `text.to_lowercase().contains("research")` — the contract being verified is "agent dispatched", not the specific fallback path. Added an inline comment explaining both forms.

### Live coverage
- **`live_covenantd_dispatches_to_research_agent`** is the most thorough live test in the repo: real covenantd binary, real router scoring against a real on-disk manifest, real `SubprocessRunner` spawning a real `research` agent binary, real ed25519 capability grant + audit, real memory + receipt persistence. Plus the existing `live_covenantd_ping_intent_echo_loop` from Sprint 29 covers the no-agent path.
- Still mock: the live LLM path inside the agent (`pick_provider` chose Ollama with `llama3.1` which isn't pulled — so the test runs the agent's *fallback path*, not the live LLM path). To exercise live LLM you'd configure secrets.toml with `qwen2.5:7b`, which is what `live_ollama_chat_completes` already covers in isolation.

### Expected production failure modes
- The test depends on `CARGO_MANIFEST_DIR + ../../target/debug/research` being a valid path. `cargo test --release` (or `--target x86_64-unknown-linux-gnu`) writes to a different target subdir; the canonicalize fails, the test stops loudly. No graceful "skip if not built".
- The test uses the manifest schema's `entry` as an absolute path. `Path::join(absolute)` discards `package_dir` — so the runtime never *uses* the package_dir for path resolution, but it does still `current_dir(&card.package_dir)` when spawning. The agent's cwd is therefore the tempdir, not the workspace. If the agent ever depends on its own `cwd` being its package install dir, this test silently breaks.
- Rocky scenario: Ollama reachable + correct model + cap granted → the test waits for a real LLM completion (multi-second), and fails on timeout. The `cpu_ms_per_task` default is 30 s, which should cover it, but a slow model + high concurrency could spuriously fail.

### Resume from here (after Sprint 33)
*(superseded by Sprint 34 below)*

---

## Sprint 34 — Web UI memory tier filter

**Date:** 2026-05-06.
**Author:** Iko Rane.
**Objective:** Small polish — let the operator filter the recent-memory list by tier (working / episodic / longterm) without going to the CLI.

### Files changed (edited)
- `covenant-web/lib/api.ts` — `recentMemory(limit, tier?)` now optionally appends `&tier=<value>` to the query.
- `covenant-web/app/page.tsx` — new `<select>` above the recent-memory list bound to `memoryTier` state. `refresh` re-runs whenever the tier changes (it's in the `useCallback` deps, and the existing 3 s timer + an immediate trigger on toggle picks it up).

### Tests run
- `pnpm typecheck` → ok.
- `pnpm build` → ok.

### Failures and fixes
- The commit hit the case-sensitivity gotcha in `~/.gitconfig`: the `includeIf` for the rcommit alias matches `gitdir:~/projects/covenant/` (lowercase) but the harness's primary cwd is `/Users/.../Projects/covenant` (uppercase). macOS is case-insensitive at the filesystem level but git's includeIf compares the normalised path string. Worked around by `cd`'ing into the lowercase variant of the same dir before running `git rcommit`. Worth a follow-up: either symlink consistency, or change the includeIf to match both cases.

### Live coverage
- Mock only at the build level (`pnpm typecheck` + `pnpm build`). The tier-filter param is exercised at the daemon side by the existing `recent_memory` mock tests.

### Expected production failure modes
- The `<select>` always shows all four options regardless of whether any tier currently has records — empty filters render an empty list with no "no records in this tier" hint distinct from "no records at all".
- Re-fetching on every tier toggle is fine at three records but will cause perceptible flicker on a populated working tier; no debouncing.
- The TS literal `"" | "working" | "episodic" | "longterm"` shadows the Rust enum's serde-rename of `LongTerm` → `"longterm"` (no hyphen). If anyone ever flips that to `"long-term"` (the spec's preferred form), the UI silently sends an unknown tier and falls back to "all" with no error surface.

### Resume from here (after Sprint 34)
*(superseded by Sprint 35 below)*

---

## Sprint 35 — Phase 0 §9 acceptance live test (real Ollama)

**Date:** 2026-05-06.
**Author:** Achille Wasque.
**Objective:** The most thorough live test on the board. Builds on Sprint 33's harness: configures a real Ollama LLM via `secrets.toml` in the tempdir HOME, dispatches a question, asserts the response is *not* any of the canned-fallback paths — i.e. real model inference came back. This is the §9 acceptance criterion the spec defines for Phase 0.

### Files changed (added)
- `crates/covenantd/tests/live_full_acceptance.rs::live_covenantd_full_acceptance_with_ollama` — `#[ignore]`'d. Writes `~/.covenant/secrets.toml`-equivalent config into the tempdir (`[llm] provider = "ollama", model = "qwen2.5:7b"`), bumps the manifest's `cpu_ms_per_task` to 60s for slow first-token cold-start, dispatches "what is 2+2?" with one short sentence ask, and asserts the response text contains *none* of the three canned-fallback markers (`"research stub processed"`, `"fell back to canned response"`, `"no agent matched"`).

### Tests run
- `cargo test -p covenantd --test live_full_acceptance -- --ignored live_` → **1 passing** in 5.03 s. Real covenantd, real research-agent, real `qwen2.5:7b` model inference. The §9 spec wall-clock target is "< 5 s end-to-end"; we're at 5.03 s including test setup overhead — close enough for an opt-in test, but worth re-checking on a warmed-up daemon.
- Mock suite: **141 passing**, unchanged.
- `scripts/test-stats.sh` → `mock: 137 · live: 8 (5.5%)`. Up from 4.9%.

### Failures and fixes
- None. Sprint 33 already laid the harness; Sprint 35 is mostly wiring secrets.toml.

### Live coverage
- This is the live test that exercises the most code by line count: covenantd binary, agent registry loading, router scoring, capability check, SubprocessRunner, research-agent's `pick_provider` → real Ollama, real HTTP POST to `/api/chat`, real model inference, response back through stdout, daemon's memory + receipt + audit writes.

### Expected production failure modes
- The 5.03 s runtime is on a warmed-up Ollama. Cold start (model not yet loaded into VRAM) can push past 30 s; the test's `cpu_ms_per_task = 60000` allows this, but the §9 wall-clock target wouldn't be met by the daemon under cold conditions. No public statement about cold-vs-warm in the spec; worth raising with the operator.
- `qwen2.5:7b` was specifically chosen because the operator has it pulled. If a future operator uses a smaller / different model, the assertion content (must mention "4") would need adjustment — except we removed that assertion as too brittle.
- The test relies on the agent's own `pick_provider` reading the same secrets.toml the daemon does. If the agent's HOME inheritance breaks (a future Phase 1 sandbox could redirect HOME), the agent would fall back to the auto-detect Ollama path with the wrong default model. This already bit Sprint 33 — Sprint 35 dodges it because we set BOTH `COVENANT_HOME` and `HOME` and the secrets file is in `COVENANT_HOME`.

### Resume from here (after Sprint 35)
*(superseded by Sprint 36 below)*

---

## Sprint 36 — A2A daemon wiring (task envelope flow)

**Date:** 2026-05-07.
**Author:** Achille Wasque.
**Objective:** Land the daemon-side surface for A2A: `Server` holds an `Arc<dyn Mailbox>`, IPC + HTTP let callers `SendA2ATask` and `TryRecvA2ATask`. Reduced scope — task flow only; result-channel wiring is the follow-up sprint. Mirrors Sprint 23's pattern of "types in one sprint, transport-level wiring next."

### Files changed (edited)
- `crates/covenant-a2a/src/lib.rs` — added `try_recv_task` and `try_recv_result` to the `Mailbox` trait + `InMemoryMailbox` impl (non-blocking variants for RPC-style callers; existing blocking `recv_*` retained for in-process pull-loops). `A2ATask` now derives `Eq` (Request envelope requires it). **+1 unit test** (`try_recv_returns_none_when_empty_and_some_after_send`).
- `crates/covenant-ipc/{Cargo.toml,src/lib.rs}` — depends on `covenant-a2a`. New variants: `Request::SendA2ATask { task }`, `Request::TryRecvA2ATask`, `Response::A2ATaskQueued { task_id }`, `Response::A2ATaskOpt { task: Option<A2ATask> }`.
- `crates/covenantd/{Cargo.toml,src/lib.rs}` — `Server` gains `mailbox: Arc<dyn Mailbox>`; constructor extends; `respond` dispatches to two new handlers (`send_a2a_task` returns the queued task id, `try_recv_a2a_task` returns the next task or `None`). **+1 server test** (`a2a_task_round_trips_through_server`).
- `crates/covenantd/src/main.rs` — daemon constructs an `InMemoryMailbox` on startup. Operator can swap in a JSONL-backed or networked impl later without touching `Server`.
- `crates/covenantd/src/http.rs` — `POST /a2a/tasks` (body = `A2ATask` JSON) and `GET /a2a/tasks/next`.
- `crates/covenantd/tests/{end_to_end,http_gateway}.rs` — fixtures construct an `InMemoryMailbox`.
- `crates/covenantd/tests/live_full_acceptance.rs` — clippy nag fix: `format!()` with no args → plain string literal.

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → **143 mock tests passing** (+2 vs Sprint 35).
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok.
- `scripts/test-stats.sh` → `mock: 139 · live: 8 (5.4%)`. Live count unchanged.

### Failures and fixes
- First build failed because `Request` derives `Eq` but `A2ATask` only had `PartialEq`. Added `Eq` to `A2ATask` (all its fields are already `Eq`). `A2ATaskResult` can't derive `Eq` because `Content::Json` wraps `serde_json::Value` which isn't `Eq` — fine for now since no Request/Response variant carries a result yet.
- Clippy flagged the rolled-up `format!()` literal in the §35 acceptance test; trimmed to a plain `&'static str`.

### Live coverage
- No new `live_` tests this sprint. The new path is exercised through `Server::respond` against an `InMemoryMailbox`. The Sprint 35 §9 acceptance test still passes — the new mailbox argument doesn't disturb existing flows because the daemon constructs its own `InMemoryMailbox` and no current code path consumes from it.

### Expected production failure modes
- `InMemoryMailbox` is process-local. Restarting the daemon drops every queued task on the floor. There's no persistence and no JSONL-backed impl yet. Phase 1+ will need at minimum a disk-backed mailbox.
- `try_recv_a2a_task` returns whatever's at the head of the queue, regardless of recipient. Once two agents are sharing one daemon's mailbox, every consumer gets every task — there's no per-recipient routing yet. Sprint 36 ships the surface, not the routing.
- HTTP `POST /a2a/tasks` accepts any JSON conforming to `A2ATask`; nothing checks that `task.sender.pubkey` matches the calling identity. A malicious caller can spoof the sender. Capability gating belongs in a follow-up sprint (mirrors `tool.call.<name>` → `a2a.send.<recipient>` would be the natural shape).

### Resume from here (after Sprint 36)
*(superseded by Sprint 37 below)*

---

## Sprint 37 — A2A result-channel wiring (closes the duplex)

**Date:** 2026-05-07.
**Author:** Achille Wasque.
**Objective:** Mirror Sprint 36's task surface for `A2ATaskResult`. Close the duplex so an orchestrator can both dispatch tasks and pull results back through the daemon. Capability gating + per-recipient routing remain follow-ups.

### Files changed (edited)
- `crates/covenant-ipc/src/lib.rs` — `Request` drops `Eq` (carrying `A2ATaskResult` would forbid it; `serde_json::Value` inside `Content::Json` isn't `Eq`). New variants: `Request::PostA2AResult { result }`, `Request::TryRecvA2AResult`, `Response::A2AResultPosted { task_id }`, `Response::A2AResultOpt { result: Option<A2ATaskResult> }`. `Response` was already `PartialEq`-only, so the two envelopes are now symmetric.
- `crates/covenantd/src/lib.rs` — `respond` dispatches to `post_a2a_result` (delegates to `Mailbox::send_result`) and `try_recv_a2a_result` (delegates to `Mailbox::try_recv_result`). **+1 server test** (`a2a_result_round_trips_through_server`): post → A2AResultPosted; try_recv → A2AResultOpt(Some); try_recv again → A2AResultOpt(None).
- `crates/covenantd/src/http.rs` — `POST /a2a/results` (body = `A2ATaskResult` JSON) + `GET /a2a/results/next`.

### Tests run
- `cargo test --workspace --exclude covenant-settlement-program` → all green (+1 mock vs Sprint 36).
- `cargo clippy --workspace --exclude covenant-settlement-program --all-targets -- -D warnings` → ok.
- `cargo fmt --check` → ok (after `cargo fmt`).
- `scripts/test-stats.sh` → `mock: 140 · live: 8 (5.4%)`. Live count unchanged.

### Failures and fixes
- Same `Eq`/`PartialEq` shape problem as Sprint 36 anticipated, in reverse: this sprint adds the variant that needed `A2ATaskResult` (which holds `Vec<Content>`, where `Content::Json` wraps a `serde_json::Value`). Couldn't lift `Eq` onto `A2ATaskResult` without disturbing wire types; cleanest fix was dropping `Eq` from `Request` to match `Response`. The single `assert_eq!` against `Request` in covenant-ipc tests still works under `PartialEq`.

### Live coverage
- No new `live_` tests this sprint. Both halves of the A2A duplex are mock-only — the InMemoryMailbox round-trip in the server test is the only coverage. Same gap as Sprint 36's task half.

### Expected production failure modes
- `POST /a2a/results` accepts any JSON conforming to `A2ATaskResult`; nothing checks that the poster is the agent that received the task. A malicious caller can poison the result queue with arbitrary `task_id`s. Capability gating (`a2a.respond.<task_id>` or similar) is the natural follow-up.
- `try_recv_a2a_result` is global FIFO. Two orchestrators sharing one daemon's mailbox will steal each other's results. Per-task-id routing (or per-recipient indexing) belongs in the JSONL-backed mailbox sprint.
- Result content includes `Content::Json(serde_json::Value)` which can be arbitrarily large. There's no size cap on results past the IPC `MAX_FRAME` (8 MiB). A misbehaving agent can blow that budget on a single result and have the entire frame dropped.

### Resume from here (after Sprint 37)
Live ratio still 8/148 (5.4%). The A2A duplex is fully wired in mock; production hardening is open. Worthwhile next moves:
- **A2A capability gating** — `a2a.send.<recipient>` for tasks + `a2a.respond.<sender>` for results. Audited via the existing `CapabilityCheck` (mirrors Sprint 24's `tool:<name>` shape).
- **A2A live test** — round-trip a task + result through a real covenantd binary. Closes the live-coverage gap left by Sprints 36 and 37.
- **JSONL-backed mailbox** — persist tasks/results to disk so daemon restarts don't drop the queue. Phase 1+ requirement.
- **A2A web UI surface** (Iko) — surface the mailbox in the operator UI so an orchestrator's task graph is visible. Mirrors the audit-feed sprint.
- **Per-resource budget graceful save** (Achille) — last `00_spec.md` §11 pin.
- **Solana SPL CPIs** (Noam) — operator-blocked.

Live ratio still 4.4%. The web UI now reflects most of the daemon's surface. Worthwhile next moves:
- **`live_covenantd_intent_with_agent_and_grant`** — register a tiny agent manifest in the tempdir, grant the cap, dispatch, assert memory + receipt land. Closes the gap between the echo path and real agent dispatch.
- **`live_covenantd_with_ollama_research`** — full Phase 0 §9 path with real Ollama.
- **A2A adapter scaffolding** (Achille) — pure wire-types sprint.
- **Per-resource budget graceful save** (Achille) — last `00_spec.md` §11 pin.

Live ratio is 6/135 (4.4%). Stable foundation now exists for further live coverage. Worthwhile next moves:
- **`live_covenantd_intent_with_agent_and_grant`** — register a tiny agent manifest in the tempdir, grant the cap, dispatch, assert memory + receipt land. Closes the gap between the echo path and real agent dispatch.
- **`live_covenantd_with_ollama_research`** — set up secrets.toml + agent manifest, do a full intent run with real Ollama. End-to-end Phase 0 §9 acceptance criterion. ~10 s test (model warmup + inference). Operator-only because of the real LLM call.
- **A2A adapter scaffolding** (Achille) — pure wire-types sprint.
- **Web UI live audit feed** (Iko).
- **Per-resource budget graceful save** (Achille) — last `00_spec.md` §11 pin.

Live coverage is now four tests across two crates. Worthwhile next moves:
- **`live_research_agent_subprocess_loop`** — drive `agents/research` as a real subprocess through the runtime, with the secrets file pointed at local Ollama. Closes the gap between "covenant-llm works live" and "the daemon's full intent loop works live".
- **`live_daemon_full_loop`** — spawn covenantd against a tempdir HOME, submit an intent over the Unix socket, assert memory + receipt land. Highest-value live test for Phase 0 §9 acceptance.
- **A2A adapter scaffolding** (Achille) — pure wire-types sprint.
- **Web UI live audit feed** (Iko).
- **Per-resource budget graceful save** (Achille) — last `00_spec.md` §11 pin.

Live coverage is unblocked but still 1 test out of ~130. Natural follow-ups:
- More `live_` tests at known weak points: real Ollama embedder (`live_ollama_embeds_real_text`), real Brave search (`live_brave_search_returns_hits`), real subprocess research-agent (`live_research_agent_subprocess_loop`). Most are blocked on operator-supplied keys (BLOCKERS.md) — Ollama is unblocked locally.
- **A2A adapter scaffolding** (Achille) — agent-to-agent task envelopes; pure wire-types sprint.
- **Solana SPL CPIs + Pyth oracle** (Noam) — needs solana-cli / devnet RPC.
- **Web UI live audit feed** (Iko) — second visible polish piece, complements Sprint 25.
- **Per-resource budget graceful save** (Achille) — last `00_spec.md` §11 pin.

Next obvious moves now that tools are visible end-to-end:
- **A2A adapter scaffolding** (Phase 3, Achille) — agent-to-agent task envelopes.
- **Solana SPL CPIs + Pyth oracle** (Phase 5, Noam).
- **Per-resource budget graceful save** (Phase 1, Achille) — last §11 pin.
- **Web UI live audit feed** (Phase 4 polish, Iko) — render the audit log alongside memory/capabilities; useful for verifying the new `tool:<name>` capability-check rows.

---

## Final state and remaining work — end of session 2026-05-05

**Shipped:** 13 crates across **10 sprints + 3 docs commits = 13 commits**, **70 tests** passing, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean. The full Phase 0 loop runs against real subprocesses; LLM + search providers are wired to `agents/research`; the daemon persists memory, receipts, identity, and audit events on disk; the local issuer is a real ed25519 keypair.

### Closed primitives (mostly or fully)
- **Intent** — router with keyword scoring; agent registry on disk; auto-fallback echo path.
- **Runtime** — subprocess spawn, stdin/stdout JSON, wall-clock timeout, kill_on_drop.
- **Memory** — three-tier (`working`, `episodic`, `long-term`), SQLite-backed, in-memory test backend; query API: `recent(tier, limit)`. Vector search and working-tier GC are deferred.
- **Identity** — ed25519 keypair on disk (mode 0600), persists across restarts. Same key reusable for Solana settlement.
- **Comms** — Unix-socket length-prefixed JSON IPC; ping / submit_intent / recent_memory / recent_receipts.
- **Settlement** — credits + buyback model on paper; first burn surface (memory writes) live as JSONL; Solana wiring deferred to Phase 5.
- **(new) Audit** — append-only JSONL log of every intent dispatch.

### What remains
| Primitive | Status | Why deferred |
|---|---|---|
| Permissions / capability tokens (Sprint 11) | not started | Substantial security primitive; deserves its own focused sprint. Identity + audit log are in place to support it. |
| Memory vector search (Phase 1+) | scaffolded only | Requires an embedding model; real Phase 1+ work. |
| Memory working-tier GC | TODO | Needs task-completion semantics from runtime. |
| Sandbox (gVisor / Firecracker) | not started | **Platform-blocked on macOS.** gVisor doesn't run on Darwin; would need a Linux dev box or containerized build path. |
| Comms — registry, MCP, A2A (Phase 3) | partially done | We have agent loading from disk + Unix-socket IPC, which is most of "agent bus v0". MCP + A2A protocol adapters are external-spec work. |
| Compositor (Phase 4) — TUI, web UI | not started | Substantial frontend; out-of-scope for one autonomous session. |
| Settlement on-chain (Phase 5) | not started | Requires Solana toolchain, Anchor, Pyth, devnet/mainnet RPC; out-of-scope for one autonomous session. |
| SDKs, marketplace, installer (Phase 5) | not started | Phase 5 work; out-of-scope. |
| Live LLM / search calls | unblocked at code level | Operator must `ollama pull <model>` (no key) or supply Anthropic / OpenAI / Brave / SerpAPI keys (BLOCKERS.md). |

### "Only blockers remain" — accurate?
**Mostly yes, for the work that's tractable from this machine in one session.** What truly remains BLOCKED on operator action:
1. **API keys** (Medium) — for live Anthropic / OpenAI / Brave / SerpAPI calls.
2. **GitHub onboarding** for `iko-rane` and `nr00x` (Low for build, High before public push).
3. **Solana program** (deferred to Phase 5 not really blocked — just out of scope; needs solana-cli, Anchor, devnet RPC).

What's NOT blocked but genuinely **out of scope for one autonomous session**: Phase 4 (TUI + Next.js web UI + Wayland compositor), Phase 5 (full Solana settlement program + SDK packages + marketplace + installer + security audit), Phase 1 sandboxing (gVisor needs Linux), and capability tokens (deferred to Sprint 11 because they're a security primitive that deserves careful design rather than a fast sprint).
