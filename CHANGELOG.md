# Changelog

All notable changes to Covenant are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows [Semantic Versioning](https://semver.org/).

The first tagged release will appear under its own heading. Unreleased work is summarized in [`ROADMAP.md`](./ROADMAP.md).

## [Unreleased]

### Added
- `refund_task` and `unstake` instructions on the settlement program; `TASK_REFUNDED`
  state and `StakeWithdrawn`/`TaskRefunded` events.
- Per-page canonical, openGraph, and Twitter card metadata across all 25
  docs pages; `landing/app/robots.ts` and `landing/app/sitemap.ts`.
- Same-origin token proxy at `agent-os/covenant-web/app/api/covenant/[...path]`
  — bearer credential stays server-side; `NEXT_PUBLIC_COVENANT_TOKEN` no longer needed.
- Cluster-flip env layout (`COVENANT_SOLANA_{DEVNET,MAINNET}_{RPC,WS}_URL`) +
  runtime helpers `resolveNetworkFromRequestHeaders` and per-cluster maps in
  `packages/config/networks.mjs`.
- Worker `/healthz` and `/metrics` HTTP surface on the proof-gen worker
  (`PROOFGEN_WORKER_HEALTH_PORT`, default 8786).
- Telegram-bot deny-by-default allowlist (`TELEGRAM_ALLOWED_USER_IDS`) and
  per-user rate limiter.

### Changed
- Capability signature: `canonical_message` uses RFC 8785 (JCS) so a scope
  re-serialised by any compliant JSON parser still verifies.
- Audit-write failures on rejection-event kinds (`AuthenticationFailed`,
  `*Rejected`, `A2ASenderMismatch`, `BudgetExhausted`, etc.) now return
  `Response::Error { "audit write failed; refusing to proceed" }` instead of the
  standard rejection.
- `release_task` checks `Clock::get()?.unix_timestamp <= task.deadline` and
  `!config.paused`; after-deadline release becomes `refund_task`.
- All Agent + StakePosition account constraints in the settlement program
  carry explicit `seeds = [...]` PDA validation.
- `std::sync::Mutex` → `parking_lot::Mutex` in `covenant-a2a` (39 sites) and
  `covenant-mcp` (8 sites); panic propagation across the Mailbox/transport
  boundary no longer poisons future callers.
- `PeerToken` equality is constant-time via `subtle::ConstantTimeEq`.
- Compute-broker `/leases/{activate,reclaim,expire-sweep}` require
  `Authorization: Bearer ${OPERATOR_BEARER_TOKEN}`. `/bonds/cancel` signed
  payload includes `nonce` + `expires_at`; in-process replay cache rejects
  reuse.
- Proof-gen rate limit moved to Redis (atomic Lua) — two API replicas
  share one bucket per agent. Witness AES key is envelope-encrypted
  under `PROOFGEN_WITNESS_WRAP_KEY` (held outside Redis); Redis-only
  compromise no longer yields plaintext.
- Proof-gen cache key now includes `agent_did` so two agents with
  identical public inputs do not share a cache slot.
- React + TypeScript versions unified across apps and packages
  (react 19.2.5, typescript 6.0.3, @types/react 19.2.14).
- io.net API key sent as `Authorization: Bearer` header (was URL query
  string); 30s `AbortController` timeout on all outbound fetches.
- Hero artwork re-encoded to AVIF (-91% on the wire vs PNG); `landing`
  preload + canvas consumers point at `/hero-bg.avif`.

### Fixed
- Capability trust root: daemon now enforces
  `signed.capability.granted_by.pubkey == self.identity.pubkey` at every
  verify callsite. An out-of-band JSONL write under the daemon UID can no
  longer self-grant operator-scope caps.
- Identity-key file mode is enforced `0o600` on existing files (was
  only on create). Symlinks rejected. Parent dir forced to `0o700`.
- Audit chain `record()` refuses to rebuild on `events.len() !=
  chain.len()`; surfaces `AuditError::ChainCorruption` instead.
- `JsonlReceiptStore::mark_batch_confirmed` rewrites atomically via
  tempfile + rename.
- `SqliteStore::row_to_record` rejects malformed `owner_pubkey` rows
  instead of collapsing them to all-zeros.
- 25 audit findings across the Node services (broker, proof-gen,
  telegram-bot, mcp-bridge, hermes-mcp-bridge, indexer) including
  `process.on('unhandledRejection'|'uncaughtException')` at every
  entry point, DLQ jobId disambiguation, mcp-bridge `prepare_create_task`
  taskId default-randomised (was colliding with taskHash).
- Identifier scrub: zero forbidden strings in source or docs across the
  full repo.

### Security
- CI workflows: `timeout-minutes` on every job; `persist-credentials:
  false` on every `actions/checkout`; CodeQL Rust matrix; zizmor
  `--min-severity medium`; cargo-audit binary integrity check.
- Pre-commit hook: `eval` replaced with explicit hostname-command
  array; leak-scan filters tokens shorter than 4 chars and common
  nouns.
- handover.sh refuses `AGENT_CMD` containing shell metacharacters.
