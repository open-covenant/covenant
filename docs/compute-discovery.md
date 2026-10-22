# Live x402 discovery

Package 9 adapts [Prism's discovery manifest](https://github.com/prismnetwork-tech/prism/pull/192)
to Covenant's released x402 V2 gateway and model-specific output-cap pricing.
This source change does not itself establish production deployment or indexer
acceptance.

## Public contract

`GET /.well-known/x402.json` and `GET /v1/models` are served by the same gateway
as `POST /v1/chat/completions`. They require no wallet or credentials. Both
successful responses and discovery errors use `Cache-Control: no-store`.

`GET /openapi.json` exposes the same live offers as OpenAPI 3.1 for indexers such
as x402scan. It includes the POST request schema, sample inputs, response schemas
and exact fixed/dynamic USD price bounds. It applies the same admission,
capacity, payment-readiness and budget checks as the manifest; unavailable offers
produce no paid paths. The configured resource origin supplies the server URL.
See [x402scan registration](compute-x402scan.md) for the integration contract and
acceptance steps. Publishing OpenAPI does not itself establish a listing.

The manifest has `schemaVersion: 1`, `x402Version: 2`, `observedAtMs`, `available`,
`models`, and `endpoints`. Its paid entries use the configured HTTPS resource URL,
never an incoming Host header. Each entry includes the exact Solana network,
USDC mint, merchant, fee payer, request schema, and pricing mode:

- Omit `pricing` for the existing flat request price.
- Send `pricing: "model_tariff_v1"` for the advertised retail tariff. The fixed
  upfront price is `base_micro_usdc + ceil(max_tokens *
  output_cap_micro_usdc_per_million / 1000000)`. Generated token counts do not
  change it.

The actual HTTP 402 response remains the payment authorization contract. Clients
send the identical request body with `Payment-Signature`; `Payment-Required`
carries the V2 requirements. An optional UUID `Idempotency-Key` identifies an
unpaid quote. A discovery document contains no payment authorization, quote ID,
native envelope, customer identity, prompt, private endpoint, or credentials.

Each chat quote also carries the x402 `bazaar` extension, so a facilitator that
catalogs resources (Coinbase's CDP facilitator does, when it settles a Base
payment) can list the endpoint with a request example, an input schema and a
sample response. The `resource` object adds `serviceName`, `tags` and `iconUrl`
for catalog display. The request example is fixed; a buyer's prompt is never
placed in it. Batch quotes carry no `bazaar` extension.

Only text chat is advertised: one configured model, 1–32 nonempty system/user/
assistant messages, the configured normalized JSON message-byte limit, bounded
output tokens, optional temperature, and no streaming, tools, vision, or batches.
The manifest does not advertise unactivated payment networks or other services.

## Availability and recovery

Each catalog read fetches bounded, uncached coordinator capacity and takes a
read-only durable-budget snapshot. Legacy supply must cover its flat native ask;
tariff supply must contain the exact configured native tariff, including its
revision and validity window. A tariff is removed within 45 seconds of expiry.
Anonymous capacity is an observation of matchable supply, not a reservation or
a guarantee that a particular approved operator will execute a later request.
The existing approved-receipt-signer check still applies to completed work.

The private authenticated verifier's `/readiness` response binds the loaded
payment identity and complete retail policy. It performs no RPC, simulation,
signing, or settlement. Malformed policy closes new discovery and quotes while
the existing `/health` and sealed-payment recovery remain available.

The gateway also checks its admission latches, journal health, quote limits,
prefunded reserve and daily budget. Affordable output caps are reported per
pricing mode. A reduced cap never rewrites the signed tariff definition.
`models[].pricing.max_output_tokens` is the effective primary-mode cap;
`legacy_pricing.max_output_tokens` is the legacy cap. Empty affordable supply
returns `available: false` and no paid endpoint; unavailable dependencies return
HTTP 503 without private diagnostic details.

GET discovery does not sign or create a quote, reserve funds, rewrite the budget
or gateway journal, dispatch a provider request, or authorize a payment. Fresh
quote creation rechecks availability and payment readiness. Already persisted
executing/settling jobs retain their original terms and recovery behavior,
independent of catalog availability, price rotation, or admission shutdown.

## Validation and rollout

Local tests exercise real HTTP discovery and quote comparison, both pricing
modes, exact rounding, capacity disappearance, tariff rotation/expiry, bounded
and malformed dependency responses, reserve/budget limits, and byte-preserving
catalog reads. Existing payment/restart and invalid-configuration recovery tests
remain required.

The standalone public acceptance command and its public-only expectations are
described in [the deployment guide](../deploy/compute-x402/DISCOVERY.md). It defaults
to GET-only checks. Its explicit `--quotes` mode creates four unpaid server quotes
to compare actual 402 requirements; it never supplies a payment signature or
uses a wallet. Those POSTs are no-payment probes, not read-only operations.

Deploy only an immutable image that passes the isolated 512 MiB persistent-state
smoke test. Preserve all identities, environment values, admission latches,
budgets, provider-loss records and financial journals. The normal service startup
performs a paid provider qualification probe; it needs an allowance for this
rollout. The completed model-pricing allowance does not carry over. No customer
purchase is needed to accept discovery. Record exact image/source, deployment,
public observations and pre/post financial-state comparisons before marking
package 9 live.
