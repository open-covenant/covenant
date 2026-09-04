# Desk intents

An intent is a conditional order written so somebody other than the desk could
fill it inside the same limits. It names the pair, the most that may leave the
wallet, the least that must come back, a deadline, and a hash of the conditions
that armed the order.

The desk signs intents. It does not fill them, and it does not run or talk to a
filler. Nothing in the desk redeems an intent today. The format ships now so a
filler can be built against a signature that already exists in the wild.

## Payload

EIP-712 typed data.

```
domain = {
  name:    "Covenant Desk",
  version: "1",
  chainId: 4663
}
```

There is no `verifyingContract`. No contract redeems these yet, and pinning an
address the desk cannot point at would be a claim it cannot support. A contract
that adopts the format later moves to `version: "2"` with the address in the
domain.

```
DeskIntent {
  address tokenIn
  address tokenOut
  uint256 maxAmountIn
  uint256 minAmountOut
  uint256 deadline
  bytes32 conditionsHash
}
```

| field | meaning |
|---|---|
| `tokenIn` | Token the wallet gives up. |
| `tokenOut` | Token the wallet receives. |
| `maxAmountIn` | Ceiling on `tokenIn`, smallest units. The order's `amountIn`. |
| `minAmountOut` | Floor on `tokenOut`, smallest units. The quote at signing time less the order's slippage bound. |
| `deadline` | Seconds since the Unix epoch. Five minutes from signing by default. |
| `conditionsHash` | keccak256 of the canonical conditions below. |

## Conditions hash

`conditionsHash` binds the signature to the exact order that produced it, so a
filler cannot reuse a signature against a different trigger or a wider bound.
It is `keccak256` over the UTF-8 bytes of a JSON array with a fixed field
order:

```json
[
  id,
  kind,
  side,
  tokenIn,          // lowercase
  tokenOut,         // lowercase
  amountIn,         // decimal string, smallest units
  [ priceLte, priceGte, priceBasis, premiumLteBps, premiumGteBps,
    atNextOpenOffsetSec, at ],
  [ maxSlippageBps, maxOrderNotionalUsd, maxBuyPremiumBps ],
  live,
  expiresAt,
  parentId
]
```

Absent trigger fields are `null`, except `priceBasis`, which is written as
`"usdOnchain"` when it was left out, because that is the basis the engine
applies. Status, timestamps, and the reason text are excluded: they change
after creation and would move the hash without changing what was agreed.

## Signing

The desk signs with `DESK_EVM_PRIVATE_KEY`, read from `keys.env` at first use.
The key is never logged, never written to the store, and never returned by any
surface.

```ts
const intent = orders.intents.fromOrder(order, { minAmountOut });
const signature = await orders.intents.sign(intent);
const ok = await orders.intents.verify(intent, signature, orders.intents.signerAddress());
```

`typedData(intent)` returns the domain, the types, the primary type, and the
message, ready for any EIP-712 library. `hash(intent)` returns the digest the
signature covers.

## What a filler would still have to build

Reading this document is not enough to fill an intent safely. A filler needs
its own answer to each of these:

- Custody. The intent is a signature, not an approval. The wallet still has to
  approve Permit2 and the router before anything can move.
- Settlement. There is no contract that checks `conditionsHash` on chain today,
  so the floor and the ceiling are honoured only by the party sending the
  transaction.
- Freshness. `minAmountOut` was computed from a quote at signing time. A filler
  is responsible for its own quote and for the deadline.
