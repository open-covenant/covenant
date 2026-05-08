# How this is built

This repo is maintained by an autonomous multi-agent engineering loop. The coordination substrate the codebase ships — capability tokens, signed identity, audit ledger, peer auth, settlement — is exercised by the agents that build it. Every artifact is checkable from this clone: file paths, line numbers, signed commits, ledger entries, test ratios.

This document is the entry point for verifying that. It maps the loop's mechanics to the artifacts that prove them.

## The recursive substrate

Most agentic-engineering systems use existing infrastructure (GitHub, JIRA, Slack, OpenAI keys) to coordinate the work of building agents. Here the relationship is closed: the primitives that Covenant ships are the same ones the build loop consumes.

| The loop needs | Covenant crate that ships it | Currently used by the loop |
|---|---|---|
| Cryptographic identity per builder | [`crates/covenant-identity/`](./agent-os/crates/covenant-identity/) | Each persona's commits sign through `~/.local/bin/covenant-commit` and a global pre-commit hook that blocks identity leakage |
| Signed capabilities for tool/action grants | [`crates/covenant-permissions/`](./agent-os/crates/covenant-permissions/) | Same model gates the daemon's IPC + HTTP gateway; the build itself uses `git rcommit` as a capability check on author email |
| Append-only audit ledger | [`crates/covenant-audit/`](./agent-os/crates/covenant-audit/) | Every plan-gate decision and security-review finding is recorded in [`agent-os/SPRINT_LOG.md`](./agent-os/SPRINT_LOG.md), the public engineering ledger |
| Peer auth + token rotation | [`crates/covenant-peer-auth/`](./agent-os/crates/covenant-peer-auth/) | Session-lock primitive prevents two autonomous sessions from racing on the same checkout — see [`hooks/pre-commit`](./hooks/pre-commit) |
| Per-resource settlement | [`crates/covenant-settlement/`](./agent-os/crates/covenant-settlement/) + [`programs/settlement/`](./agent-os/programs/settlement/) | Off-chain credit/burn ledger live; on-chain Solana wiring is scaffolded, not yet running (Phase 5) |
| Agent-to-agent messaging | [`crates/covenant-a2a/`](./agent-os/crates/covenant-a2a/) | Used by the daemon's mailbox and live-tested across daemon restart in [`crates/covenantd/tests/live_restart_a2a.rs`](./agent-os/crates/covenantd/tests/live_restart_a2a.rs) |

The recursion isn't rhetorical. The same ed25519 keypair shape that gates a `tool.call.<name>` capability check at runtime gates an `aw@opencovenant.org` commit at build time. The same "subject + action + signature" canonical form that signs a Covenant `SignedCapability` is the model the workflow uses for granting a persona authorship rights over a file domain.

## The loop

[`agent-os/WORKFLOW.md`](./agent-os/WORKFLOW.md) defines an 11-step sprint loop. Cadence is one sprint per session: a session inspects the repo, plans, executes, validates, runs subagent reviews, integrates, logs to the ledger, commits, pushes, merges, then runs `scripts/handover.sh` to spawn the next session and exits. The next session starts with a clean context window and reads `HANDOVER.md` to pick up where the prior left off.

This is sustained, not a demo. Sprint 0 was 2026-05-05; Sprint 72 landed 2026-05-08. 72 timestamped entries are in the public ledger.

A session runs:

1. **Inspect.** Read `00_spec.md`, `PROJECT_STATE.md`, the tail of `SPRINT_LOG.md`, `BLOCKERS.md`.
2. **Plan.** Define one sprint with tight, integratable scope. If more than one viable architectural option exists, the **Plan-gate** fires (a `Plan` subagent decides between options before any code is written; the decision lands in the sprint entry's "Plan-gate" subsection).
3. **Execute.** Direct work for greenfield/scaffolding/cross-module wiring. The **Fan-out-gate** fires when a sprint touches more than three crates (one subagent per crate or logical unit).
4. **Validate.** `cargo check`, `cargo test --workspace`, `cargo fmt --check`, `cargo clippy -- -D warnings`, plus per-language tools.
5. **Subagent review gate.** If the staged diff modifies anything in the security-sensitive list (`crates/covenant-permissions/`, `crates/covenant-identity/`, `crates/covenant-audit/`, the daemon's identity/audit/settlement plumbing, the Anchor program, or anything reading/writing `$COVENANT_HOME/{identity,capabilities,audit}/`), the **Security-gate** fires — a `general-purpose` subagent runs the `security-review` skill on the staged diff before commit. Findings land in the sprint entry's "Failures and fixes" or "Expected production failure modes".
6. **Integrate.** Wire the new module to the rest. Verify nothing else broke.
7. **Log.** Append to `SPRINT_LOG.md`, update `PROJECT_STATE.md`.
8. **Commit** via the persona-routing wrapper (file-path-based: web → `ir`, Solana → `nr`, default → `aw`).
9. **Push, merge, cleanup.** Push the sprint branch, fast-forward merge to `main`, delete the branch on both ends.
10. **Handover.** Run `scripts/handover.sh`, which generates a fresh `COVENANT_SESSION_ID`, writes it to `.covenant-session-id`, spawns the next session in a new terminal, and exits.

## The three personas

Three pseudonymous engineers own three file domains. Email scoping makes the identities first-class git citizens. The pre-commit hook chain enforces correct attribution.

| Initials | Name | Email | Domain |
|---|---|---|---|
| `aw` | Achille Wasque | `aw@opencovenant.org` | Rust core, daemon, types, workspace infrastructure, internal docs |
| `ir` | Iko Rane | `ir@opencovenant.org` | Web frontend ([`agent-os/covenant-web/`](./agent-os/covenant-web/) — Next.js, React, TSX, CSS) |
| `nr` | Noam Rook | `nr00x@opencovenant.org` | Solana programs ([`agent-os/programs/settlement/`](./agent-os/programs/settlement/) — Anchor, IDL) |

The wrapper at `~/.local/bin/covenant-commit` routes commits by staged file path. Mixed-domain commits get split. The global pre-commit hook at `~/.config/git/covenant-hooks/pre-commit` blocks commits where the author email doesn't match the domain or where the diff/message leaks `$USER`, `$HOME`, `hostname`, or any string that would deanonymize the operator. `--no-verify` is project-forbidden; the loop never bypasses its own enforcement.

`git log --format='%an <%ae>' | sort -u` shows the rotation in practice.

## The four mandatory gates

The gates are encoded in [`agent-os/WORKFLOW.md`](./agent-os/WORKFLOW.md) and are not optional during the sprint loop.

- **Plan-gate** — fires when a sprint admits more than one viable architectural option. A `Plan` subagent receives the sprint brief plus the named axes (e.g., "where to store the new map: JSONL vs SQLite vs in-memory only"); its reasoning lands in the sprint entry's "Plan-gate" subsection. Catches **traps**: options that look plausible but break an invariant from a prior sprint. Recent example: Sprint 71's plan-gate caught three traps before any code was written, including one that would have broken an audit-renderer contract two sprints downstream.
- **Security-gate** — fires when the staged diff touches identity, capabilities, audit, settlement, the Anchor program, or `$COVENANT_HOME/{identity,capabilities,audit}/` paths. A `general-purpose` subagent runs the `security-review` skill on the diff and returns 0/0/0 (HIGH/MEDIUM/LOW) or a triage list. Findings are closed in the same sprint or carried explicitly. Recent example: Sprint 55's security-review caught a TOCTOU race in peer-registry compaction (MEDIUM) on the first cut; closed in-sprint by swapping the in-memory mutation order, validated by a 2000-iteration concurrent-resolve regression test.
- **Fan-out-gate** — fires when a sprint touches more than three crates. One subagent per crate or logical unit; serial work across more than three crates is forbidden. Forces the loop to either parallelize or split the sprint.
- **Test-expansion-gate** — fires when a sprint adds a new public surface but only covers happy-path tests. A subagent expands the test suite for the documented failure modes named in the sprint entry's "Expected production failure modes" section.

Each gate that fires (and each gate that didn't fire and why) is recorded in the sprint entry. A reader can audit the loop's reasoning without trusting it.

## Honesty markers

The loop is built to make over-claiming hard.

- **Mock vs live tests are syntactically distinct.** [`agent-os/WORKFLOW.md`](./agent-os/WORKFLOW.md) requires every test that exercises a real backend (real Ollama, real subprocess, real Solana RPC) to start with `live_`. The script [`agent-os/scripts/test-stats.sh`](./agent-os/scripts/test-stats.sh) prints both counts. The current ratio (16 live / 363 total = 4.4%) is the production-readiness signal — a green `cargo test` says interfaces are correct against the doubles we authored; a green `cargo test -- --ignored live_` says the system runs against real backends.
- **Phase rollups are operator-only.** The autonomous loop ships sprints; only the human operator promotes a Phase from open → substantively complete in a state file. The rule lives at [`agent-os/WORKFLOW.md`](./agent-os/WORKFLOW.md) under "Sprint-entry discipline." This prevents the loop from grading its own homework.
- **Every sprint declares three expected production failure modes.** Per `WORKFLOW.md`: "If you can't articulate three, the sprint hasn't actually understood the work — it has produced scaffold that passes its own tests." Each sprint entry in `SPRINT_LOG.md` ends with this section. A reader can grade the loop's calibration by checking which predicted failure modes actually fired in later sprints.
- **`Phase X complete` is gated on at least one live test.** Per `WORKFLOW.md`: "never write `Phase X complete` in any state file unless at least one `live_` test exercises the path end-to-end." This is what keeps the loop from over-claiming Phase rollups even before the operator-only check.

## Self-amendment

The loop has demonstrably hardened its own workflow rules in response to past failures. Each amendment is itself a sprint with a ledger entry.

- **Sprint 58 (env-test flake) → Sprint 63 (workflow rule).** Sprint 58 hit silent test flake under `cargo test --workspace` parallelism: a `with_env` save/restore helper races the process-global env-var table. Verified by running the workspace three times and seeing 2/3 fail on different tests. Fix: split env-reading into a pure `Option<&str>` helper plus a one-line wrapper. Sprint 63 codified this as a workflow rule: env-touching code must shape as pure functions, `std::env::set_var`/`remove_var` and save/restore helpers are forbidden in test code.
- **Sprint 58c (parallel-session collision) → session-lock primitive.** A second autonomous session checked out `main` from this session's sprint branch mid-flight. Recovery: `git switch -C` transplanted staged work onto the new tip. Fix: `.covenant-session-id` (gitignored, chmod 0600) + `hooks/pre-commit` enforce one-session-per-checkout. The newer session's spawn flips the file's id; older session's stale env-var fails the hook. The loop's enforcement of its own coordination primitive.
- **Sprint 49 (cross-peer revoke via signature replay) — caught by mid-sprint security-review.** Closed in the same sprint by adding `revoke_capability` checking `list_for_subject(peer.pubkey)` and a `CapabilityRevokeRejected` audit row. The security-gate produced an actionable HIGH finding mid-sprint.
- **Sprint 26 (first `live_` test) — convention codified.** Before Sprint 26, all tests were mock. Sprint 26 shipped `live_stdio_mcp_initialize_lists_and_calls` (real subprocess JSON-RPC). Sprints 27–35 added live coverage to LLM, embeddings, research-agent subprocess, full daemon loop, full Phase-0 acceptance. The discipline that "Phase complete needs at least one live test" comes directly from this sequence.

These aren't claims of self-improvement; they're records of the loop noticing a gap and shipping a rule. Visible in the ledger.

## What this isn't

Defensive disclosure. Don't read these claims into the repo:

- **Not "fully autonomous engineering."** A human operator runs the trust prompts, approves destructive operations, and is the only authority that promotes Phase open → complete. The loop ships sprints; the operator audits whether they roll up.
- **Not "recursive self-improvement" in the Sakana DGM / SICA sense.** Those systems modify their own scaffolding to drive a measurable benchmark delta on unseen tasks. The loop here amends its own workflow rules in response to its own past failures, but it does not re-train, re-prompt, or re-scaffold itself with a held-out eval. That's a strict-er bar; we don't claim it.
- **No SWE-bench numbers, no merge-rate headline.** SWE-bench Verified is contaminated as of Feb 2026; merge-rate without a rework-rate companion is gameable. The signals here (live test ratio, gate-pass rate per sensitive sprint, sprints landed) are reproducible from the repo and don't depend on external leaderboards.
- **Settlement on-chain isn't live.** The Solana program at [`agent-os/programs/settlement/`](./agent-os/programs/settlement/) is scaffolded; receipts are recorded off-chain in JSONL. Phase 5 is open. The roadmap is `00_spec.md`.
- **Cryptographic attestation is local-key, not sigstore.** Each persona signs commits via a wrapper that enforces email scoping and a global pre-commit hook that blocks identity leakage. The keys are real ed25519 keys; they are not yet attested through a sigstore/Fulcio chain to a public registry. That's the named gap below.

## Named gaps

The loop publishes what it doesn't have.

- **Sigstore / Fulcio keyless attestation chain.** GitHub Copilot cloud agent shipped this on 2026-04-03. Adding `sigstore-rs` signing on top of the existing ed25519 personas, attesting to a public registry, and publishing a verifiable chain would move "verifiable autonomous engineering" from claim to checkable artifact. One or two sprints of work; tracked in the ledger.
- **On-chain settlement live.** Phase 5 in `00_spec.md`. Requires Solana toolchain wiring and an SPL-mint launch by the operator (out of the loop's authority).
- **Live test ratio is 4.4%.** The honest signal. Mock interfaces are correct; system-under-real-backends is partially exercised. Worth growing.
- **Phase-1 multi-peer not yet live.** Several sprints have shipped infrastructure for it (peer auth, registry compaction, action-grammar accept-both-shapes for `a2a.{send,recv,respond}.<peer>`), but a second authenticated peer hasn't actually connected to a daemon. The display-collision attack is closed at the check layer; v0 is single-peer until a second peer arrives.

## How to verify

Everything above is checkable from this clone.

```bash
# the public engineering ledger — every sprint, plan-gate decision, security finding
cat agent-os/SPRINT_LOG.md

# the persona rotation
git log --format='%an <%ae>' | sort -u

# the gates
sed -n '/^## Subagent gates/,/^## Review standard/p' agent-os/WORKFLOW.md

# the live-vs-mock test ratio
bash agent-os/scripts/test-stats.sh

# the session-lock and identity hooks
cat hooks/pre-commit

# the substrate
ls agent-os/crates/
```

Open an issue if a claim here doesn't match what's in the repo.
