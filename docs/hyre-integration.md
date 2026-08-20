# Hyre x402 Provider

Hyre is exposed to Covenant agents as a *provider profile* over the generic
outbound x402 gateway (`covenant-x402`), not as a parallel runtime. An agent
calls Hyre through capability-gated MCP tools; the daemon signs the x402
payment, debits the agent's budget, and writes a settlement receipt. The agent
never holds the funding key.

The profile lives in the `covenant-hyre` crate plus a thin daemon bridge in
`covenantd::hyre`. It adds no Hyre-specific code to the core crates: settlement,
budgeting, audit, and attestation are reused from the existing x402 accounting
path that backs every paid call.

> See also: [Covenant x402](./x402.md) for the generic pay-per-call layer Hyre rides on, and [Covenant × zauth](./zauth.md) for x402 endpoint discovery and monitoring.

## Quick start (operator)

```sh
# 1. Build the funding-key sidecar (its own workspace; isolates solana-sdk v4).
cd agent-os/crates/covenant-x402-signer && cargo build --bin covenant-x402-signer

# 2. Point a funded Solana keypair at it. The funder needs (a) a USDC token
#    account that exists on chain — PayAI does not create ATAs on the hot path
#    — and (b) enough USDC for the per-call price. SOL is not needed; PayAI's
#    facilitator co-signs as fee payer and pays gas.
export COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/funder.json
export COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com
export COVENANT_X402_SIGNER_BIN=$PWD/target/debug/covenant-x402-signer

# 3. De-risk the wire format against the live facilitator (free — no /settle).
cd ../../.. && cargo run -p covenant-hyre --example live_payai_verify_smoke
# expects: isValid: true, payer = your funder pubkey

# 4. Run a real paid call ($0.01 USDC against /defi/tvl).
cargo run -p covenant-hyre --example live_paid_call -- --confirm

# 5. Or exercise the full daemon path (DaemonHyreExecutor with the in-memory
#    settlement/audit/budget the daemon uses in production).
cargo run -p covenantd --example live_daemon_paid_call -- --confirm
```

To enable the profile on a real `covenantd` instance, set the env in the
[Configuration](#configuration) table and start the daemon — it will pick up
the sidecar and register `hyre.*` MCP tools.

## Components

| Concern | Where | Notes |
| --- | --- | --- |
| Endpoint catalog + pricing | `covenant-hyre::manifest`, `::catalog` | Parsed from Hyre's published OpenAPI (`x-payment-info`). No rebuild when Hyre adds endpoints. |
| MCP tools | `covenant-hyre::tools` | One `hyre.*` tool per endpoint, plus the high-level `hyre.ask`. |
| 402 challenge + pay loop | `covenant-hyre::x402` | Hyre's challenge wire shape, parsing, option selection, and the 402-then-pay loop. Reuses the `covenant_x402::Signer` sidecar. |
| Paid execution | `covenant-hyre::PaidExecutor` → `covenantd::hyre::DaemonHyreExecutor` | The daemon binds the caller as payer, runs the loop, and records accounting via `covenantd::x402::record_paid_call`. |
| Settlement / budget / audit | `covenantd::x402::record_paid_call` (unchanged) | A Hyre call is a `ResourceKind::Tool` receipt recording the live atomic amount. Rolls into the same Merkle batch and optional Synapse mirror. |
| Resale publishing | `covenant-hyre::publish` | Per-tool flag through the SAP bridge; no-op when the bridge is off. |

## Catalog

`covenant-hyre` vendors Hyre's OpenAPI at `assets/hyre-openapi.json` as the
offline boot default and can refresh it from `${base_url}/openapi.json`. Only
the Solana root paths are surfaced — the `/base/*` and `/skale/*` mirrors are
dropped because v1 settles on Solana through the PayAI facilitator. Pricing is
read from each operation's `x-payment-info.price` (USD, six decimals) and
converted to atomic USDC; budget credits are the cent value ($0.08 → 8).

Each endpoint becomes a `covenant_x402::RegistryEntry` (server title `Hyre`), so
the daemon's catalog tooling treats Hyre like any other x402 provider. The
catalog price is a discovery hint; the authoritative amount comes from the live
402 challenge and is what the receipt records.

At startup the daemon loads the live manifest from `${base_url}/openapi.json`
(10s timeout) and falls back to the vendored copy offline, so a restart picks up
Hyre's current endpoints without a rebuild. `x-payment-info` is generated from
the same `*_PRICES` constants as the live 402 challenge, so the advertised price
cannot drift from what gets enforced — confirmed by Hyre. For change detection
between refreshes, use a content hash (or ETag if/when Hyre exposes one on
`/openapi.json`) rather than `info.version`, which is bumped manually per
release and lags actual endpoint changes.

## x402 challenge format

Hyre serves the mainline x402 challenge shape:

- the 402 body is an object `{"accepts": [ … ], "x402Version": 1}`;
- options use `maxAmountRequired`, with the CAIP-2 id and `feePayer` in the base64 `payment-required` header;
- the body's `network` is the short `"solana"`, while the capability uses the CAIP-2 `solana:5eykt4…`.

`covenant-hyre::x402` parses this directly: it reads the `accepts` object (or a
bare array), accepts either amount spelling, matches the short and CAIP-2 network
forms, selects the option within the per-call cap, and normalises it onto the
operator's CAIP-2 rail before handing it to the signer. It reuses the
`covenant_x402::Signer` sidecar and the `PaymentRequirements` type the signer
consumes.

The selected option's `extra.feePayer` carries PayAI's sponsor wallet
(`2wKup…`). The production flow follows the standard x402 spec end-to-end:
the funder partial-signs a v0 `VersionedTransaction` whose `payerKey` is
PayAI's sponsor (funder slot signed, fee-payer slot left empty), wraps it
in the canonical x402 envelope, base64-encodes that as the `X-PAYMENT`
header, and re-POSTs to the Hyre endpoint. Hyre's middleware is the one
that calls `https://facilitator.payai.network/verify` and `/settle`;
PayAI co-signs as fee payer, lands the tx, and Hyre returns the response
to us. The client never POSTs to the facilitator on the hot path. We
carry `feePayer` through `covenant_x402::PaymentRequirements::extra` so
the signer can set the v0 payer slot. `covenant-hyre`'s
[`examples/live_facilitator_smoke`](../agent-os/crates/covenant-hyre/examples/live_facilitator_smoke.rs)
keeps a direct `/verify` probe as a diagnostic for when the production
path breaks.

## Tools

`hyre.<path>` names flatten the path and strip braces:
`/trenches/token/{mint}/snipers` → `hyre.trenches.token.mint.snipers`. Path
parameters are required arguments; query parameters are optional. `/ask` is the
single high-level `hyre.ask` tool taking a `query` string for agents that don't
want the structured surface. A tool marshals arguments into a `PaidRequest` and
hands it to the `PaidExecutor`; it never makes a network call itself.

## Capabilities and budget

A `hyre.*` call goes through the daemon's normal `call_tool` path and is gated
by the `tool.call.<name>` capability and its scope — the per-endpoint allowlist
is the set of `tool.call.hyre.*` capabilities an agent holds, and the TTL is the
capability's expiry. The per-call cap is the config cap (or the endpoint's
published price), enforced inside the gateway before the signer runs. Per-day
and total budget are the agent's budget-ledger capacity, debited on success.
Out-of-policy calls are rejected before they leave the host.

The daemon binds the **caller** as payer per call, so the budget debit,
settlement receipt, and audit event (`ExternalPaymentSettled`) all land against
the agent that invoked the tool, sharing one receipt id.

## Configuration

The profile is opt-in and requires the x402 funding-key sidecar
(`Server::with_x402_dispatch`).

### Daemon env

| Var | Effect |
| --- | --- |
| `COVENANT_HYRE_ENABLED` | Truthy enables the profile. |
| `COVENANT_HYRE_BASE_URL` | Override the API host (default `https://mpp.hyreagent.fun`). |
| `COVENANT_HYRE_NETWORK` / `COVENANT_HYRE_ASSET` | Override the settlement rail. **Warning:** the selector compares `accept.asset` against this value, so overriding `COVENANT_HYRE_ASSET` away from USDC effectively disables the asset pin. Only set when Hyre's challenge actually diverges. |
| `COVENANT_HYRE_PER_CALL_CAP` | Atomic-USDC per-call ceiling. **Warning:** `0` defers to the endpoint-published price, which means a poisoned manifest refresh would inflate the cap. Set explicitly (e.g. `12000` = $0.012 for the $0.01 `/defi/tvl` endpoint). |
| `COVENANT_HYRE_ALLOW` | Comma-separated endpoint-slug allowlist (`defi/tvl,defi/yields`). |
| `COVENANT_HYRE_MARKUP_BPS` | Resale markup in basis points for SAP-bridge publishing. |
| `COVENANT_X402_SIGNER_BINARY` | Absolute path to the built `covenant-x402-signer` binary. Required when the Hyre profile is enabled. |

### Sidecar env (read inside the spawned signer process)

The daemon's `SubprocessSigner` calls `.env_clear()` before spawning, so the
sidecar only sees what's explicitly passed to it:

| Var | Effect |
| --- | --- |
| `COVENANT_X402_FUNDING_KEYPAIR` | Path to the funder's Solana keypair JSON. Required. The funder must hold USDC and have an existing USDC ATA on the cluster the RPC points at. |
| `COVENANT_X402_RPC_URL` | Solana RPC for blockhash + ATA-existence checks. Defaults to `https://api.mainnet-beta.solana.com`. |

### Security pins (built into the code, not configurable)

- **payTo:** `to_requirements` rejects any 402 whose `pay_to` differs from
  `covenant_hyre::config::PAY_TO` (`7G73…`). Prevents fund redirection if
  Hyre's response is MITM'd or compromised.
- **PayAI sponsor:** `to_requirements` rejects any 402 whose
  `extra.feePayer` differs from `covenant_hyre::config::PAYAI_FEE_PAYER`
  (`2wKup…`). Prevents a substituted sponsor pubkey from landing in the
  v0 message's `payerKey` slot. (PayAI would reject it at `/verify`, but
  the explicit pin closes the only way an attacker pubkey enters the
  signed message besides `payTo`.)
- **USDC mint allowlist:** `PayaiSolanaSigner::build_payment` refuses to
  build a transfer for any mint not in `decimals_for_mint` (currently
  USDC mainnet + devnet). Token-2022 mints are not supported.
- **Source ATA pre-check:** the sidecar runs `getAccountInfo` against
  the funder's ATA before building the transaction, failing early with
  an actionable error if the ATA does not exist. PayAI removed
  on-the-fly ATA creation, so a missing source ATA would otherwise
  silently fail at `/settle`.

### Live examples

| Example | What it does | Cost |
| --- | --- | --- |
| `cargo run -p covenant-hyre --example live_challenge` | GETs Hyre's live `/defi/tvl` and `/ask`, parses the 402, runs the selector, prints the pin matches. | free |
| `cargo run -p covenant-hyre --example live_facilitator_smoke` | POSTs a deliberately malformed payload to `https://facilitator.payai.network/verify`. Asserts `isValid: false` with reason `invalid_exact_svm_payload_transaction_could_not_be_decoded`. Diagnostic for the HTTP round-trip. | free |
| `cargo run -p covenant-hyre --example live_payai_verify_smoke` | Drives the real sidecar to produce a real funder-signed v0 transaction, then POSTs to PayAI's `/verify`. Asserts `isValid: true`. **This is the de-risking gate before any paid call.** | free |
| `cargo run -p covenant-hyre --example live_paid_call -- --confirm` | Real $0.01 USDC paid call against Hyre `/defi/tvl` via `execute_paid` (the inner 402-then-pay loop). Dry-run without `--confirm`. | $0.01 USDC |
| `cargo run -p covenantd --example live_daemon_paid_call -- --confirm` | Same call but through `DaemonHyreExecutor` — exercises the full daemon path (budget pre-check, settlement receipt, audit event, issuer/payer split). Dry-run without `--confirm`. | $0.01 USDC |

With the profile disabled, no `hyre.*` tool is advertised or callable and no
Hyre call ever leaves the host.

`FEE_PAYER_PRIVKEY` in Hyre's own env is for their separate `/mpp/*` rail, not
the x402 path — do not conflate. On the x402 path Hyre holds no funder key for
us; we sign as funder, PayAI's facilitator co-signs as fee payer.

## Settlement, attestation, and the SAP bridge

Nothing Hyre-specific exists on the settlement or attestation path. A Hyre
receipt is an ordinary `ResourceKind::Tool` receipt; operators who have enabled
the Synapse attestation mirror get its root mirrored on the same path as every
other receipt, and operators with the mirror off see no on-chain footprint
beyond the x402 payment itself. The integration is orthogonal to the SAP bridge:
either can ship first.
