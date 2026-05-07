# Production Readiness

Status legend: 🟥 not started · 🟨 in progress · 🟩 done.

_Last updated: 2026-05-05 — end of Sprint 14. Phase 1 + Phase 2 substantively complete._

| # | Area | Status | Notes |
|---|---|---|---|
| 1 | Build stability | 🟩 | `cargo build --workspace` green for 14 crates incl. 3 binaries. `cargo clippy --workspace --all-targets -- -D warnings` clean. |
| 2 | Test coverage | 🟨 | 87 tests across 14 crates. Capability lifecycle covered; vector-search cosine + tier-filter covered; semantic retrieval verified live. Promotes to 🟩 with Phase 3 comms adapters and a live search-provider end-to-end. |
| 3 | Runtime configuration | 🟨 | `$COVENANT_HOME` honored by both binaries (default `~/.covenant`). Secrets file path documented in BLOCKERS.md, not yet loaded. |
| 4 | Security posture | 🟩 | Commit rotation + leak-detecting hooks + commit-msg redaction. Daemon uses a real ed25519 local identity (mode 0600, persistent). Capability primitive: sign/verify/expiry/revoke, daemon-signed grants, **hard-enforced at dispatch**. Audit log records every grant + check + dispatch. Sandbox still TODO (gVisor needs Linux; macOS workaround is a platform limitation, not a code blocker). |
| 5 | Error handling | 🟨 | `thiserror` in lib crates, `anyhow` with `Context` in binaries. Boundary-only validation per CLAUDE rules. |
| 6 | Observability | 🟨 | `tracing` + `tracing-subscriber` (`EnvFilter`) wired in `covenantd`. Default level `covenantd=info`. Spans/metrics not yet added. |
| 7 | Deployment | 🟥 | One-line installer is Phase 5. |
| 8 | Documentation | 🟨 | `00_spec.md` + workflow files + README. Per-crate docs minimal (terse-style). |
| 9 | Performance | 🟥 | Phase 0 target: < 5 s end-to-end. Untestable until Sprint 3 (real agent in the loop). |
| 10 | Data integrity | 🟩 | SQLite memory + JSONL receipts + JSONL audit + JSONL caps (granted ⊝ revoked). Real-binary semantic search verified end-to-end with Ollama + `nomic-embed-text`. Working-tier GC still deferred; SQL-side vector index (sqlite-vec / LanceDB) deferred. |
| 11 | External integrations | 🟨 | LLM is **live** end-to-end via Ollama + `qwen2.5:7b` (verified 2026-05-05, ~11 s round-trip). Search still mock; promotes to 🟩 when an operator drops a Brave or SerpAPI key in `~/.covenant/secrets.toml`. Solana settlement wiring is Phase 5. |
| 12 | Release checklist | 🟥 | Versioning, signing, audit — Phase 5. |

## Definition of "production-ready"
Per `00_spec.md`, production-ready means Phase 5 complete: full settlement on-chain, SDKs published, marketplace live, security audit passed, one-line installer working, and the Phase 5 milestone met (per source plan: ≥ 1000 GitHub stars and ≥ 10 community-published agents).

This is many sprints away. Each sprint moves at least one column toward green.

## Movement this sprint
Sprint 1:
- #1 Build: 🟥 → 🟨 (workspace compiles).
- #2 Test: 🟥 → 🟨 (first 10 tests pass).
- #5 Error handling: 🟥 → 🟨 (typed errors in both crates).
- #8 Documentation: 🟥 → 🟨 (workflow files + per-crate header docs).

Sprint 2:
- #1 Build: 🟨 → 🟩 (full workspace incl. 2 binaries; clippy strict clean).
- #2 Test: 🟨 still (now 18 tests incl. an end-to-end; promotes when router/agent tests land).
- #3 Runtime config: 🟥 → 🟨 (`$COVENANT_HOME` plumbed through daemon + CLI).
- #6 Observability: 🟥 → 🟨 (tracing wired in daemon).

Sprint 3:
- #2 Test: 🟨 still (now 27 tests; daemon-with-router lib unit tests added).
- #3 Runtime config: 🟨 still (now also reads `$COVENANT_HOME/agents/*.toml` at startup).

Sprint 4:
- #2 Test: 🟨 still (now 32 tests incl. three real-subprocess paths).
- #3 Runtime config: 🟨 still (`$COVENANT_HOME/agents/<id>/agent.toml` package convention).
- #4 Security posture: 🟨 still — wall-clock timeout + `kill_on_drop` give a coarse safety floor; real isolation lands Phase 1 (gVisor) / Phase 5 (Firecracker).

Sprint 5:
- #2 Test: 🟨 still (now 39 tests incl. memory-persistence and end-to-end memory query).
- #10 Data integrity: 🟥 → 🟨 (SQLite-backed memory, persistence verified, GC still TODO).

Sprint 6:
- #2 Test: 🟨 still (now 44 tests incl. JSONL round-trip and end-to-end receipt query).
- #10 Data integrity: 🟨 → 🟨 (now also persisted JSONL receipts; on-chain flush still TODO Phase 5).

Sprint 7:
- #2 Test: 🟨 still (now 51 tests incl. LLM provider mocks + missing-key paths + secrets-config parse).
- #11 External integrations: 🟥 → 🟨 (LLM provider abstraction unblocked; live Anthropic / OpenAI still need keys; Ollama works without).

Sprint 8:
- #2 Test: 🟨 still (now 60 tests incl. search providers + agent integration).
- #11 External integrations: 🟨 still (now also search abstraction + agent fully wired). Promotes to 🟩 once a live end-to-end is recorded with at least one real provider configured.

Sprint 9:
- #2 Test: 🟨 still (now 65 tests incl. identity sign/verify, persistence).
- #4 Security posture: 🟨 → 🟨 (now real ed25519 issuer instead of zero-pubkey placeholder; sandbox still TODO).

Sprint 10:
- #2 Test: 🟨 still (now 70 tests incl. audit JSONL round-trip and end-to-end audit-write paired with each dispatch).
- #4 Security posture: 🟨 → 🟨 (audit log records every dispatch; capability tokens still TODO; sandbox still platform-blocked on macOS).

Sprint 11:
- #2 Test: 🟨 still (now 76 tests incl. capability sign/verify/expiry, JSONL store round-trip).
- #4 Security posture: 🟨 → 🟨 (capability primitive shipped read-only; enforcement at dispatch lands Sprint 12).

Sprint 12:
- #2 Test: 🟨 still (now 77 tests incl. grant signing + cap-check audit path).
- #4 Security posture: 🟨 → 🟨 (capabilities are now signed by the daemon's local identity; cap-check audit event records every routed dispatch; hard reject lands Sprint 13).

Sprint 13:
- #2 Test: 🟨 still (now 82 tests incl. revocation tombstones + hard-rejection dispatch path).
- #4 Security posture: 🟨 → 🟩 (capability lifecycle complete: grant → check → enforce → revoke → re-reject). Sandbox remains the only "real" v0 security gap and it's platform-blocked on macOS.

Sprint 14:
- #2 Test: 🟨 still (now 87 tests incl. cosine basics + tier-filtered semantic search + mock embedder determinism).
- #10 Data integrity: 🟨 → 🟩 (semantic retrieval verified live; vector index optimisation deferred but functionally complete).
