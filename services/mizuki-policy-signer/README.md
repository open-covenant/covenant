# Mizuki Policy Signer

This service is the financial safety boundary for Mizuki. It verifies settled payments, constructs narrowly defined transactions, persists signed bytes before broadcast, and reconciles retries without authorizing a second payment.

It is deliberately deployed separately from the Mizuki application. The application must not receive the signer key, database credentials, deployment credentials, or price-service credentials.

## Enforced policy

- Refund liability registration and execution each require a short-lived, action-bound Ed25519 authorization from a dedicated job authority key. Startup rejects that authority if it equals either custody authority.
- A finalized settlement can be registered for 24 hours so a signer or API outage cannot silently erase the refund guarantee. Per-operation and rolling daily limits still bound registration exposure.
- A finalized payment must be registered within one hour by default. One settlement binds to one job permanently, preventing a compromised request key from sweeping historical treasury payments.
- The signer reads finalized Solana data to derive the original token owner, treasury, mint, decimals, and exact amount.
- The configured token program, source owner, destination owner, mint, decimals, transaction signature, and finalized slot must agree.
- Exactly one checked transfer may target the treasury, and its amount must equal the treasury account's net token increase.
- A settlement can produce one registered liability and one finalized refund forever.
- A refund returns the exact raw token amount to the verified original owner.
- Each funded operation is capped at `$25` by default.
- Customer refunds and discretionary bounty funding have independent `$100` rolling 24-hour caps. Bounty reservations cannot consume protected refund capacity.
- Refund authorization checks the independently observed treasury token balance before reservation and again before signing.
- Refund-token balance, bounty-authority balance, and rent facts must agree exactly across both finalized RPC views. A conservative minimum is not used to hide provider disagreement.
- Contributor escrow transactions are built by the signer against one configured program. Serialized transactions and arbitrary instruction data are never accepted from callers.
- Refunds and contributor escrow use distinct authority keys. Startup fails if either key does not match its configured public authority or if the two keys are the same.
- The finalized escrow program must use loader v3 with no upgrade authority. Its executable bytes must match one pinned SHA-256 hash on both RPC providers before every escrow signature.
- Only configured token programs, the configured escrow program, the associated-token program, the memo program, and required system instructions can appear in a signed transaction.
- Escrow lamports are calculated from a fixed USD request only when two independently configured, freshness-bounded SOL/USD feeds agree within five percent by default.
- Both price observations are persisted with the operation. Conversion uses the lower agreed price so the bounty is not underfunded in USD terms.
- Converted escrow value must also remain below a separately configured absolute lamport ceiling.
- Bounty principal, state rent, vault rent, guard rent, and a transaction-fee reserve must be spendable before a reservation is accepted.
- Escrow is two-stage: a claimant-free vault is funded before publication, then the authority binds one wallet and an independently authenticated GitHub identity from a single-use signed challenge.
- The on-chain guard remains after state and vault rent are recovered, permanently preventing the same bounty digest from funding twice.
- Escrow release and refund are mutually exclusive durable resolutions. Release is valid only before the immutable claim expiry; refund is valid at or after it, using finalized chain time and the same on-chain boundary.
- Release requires an independently verified merged GitHub PR authored by the bound claimant, targeting the immutable repository, closing the immutable issue, and merged no later than claim expiry.
- Mock adapters are test-only. The deployable HTTP entry point refuses to start in mock mode.

The bearer token protects availability. Transaction safety does not rely on the caller being honest.

## Crash-safe transaction flow

1. Atomically reserve the idempotency key, protected resource, and rolling spend capacity.
2. Build and sign the allowed transaction.
3. Persist the complete signed wire transaction and deterministic signature.
4. Broadcast those persisted bytes.
5. Reconcile the signature to finalized chain state.

If the process exits after step 3, the same signed bytes are broadcast after restart. If it exits during step 4, the stored signature is reconciled first. An unobserved transaction is rebuilt only after its blockhash is definitively expired. Re-broadcasting identical bytes cannot create a second transfer.

## API

All `/v1` endpoints require `Authorization: Bearer <token>`. Mutation endpoints also require an `Idempotency-Key` header containing 8–128 letters, digits, dots, colons, underscores, or hyphens.

### Refund authorization

The application holds a dedicated Ed25519 request-authority key. This is not a Solana transaction key. The signer receives only its base58-encoded 32-byte public key. Authorization signatures are standard base64 encodings of the raw 64-byte Ed25519 signature and expire within 15 minutes by default.

Registration signs these exact UTF-8 bytes, with no trailing newline:

```text
Mizuki refund liability registration
Version: 1
Job: <jobId>
Settlement: <settlementSignature>
Expires At: <authorizationExpiresAt normalized with Date.toISOString()>
```

Execution uses the same format but changes the first line to `Mizuki refund execution authorization`.

### `POST /v1/refund-liabilities`

```json
{
  "jobId": "job-01",
  "settlementSignature": "<base58-signature>",
  "authorizationExpiresAt": "2026-08-22T12:10:00.000Z",
  "authorizationSignature": "<base64-64-byte-ed25519-signature>"
}
```

This route requires an `Idempotency-Key`. The signer verifies the action-bound signature, dual-RPC finalized settlement facts, transaction time, exact asset and amount, rolling liability limit, and treasury backing before durably binding the settlement to the job. Registration is only allowed during the configured recent-settlement window. Payer, recipient, mint, decimals, and amount cannot be supplied by the caller.

### `POST /v1/refunds`

```json
{
  "jobId": "job-01",
  "settlementSignature": "<base58-signature>",
  "authorizationExpiresAt": "2026-08-22T12:10:00.000Z",
  "authorizationSignature": "<base64-64-byte-ed25519-signature>"
}
```

This route requires a fresh execution signature and a matching registered liability. Authorization TTL and signature fields are excluded from the stable idempotency hash, so a crashed request can retry with fresh authorization and the same idempotency key.

### `POST /v1/refund-liabilities/:liabilityId/discharge`

```json
{
  "jobId": "job-01",
  "settlementSignature": "<base58-signature>",
  "repository": "owner/repository",
  "pullRequestNumber": 23,
  "authorizationExpiresAt": "2026-08-22T12:10:00.000Z",
  "authorizationSignature": "<base64-64-byte-ed25519-signature>"
}
```

Successful work releases its outstanding refund reserve only through this route. The application signs these exact UTF-8 bytes, with no trailing newline:

```text
Mizuki refund liability discharge authorization
Version: 1
Job: <jobId>
Settlement: <settlementSignature>
Repository: <lowercase-owner/repository>
Pull Request: <pullRequestNumber>
Expires At: <authorizationExpiresAt normalized with Date.toISOString()>
```

The signer independently queries the fixed GitHub GraphQL endpoint and requires a public base repository, merged PR, merge commit, and PR creation no earlier than the registered settlement time. It persists the evidence hash before releasing reserve capacity. Refund reservation and successful-work discharge lock the same liability row, so only one can start. The rolling registration cap remains consumed for 24 hours even after discharge.

### `POST /v1/escrows`

```json
{
  "bountyId": "bounty-01",
  "amountUsdCents": 1000,
  "acceptanceHash": "<64-character-lowercase-hex-hash>",
  "expiresAt": "2026-09-01T12:00:00.000Z",
  "repository": "owner/repository",
  "issueNumber": 17
}
```

Expiry must be between one hour and eight days. The signer computes lamports; callers cannot provide them. The returned `escrow_reserve` operation ID is the reservation ID for every later route. The application must not publish the bounty as open until this operation is finalized.

Finalized operation responses expose `reservationId`, `bountyDigest`, `escrowAddress`, `vaultAddress`, `guardAddress`, and `transactionSignature`. They never expose signed wire bytes.

### `POST /v1/github/identity-grants`

```json
{
  "accessToken": "<claimant-oauth-access-token>"
}
```

The signer calls the fixed official GitHub `/user` endpoint and returns a ten-minute, single-use grant containing the immutable numeric GitHub ID and login. The OAuth token is never persisted or logged. Callers cannot supply or override the resulting identity.

### `POST /v1/escrows/:reservationId/bind-challenges`

```json
{
  "claimantWallet": "<base58-wallet>",
  "githubGrantId": "00000000-0000-4000-8000-000000000000"
}
```

The identity grant is consumed atomically while the challenge is created, so it cannot bind a second wallet or bounty. The signer returns `id`, `message`, `expiresAt`, and `claimExpiresAt`. The claimant signs the exact UTF-8 `message` with the stated wallet. The challenge expires after ten minutes by default; the immutable claim deadline is exactly 48 hours by default and is returned to the application as the single public deadline.

### `POST /v1/escrows/:reservationId/bind`

```json
{
  "challengeId": "00000000-0000-4000-8000-000000000000",
  "signature": "<base64-encoded-64-byte-ed25519-signature>"
}
```

The signer verifies the stored challenge, wallet signature, challenge freshness, immutable bounty fields, and previously authenticated GitHub identity. Challenge consumption and the `escrow_bind` operation reservation are atomic. A vault can be bound once.

### `POST /v1/escrows/:operationId/release`

```json
{
  "pullRequestNumber": 23
}
```

The signer queries the fixed official GitHub GraphQL endpoint using its own read-only credential. It verifies repository, bound author, authorization time, merge state, merge commit, authoritative closing-issue references, and merge time. Release is rejected when finalized chain time is at or after `claimExpiresAt`. The caller cannot supply a merge receipt; the signer derives and persists it before signing.

### `POST /v1/escrows/:operationId/refund`

```json
{
  "reasonCode": "expired"
}
```

Allowed reason codes are `expired`, `rejected`, and `dispute_resolved`.
An unbound reservation can be refunded at or after its offer expiry. A bound reservation can be refunded at or after its immutable claim expiry. The program enforces the same strict split: release only before claim expiry and refund at or after it. No reason code can bypass time policy.

The permanent on-chain guard means a terminal bounty ID cannot be reused. Reopening requires a new external bounty record and must wait for the prior refund to finalize.

### `GET /v1/operations/:id`

Returns the durable operation state without exposing signed wire bytes or internal settlement evidence.

### `GET /v1/readiness`

Returns independently observed finalized refund capacity after subtracting every outstanding registered liability and the consumed rolling liability limit. A liability remains pending while its refund is prepared, submitted, or reconciling and is removed only after finalized refund state, preventing double subtraction. The authenticated response includes `refundTreasury`, `refundMint`, `refundDecimals`, `finalizedBalanceRaw`, `pendingRefundRaw`, `treasuryAvailableRefundRaw`, `remainingRefundLimitUsdCents`, and `availableRefundRaw`; raw amounts are decimal strings. `availableRefundRaw` is the lower of protected treasury capacity and rolling-limit capacity. Dependency or RPC disagreement returns HTTP 503 with `healthy: false`.

Readiness is healthy only when the database, both RPC providers, both named price observations, the signer-owned GitHub credential, the immutable pinned escrow program, refund-token custody, and bounty custody all pass a fresh probe.

### `GET /v1/readiness/evidence`

Returns the authenticated evidence behind the readiness decision. It reports boolean dependency checks, provider counts, the public program ID and executable hash, immutable-program status, public custody addresses and finalized balances, and the two bounded price observations. It does not return provider URLs, credentials, private keys, database details, or GitHub identity. Any failed required check returns HTTP 503 and leaves unavailable evidence `null`.

### Operational endpoints

- `GET /health` returns 200 only when the full production readiness probe passes and otherwise returns 503 without dependency details.
- `GET /metrics` emits Prometheus metrics without credentials or transaction payloads.

## Run locally

```bash
cp .env.example .env
pnpm install --ignore-workspace
pnpm typecheck
pnpm test
pnpm build
```

Mock adapters are available to the test suite through dependency injection. The HTTP entry point intentionally cannot start with them. Local server testing therefore requires local Postgres, RPC, signer, asset, program, and price-service configuration.

Production requires Postgres, distinct refund and escrow Solana keys, the controlled refund-token treasury, a separately budgeted SOL escrow pool, the escrow program configuration, two independent Solana RPC providers, two independent price endpoints, and a read-only GitHub credential. Production primary and secondary providers must use different hostnames; different paths or credentials on one hostname do not establish independence. Remote database connections must require TLS except for Render's single-label `dpg-*` private-network host; RPC and price endpoints must use HTTPS unless they target loopback. Database migrations and one full dependency probe run at startup. A degraded startup is reported through stderr and HTTP 503 readiness while the process remains available for durable recovery and already-authorized refund work. Use a database role scoped only to the signer schema.

Every Solana JSON-RPC HTTP request from both independent connections is capped by `MIZUKI_SIGNER_RPC_TIMEOUT_MS`. The default and production Blueprint value are `5000`; configuration rejects values below `1000` or above `10000`. Caller cancellation is preserved, RPC timeouts and HTTP 429/5xx responses become retryable policy failures, and web3's internal rate-limit retry is disabled so only the single scheduled recovery runner retries durable work.

Shutdown stops recovery scheduling and new HTTP intake, waits at most 30 seconds for the one active recovery, then lets active HTTP requests drain. The database pool closes only after recovery and HTTP work have settled, so teardown cannot race a live leased mutation. If recovery misses the grace, HTTP connections are forced closed, pool closure is skipped, and the process exits non-zero; the operating system closes its sockets and the durable lease is recovered after restart. A 90-second process deadline keeps the complete SIGTERM path below Render's 120-second termination window.

Required production settings without defaults are `MIZUKI_SIGNER_AUTH_TOKEN`, `MIZUKI_SIGNER_DATABASE_URL`, `MIZUKI_SIGNER_RPC_URL`, `MIZUKI_SIGNER_SECONDARY_RPC_URL`, `MIZUKI_REFUND_PRIVATE_KEY_JSON`, `MIZUKI_ESCROW_PRIVATE_KEY_JSON`, `MIZUKI_SIGNER_GITHUB_TOKEN`, `MIZUKI_JOB_AUTHORITY_PUBLIC_KEY`, `MIZUKI_REFUND_TREASURY`, `MIZUKI_ESCROW_AUTHORITY`, `MIZUKI_REFUND_MINT`, `MIZUKI_ESCROW_PROGRAM_ID`, `MIZUKI_ESCROW_PROGRAM_DATA_SHA256`, `MIZUKI_SOL_USD_PRICE_URL`, and `MIZUKI_SOL_USD_SECONDARY_PRICE_URL`. Set `NODE_ENV=production` and `MIZUKI_SIGNER_MOCK_MODE=false`. Either price token is optional when its endpoint does not authenticate requests. Every bounded policy setting and its default is recorded in `.env.example`; production operators should set them explicitly rather than relying on defaults.

## Devnet artifact canary

The devnet runner is read-only unless `--execute` is present. It never deploys a program, requests an airdrop, creates a key, or accepts a mainnet cluster. The RPC URL and all three role keypairs must be explicit, owner-only regular files. The authority, claimant, adversary, and program keys must be distinct.

Build the signer, put the one devnet RPC URL in an owner-only file, and run the read-only gate first:

```bash
chmod 600 /secure/devnet/rpc-url /secure/devnet/*.json
pnpm --filter @covenant/mizuki-policy-signer build
pnpm --filter @covenant/mizuki-policy-signer canary:devnet -- \
  --rpc-url-file /secure/devnet/rpc-url \
  --program-id DEVNET_PROGRAM_ID \
  --artifact /artifacts/mizuki_escrow_program.so \
  --artifact-sha256 APPROVED_SHA256 \
  --artifact-commit APPROVED_GIT_COMMIT \
  --authority-keypair /secure/devnet/authority.json \
  --claimant-keypair /secure/devnet/claimant.json \
  --adversary-keypair /secure/devnet/adversary.json \
  --output /evidence/mizuki-devnet-dry-run.json
```

The gate checks the authoritative devnet genesis hash, the loader-v3 deployment, byte-for-byte equality between finalized program data and the supplied SBPFv3 artifact, role separation, rent, funding capacity, and fresh PDA addresses. Add `--execute` and choose a new output path only after reviewing the dry-run receipt.

The live sequence publishes finalized transactions for prefunded-PDA adoption, fund, bind, exact release, wrong-claimant rejection, release replay rejection, fund replay rejection, expired-release rejection, bound expiry refund, and unbound expiry refund. It proves state and vault closure, the permanent guard commitment, transaction-level principal deltas, and exact refund accounting. The resulting mode-`0600` JSON omits RPC URLs, key paths, wallet addresses, balances, error logs, and all private material. It includes public signatures, bounty digests, the program ID, artifact provenance, assertion results, and a SHA-256 of the receipt payload. The runner refuses to overwrite an existing receipt.

## Deployment separation

- Deploy from a repository or protected path that Mizuki cannot modify.
- Use a distinct cloud project and service account.
- Deny shell access from the application network.
- Permit ingress only from the application service and operator network.
- Keep `/metrics` private at the network edge even though it contains no secrets.
- Alert on `reconciling`, `daily_limit_exceeded`, RPC disagreement, and stale price observations.
- Back up Postgres continuously; the operation rows are part of the payment proof.
- Rotate the bearer token independently of the transaction key.
- Give the GitHub token public-repository metadata read access only; it does not need content writes, administration, or workflow access.
- Finalize the escrow program before enabling the signer. Pin the SHA-256 of the dumped executable bytes only after reviewing the deployment and repeating escrow canaries.

## Mainnet gate

Do not enable public jobs until the transaction canaries have finalized and every failure drill below has passed:

1. One exact `$2` refund canary.
2. One `$10` contributor reserve, signed bind, and release canary.
3. One unbound expiry refund and one bound claim-expiry refund canary.
4. A forced process exit after broadcast followed by successful reconciliation with one transfer.
5. Concurrent duplicate requests producing one durable operation and one economic effect.
6. A deliberate price-feed disagreement failing closed without reserving or signing an escrow.
7. A staged RPC custody disagreement returning unhealthy readiness without reserving or signing.
8. A rejected GitHub signer credential returning unhealthy readiness while refund recovery remains available.
