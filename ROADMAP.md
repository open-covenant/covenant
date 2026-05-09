# Roadmap

Covenant is moving toward an open agent-native operating layer for long-running autonomous software systems. The roadmap is organized by system capability, not by marketing milestones.

## Now: Harden the Local Control Plane

The current priority is making the local daemon and CLI reliable under real engineering use.

- Keep identity, peer auth, capability checks, audit rows, and token rotation strict.
- Expand live tests across daemon, CLI, HTTP, MCP, A2A, and subprocess boundaries.
- Add durable queueing for A2A tasks so daemon restarts do not drop work.
- Improve memory lifecycle: working-tier cleanup, compaction policy, and drift checks.
- Keep local validation and CI equivalent through `agent-os/scripts/validate.sh`.
- Make public docs distinguish implemented, experimental, and planned behavior.

## Next: Resumable Autonomous Maintenance

The next layer is a repeatable agentic maintenance loop that can run in bounded cycles.

- Task lifecycle state persisted in a simple inspectable format.
- Handoff artifacts that let a fresh session resume without private chat context.
- Review gates for architecture, security, docs drift, and missing tests.
- Human escalation records for credentials, destructive operations, and production authority.
- Regression hardening from prior failures into guard scripts and tests.
- Project memory conventions for roadmap, status, architecture, and known gaps.

## Then: Stronger Execution and Policy

The runtime needs stronger isolation and clearer policy boundaries before it can host untrusted agents.

- Linux sandboxing with gVisor first; Firecracker remains a later target.
- Per-resource budgets with pause, save, and resume semantics.
- Secret access through daemon-mediated tools instead of direct environment exposure.
- Capability scopes for agent-to-agent send, receive, respond, tool calls, memory writes, and external gateways.
- Operator-visible provenance for every privileged action.

## Later: Networked Agents and Settlement

The multi-host and economic layers are planned, not claimed as production.

- Multi-peer operation across authenticated hosts.
- Public provenance through signed artifacts and transparency-log attestation.
- Solana settlement program wired to real credit mint, burn, treasury, and provider-payout flows.
- SDKs for agent authors.
- Installer and upgrade path for local machines.
- Marketplace or registry only after the security and settlement boundaries are demonstrably sound.

## Research Tracks

- Durable project memory that survives long time horizons without stale context poisoning.
- Verifiable autonomous engineering: signed tasks, signed reviews, and auditable repair loops.
- Policy-aware tool orchestration across MCP, local commands, browsers, and code execution.
- Continuous repair loops that can detect regressions, bisect causes, and propose fixes.
- Human-directed autonomy: humans set strategy and authority boundaries; agents execute and maintain.

## Non-goals for the Current Stage

- Claiming production sandboxing before it exists.
- Claiming on-chain settlement before the program is deployed and audited.
- Treating generated code as complete without tests and review.
- Building a personality-driven multi-agent demo instead of inspectable infrastructure.
- Hiding human authority behind autonomous branding.
