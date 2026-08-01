# Recursive Engineering Model

Covenant is developed with an autonomous engineering loop that mirrors the operating-layer primitives in the codebase. The loop is not presented as full self-improvement, full autonomy, or a benchmarked replacement for maintainers. It is an inspectable process for letting agents perform bounded engineering work while preserving review, provenance, and human authority boundaries.

This file records how the public project relates to that loop. The detailed engineering protocol is maintained internally.

## What Is Real Today

The repository already contains concrete substrate for agentic operation:

| # | Primitive | Repository implementation | Workflow use |
|---|---|---|---|
| 1 | Intent | Daemon dispatch + router, shaped in `covenant-types` and `covenant-router` | Natural-language requests route to agents, tools, and workflows over typed shapes. |
| 2 | Runtime | `covenant-runtime` | Agents run as bounded trusted-local subprocesses; an opt-in Linux gVisor runner is selectable with ignored live coverage gated on `runsc` and an explicit rootfs. |
| 3 | Memory | `covenant-memory` | Project state is persisted across working, episodic, and long-term tiers. |
| 4 | Identity | `covenant-identity` and peer auth | Local sessions and commits can be tied to scoped identities. |
| 5 | Permissions | `covenant-permissions` | Security-sensitive operations are expected to pass capability and review gates. |
| 6 | Comms | `covenant-ipc`, `covenantd` HTTP gateway, `covenant-mcp`, `covenant-a2a` | Tool and peer boundaries are explicit, not implicit process sharing. |
| 7 | Compositor | `agent-os/covenant-web` Next.js console and `covenant-tui` terminal binary | Operator-facing surface for agent state, decisions, and audit; the TUI renders intent, memory, audit, capabilities, A2A, chain-receipts, and peer-registry views over the daemon IPC. |
| 8 | Settlement | `covenant-settlement` plus the `agent-os/programs/settlement` Solana program, deployed on mainnet | Resource accounting exists locally, including rollback-backed receipt backfill primitives. The settlement program is deployed on Solana mainnet — credits, staking, slashing, and on-chain receipt anchoring are implemented on-chain; the daemon-driven economic lifecycle (per-intent consume) is not yet production. |

Audit (`covenant-audit`) is a cross-cutting accountability layer underlying Identity, Permissions, and Settlement — append-only JSONL events, local hash-chain integrity reports, retention controls, signed actions, and audit-root attestations.

The inbound agent-to-service payment rail over HTTP 402 (x402) is live today and is distinct from the internal-receipt settlement above: Covenant operates a deployed x402-v2 seller that settles in USDC on Solana mainnet, while the daemon's legacy-v1 outbound signer path is parked pending transaction-bound authorization and crash-safe prepayment reservation. The seller's paid endpoints return registration observations, publisher-signed caller-supplied statements, bounded PayAI-linked transfer observations, and seller-reported DCAP-monitor results. Its signing key is served at `/.well-known/x402`, but a key served beside a statement is not independent attribution: consumers must pin the expected Covenant key through a trusted external channel. Registration and payment observations do not prove real-world identity, x402 job delivery, quality, reputation, or enclave origin. Covenant also operates an escrow service and an Ephemeral-Rollup credit facilitator (the facilitator settles ER credits on devnet). What is *not* production is the daemon-driven anchoring of internal resource receipts to the settlement program; the inbound payment rail being live does not change that boundary.

Covenant's signed-evidence projection also reaches Base mainnet (chain 8453) without moving the token: the foundation agent is registered under ERC-8004 (Identity Registry `0x8004A169FB4a3325136EB29fA0ceB6D2e539a432`, agent id 58403), a bond-receipt verifier (`0xBee387DD4A2fF215d6f997E5DA464C92285BCb6e`, `ecrecover`-based, native-USDC-denominated) and an EAS score schema are deployed, audit-root statements can be re-expressed as EAS off-chain statements signed by a configured secp256k1 issuer (`0x186953d5b4A290f8f53b8377cb38EDA75D664211`), and agents resolve through a live ENS CCIP-Read gateway (`opencovenant.eth`, `*.agents.opencovenant.eth` resolving to the Solana identity, served from `ens-gateway.opencovenant.org`). The crate also contains an experimental format utility for signing caller-supplied score projections, but it is not wired to publication and does not verify the supplied Solana reference or score derivation. `ecrecover` proves that bytes were signed by the configured address; it does not establish signer identity or claim truth. `$CVNT` stays a single Solana mint, and the Solana/Base evidence and bond surfaces described here use chain-local USDC; other integrations may use their own settlement assets. A build-time quarantine guard fails CI if the mint or a bridge primitive leaks into a cross-chain crate. A Base x402 seller is live as well, settling USDC on Base mainnet over EIP-3009 for publisher-signed caller-supplied statements. What is *not* yet exercised on Base: no score has been written on-chain (the EAS schema is registered but writes stay gated), no USDC bond has been funded or slashed (the verifier is deployed but unfunded), the Base outbound-pay path is built but not wired into the default daemon, and trust-minimized cross-chain enforcement (slashing a Solana bond from an EVM-proven event) is unbuilt.

The loop also has practical enforcement:

- a tracked pre-commit hook for one-session-per-checkout protection;
- a validation script used by local development and CI;
- guard scripts for known regression classes;
- a test convention that separates mock tests from opt-in `live_` tests;
- explicit human escalation rules for credentials, production deployment, legal decisions, and destructive operations.

## Engineering Cycle

Each autonomous cycle should be small enough to review and large enough to move a real system boundary. The cycle is:

1. Inspect repository state and prior handoff notes.
2. Define the next bounded task and its expected production failure modes.
3. Plan the implementation before editing.
4. Implement the change.
5. Run self-review against correctness, scope, style, and security impact.
6. Trigger cross-review or security review when the gates require it.
7. Run validation and repair failures.
8. Update docs, status, and project memory.
9. Integrate the change with a human-readable commit history.
10. Hand off enough context for a fresh agent session to resume.

The lifecycle is intentionally conservative. A change is not complete because code was generated. It is complete when the behavior is implemented, validated, reviewed at the appropriate level, and documented where the public contract changed.

## Review Gates

The tracked workflow defines six gates:

| Gate | Trigger | Required outcome |
|---|---|---|
| Plan gate | More than one credible architecture path | Record the chosen design and rejected alternatives. |
| Security gate | Identity, permissions, audit, settlement, secrets, or sandbox boundaries changed | Security review before integration. |
| Fan-out gate | Broad cross-crate or cross-app changes | Split ownership by module or reduce scope. |
| Test-expansion gate | New public behavior with shallow tests | Add failure-mode coverage or record the gap. |
| Docs gate | Public terminology, commands, or architecture changed | Update public docs in the same change. |
| Escalation gate | Credentials, destructive operations, legal/business calls, production deploys | Stop that path and continue only with unblocked work. |

These gates are useful only when they create artifacts a future maintainer can inspect. Silent reasoning does not count.

## Provenance

The public model is scoped provenance, not personality. Work should be attributable by domain, task, reviewer, validation command, and commit, without relying on named agent personas or private operator details.

Accepted provenance signals:

- signed commits or signed release artifacts where available;
- CI run links and validation command output;
- structured audit rows for daemon actions;
- review notes for security-sensitive changes;
- status docs that distinguish implemented, experimental, and planned work.

Public keyless attestation has landed for release-subject manifests via sigstore keyless (cosign + Fulcio + Rekor); the remaining work is extending the signing workflow to audit-root attestations and release-scope manifests. Local signing identities (operator-held ed25519 keys for unsigned audit-root attestations) remain separate from this CI-driven keyless path.

## Honesty Boundaries

Covenant does not currently claim:

- full autonomous software engineering without human authority;
- production sandbox-grade isolation for arbitrary untrusted agents;
- production multi-peer operation across untrusted hosts;
- the daemon-driven on-chain settlement lifecycle in production (the Solana settlement program is live on mainnet; per-intent consume is not yet production);
- the Base signed-evidence projection beyond what is deployed: on-chain score writes (the EAS schema is registered, but no score is written on-chain yet), funded USDC bonds or live slashing (the bond-receipt verifier is deployed but unfunded), Base-default outbound payments (built, not wired into the default daemon), and trust-minimized cross-chain enforcement;
- public benchmarked self-improvement;
- a release-ready installer, or a multi-language SDK ecosystem beyond the published TypeScript `@covenant-org/sdk`.

Those are roadmap items. The current system is a working local substrate with strong primitives in some areas and deliberately marked gaps in others.

## Verification

Useful local checks:

```bash
bash agent-os/scripts/validate.sh --quick
bash agent-os/scripts/test-stats.sh
```

The full Rust validation gate is:

```bash
bash agent-os/scripts/validate.sh
```

Live tests are opt-in:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```
