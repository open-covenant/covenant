# Covenant — Build Spec v0

**Status:** pre-scaffold. Pins the cross-cutting decisions needed before week 1 of code.

**Source:** `projects/research-agent/research/Agentic OS/` (`Agentic-OS.pdf`, `agentic-os-plan.pdf`, April 2026). This spec supersedes them where they conflict and pins choices they left open. Marketing copy lives elsewhere; this file is for builders.

---

## 1. Thesis

Open agent-native operating layer. Sits above Linux/macOS, replaces the desktop/file/app metaphor with primitives for agentic work — the coordination layer that lets humans and agents safely share a computer, delegate work, remember context, enforce permissions, and settle usage. Local-first. Solana for settlement. Token: `$covnt`. CLI / crate root / daemon name: `covenant`.

## 2. Canonical Primitives

Eight primitives. This ordering is canonical; copy that ships with 6 or 7 is wrong and should be reconciled here.

| # | Primitive | Replaces | One-line role |
|---|---|---|---|
| 1 | Intent | App launcher | Route natural-language goals to agents, tools, workflows |
| 2 | Runtime | Process manager | Sandboxed agent execution + lifecycle |
| 3 | Memory | Filesystem | Semantic, tiered, queryable context |
| 4 | Identity | User accounts | One model for humans, agents, tools, services |
| 5 | Permissions | (no clean analog) | Capability tokens + delegation chains |
| 6 | Comms | IPC / sockets | Agent bus + registry + MCP + A2A |
| 7 | Compositor | Window manager | Surface agent state, decisions, results |
| 8 | Settlement | (no analog) | BME-driven economic coordination |

## 3. Phase Map

Six phases, 36 weeks. Settlement appears at both ends: interface stub in Phase 0, on-chain wiring in Phase 5. Identity & Permissions stays one phase but expands by 2 weeks because it is now genuinely two primitives.

| Phase | Weeks | Deliverable |
|---|---|---|
| 0 — Foundation | 1–4 | Daemon, intent router v0, first agent (research), settlement trait + no-op impl |
| 1 — Memory & Sandboxing | 5–10 | LanceDB + SQLite memory layer, gVisor sandbox, first burn surface (memory writes) |
| 2 — Identity & Permissions | 11–17 | ed25519 identity, capability tokens, delegation chains, audit log |
| 3 — Comms | 18–22 | Agent bus, registry, MCP bridge, A2A adapter, orchestrator agent |
| 4 — Compositor & Interface | 23–30 | TUI (Ratatui), intent bar, agent panel, memory browser, web UI (Next.js) |
| 5 — Settlement-on-chain & Ecosystem | 31–36 | Settlement program (credits + buyback), SDKs, marketplace, installer |

Sum: 4 + 6 + 7 + 5 + 8 + 6 = 36.

## 4. Core Data Shapes (v0)

Rust. Lives in `covenant-types` crate; consumed by every other crate.

```rust
struct Intent {
    id: Uuid,
    text: String,             // natural language
    issuer: AgentId,          // human or agent
    issued_at: u64,           // epoch ms
    priority: Priority,       // Low | Normal | High
    parent: Option<Uuid>,     // for delegated sub-intents
}

struct AgentId {
    display: String,          // "research@local"
    pubkey: [u8; 32],         // ed25519
}

struct Capability {
    subject: AgentId,
    action: String,           // dotted: "memory.write", "tool.web_search"
    scope: serde_json::Value, // shape is action-specific, e.g. {"path": "research/*"}
    granted_by: AgentId,      // root of the delegation chain
    expires_at: Option<u64>,
}

struct MemoryRecord {
    id: Uuid,
    tier: MemoryTier,         // Working | Episodic | LongTerm
    owner: AgentId,
    text: String,
    embedding: Vec<f32>,      // dim from chosen embed model
    metadata: serde_json::Value,
    created_at: u64,
    parent: Option<Uuid>,     // for derived memories
}

struct SettlementReceipt {
    id: Uuid,
    payer: AgentId,
    resource: ResourceKind,   // Compute | Memory | Tool | Message | Registration
    credits_consumed: u64,    // USD-pegged credits destroyed at this event
    settled_at: u64,
    onchain_sig: Option<String>, // None until the batched burn lands on Solana
}
```

`covnt → credits` minting and `credits → covnt` re-mint to providers are protocol-level flows, not per-receipt. The receipt records the consumption event only.

## 5. Agent Manifest

`agent.toml` v0:

```toml
[agent]
id = "research"
name = "Research Agent"
version = "0.1.0"
runtime = "python3"               # python3 | node | rust-bin
entry = "main.py"

[capabilities]
required = ["intent.subscribe", "memory.write", "memory.read", "tool.web_search"]
optional = ["tool.gpu_inference"]

[resources]
cpu_ms_per_task = 30_000
memory_mb = 512
disk_mb = 100
network = "outbound-https-only"   # off | outbound-https-only | full

[settlement]
budget_credits_per_hour = 10      # ignored Phase 0; enforced from Phase 1
priority = "normal"               # low | normal | high (intent bus routing weight)
```

Capabilities use dotted paths. Reserved namespaces: `intent.*`, `memory.*`, `identity.*`, `tool.*`, `agent.*`.

## 6. Identifier Scheme

Two layers:

- **Display** — `name@host`, e.g. `research@local`. Used in CLI, logs, UI.
- **Wire** — ed25519 pubkey, base58. Used in audit log, capability tokens, settlement receipts, on-chain.

Daemon maintains a local registry mapping display ↔ pubkey. Solana is ed25519-native, so the same key signs settlement transactions — no second keypair system.

Cross-host resolution (Phase 3): `name@host.tld` resolves via the comms registry.

## 7. Threat Model

### Trust boundaries

| ID | Boundary | Enforcement |
|---|---|---|
| B1 | Host kernel ↔ agent | gVisor (Phase 1) → Firecracker (Phase 5). Agents cannot syscall the host. |
| B2 | Agent ↔ agent (same human) | Capability tokens. No shared memory. Comms only via bus. |
| B3 | Agent ↔ agent (cross-human / marketplace) | Cap tokens + identity verification + slashable stake |
| B4 | Local ↔ external service (tool, API) | Secrets in OS keychain. Daemon proxies; agent never sees raw credential. |
| B5 | Local ↔ Solana | Daemon holds settlement key. Agent code never holds chain keys. |

### Adversary models

- **Malicious local agent.** Bounded by declared budget, declared capabilities, sandbox. Cannot escalate to host, cannot read peer-agent memory without delegation, cannot drain wallet.
- **Malicious marketplace agent.** Above + slashable identity stake + on-chain reputation. Sandbox escape is the high-severity failure mode; mitigated by audit + bounty in Phase 5.
- **Compromised tool / MCP server.** Daemon validates responses against declared schema. Capability scope limits blast radius — a `tool.web_search` cap cannot be silently upgraded to data exfiltration.
- **Compromised host kernel.** Out of scope. If the kernel is owned, all primitives fail.

## 8. Settlement Model

**Token — `$covnt`.** Standard SPL token. Launched manually on pump.fun by the operator; **not part of the build**. Fixed supply once the bonding curve graduates to Raydium (mint authority renounced). The source plan's BME-with-remint is therefore not feasible — this spec replaces it with a fixed-supply credits + buyback model.

**Credits.** USD-pegged accounting unit, minted by the Settlement Program (Phase 5). Mint inputs: covnt at Pyth oracle rate, or USDC 1:1 as fallback. Credits are destroyed at the moment of resource consumption — that destruction event is the `SettlementReceipt` from §4.

**Provider payout.** Settled in USDC by default; optional payout in covnt routed through Raydium at request time. A protocol treasury (USDC-denominated) accrues a fixed portion of every credit-mint inflow.

**Deflationary surface.** Two layers:
- **Credits** destroyed per consumption event — deterministic, scales 1:1 with usage.
- **covnt** destroyed via treasury buyback-and-burn from a slice of credit-mint fees — probabilistic, scales with usage but routed through market price.

**Phase 0 stub.** Settlement trait exposes `record(receipt) -> Result<()>` with a disk-log no-op implementation. No tokens move; no Solana RPC. Phase 1 attaches the first burn surface (memory writes) — still local-accounted, queued for later batched settlement. Phase 5 wires the on-chain program and flushes the queue.

## 9. Phase 0 Acceptance Test

```
Given: covenant daemon running on macOS or Linux,
       research-agent v0.1 registered via `covenant install ./research-agent`.
When:  $ covenant intent "find recent papers on agent memory"
Then:
  - Daemon receives intent over Unix socket at $COVENANT_HOME/sock
  - Intent router v0 (regex + cosine similarity over capability cards) matches research-agent
  - Research agent is spawned as a subprocess; intent JSON arrives on stdin
  - Agent calls web search tool, summarizes top 5 results
  - Result returned as { intent_id, status: "ok", text, sources, settlement: null }
  - CLI prints text to stdout
  - End-to-end latency < 5s on unloaded laptop, excluding LLM call time
  - No file written outside $COVENANT_HOME (~/.covenant by default)
  - Result lives in the working memory tier and clears at session end
```

A test harness asserts each step. Phase 0 ships the day this passes.

## 10. License

- **Core** (`covenant-*` crates, daemon, runtime, memory, identity, permissions, comms, compositor, settlement): **Apache 2.0**.
- **SDKs** (`covenant-sdk-py`, `covenant-sdk-ts`, `covenant-sdk-rs`): **MIT**.
- **Marketplace packages:** MIT default; agent authors can override.

Apache for core because protocol-surface projects need explicit patent grants — protects contributors and downstream integrators. MIT for SDKs because integration friction matters more than viral copyleft. AGPL was rejected: would scare off the compute / storage operators that the BME two-sided market depends on.

## 11. Deferred Decisions

Specified during the phase that consumes them. Listed explicitly so they don't drift into Phase 0 thrash.

| Item | Decided in |
|---|---|
| Memory query API (vector + structured filter syntax) | Phase 1 |
| Local ↔ Solana settlement seam (burn batching, offline behavior) | Phase 1 interface, Phase 5 wire-up |
| Treasury policy (buyback rate, frequency, slippage caps, USDC↔covnt payout split) | Phase 5 |
| Oracle source confirmation (Pyth assumed; Switchboard fallback?) | Phase 5 |
| Capability token wire format (macaroons vs. JWT-like vs. custom) | Phase 2 |
| Registry / discovery protocol (local vs. networked) | Phase 3 |
| A2A interop spec (agent card schema, task envelope) | Phase 3 |
| First-5-minutes UX flow | Phase 4 |
| Existing-framework interop (LangGraph, CrewAI wrap-and-run) | Phase 5 |

### Pins from external review (2026-05-05)

Surfaced while reading `thewaltero/mythos-router` for build-process and runtime ideas. Deferred but enumerated so the relevant phase doesn't rediscover them mid-sprint.

| Item | Decided in |
|---|---|
| Memory compaction policy — when working/episodic/long-term tiers compress, and on what signal (count, age, access-frequency, semantic clustering) | Phase 1 |
| ~~`.covenantignore` — per-project allow/deny list for memory auto-ingestion and tool scans~~ | **Sprint 21 (closed)** — gitignore-style; daemon loads `$COVENANT_HOME/.covenantignore`; ignored intents skip memory + receipt and emit `IntentIgnored` audit. |
| Per-resource budget mid-task graceful save — when an agent hits `budget_credits_per_hour`, the runtime pauses, persists partial state, settles consumed credits, and queues a resume | Phase 1 |
| ~~`covenant verify` command — codebase ↔ memory drift scan that flags stale memory references, missing files, and orphaned records~~ | **Sprint 20 (closed)** — `covenant verify` runs three drift checks (memory↔audit, capability↔audit, memory↔receipts). |
