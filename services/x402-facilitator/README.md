# Covenant x402 facilitator

A public x402 v2 facilitator for USDC payments on Solana and Base. Sellers
point their x402 middleware at it to verify and settle payments. It is free,
needs no account or API key, and pays the network fees itself.

```
https://facilitator.opencovenant.org
```

## What it settles

| Network | Scheme  | Asset | Network fees paid by |
| ------- | ------- | ----- | -------------------- |
| Solana mainnet (`solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`) | `exact` | USDC `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` | the fee payer listed at `GET /supported` |
| Base mainnet (`eip155:8453`) | `exact` (EIP-3009) | USDC `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913` | the signer listed at `GET /supported` |

On Solana it also accepts payments from wallets that add their own
safety-check (guard) instructions around the transfer, as Phantom does. The
reference verifier rejects these payments (x402-foundation/x402#3318).

## Use it

With the official TypeScript SDK:

```ts
import { HTTPFacilitatorClient } from '@x402/core/server';

const facilitator = new HTTPFacilitatorClient({ url: 'https://facilitator.opencovenant.org' });
```

Pass it to your x402 resource server or middleware as you would any other
facilitator. Fee payer and signer addresses come from `GET /supported`.

## Endpoints

| Route | Purpose |
| ----- | ------- |
| `GET /supported` | Schemes, networks, signer addresses and extensions (`bazaar`). |
| `POST /verify` | `{x402Version, paymentPayload, paymentRequirements}` in, `{isValid, invalidReason?, payer?}` out. |
| `POST /settle` | Same body; `{success, transaction, network, payer?, errorReason?}` out. |
| `GET /discovery/resources` | Bazaar catalog. Filters: `type`, `payTo`, `scheme`, `network`, `extensions`; pages with `limit` (1 to 100) and `offset`. |
| `GET /discovery/search?query=` | Catalog search by words in the URL, name, description and tags. |
| `GET /` | Networks, limits and settlement counts. |
| `GET /healthz`, `GET /readyz` | Liveness, and whether each signer can pay for gas right now. |

## Limits

Because the facilitator pays network fees for everyone, these limits stop
anyone from draining its gas balance:

- Only USDC, and only the `exact` scheme.
- Payments under $0.001 are refused (`amount_below_minimum`).
- A payment to the payer's own address is refused (`self_send_not_allowed`).
- Each receiving address can settle 500 payments per network per UTC day,
  and the facilitator settles at most 2,000 per network per day
  (`pay_to_daily_limit_reached`, `facilitator_daily_limit_reached`).
- On Solana, the priority fee a payer sets is capped. For most wallets, a
  payment can request at most 100,000 compute units at up to 0.1 lamports
  each. For wallets that add guard instructions, the cap is 20,000 lamports in
  total.
- On Base, smart wallets that are not yet deployed (ERC-6492) are refused.
- Each client IP address can make 120 verify and settle calls per minute.

## Bazaar

When a settled payment carries the x402 `bazaar` extension, the resource is
added to the catalog at `/discovery/resources`, keyed by its URL and method (or
MCP tool name). The result of cataloging is returned to the seller in the
`EXTENSION-RESPONSES` header. Only settled payments are cataloged.

## Run it

```bash
pnpm --filter @covenant/x402-facilitator build
pnpm --filter @covenant/x402-facilitator start
```

| Variable | Meaning |
| -------- | ------- |
| `COVENANT_FACILITATOR_PUBLIC_URL` | Public HTTPS URL, shown at `/`. |
| `COVENANT_FACILITATOR_DATA_DIR` | Directory for the SQLite ledger and catalog. Use a persistent disk. |
| `COVENANT_FACILITATOR_SOLANA_RPC_URL` | Solana mainnet RPC. |
| `COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON` | Fee payer key, a JSON array of 64 bytes. |
| `COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PUBLIC_KEY` | The fee payer's address. It must match the key above. |
| `COVENANT_FACILITATOR_BASE_RPC_URL` | Base RPC. |
| `COVENANT_FACILITATOR_BASE_SIGNER_PRIVATE_KEY` | Signer key, `0x` and 64 hex characters. |
| `COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS` | The signer's address. It must match the key above. |
| `COVENANT_FACILITATOR_BASE_NETWORK` | `eip155:8453` (default), or `eip155:84532` for a Base Sepolia test instance. |
| `COVENANT_FACILITATOR_MIN_AMOUNT` | Smallest payment in USDC atomic units. Default `1000`. |
| `COVENANT_FACILITATOR_DAILY_SETTLEMENTS` | Maximum settlements per network per day. Default `2000`. |
| `COVENANT_FACILITATOR_DAILY_SETTLEMENTS_PER_PAY_TO` | Maximum settlements per receiving address per network per day. Default `500`. |
| `COVENANT_FACILITATOR_UNMETERED_PAY_TO` | Comma-separated receiving addresses that are exempt from the daily caps. |

Each network is enabled by setting its RPC URL, key and address together. The
service refuses to start if a key does not match its configured address. Use
keys that are not used for anything else, because block explorers attribute
every USDC transfer those keys sign to your facilitator.
