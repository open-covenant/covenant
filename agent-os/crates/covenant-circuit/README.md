# covenant-circuit

Explicit Circuit clients and x402 development surfaces for Covenant. Circuit exposes an
OpenAI-compatible inference gateway and an on-chain data API, both metered with x402 and
settled in **CIRC** (a Token-2022 mint) on Solana. This crate contains the pay-and-retry
loop, process-local spend guards, a pluggable payer, and an optional tool factory.

The production Covenant daemon does not advertise or invoke `circuit.*` tools and does not
construct a Circuit signer. That path is parked until it has transaction-bound authorization
and a durable, crash-safe prepayment reservation/idempotency record. The library clients and
examples remain available for explicit development use.

## What's here

- **`Inference`** — the decentralized 72B, OpenAI-compatible, paid per call. Optional
  `X-Internal-Key` bypass for trusted co-located callers.
- **`DataClient`** — the Circuit Data API. Free endpoints (`quote`, `status`) return 200;
  the rest are paid. Named methods for the common endpoints plus a generic `get(path, query)`.
- **`X402`** — the pay-and-retry engine: on a 402, parse the `payment` block, enforce the
  capability, settle the CIRC, then send one redirect-free request with
  `X-Payment-Signature`. It does not retry a paid request without an endpoint-defined
  idempotency contract.
- **`CircuitCapability` + `SpendLedger`** — process-local spend scoping checked before the
  payer: a
  per-call cap, a treasury/recipient pin, an endpoint-host allowlist, and a cumulative
  budget. The recipient and amount come from the (untrusted) endpoint, so all four checks
  run first. The ledger is not durable across processes or restarts.
- **`CircPayer`** — the settlement seam. `MockCircPayer` for tests/dry-runs; `SolanaCircPayer`
  (behind the `solana` feature) builds a Token-2022 `transfer_checked`, submits it, and
  returns the confirmed signature. CIRC carries only metadata extensions (no transfer fee,
  no hook), so a plain `transfer_checked` settles it exactly.
- **`circuit_tools()`** — the MCP `Tool` set (`circuit.inference`, `circuit.data.query`,
  `circuit.data.token_price`, `circuit.data.market_overview`) for an explicit caller-owned
  registry. Covenantd filters these names from its tool list and rejects their execution.
  Each result carries a `circuit` result block with the payer-reported signature and spend.

## Demos

```
# Offline, deterministic, no money — the full flow against a local mock:
cargo run -p covenant-circuit --example demo_shape

# Live settlement against the real endpoints (spends real CIRC):
CIRCUIT_KEYPAIR=~/.config/solana/id.json \
cargo run -p covenant-circuit --features solana --example live_demo -- --confirm
```

## Tests

```
cargo test -p covenant-circuit
```

Wiremock covers the 402 loop, redirect refusal, the single-paid-attempt contract, every
capability guard (per-call cap, treasury pin, host allowlist, cumulative budget), the
free-endpoint path, and a tool call.

## Daemon boundary

The old `COVENANT_CIRCUIT_ENABLED`, keypair, and RPC settings cannot activate Circuit in
`covenantd`, even when the compatibility feature is compiled. Re-enabling the daemon path
requires a signer-consumed, exact one-use authorization plus durable reservation and replay
semantics before any payment or network retry. A post-payment result block is not sufficient
evidence to synthesize settlement accounting.
