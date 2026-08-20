# covenant-circuit

Circuit LLM as first-class, paid Covenant tools. Circuit exposes an OpenAI-compatible
inference gateway and an on-chain data API, both metered with x402 and settled in **CIRC**
(a Token-2022 mint) on Solana. This crate makes both native Covenant tools: it runs the
x402 pay-and-retry loop, enforces capability scoping before any CIRC leaves the wallet, and
settles through a pluggable payer so a Covenant-paid call lands in Circuit's treasury exactly
like a first-party one — feeding their staking and operator rewards, untouched.

## What's here

- **`Inference`** — the decentralized 72B, OpenAI-compatible, paid per call. Optional
  `X-Internal-Key` bypass for trusted co-located callers.
- **`DataClient`** — the Circuit Data API. Free endpoints (`quote`, `status`) return 200;
  the rest are paid. Named methods for the common endpoints plus a generic `get(path, query)`.
- **`X402`** — the pay-and-retry engine: on a 402, parse the `payment` block, enforce the
  capability, settle the CIRC, retry with `X-Payment-Signature`.
- **`CircuitCapability` + `SpendLedger`** — spend scoping enforced before settlement: a
  per-call cap, a treasury/recipient pin, an endpoint-host allowlist, and a cumulative
  budget. The recipient and amount come from the (untrusted) endpoint, so all four checks
  run first.
- **`CircPayer`** — the settlement seam. `MockCircPayer` for tests/dry-runs; `SolanaCircPayer`
  (behind the `solana` feature) builds a Token-2022 `transfer_checked`, submits it, and
  returns the confirmed signature. CIRC carries only metadata extensions (no transfer fee,
  no hook), so a plain `transfer_checked` settles it exactly.
- **`circuit_tools()`** — the MCP `Tool` set (`circuit.inference`, `circuit.data.query`,
  `circuit.data.token_price`, `circuit.data.market_overview`), the same factory shape the
  daemon's registry consumes. Each result carries a `circuit` provenance block (settling
  signature + CIRC spent) for the daemon to turn into a settlement receipt and audit row.

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

Wiremock covers the 402 loop and every capability guard (per-call cap, treasury pin, host
allowlist, cumulative budget), the free-endpoint path, and a tool call.

## Wiring into the daemon (next step)

The tools register exactly like `covenant-acedata`: add `covenant-circuit` to
`covenantd/Cargo.toml`, build the shared `Inference` + `DataClient` from config, and
`tools_vec.extend(circuit_tools(inference, data, &cfg))` in `covenantd/src/main.rs`.

Because settlement is on-chain, the real payer needs the `solana` feature and a funding key.
The daemon keeps that dep tree out of its default build (see its `Cargo.toml` note), so
in-daemon registration lands behind a daemon feature alongside the funding-key path — the
same "separate change" the daemon defers today. The daemon-side governance handler that
writes the settlement receipt + audit provenance row (and optionally a memory record) mirrors
`acedata_tool_call`; that is Phase 1.5.
