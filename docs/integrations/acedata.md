# AceData integration

Status: experimental, off by default

Crate: `agent-os/crates/covenant-acedata`

## Current boundary

The production daemon can expose three AceData tools when an operator enables
the integration and supplies `COVENANT_ACEDATA_API_KEY`:

- `acedata.image.generate`
- `acedata.music.generate`
- `acedata.search`

That API key is an externally billed credential. A tool capability and optional
model allowlist control which registered operation an agent may request; they do
not impose a monetary cap, reserve provider credits, make a grant one-use, or
bind the request to AceData's eventual charge. An absent tool allowlist enables
all three tools. Operators must therefore enforce spend limits at the provider
account and should not expose these tools to untrusted agents.

Each successful call returns the provider response plus a local provenance
object containing provider, tool, model, hashes, asset references, and task ID.
The daemon can append an `AceDataGeneration` row to its local hash-chained audit
log and a local `ResourceKind::Tool` receipt. The recorded cost is currently
zero because the integration does not receive or reconcile authoritative
per-call billing metadata. The output hash covers canonical response JSON, not
downloaded asset bytes.

These records show what the configured daemon wrote about a provider response.
They do not prove who caused the generation, that the log is complete, that the
named model ran, that an asset URL still serves the same bytes, or what the
provider charged.

## Billing modes

| Mode | Production daemon | Boundary |
| --- | --- | --- |
| Bearer API key | Opt-in | Externally billed; model-scoped capability only; no local monetary budget or durable reservation. |
| Keyless x402 | Parked | Lower-level client and historical manual example remain, but the production daemon does not use a funding-key fallback. |

The reusable x402 path is not a supported daemon payment path or W009
enforcement. Re-enabling it requires the same exact-intent, one-use approval,
durable reservation, transaction decoding, and settlement reconciliation as any
other outbound payment path.

## Configuration

- `COVENANT_ACEDATA_ENABLED` — enables startup configuration parsing.
- `COVENANT_ACEDATA_API_KEY` — required billed credential. Without it the
  production daemon disables the integration; it does not fall back to x402.
- `COVENANT_ACEDATA_BASE_URL` — provider host override.
- `COVENANT_ACEDATA_ALLOW` — comma-separated tool allowlist; empty means all
  three current tools.
- `COVENANT_ACEDATA_IMAGE_MODEL` and `COVENANT_ACEDATA_MUSIC_MODEL` — default
  model names.

Before enabling this integration, use a dedicated provider credential with an
external spend limit, grant only named `tool.call.acedata.*` actions, pin the
model scope, and treat the local receipt's zero cost as unknown—not free.

## Optional audit anchoring

If the separate SAP publisher is explicitly enabled, an operator can publish a
root associated with the local audit log. A matching root and inclusion proof
show only that the publisher committed to those supplied log bytes. They do not
establish log completeness, provider execution, asset content, price, payer
authorization, or claim truth. This is not C2PA and is not achieved merely by
enabling AceData.

## Historical manual x402 observation

On 23 June 2026, a manual mainnet interoperability check observed AceData return
an x402 v2 challenge and accept a separately signed Solana USDC payment for a
search request. That one observation was not made through the production daemon
and does not establish current endpoint behavior, settlement finality, delivery
semantics, or W009 enforcement.

The historical Solana option used a self-paid fee-payer profile. The manual
caller signed with the lower-level `covenant-x402-signer`, adapted its legacy v1
payload to the provider's v2 envelope, and independently chose to spend funds.
Do not automate that flow until the production signer consumes an exact,
transaction-bound authorization and owns crash-safe idempotency.
