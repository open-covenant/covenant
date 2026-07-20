# @covenant-org/blockrun

A trust receipt for every [BlockRun](https://blockrun.ai) call.

BlockRun settles your agent's paid calls over x402. It proves the money moved. It
does not bind that payment to what you asked for, which model actually served the
request, or what came back. This package sits around your `fetch` and emits a
**Covenant receipt** for each call — the same receipt the Covenant daemon
produces, so anyone can check it.

It never changes how the call is made or how the money moves.

## Install

```bash
npm install @covenant-org/blockrun
```

## Use

Wrap the `fetch` your BlockRun client uses:

```ts
import { withCovenantReceipts } from "@covenant-org/blockrun";

const fetchWithReceipts = withCovenantReceipts(globalThis.fetch, {
  onReceipt: (receipt) => {
    // persist it, log it, post it to your audit trail — it's plain JSON
    console.log(receipt.verdict, receipt.modelServed, receipt.payment.tx);
  },
});

// hand it to your BlockRun / OpenAI-compatible client
const client = new OpenAI({ fetch: fetchWithReceipts, /* … */ });
```

`onReceipt` is called **after** the wrapped `fetch` resolves, not during it — the
receipt is built off the response clone so it never delays your call. Read
receipts in the callback (persist, log, enqueue); don't expect one to exist the
line after `await fetch(...)`. A call is only receipted on a 2xx response; a
plain 2xx with no preceding 402 gets a receipt with empty payment fields. Pass
`onError` to see any receipt-build failure (it never affects the call).

Each completed call yields a `CallReceipt`:

```jsonc
{
  "provider": "blockrun",
  "endpoint": "/api/v1/chat/completions",
  "modelRequested": "gpt-4o",
  "modelServed": "gpt-4o-mini",     // the router substituted a cheaper model
  "verdict": "substituted",          // delivered | substituted | unverified
  "inputSha256": "…",
  "outputSha256": "…",
  "routing": { "model": "gpt-4o-mini", "savingsPct": 78 },
  "payment": {
    "network": "eip155:8453",
    "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "amount": "3000",
    "amountUsdc": 0.003,
    "payTo": "0xe9030014F5DAe217d0A152f02A043567b16c1aBf",
    "tx": "0x…"                       // the on-chain settlement signature
  }
}
```

`verdict` is the point: `delivered` when you got the model you asked for,
`substituted` when the router swapped it, `unverified` when no served model was
reported. It turns "cheapest capable model" from a claim into something checkable.

## Verify a receipt

Offline, no network, no payment:

```ts
import { verifyReceipt } from "@covenant-org/blockrun";

const result = await verifyReceipt(receipt);
// { valid, digest, verdictConsistent, statedVerdict, expectedVerdict }
```

`digest` and the hashes are RFC 8785 (JCS) SHA-256, identical to the Covenant
`covenant-blockrun` crate — a receipt built here verifies in the daemon and vice
versa.

Apache-2.0.
