# Reference Covenant agents — v1 batch

What we send chiefy7 first. Each agent here has to be a real working
Covenant runtime registered on the SAP program AR indexes, with a
published audit-root attestation. Not test fixtures — the live SAP
program is what AR's indexer reads, so anything we publish for this
batch shows up on `app.agentranking.io/agents/900001/<id>` for real.

Constraint: small batch, broad coverage. Three agents, each
demonstrating a distinct slice of what Covenant can prove. Once these
land cleanly we expand.

## Selection criteria

1. Real runtime. No stubs. Each agent must actually serve its declared
   capabilities — feed-side checks will catch unresponsive ones.
2. Distinct archetype. AgentRanking's archetype enum (`trading,
   arbitrage, sniper, yield, builder, analytics, bridge, copy_trader,
   social, mev, telegram`) is keyword-classified — we pick agents that
   classify cleanly to non-overlapping buckets so the verified concept
   isn't tested only inside one category.
3. Operator we control. v1 reference agents are operated by Covenant
   core, not third parties. Once the verified concept is live, we open
   up to third-party operators with their own keypairs.
4. Already-known identity on Solana. Reuse existing wallets where we
   have one rather than spin up new ones — fewer moving parts to debug
   when something doesn't index.

## Batch 1

`covenant-coder` — builder archetype. Capabilities
`tool.code_exec`, `tool.web_search`, `intent.code`. Backed by
`services/coding-gateway`; the sandbox already produces dense conduct
events, so verified bit and signal feed both light up on day one.

`covenant-audit` — analytics archetype. Capabilities `audit.verify`,
`tool.attest`. Wraps `covenant audit verify`; demonstrates audit-root
recomputation against a public source. The agent that verifies other
agents.

`covenant-router` — bridge archetype. Capabilities `intent.route`,
`a2a.delegate`. An A2A intent router — exercises the cross-agent
reputation case, since it generates `intent_dispatched` and
`a2a_result_rejected` events on other agents.

Three covers builder / analytics / bridge — three of AR's archetype
buckets, three different `outcome` distributions, three different
`x402Endpoint` shapes. Enough variety to stress the schema without
becoming an operations burden.

## Per-agent specs

### `covenant-coder`

- **Wallet:** new keypair, custody at
  `~/.config/solana/covenant-coder-agent.json`. Funded from devnet
  faucet for Phase 1, mainnet from treasury for Phase 2.
- **Manifest fields:**
  - `name`: "Covenant Coder"
  - `description`: "Public Covenant coding agent. Hermes-compatible
    gateway, fully sandboxed, audit-rooted runs."
  - `capabilities`: `[tool.code_exec, tool.web_search, intent.code]`
  - `pricing`: free during v1 (zero µUSD tiers, gated by Turnstile);
    paid tier deferred until coding gateway billing lands
  - `protocols`: `["covenant.coding/v1", "x402"]`
  - `agentUri`: `https://covenant.tools/agents/coder/.well-known/agent.json`
  - `x402Endpoint`: existing coding gateway URL
- **Backing service:** existing `services/coding-gateway`.
- **Audit log:** `events.jsonl` from a real run, mirrored at
  `agentUri`/audit/ so AR's verifier can fetch (check 5).
- **Signature form:** v0 self-attested for first publish, upgrade to
  v1 sigstore via release.yml on next sandbox release.

### `covenant-audit`

- **Wallet:** reuse the daemon identity from
  `~/.config/solana/covenant-agent.json` (memory: PDA `CkyhgJ…`,
  wallet `AdChc…`). This is already on the SAP program — main test of
  whether AR sees it.
- **Manifest fields:**
  - `name`: "Covenant Audit"
  - `description`: "Verifies Covenant audit roots and provenance
    envelopes. Public CLI."
  - `capabilities`: `[audit.verify, tool.attest]`
  - `pricing`: free.
  - `protocols`: `["covenant.audit/v1"]`
  - `agentUri`: `https://covenant.tools/agents/audit/.well-known/agent.json`
  - `x402Endpoint`: null (CLI-first agent, no HTTP service yet)
- **Backing service:** `agent-os/scripts/provenance.mjs` wrapped in a
  tiny CLI surface (already exists per memory: `covenant audit verify`).
- **Notes:** This is the one agent where the audit log is itself the
  thing being audited — self-referential but well-defined. The agent
  publishes its own audit root and the verifier (which it implements!)
  can re-check it.

### `covenant-router`

- **Wallet:** new keypair, custody at
  `~/.config/solana/covenant-router-agent.json`.
- **Manifest fields:**
  - `name`: "Covenant Router"
  - `description`: "A2A intent router across registered Covenant
    agents. Open peer discovery via SAP `findAgentsByProtocol`."
  - `capabilities`: `[intent.route, a2a.delegate]`
  - `pricing`: free.
  - `protocols`: `["covenant.a2a/v1"]`
  - `agentUri`: `https://covenant.tools/agents/router/.well-known/agent.json`
  - `x402Endpoint`: TBD — a tiny HTTP service that exposes the routing
    endpoint. Smallest backing service of the three.
- **Backing service:** new `services/router` (small), wraps
  `SapBridge.findAgentsByProtocol` + intent dispatch from
  `agent-os/crates/covenant-types`.
- **Notes:** This agent generates the cross-agent conduct events that
  show up on the other two agents' signal feeds — useful for testing
  whether AR's reputation calc actually consumes cross-agent signals
  or only self-emitted ones.

## Phasing per agent

All three agents go through the same six-step pipeline: register on SAP
devnet (Phase 1), wait for AR to index the profile, publish the
audit-root attestation, confirm AR surfaces the "Covenant Verified"
section, promote the agent to SAP mainnet (Phase 2), and confirm the
signal-feed `/agents/...` endpoint returns 200 with the expected
projection.

The one difference: `covenant-audit` starts at step one already done —
its wallet and PDA exist on the program covenant config points at
today. The other two start from a fresh keypair.

`covenant-audit` is live on both devnet and mainnet at PDA
`CkyhgJdpW7YyUKasXcGD2CnUYgzijgS5ZHTV8zxihnjC` (wallet
`AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb`), program
`SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ`.

On-chain name is **Covenant** (the daemon identity), not "Covenant
Audit"; this is the production daemon and the reference agent at the
same time. Capabilities currently published: `covenant:settlement`,
`x402:pay`, `covenant:audit`. Protocols: `covenant.runtime/v1`,
`a2a`, `x402` (the `covenant.runtime/v1` string was added 2026-05-30
to make AR's detector pick this agent up).

Devnet update sig:
`2U55eLjre25dh1PvGAEi1i6j6SbSQWqsJYUxBYEtFVRGTdycc4NMQtx62q9wRrzHVxNn4UKRzT4jKC3pshgNuF6T`.
Mainnet update sig:
`3bu82J6eax2ssLAiC8XfLSavHzNYqginht3gMWeBzqwm8udSpT5skDwf1tsFkwtmXAd7Jbxn3WWpDA9L2zzsoEsu`.

Tier on the AR profile once their next SAP sync runs: `indexed`. To
climb the tiers we still need to (1) host audit material under
`https://opencovenant.org/audit/`, (2) publish an audit-root
attestation pointing at it, (3) optionally upgrade the signature to
sigstore-keyless for `verified_attested`. Until those land, this agent
is detected but not verified.

## Manifest publish flow

For each agent:

1. `solana-keygen new --outfile ~/.config/solana/covenant-<slug>-agent.json`
2. Fund wallet (`solana airdrop 2` devnet; mainnet via treasury later).
3. `node packages/sap-bridge/dist/worker.mjs publish-agent <<< '<manifest json>'`
   (memory: capability id must be `namespace:method`; manifest worker
   expects camelCase JSON in stdin, encodes to snake_case on chain).
4. Verify via `describe-agent <<< '{"pda":"<pda>"}'` that the round-trip
   shows the manifest we sent.
5. Generate audit events (real, not synthetic) — run the agent for at
   least one invocation cycle.
6. `covenant audit verify --json` to compute root hash.
7. `node packages/sap-bridge/dist/worker.mjs attest-root <<< '<envelope>'`
   to publish the audit-root attestation to the ledger PDA.
8. Confirm AR indexer sees the profile at
   `app.agentranking.io/agents/900001/<assigned-id>` within their
   documented latency window.

Scripted as `scripts/agentrank-publish-reference.mjs` — not on this
branch yet; lands in Phase 1.

## Domains / hosting

- `agentUri` host: `covenant.tools` subpath per agent. Already covers
  TLS + DNS via existing infra; no new domains.
- Audit log mirror: same host, `/agents/<slug>/audit/events.jsonl`
  plus `/agents/<slug>/audit/events.chain.jsonl`. Static files behind
  CDN. AR's verifier can range-request without auth.

## Open questions for chiefy7

1. **Slug authority.** Do we pick `covenant-coder` / `covenant-audit` /
   `covenant-router` or are slugs assigned at index time from the
   on-chain `agentId` field? Answer determines whether we set
   `agentId` to the desired slug at registration time.
2. **`sapTwinMatch`.** AR's profile schema has a "twin match" field
   suggesting EVM ↔ SAP linkage. None of our reference agents have an
   EVM counterpart; should we leave it null or is there a value that
   means "Solana-native, no twin"?
3. **Latency floor.** How fast after `publish-agent` does AR's
   indexer see a new agent? Affects how we sequence the Phase 1 demo.
4. **Devnet vs. mainnet surfacing.** Does AR's SAP indexer cover both
   clusters or mainnet only? If mainnet only, Phase 1 collapses
   straight into Phase 2 and we fund mainnet upfront.
