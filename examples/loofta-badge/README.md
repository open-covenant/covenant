# Covenant verified-enclave badge for Loofta Pay

Loofta settles inside MagicBlock Private Ephemeral Rollups, which are Intel TDX
enclaves. Covenant DCAP-verifies those enclaves on mainnet and keeps a
verified-ER attestation live in the Solana Attestation Service. This turns that
into a drop-in badge: "settled in a Covenant-verified enclave", with the live
TCB status, the enclave measurement, and a link to the on-chain attestation.

Read-only. One RPC round-trip per check (the attestation and its issuer
credential), no keys, no funds. It fails closed: a lapsed, missing, or
wrong-signer attestation renders as not verified, never a false green.

## Try it

```
npm install
node demo.mjs
```

Resolves the ER Loofta settles on today, prints the badge as text and JSON, runs
the route-and-verify path, and writes `badge-preview.html`.

## Integrate

The check runs server-side, next to where Loofta already handles settlement (the
Pay Button `callbackUrl` / `onSuccess`). Resolve once, attach the badge to the
receipt.

```js
import { verifyEnclave, badgeHtml } from "./badge.mjs";

// `validator` is the ER identity the payment settled on.
const badge = await verifyEnclave({
  rpcUrl: process.env.SOLANA_RPC,
  validator: settlementValidator,
});

receipt.covenantBadge = badge;            // ship the data to the client, or
receipt.covenantBadgeHtml = badgeHtml(badge); // render server-side
```

If you let Covenant pick the enclave instead of pinning one, `routeAndVerify`
returns the endpoint to send to and the badge together:

```js
import { routeAndVerify } from "./badge.mjs";
const { endpoint, badge } = await routeAndVerify({ rpcUrl: process.env.SOLANA_RPC });
```

## What the badge asserts

- The ER the payment settled on is a genuine Intel TDX enclave, DCAP-verified
  against Intel's PCCS (fresh challenge, `report_data` bound, TCB checked).
- The verification is signed by an authorized signer of the Covenant credential
  and has not lapsed (attestations expire and are renewed by the registry
  monitor; a stale enclave fails closed).
- `mr_td` identifies the exact enclave image; `verifiedAt` is when it last
  passed.

It does not reveal, touch, or depend on payment amounts or parties. It is a
statement about where the payment ran, not what it was.

## API

- `verifyEnclave({ rpcUrl | connection, validator })` → badge data.
- `routeAndVerify({ rpcUrl | connection })` → `{ endpoint, validator, badge, routes }`.
- `badgeHtml(data, { theme, compact })` → self-contained inline-styled HTML.
- `badgeText(data)` → one-line plain text.

Built on [`@covenant-org/verified-er`](../../packages/verified-er).
