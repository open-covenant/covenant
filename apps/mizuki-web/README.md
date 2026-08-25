# Mizuki the Mech website

Public customer website for Mizuki the Mech, an AI maintenance agent that delivers a validated pull request or returns the quoted USDC payment. After a refund finalizes, a separate SOL-funded maintenance bounty can make the unresolved issue available to contributors.

## Product routes

- `/work` quotes a public GitHub issue and settles the fixed USDC price through a Wallet Standard-compatible Solana wallet.
- `/jobs/:id` polls a public job receipt without repeating financial actions.
- `/bounties` lists funded maintenance bounties and their current claim state.
- `/bounties/:id` starts GitHub identity verification, proves payout-wallet ownership, and submits a claim.
- `/treasury` distinguishes the finalized refund reserve, outstanding refund obligations, planning estimates, and on-chain transactions.
- `/capabilities` shows benchmark-backed capability evidence.
- `/activity` consumes the public server-sent event stream.

Token information is informational only and never controls customer jobs.

## Run locally

Requires Node.js 22 or newer and pnpm 10.

```sh
cp .env.example .env.local
pnpm install --ignore-workspace
pnpm dev
```

Set `MIZUKI_API_URL` to the server-side API origin and give the web and API services the same `MIZUKI_WEB_PROXY_SECRET` of at least 32 UTF-8 bytes. Browser requests use the same-origin `/api/mizuki/*` proxy so x402 headers, cookies, client identity, and SSE remain available without broad cross-origin permissions. On Render, the proxy validates Cloudflare's overwritten `CF-Connecting-IP` value and replaces all inbound Mizuki context headers before authenticating that address to the API. It never derives identity from `X-Forwarded-For`, which Cloudflare appends to caller-controlled values.

Set `MIZUKI_DEMO_MODE=1` only for visual development. Demo data is clearly labeled and does not simulate POST requests, payment, wallet proof, or claims.

## Expected API

The site consumes these public endpoints:

- `POST /v1/quotes`
- `POST /v1/jobs`
- `GET /v1/jobs/:id`
- `GET /v1/admission`
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
pnpm smoke
```

The production build emits a standalone Next.js server. The smoke test starts that output against a fake API and verifies receipt 404s, outage handling, valid receipts, and clean shutdown. Do not add a shared `loading.tsx` or Suspense boundary above the job and bounty detail routes: Next.js sends streamed not-found pages with HTTP 200.

Configure OAuth callback and cookie domains so the GitHub session is valid for the same public origin used by the proxy.
