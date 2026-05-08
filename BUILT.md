# How this is built

Covenant is built and maintained by an autonomous multi-agent engineering loop. The coordination substrate the codebase exposes — capability tokens, signed identity, an append-only audit ledger, peer authentication, settlement primitives — is the same substrate the build loop consumes. This recursion is the project's distinguishing property.

This document describes the engineering architecture: the recursive substrate, the cycle, the persona model, the four-gate review process, the honesty markers, and the gaps still open.

## Recursive substrate

| Primitive shipped by the codebase | Consumed by the build loop as |
|---|---|
| Cryptographic identity ([`crates/covenant-identity/`](./agent-os/crates/covenant-identity/)) | Per-persona ed25519 commit signing through `~/.local/bin/covenant-commit`; a global pre-commit hook blocks identity leakage |
| Signed capabilities ([`crates/covenant-permissions/`](./agent-os/crates/covenant-permissions/)) | The same model gates the daemon's IPC and HTTP gateway; the commit-routing wrapper functions as a capability check on author email |
| Append-only audit ledger ([`crates/covenant-audit/`](./agent-os/crates/covenant-audit/)) | Every plan-gate decision and security-review finding writes a structured row at the daemon level; the loop's bookkeeping mirrors the same shape |
| Peer authentication and token rotation ([`crates/covenant-peer-auth/`](./agent-os/crates/covenant-peer-auth/)) | A session-lock primitive, [`hooks/pre-commit`](./hooks/pre-commit), prevents two autonomous sessions from racing on the same checkout |
| Per-resource settlement ([`crates/covenant-settlement/`](./agent-os/crates/covenant-settlement/), [`programs/settlement/`](./agent-os/programs/settlement/)) | Off-chain credit/burn ledger live; on-chain Solana wiring is scaffolded, not yet running |
| Agent-to-agent messaging ([`crates/covenant-a2a/`](./agent-os/crates/covenant-a2a/)) | The daemon's mailbox; live-tested across daemon restart at [`crates/covenantd/tests/live_restart_a2a.rs`](./agent-os/crates/covenantd/tests/live_restart_a2a.rs) |

The recursion is structural, not rhetorical. The same ed25519 keypair shape that authorizes a `tool.call.<name>` capability check at runtime authorizes an `aw@opencovenant.org` commit at build time. The same `(subject, action, signature)` canonical form that signs a Covenant `SignedCapability` is the model the workflow uses to grant a persona authorship rights over a file domain.

## Engineering cycle

The loop runs one engineering cycle per session. A session inspects the repository, plans, executes, validates, runs subagent reviews, integrates, commits, pushes, fast-forward merges to `main`, then spawns the next session and exits. The next session starts with a clean context window and resumes from a structured handover artifact.

A session executes the following steps:

1. **Inspect.** Read the spec, the live state snapshot, and the recent change history.
2. **Plan.** Define one cycle with tight, integratable scope. If more than one viable architectural option exists, the **plan-gate** fires and a `Plan` subagent decides between options before any code is written.
3. **Execute.** Direct work for greenfield, scaffolding, and cross-module wiring. The **fan-out-gate** fires when a cycle touches more than three crates; one subagent handles each crate or logical unit.
4. **Validate.** `cargo check`, `cargo test --workspace`, `cargo fmt --check`, `cargo clippy -- -D warnings`, and per-language tooling.
5. **Security review.** When the staged diff modifies anything in the security-sensitive list — `crates/covenant-permissions/`, `crates/covenant-identity/`, `crates/covenant-audit/`, daemon identity / audit / settlement plumbing, the Anchor program, or `$COVENANT_HOME/{identity,capabilities,audit}/` paths — the **security-gate** fires; a `general-purpose` subagent runs the `security-review` skill on the staged diff before commit.
6. **Integrate.** Wire the new module to the rest of the workspace.
7. **Commit** through the persona-routing wrapper, with file-path-based dispatch (web → `ir`, Solana → `nr`, default → `aw`).
8. **Push, merge, cleanup.** Push the cycle branch, fast-forward merge to `main`, delete the branch on both ends.
9. **Handover.** Generate a fresh `COVENANT_SESSION_ID`, write it to the gitignored `.covenant-session-id`, spawn the next session, exit.

## Persona model

Three pseudonymous engineering personas own three file domains. Email scoping makes the identities first-class git citizens; the pre-commit hook chain enforces correct attribution.

| Initials | Identity | Email | Domain |
|---|---|---|---|
| `aw` | Achille Wasque | `aw@opencovenant.org` | Rust core, daemon, types, workspace infrastructure |
| `ir` | Iko Rane | `ir@opencovenant.org` | Web frontend ([`agent-os/covenant-web/`](./agent-os/covenant-web/) — Next.js, React) |
| `nr` | Noam Rook | `nr00x@opencovenant.org` | Solana programs ([`agent-os/programs/settlement/`](./agent-os/programs/settlement/) — Anchor) |

The wrapper at `~/.local/bin/covenant-commit` routes commits by staged file path. Mixed-domain commits split. The global pre-commit hook at `~/.config/git/covenant-hooks/pre-commit` rejects commits whose author email does not match the domain or whose diff or message would leak operator-identifying strings. Hook bypass (`--no-verify`) is project-forbidden; the loop never circumvents its own enforcement.

The rotation is verifiable: `git log --format='%an <%ae>' | sort -u`.

## Review architecture

Four mandatory gates fire on conditions encoded in the workflow definition. Each gate produces a recorded artifact.

- **Plan-gate.** Fires when a cycle admits more than one viable architectural option. A `Plan` subagent receives the brief plus the named axes (e.g., persistence backend, locking strategy, audit attribution shape). The subagent's reasoning is recorded; the chosen option is implemented. Plan-gate has historically caught design choices that would have silently broken downstream invariants — for example, audit-renderer contract regressions detected before any code was written.
- **Security-gate.** Fires on staged diffs touching identity, capabilities, audit, settlement, the Anchor program, or `$COVENANT_HOME/{identity,capabilities,audit}/` paths. A `general-purpose` subagent runs the `security-review` skill on the staged diff. The gate has surfaced and closed both HIGH-severity issues (cross-peer revocation via signature replay) and MEDIUM-severity TOCTOU races in registry compaction; in each case, the fix and an accompanying regression test landed in the same cycle.
- **Fan-out-gate.** Fires when a cycle touches more than three crates. One subagent handles each crate or logical unit. Serial cross-crate work above the threshold is forbidden; the cycle either parallelizes or splits.
- **Test-expansion-gate.** Fires when a cycle introduces a new public surface but covers only happy-path behaviour. A subagent expands the test suite for the failure modes the cycle has documented.

## Honesty markers

The system is engineered to make over-claiming difficult.

- **Mock and live tests are syntactically distinct.** Tests that exercise a real backend (real Ollama, real subprocess, real Solana RPC) start with `live_`. The script [`agent-os/scripts/test-stats.sh`](./agent-os/scripts/test-stats.sh) reports both counts. The live-to-total ratio is the production-readiness signal: a green `cargo test` run validates interfaces against test doubles; a green `cargo test -- --ignored live_` run validates the system against real backends.
- **Phase rollups are operator-only.** The autonomous loop ships engineering cycles. Promotion of a Phase from open to substantively complete in the spec is reserved to the human operator. The loop does not grade its own homework.
- **Each cycle declares three expected production failure modes.** The articulation is a sufficiency check on the cycle's understanding: a cycle that cannot name three failure modes has produced scaffolding that passes its own tests rather than work that anticipates reality. Subsequent cycles can be graded on which predicted failure modes actually fire.
- **Phase completion is gated on live test coverage.** A Phase cannot be marked complete in any state file unless at least one `live_` test exercises the path end-to-end. The constraint applies before the operator-only rollup check.

## Self-amendment discipline

The loop encodes lessons into workflow rules. Each amendment is itself an engineering cycle with a recorded artifact.

- **Test parallelism and global state.** Workspace tests share a process-global environment-variable table; save/restore helpers raced under `cargo test --workspace`. The codified rule splits environment-reading code into a pure `Option<&str>` helper plus a one-line wrapper. `std::env::set_var` and `std::env::remove_var` are forbidden in test code.
- **Parallel-session coordination.** Two autonomous sessions briefly raced on the same checkout. The codified rule introduces a per-clone `.covenant-session-id` (gitignored, mode 0600) plus a tracked `hooks/pre-commit` that refuses commits when the session-id environment variable disagrees with the file. The loop enforcing its own coordination primitive.
- **Cross-peer revocation via signature replay.** A mid-cycle security review identified a class of attack where a peer could revoke another peer's capabilities by replaying a stored signature. Closed in the same cycle by adding subject-ownership verification on the revoking peer's pubkey before any in-memory mutation, with a `CapabilityRevokeRejected` audit row recording the rejection.
- **Live testing convention.** Live coverage was added as a discipline rather than emerging organically. The first `live_` test exercised real subprocess JSON-RPC; subsequent cycles extended live coverage across LLM inference, embedding generation, the research-agent subprocess loop, the full daemon, and the Phase-0 acceptance criterion. The Phase-completion-requires-live-test rule originates here.

## Scope disclosure

Defensive boundaries; no claim above is intended to imply the following.

- **Not fully autonomous engineering.** A human operator runs trust prompts, approves destructive operations, and is the sole authority over Phase rollups.
- **Not recursive self-improvement in the Sakana DGM or SICA sense.** Those systems modify their own scaffolding to drive a measurable delta on a held-out evaluation. This loop amends its own workflow rules in response to past failures, but it does not re-train, re-prompt, or re-scaffold against a held-out benchmark.
- **No SWE-bench Verified scores quoted.** The benchmark has been contaminated as of February 2026; numbers reported against it are not informative. Merge rate is similarly gameable without a paired rework-rate companion. The reproducible signals here are the live test ratio, the gate-pass rate per security-sensitive cycle, and the persona rotation in `git log`.
- **Settlement is off-chain.** The Solana program at [`agent-os/programs/settlement/`](./agent-os/programs/settlement/) is scaffolded; receipts are recorded off-chain in JSONL. The on-chain layer is open work, tracked in [`agent-os/00_spec.md`](./agent-os/00_spec.md).
- **Attestation is local-key, not keyless.** Persona signing uses real ed25519 keys; the keys are not yet attested through a sigstore/Fulcio chain to a public registry. That is the foremost named gap below.

## Named gaps

- **Sigstore / Fulcio keyless attestation.** Adding `sigstore-rs` signing on top of the existing ed25519 personas and attesting to a public transparency log would move "verifiable autonomous engineering" from claim to artifact directly checkable by a third party.
- **On-chain settlement.** Requires Solana toolchain wiring and an SPL-mint launch (operator authority).
- **Live test ratio.** Mock interfaces are correct; system-under-real-backends is partially exercised. Continued growth is the honest signal.
- **Phase-1 multi-peer.** Infrastructure has shipped (peer authentication, registry compaction, action-grammar accept-both-shapes for `a2a.{send,recv,respond}.<peer>`); a second authenticated peer has not yet connected to a daemon in production. v0 is single-peer.

## Verification

Every claim above is checkable from this clone.

```bash
# persona rotation
git log --format='%an <%ae>' | sort -u

# live-vs-mock test ratio
bash agent-os/scripts/test-stats.sh

# session-lock and identity hooks
cat hooks/pre-commit

# the substrate
ls agent-os/crates/

# the authoritative product spec
cat agent-os/00_spec.md
```

Open an issue against [open-covenant/covenant](https://github.com/open-covenant/covenant) if a claim here does not match what is in the repository.
