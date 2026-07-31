# Hyre x402 Provider

## Current status

`covenant-hyre` is a lower-level provider profile and x402 client. Its
daemon-owned payment integration is parked. `covenantd` does not advertise
`hyre.*` tools, construct a funding-key sidecar, or make a Hyre payment, even
when the legacy environment flags are set.

The previous daemon flow paid before a durable reservation and did not consume
a transaction-bound authorization. A caller supplied important policy fields,
while the budget debit and receipt happened only after the paid retry. Local
accounting could therefore neither prevent a concurrent or crash retry from
paying twice nor turn the call into W009 enforcement.

The reusable parsing, selection, signer, and explicit development examples stay
in the repository. `covenantd::hyre::DaemonHyreExecutor` is a fail-closed
compatibility boundary until the missing wallet controls exist.

See [Covenant x402](./x402.md) for the shared payment boundary and
[Covenant × zauth](./zauth.md) for inbound endpoint discovery.

## Safe checks

These commands do not pay:

```sh
cargo run -p covenant-hyre --example live_challenge
cargo run -p covenantd --example live_daemon_paid_call
```

The first reads and validates Hyre's public challenge. The second proves that
the daemon executor returns `LEGACY_OUTBOUND_PARKED` without signer or network
activity and without settlement, audit, or budget mutation.

There is no production environment flag that enables the daemon path.

## Components

| Concern | Where | Current boundary |
| --- | --- | --- |
| Catalog and pricing | `covenant-hyre::manifest`, `::catalog` | Parses Hyre's OpenAPI for explicit lower-level use. The daemon does not load it at startup. |
| Tool specifications | `covenant-hyre::tools` | Can generate `hyre.*` specs, but the production daemon does not advertise them. |
| Challenge and paid loop | `covenant-hyre::x402` | Reusable development code; not a production wallet boundary. |
| Daemon adapter | `covenantd::hyre::DaemonHyreExecutor` | Returns `LEGACY_OUTBOUND_PARKED` before signer or network activity. |
| Accounting helper | `covenantd::x402::record_paid_call` | Retained for tests; not wired to daemon-owned payment. A local row is not chain-settlement proof. |

## Catalog and challenge format

The crate vendors Hyre's OpenAPI at `assets/hyre-openapi.json` and can refresh it
from `${base_url}/openapi.json` with a bounded client. Only the Solana root paths
are selected by the current profile. Catalog prices are discovery hints; a
manual client must use the live 402 requirement and must not infer payment or
delivery from a local receipt.

The parser accepts Hyre's mainline challenge forms:

- an object containing `accepts`, or a bare option array;
- `maxAmountRequired` and the supported amount aliases;
- short and CAIP-2 Solana network identifiers;
- the `feePayer` carried in the payment-requirement metadata.

Before building a Solana payment, the lower-level profile checks the configured
network, USDC mint, recipient, amount ceiling, and PayAI fee payer. The signer
also requires an existing funder token account. These checks reduce substitution
risk in the development path; they do not provide one-use authorization,
crash-safe idempotency, or settlement reconciliation.

The shared HTTP client does not follow redirects. A 402, resource error, or
timeout does not prove that settlement failed, so a manual operator must inspect
the facilitator result and chain state before retrying.

## Tool shape

The lower-level crate flattens paths into names such as
`hyre.trenches.token.mint.snipers` and exposes `hyre.ask` for the high-level
query route. A generated tool only marshals arguments into a `PaidRequest` and
hands it to a `PaidExecutor`.

These names are not present in the production daemon tool list. The legacy
`tool.call.hyre.*` and `x402.outbound.pay` capability schemas remain for wire
compatibility, but no grant can currently authorize a Hyre payment.

## Legacy daemon configuration

| Variable | Current behavior |
| --- | --- |
| `COVENANT_HYRE_ENABLED` | Recognized only to emit a parked-path warning; no tools are registered. |
| `COVENANT_X402_ENABLED` | Recognized only to emit a parked-path warning; no signer config is constructed. |
| Other legacy Hyre/x402 variables | Ignored by `covenantd` while the path is parked. |

No funding-key sidecar is spawned by the daemon. The standalone signer has its
own explicit development configuration in [Covenant x402](./x402.md).

## Development examples

| Example | Effect | Cost |
| --- | --- | --- |
| `live_challenge` | Reads Hyre's live 402 and prints selection/pin results. | free |
| `live_facilitator_smoke` | Sends a deliberately invalid diagnostic payload to the facilitator. | free |
| `live_payai_verify_smoke` | Produces a signed transaction and asks the facilitator to verify it without settlement. It handles real key material. | free |
| `live_paid_call -- --confirm` | Makes a real lower-level $0.01 USDC call. It bypasses daemon policy and reservation; use only with a disposable, tightly funded key after manual challenge review. | $0.01 USDC |
| `live_daemon_paid_call` | Proves the daemon adapter stays parked and writes no accounting state. | free |

## Requirements before re-enabling

The daemon path stays parked until all of these hold:

1. Parse the received challenge into a closed, canonical payment intent.
2. Bind trusted endpoint, recipient, network, mint, token program, amount,
   scheme, fee payer, request body, Memo, writable accounts, and compute budget.
3. Require a signed, scoped, expiring, one-use approval over that exact intent
   and final transaction.
4. Decode and match the final transaction inside an isolated signer boundary.
5. Durably reserve approval and budget consumption before signing, with
   crash-safe idempotency and an explicit multi-process ownership model.
6. Reconcile transaction finality and resource delivery as separate outcomes
   before any retry.

Only after those controls are implemented and exercised end to end should Hyre
tools return to the production daemon catalog.
