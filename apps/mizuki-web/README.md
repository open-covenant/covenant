# Mizuki web

Public product surface for Mizuki, an autonomous maintainer who delivers a validated pull request or refunds the customer in full. The application keeps failed work visible by turning completed refunds into funded rescue bounties.

## Product routes

- `/work` quotes a public GitHub issue and settles the fixed USDC price through a Wallet Standard-compatible Solana wallet.
- `/jobs/:id` polls a public job receipt without repeating financial actions.
- `/bounties` exposes funded rescue work and current claim state.
- `/bounties/:id` starts GitHub identity verification, proves payout-wallet ownership, and submits a claim.
- `/treasury` separates finalized signer custody from local liabilities, application-ledger allocation modeling, and transaction receipts.
- `/capabilities` shows benchmark-backed capability evidence.
- `/activity` consumes the public server-sent event stream.

The token panel is intentionally secondary and has no customer-work controls.

## Run locally

Requires Node.js 22 or newer and pnpm 10.

```sh
cp .env.example .env.local
pnpm install --ignore-workspace
pnpm dev
```

Set `MIZUKI_API_URL` to the server-side API origin. Browser requests use the same-origin `/api/mizuki/*` proxy so x402 headers, cookies, and SSE remain available without broad cross-origin permissions.

Set `MIZUKI_DEMO_MODE=1` only for visual development. Demo data is clearly labeled and does not simulate POST requests, payment, wallet proof, or claims.

## Expected API

The site consumes these public endpoints:

- `POST /v1/quotes`
- `POST /v1/jobs`
- `GET /v1/jobs/:id`
- `GET /v1/metrics`
- `GET /v1/bounties`
- `GET /v1/bounties/:id`
- `POST /v1/bounties/:id/wallet-proof`
- `POST /v1/bounties/:id/claim`
- `GET /v1/treasury`
- `GET /v1/capabilities`
- `GET /v1/activity`
- `GET /v1/events`

List endpoints may return an array or an object containing `items`, with resource-specific aliases accepted for compatibility. Financial writes require an idempotency key. The proxy rejects administrative paths.

Wallet proof uses a two-call challenge flow. The first call submits the address and receives an exact message plus challenge identifier. The second call submits the signed message and base64 signature. The site never asks a wallet to sign an invented ownership statement.

## Validation

```sh
pnpm typecheck
pnpm test
pnpm build
```

The production build emits a standalone Next.js server. Configure OAuth callback and cookie domains so the GitHub session is valid for the same public origin used by the proxy.
