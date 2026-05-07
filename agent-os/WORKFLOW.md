# Agentic Workflow

How autonomous multi-agent engineering runs against this repo. This file describes process; the product spec is `00_spec.md`.

## Source of truth (read order)
1. `00_spec.md` — opinionated build spec (8 primitives, 6 phases, locked names, locked decisions). Authoritative.
2. `~/projects/research-agent/research/Agentic OS/` — original positioning + tokenomics PDFs (April 2026). Superseded by `00_spec.md` where they conflict.
3. `PROJECT_STATE.md` — live status. Read after the spec.
4. `SPRINT_LOG.md` tail — what shipped last and what's next.
5. `BLOCKERS.md` — only real human-only blockers.

## Identities & commits
Three pseudonymous personas, routed by `git rcommit` (the wrapper at `~/.local/bin/covenant-commit`):

| Initials | Name | GitHub | Domain |
|---|---|---|---|
| aw | Achille Wasque | `achillewasque` | Default. Rust core, daemon, types, infra, docs |
| ir | Iko Rane | `iko-rane` | Web frontend (Next.js, TSX, CSS) |
| nr | Noam Rook | `nr00x` | Solana programs, Anchor, IDL |

Pre-commit hook (`~/.config/git/covenant-hooks/pre-commit`) blocks: wrong author email, leaked `$USER`/`$HOME`/`hostname`, leaked global git identity. Commit-msg hook redacts the same in messages. `--no-verify` is project-forbidden.

## Sprint loop
1. **Inspect.** Read repo state + tail of SPRINT_LOG.md. Refresh model of what exists.
2. **Plan.** Define one sprint with tight, integratable scope (a few hours of focused work).
3. **Execute.** Direct work for greenfield/scaffolding/cross-module wiring; subagent for review/parallel/scoped tasks.
4. **Validate.** `cargo check --workspace`, `cargo test --workspace`, `cargo fmt --check`, plus per-language tools when relevant.
5. **Integrate.** Wire the new module to the rest. Verify nothing else broke.
6. **Log.** Append to SPRINT_LOG.md, update PROJECT_STATE.md and PRODUCTION_READINESS.md.
7. **Commit** via `git rcommit` (rotation routes by file paths). Mixed-domain commits are split.
8. **Decide next.** Pick the next sprint. Loop.

## Subagent usage
Claude Code exposes Agent types: `Explore` (read-only search), `Plan` (read-only design), `general-purpose` (full tools). The conceptual roles map onto these:

| Conceptual role | Realised as |
|---|---|
| Architect | `Plan` agent for design tradeoffs, or direct |
| Scaffold | Direct (greenfield is faster than briefing) |
| Builder | Direct, or `general-purpose` for isolated modules |
| Integration | Direct (cross-module wiring needs full context) |
| QA / Test | `general-purpose` for test-suite expansion after a feature lands |
| Security | `general-purpose` running the `security-review` skill before sprint commits |
| DevOps / Production | `general-purpose` for CI/build/release work |
| Documentation | Direct, or `Explore` for cross-doc consistency check |

Subagent briefs are self-contained (file paths, line numbers, what to change, why). No "synthesise the result" prompts.

## Review standard
Every sprint's output is reviewed against:
- correctness (does it run?)
- alignment with `00_spec.md`
- simplicity (no premature abstraction)
- testability (can it be exercised?)
- no AI tells (no generic boilerplate, no over-commenting)
- no leaked personal info (hooks enforce, but inspect the diff anyway)
- no fake/stubbed logic without an explicit `// TODO(phase-N)` marker

If weak: fix in the same sprint, or carry to the next with a SPRINT_LOG note.

## Validation suite
Native cargo workflow:
- `cargo check --workspace`
- `cargo test --workspace`
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings` (target — not enforced until Sprint 1 passes)

Per-language addenda when crates exist for them:
- Web (Phase 4+) → `pnpm test`, `pnpm typecheck`, `pnpm build`
- Programs (Phase 5+) → `anchor test`, `anchor build`

## Mock vs live tests (production-readiness signal)

A green `cargo test` run says the *interface* is correct against the test doubles we author. It says nothing about whether the system runs against real backends. To stop that conflation:

- **Test naming convention.** A test that exercises a real backend (real Ollama, real network search, real Solana RPC, real spawned subprocess hitting external APIs) **must** start with `live_`. Tests using `Mock*`, `InMemory*`, or fixture data are mock by default and need no prefix.
- **Reporting.** `scripts/test-stats.sh` prints `mock` / `live` counts alongside the total. The `live / total` ratio is the real production-readiness signal — a sprint that shipped 20 new tests but zero `live_` ones moved interface-correctness, not shipping-correctness.
- **PROJECT_STATE.md test table.** Sprint-close updates split unit / integration / **live** counts in the test-status table.
- **Live tests can be `#[ignore]`d to keep CI fast.** Run them with `cargo test -- --ignored live_` before any phase-completion claim.

Rule of thumb: never write `Phase X complete` in any state file unless at least one `live_` test exercises the path end-to-end.

## Sprint-entry discipline (against "done" overclaiming)

Every sprint entry in `SPRINT_LOG.md` ends with two short sections, both required:

1. **Live coverage.** Which paths in this sprint have a `live_` test? Which still rely entirely on mocks? Be specific (e.g., "MCP `tools/list` — mock only; native `EchoTool` — mock only; no live MCP server has been spoken to over a real subprocess yet").
2. **Expected production failure modes.** Three short bullet points: where this code will break first when run against reality. If you can't articulate three, the sprint hasn't actually understood the work — it has produced scaffold that passes its own tests. Examples: "Ollama process dies mid-stream → embedder returns empty vector → memory write goes through with zero-vector and never matches semantic queries"; "Solana RPC rate-limits the burn ix → settlement audit log diverges from on-chain state".

These two sections gate Phase rollups. Operator-only rule: only the human operator promotes a Phase from open → substantively complete in any state file. The LLM can ship sprints; the operator audits whether they roll up.

## Blocker policy
Real human-only blockers (API keys, business decisions, account creation, payments, destructive prod migrations) go in BLOCKERS.md with a concrete action. Everything else is workaround-able with mocks, stubs, or scope adjustments. A blocker only halts the project when *every* remaining path depends on it.

## Production-readiness
Tracked in PRODUCTION_READINESS.md across 12 columns. Every column is red at Sprint 0; that's expected. Each sprint moves at least one column toward green. "Production-ready" per `00_spec.md` means Phase 5 complete (settlement on-chain, SDKs published, marketplace live, security audit passed, one-line installer).

## Resuming a stopped session
The next Claude Code session reads, in order:
1. `HANDOVER.md` — short pointer + verify commands + continuation rules.
2. `00_spec.md` — re-anchor on the product.
3. `PROJECT_STATE.md` — current snapshot.
4. The tail of `SPRINT_LOG.md` — last sprint's output and the `Resume from here` block.
5. `BLOCKERS.md` — anything new the human must do.

Then it continues from the `Resume from here` instruction.

## Handover protocol (when this session gets heavy)

The autonomous loop can spawn a fresh Claude Code session when it senses the current run has accumulated enough state that a clean context will produce better next-sprint work. Mechanism:

1. **Refresh `HANDOVER.md`** — the "What just happened" line and any environment changes since the last refresh. Keep it thin; it's a *pointer* to `SPRINT_LOG.md`'s tail, not a duplicate.
2. **Run `scripts/handover.sh`**:

       scripts/handover.sh                                            # current dir
       CLAUDE_CMD='claude --dangerously-skip-permissions' scripts/handover.sh

   On macOS this opens a new Terminal window via `osascript` and runs `CLAUDE_CMD` (default: `claude --model claude-opus-4-7 --dangerously-skip-permissions`) with an initial prompt that points at `HANDOVER.md`. On Linux it tries `gnome-terminal` / `kitty` / `alacritty` / `wezterm` / `xterm` in order. The script invokes the `claude` binary directly — zsh aliases like `cc` don't propagate to the spawned non-interactive shell.
3. **Trust prompt auto-confirmed.** macOS branch sends a `return` keystroke via `System Events` ~3s after spawning the tab; the keystroke is harmless if the folder was already trusted (lands in Claude's empty input box). Override the wait via `COVENANT_HANDOVER_TRUST_DELAY=<seconds>` if a slower machine needs longer. Linux branch relies on the trust state being cached for the project folder.
4. **The current session** can either exit cleanly or stay open as a read-only audit window; the new session has full ownership of the next sprint.

When to trigger:
- ≈ 25+ commits in one autonomous run, OR
- A new sprint enters a substantially different domain (MCP spec interpretation, Solana SPL programming, Tailwind migration), OR
- The operator explicitly requests a handover.

When **not** to trigger:
- Mid-sprint (always finish + commit current work first).
- During a smoke test (always tear the test daemon down first).
- When `BLOCKERS.md` shows an unresolved blocker that the next session cannot also work around.
