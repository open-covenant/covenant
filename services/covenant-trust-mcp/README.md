# Covenant Trust

Covenant Trust is a standalone, catalog-neutral service layer for pre-payment
agent checks. Marketplaces, skill catalogs, and x402 clients remain consumers:
they ask for evidence, apply their own policy, and decide whether to pay.

The service deliberately returns facts rather than a universal
`trusted: true` verdict:

- MPL Core asset facts, indexed AgentIdentity-plugin presence, registry PDA
  owner matching, and Covenant validation envelopes
- coverage-limited PayAI-sponsored USDC transfer history for a Solana wallet
- Ed25519 attestation integrity, with optional expected-signer matching

Every operation is read-only or pure cryptographic verification. The service
holds no transaction signer and cannot move funds.

## Interfaces

| Interface | Route |
|---|---|
| MCP over Streamable HTTP | `POST /mcp` |
| OpenAPI | `GET /openapi.json` |
| Agent identity and validation | `GET /v1/agents/{asset}` |
| Observed transfer history | `GET /v1/payment-history/{wallet}` |
| Attestation signature verification | `POST /v1/attestations/verify` |
| Health | `GET /health` |

The MCP surface exposes the same three checks as structured tools.

## Trust boundaries

- Transfer history covers only the reported window of recent PayAI fee-payer
  signatures. Fee sponsorship does not prove an x402 request, settlement
  receipt, job completion, or lifetime reputation. Each aggregate includes the
  contributing transaction signatures, slots, senders, amounts, and observation
  time so a consumer can independently inspect the underlying transfers.
- A valid `covenant.attest.v1` signature proves authorship by the carried key.
  It proves Covenant authorship only when that key matches an independently
  trusted signer.
- Validation-record authorship is checked against the AppData
  `data_authority`, including its `Address` variant, not the caller-controlled
  top-level authority field.
- `recordAuthentic` covers the validation envelope and author key only. The
  service does not yet decode the MIP-014 registration account, recompute the
  committed evidence, or make a policy decision; those fields remain `null`.
- The proposed v1 profile (`mpl.agent.validation-record.v1` /
  `org.opencovenant.audit-chain.v1`) is separate from Covenant's deployed
  legacy record. The legacy `sha256-merkle` label means the documented linear
  SHA-256 chain; it is never accepted as a v1 algorithm.
- The v1 record list is a bounded scan of assets currently owned by the pinned
  validator. Ownership indexing can omit authentic records, so the response
  always reports `coverage.complete: false`.
- Registration and metadata URIs are untrusted on-chain strings. Covenant Trust
  returns them but never fetches them.
- DAS is an indexed view of chain state. Consumers requiring a trust-minimized
  path should independently decode the underlying accounts or compare multiple
  providers.

## Run

```sh
npm ci
npm run build
COVENANT_SOLANA_MAINNET_RPC_URL=<das-capable-rpc> npm start
```

For local stdio MCP:

```sh
COVENANT_SOLANA_MAINNET_RPC_URL=<das-capable-rpc> node dist/server.js --stdio
```

After a public deployment:

```sh
claude mcp add --transport http covenant-trust https://<host>/mcp
codex mcp add covenant-trust --url https://<host>/mcp
```

## Verify

```sh
npm test
npm run test:smoke
```

The smoke test exercises both stdio and Streamable HTTP without external RPC
calls. A live DAS/RPC run is opt-in:

```sh
COVENANT_SOLANA_MAINNET_RPC_URL=<das-capable-rpc> npm run test:live
```

## Configuration

- `COVENANT_SOLANA_MAINNET_RPC_URL`: DAS-capable mainnet RPC. Required for the
  DAS-backed identity surface in production.
- `PORT`: HTTP port, default `8930`.
- `RPC_TIMEOUT_MS`: per-RPC timeout, default `9000`.
- `PAYMENT_HISTORY_LIMIT`: recent PayAI signatures to inspect, default `100`, maximum
  `1000`.
- `RATE_LIMIT_PER_MINUTE`: per-client HTTP request limit, default `30`.

The service is deployable but is not documented here as publicly live until its
production URL and live verification have been confirmed.
