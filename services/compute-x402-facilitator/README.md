# Covenant x402 payment verifier

Private Solana payment component for `covenant-compute-gateway`. This service uses
the pinned official `@x402/svm` 2.28.0 exact-payment verifier. It listens only on
loopback, requires a shared bearer secret, and does not expose a public
facilitator or signing endpoint.

## Deployment contract

Run Node 24 (`npm ci --omit=dev --ignore-scripts`, then `npm start`) alongside the
Rust `covenant-compute-gateway` binary. Keep one gateway replica with its durable
volume. Start the verifier before opening public gateway traffic.

Shared environment:

- `COVENANT_GATEWAY_BRIDGE_SECRET`: at least 32 random characters; persist across
  deployments because it authenticates the stored payment evidence.
- `COVENANT_GATEWAY_NETWORK`: Solana mainnet CAIP-2 identifier
  `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` (devnet available for rehearsal).
- `COVENANT_GATEWAY_ASSET`: exact official USDC mint for the selected network.
- `COVENANT_GATEWAY_PAY_TO`: merchant wallet address, with its USDC ATA created.
- `COVENANT_GATEWAY_FEE_PAYER`: dedicated funded facilitator public key.
- `COVENANT_GATEWAY_RESOURCE_URL`: canonical public HTTPS URL ending in
  `/v1/chat/completions`.
- `COVENANT_GATEWAY_PRICE_MICRO_USDC`: fixed customer price, positive integer.

Verifier only:

- `COVENANT_GATEWAY_SOLANA_RPC`: HTTPS RPC for the configured network; genesis is
  checked at startup and before each verification/settlement.
- `COVENANT_GATEWAY_FEE_PAYER_KEY_FILE`: private absolute JSON file containing the
  64-byte Solana keypair; no group/world permissions. Never put this in a repo.
- `COVENANT_GATEWAY_BRIDGE_PORT`: optional, default `4043`.

Rust gateway:

- `COVENANT_GATEWAY_ENABLED=true` and `COVENANT_GATEWAY_HOME`: absolute persistent
  directory; private identity, replay journal and exclusive process lock live here.
- `COVENANT_GATEWAY_BRIDGE_URL=http://127.0.0.1:4043`.
- `COVENANT_COMPUTE_COORDINATOR_URL`: actual funded coordinator endpoint.
- `COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC` and `COVENANT_COMPUTE_BUDGET_HOME`: mandatory persistent rolling spend cap.
- `COVENANT_GATEWAY_RESERVE_MICRO_USDC`: lifetime operator-cost ceiling. Completed
  costs, uncertain work and known seller losses all consume it. Customer payments
  never automatically replenish it or imply stablecoin conversion.
- `COVENANT_GATEWAY_OFFER_MICRO_USDC`, `COVENANT_GATEWAY_MODEL`,
  `COVENANT_GATEWAY_MAX_TOKENS`: exact native inference offer and input limits.
- `COVENANT_GATEWAY_LISTEN`: public listener address, default `127.0.0.1:4042`.

To increase a funded lifetime reserve, verify the buyer's available coordinator
balance first, raise the configured ceiling and explicitly set
`COVENANT_GATEWAY_ALLOW_RESERVE_INCREASE=true` for that startup. The journal is
updated atomically with all prior entries retained. Reductions and unapproved
increases fail closed. Never delete, rotate or reset the journal to add capacity.
Changes to merchant, network, buyer, model, prices, fee payer or canonical URL need
an explicit migration/recovery review; they cannot silently reinterpret old work.

## Public protocol and recovery

POST `/v1/chat/completions` with `model`, text-only `messages`, `max_tokens` and
optional `temperature`. A UUID `Idempotency-Key` reuses the initial quote. A 402
responds with the standard Base64 `PAYMENT-REQUIRED` V2 header. Clients sign the
returned exact-SVM requirements and repeat the same request with
`PAYMENT-SIGNATURE`; no Covenant-specific payment header is required.

The official SDK's `extra.memo` binds quote UUID and sorted-JSON request SHA-256
inside the signed payment transaction. The gateway verifies it, signs stable
native Compute job IDs, then fsyncs the payment and jobs before dispatch. The
verifier signs the fee-payer slot locally during admission; it never broadcasts
from `/verify`. Static validation caps each sponsored transaction at two required
signatures, 60,000 compute units and 1,000 microlamports per CU (at most 10,060
lamports). Smart-wallet/CPI payments are disabled for this release.

The full quote/request memo consumed 43,642 compute units in a read-only mainnet
simulation. The installed Covenant client raises the official SDK's unsigned
20,000-unit instruction to a fixed 60,000 before AgentKit signs; every other
instruction and the blockhash remain unchanged. The bridge still runs the
official signature, memo, transfer and live simulation checks. A transaction
above 60,000 units or 1,000 microlamports per unit is rejected before simulation
or fee-payer signing.

Only output whose native operator receipt verifies can trigger customer
settlement. `/settle` always queries historical signature state before sending
exactly the persisted transaction bytes. Lost RPC responses, expiry and unknown
execution remain pending; they never mint a replacement payment or release a
result. Finalized success additionally requires an exact transaction readback.
Native operator payments are never reversed to make a failed customer charge
whole. The gateway bears that bounded reserve loss.

The Rust recovery worker resumes pending work after restart even without client
traffic. HTTP 202 means retry the identical body and original signed header.
Successful responses carry the standard Base64 `PAYMENT-RESPONSE` plus a native
signed envelope, signed receipt and exact output JSON for independent client
verification. Cached output expires after five minutes; permanent replay records
remain and return 410 rather than repurchasing.

`GET /health` is process liveness, not proof of funded inference capacity or a
successful paid transaction. Production acceptance must demonstrate those
separately. Set `COVENANT_GATEWAY_ADMISSIONS_ENABLED=false` on rollback; recovery and
settlement continue against the original durable state while new quotes and
payment admissions are refused. Keep `COVENANT_GATEWAY_ENABLED=true` so the
process and recovery worker stay alive.

## Focused verification

```sh
npm test --prefix services/compute-x402-facilitator
cargo test --manifest-path agent-os/Cargo.toml -p covenant-compute-gateway
```

Tests use real Solana signatures with the official verifier, mocked chain reads,
restart/concurrency fault cases and a real local HTTP server. They do not spend
funds or establish production acceptance.
