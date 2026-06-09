# Xona Agent x402 Provider

[Xona Agent](https://xona-agent.com) is exposed to Covenant agents as a
*provider profile* over the generic outbound x402 gateway (`covenant-x402`),
not as a parallel runtime. An agent calls Xona through capability-gated MCP
tools; the daemon signs the x402 payment, debits the agent's budget, and writes
a settlement receipt. The agent never holds the funding key.

The profile lives in the `covenant-xona` crate plus a thin daemon bridge in
`covenantd::xona`. It adds no Xona-specific code to the core crates: settlement,
budgeting, audit, and attestation are reused from the existing x402 accounting
path that backs every paid call.

Unlike the Hyre profile, Xona's Solana endpoints are **self-paid**: the 402
challenge carries no sponsor `feePayer`, so the funder pays its own gas and the
signer sidecar settles with `SolanaSigner` (not PayAI's sponsored flow).

## Quick start (operator)

```sh
# 1. Build the funding-key sidecar (its own workspace; isolates solana-sdk).
cd agent-os/crates/covenant-x402-signer && cargo build --bin covenant-x402-signer

# 2. Point a funded Solana keypair at it. The funder needs (a) a USDC token
#    account that exists on chain, (b) enough USDC for the per-call price, and
#    (c) a little SOL for gas — Xona is self-paid, there is no sponsor.
export COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/funder.json
export COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com
export COVENANT_X402_SIGNER_BIN=$PWD/target/debug/covenant-x402-signer

# 3. Resolve the catalog + 402 challenge for free (dry run, no payment).
cd ../../.. && cargo run -p covenant-xona --example live_paid_call

# 4. Run a real paid call ($0.03 USDC against image/creative-director).
cargo run -p covenant-xona --example live_paid_call -- --confirm
```

To enable the profile on a real `covenantd` instance, set the env in the
[Configuration](#configuration) table and start the daemon — it will build the
catalog and register `xona.*` MCP tools.

## Components

| Concern | Where | Notes |
| --- | --- | --- |
| Endpoint catalog + pricing | `covenant-xona::catalog` | Filtered from the orbit-x402 registry. No rebuild when Xona adds endpoints. |
| MCP tools | `covenant-xona::tools` | One `xona.*` tool per endpoint, with a permissive `{prompt, …}` body schema. |
| 402 challenge + pay loop | `covenant-xona::x402` | Standard x402 challenge parsing, option selection with a payee pin, and the 402-then-pay loop. Reuses the `covenant_x402::Signer` sidecar. |
| Paid execution | `covenant-xona::PaidExecutor` → `covenantd::xona::DaemonXonaExecutor` | The daemon binds the caller as payer, runs the loop, and records accounting via `covenantd::x402::record_paid_call`. |
| Settlement / budget / audit | `covenantd::x402::record_paid_call` (unchanged) | A Xona call is a `ResourceKind::Tool` receipt recording the live atomic amount. Rolls into the same Merkle batch and optional Synapse mirror. |

## Catalog

Xona publishes its endpoints to the [orbit-x402 registry](https://api.orbitx402.com)
(`GET /api/services-list`), which Covenant already polls through
`covenant_x402::OrbitClient`. `covenant-xona` filters that registry down to
entries whose `serverTitle` starts with `Xona Agent` **and** that carry a
pricing option on the configured `(network, asset)` rail.

Xona lists the same logical endpoints on three chains — Solana (bare slugs,
e.g. `image/creative-director`), Base mainnet (`base-main/*`), and Base testnet
(`base/*`). Covenant's funding key is a Solana keypair, so the default config
keeps only the **Solana-settled** endpoints (network `solana:5eykt4…`, asset
USDC `EPjFWdd5…`); the Base mirrors are dropped because the daemon cannot pay
them. Endpoints with no pricing on the rail are dropped too. Budget credits are
the cent value of the published price ($0.03 → 3).

At startup the daemon crawls the live registry (20s timeout) and falls back to
the vendored snapshot at `assets/xona-orbit-snapshot.json` if the crawl fails or
is slow — so a restart picks up Xona's current Solana endpoints without a
rebuild, and an offline boot still has the catalog. The registry price is a
discovery hint; the authoritative amount comes from the live 402 challenge and
is what the receipt records.

## x402 challenge format

Xona serves the mainline x402 challenge shape:

- the 402 body is a bare array of payment options (also tolerates
  `{"accepts": [ … ]}`);
- each option is a `covenant_x402::PaymentRequirements`: `amount` (atomic),
  `asset`, `payTo`, `scheme` (`"exact"`), and a CAIP-2 `network`;
- there is **no** `extra.feePayer` — the payment is self-paid.

`covenant-xona::x402` parses this directly, matches the option on the operator's
`(network, asset)` within the per-call cap, **pins the option's `payTo` to the
registry-advertised Xona payee** (binding the live challenge to discovery so a
manipulated 402 cannot redirect funds), normalises it onto the operator's CAIP-2
rail, and hands it to the `covenant_x402::Signer` sidecar. With no `feePayer`
present, the sidecar dispatches to `SolanaSigner`: the funder builds and signs a
legacy transfer, pays its own gas, and re-POSTs the request with the
base64 `x-payment` header.

## Tools

`xona.<slug>` names flatten the path: `image/creative-director` →
`xona.image.creative-director`, `tokens-api/asset-risk-summary` →
`xona.tokens-api.asset-risk-summary`. Xona's registry does not publish
per-endpoint argument shapes, so each tool surfaces a permissive object schema
with a documented `prompt` field and `additionalProperties: true`; the call
arguments are forwarded verbatim as the POST body. A tool marshals arguments
into a `PaidRequest` and hands it to the `PaidExecutor`; it never makes a
network call itself.

## Capabilities and budget

A `xona.*` call goes through the daemon's normal `call_tool` path and is gated
by the `tool.call.<name>` capability and its scope — the per-endpoint allowlist
is the set of `tool.call.xona.*` capabilities an agent holds, and the TTL is the
capability's expiry. The per-call cap is the config cap (or the endpoint's
published price), enforced before the signer runs. Per-day and total budget are
the agent's budget-ledger capacity, debited on success. Out-of-policy calls are
rejected before they leave the host.

The daemon binds the **caller** as payer per call, so the budget debit,
settlement receipt, and audit event (`ExternalPaymentSettled`) all land against
the agent that invoked the tool, sharing one receipt id.

## Configuration

The profile is opt-in and requires the x402 funding-key sidecar
(`Server::with_x402_dispatch`).

### Daemon env

| Var | Effect |
| --- | --- |
| `COVENANT_XONA_ENABLED` | Truthy (`1`/`true`/`yes`) enables the profile. |
| `COVENANT_XONA_NETWORK` / `COVENANT_XONA_ASSET` | Override the settlement rail (default Solana mainnet + USDC). **Warning:** the selector compares the option's `asset` against this value, so overriding it away from USDC effectively disables the asset pin. |
| `COVENANT_XONA_PER_CALL_CAP` | Atomic-USDC per-call ceiling. **Warning:** `0` defers to the registry-published price, which means a poisoned registry refresh could inflate the cap. Set explicitly (e.g. `35000` = $0.035 for the $0.03 `image/creative-director` endpoint). |
| `COVENANT_XONA_ALLOW` | Comma-separated endpoint-slug allowlist (`image/creative-director,audio/speech-to-text`). |
| `COVENANT_XONA_SERVER_TITLE_PREFIX` | Registry `serverTitle` prefix identifying Xona's entries (default `Xona Agent`). |
| `COVENANT_X402_SIGNER_BINARY` | Absolute path to the built `covenant-x402-signer` binary. Required when the Xona profile is enabled. |

### Sidecar env (read inside the spawned signer process)

| Var | Effect |
| --- | --- |
| `COVENANT_X402_FUNDING_KEYPAIR` | Path to the funder's Solana keypair JSON. Required. The funder must hold USDC, have an existing USDC ATA, and hold a little SOL for gas (Xona is self-paid). |
| `COVENANT_X402_RPC_URL` | Solana RPC for blockhash + ATA checks. Defaults to `https://api.mainnet-beta.solana.com`. |

### Security pins (built into the code, not configurable)

- **payTo:** `to_requirements` rejects any 402 whose `payTo` differs from the
  registry-advertised Xona payee for that endpoint (`9VaDVp1…` on Solana).
  Binds the live challenge to the discovery source and prevents fund
  redirection if Xona's response is MITM'd.
- **Rail filter:** only endpoints priced on the configured `(network, asset)`
  enter the catalog, so the Solana funding key is never asked to pay a Base
  endpoint it cannot settle.

### Live example

| Example | What it does | Cost |
| --- | --- | --- |
| `cargo run -p covenant-xona --example live_paid_call` | Builds the catalog (live registry, vendored fallback), POSTs `image/creative-director` to fetch the 402, runs the selector, and prints the payee-pin match. | free |
| `cargo run -p covenant-xona --example live_paid_call -- --confirm` | Real $0.03 USDC paid call via `execute_paid` (the inner 402-then-pay loop, self-paid `SolanaSigner`). | $0.03 USDC |

With the profile disabled, no `xona.*` tool is advertised or callable and no
Xona call ever leaves the host.

## Resale publishing (SAP bridge)

`covenant-xona::publish` derives per-tool resale descriptors so an operator who
runs the SAP bridge can republish a marked-up Xona tier (markup set via
`COVENANT_XONA_MARKUP_BPS`, the flow back through Covenant settlement). The
bridge owns the on-chain registration and its own capability; with the bridge
off, `publishable` returns nothing and the profile is unaffected.

## Discovery (provider-agnostic)

Xona is also reachable through generic x402 provider discovery: with
`COVENANT_X402_DISCOVERY_ENABLED`, a `Request::DiscoverProviders { server_title }`
(capability-gated by `x402.discover`) serves the cached orbit-x402 catalog —
filtered to Xona via `server_title: "Xona Agent …"` — as `DiscoveredProvider`
rows an agent can read before deciding to pay. Discovery is read-only: it never
signs, pays, or touches the budget. An inbound resale quality gate
(`covenantd::smart_layer`, default-off, fail-closed) is scaffolded for operators
who resell Xona-fronted tools.

## Settlement, attestation, and the SAP bridge

Nothing Xona-specific exists on the settlement or attestation path. A Xona
receipt is an ordinary `ResourceKind::Tool` receipt; operators who have enabled
the Synapse attestation mirror get its root mirrored on the same path as every
other receipt, and operators with the mirror off see no on-chain footprint
beyond the x402 payment itself.
