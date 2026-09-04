# Lighter on Robinhood Chain: what the live host answers

Checked 2026-09-04 against `https://api.rh.lighter.xyz`. Anything here that
looks wrong should be re-read from the host rather than corrected from memory.

## The SDK works against this host

`lighter-agent-sdk@0.1.1` reaches the instance with no changes:
`new LighterRestClient({ network: 'robinhood' })`, or with the base URL from
`config.hedge.lighterBaseUrl` passed straight to `network`, which
`resolveEndpoint` accepts as a custom origin. No thin fetch client was needed
for reads. The one field the SDK drops is `default_initial_margin_fraction`,
which decides how much collateral a short of a given size needs, so the client
reads that one field from `/api/v1/orderBookDetails` itself.

## Markets

`GET /api/v1/orderBooks` returns 84 books: 57 perpetuals and 27 `X/USDG` spot
pairs. Market ids are unique across both kinds, and so are the symbols, because
the spot pairs carry the slash. FACTS records 83 books, so the instance has
listed one more since that check.

| symbol | market id | size decimals | price decimals | min base amount |
|---|---|---|---|---|
| NVDA | 15 | 4 | 2 | 0.0400 |
| AAPL | 10 | 4 | 2 | 0.0200 |
| SPY | 26 | 4 | 2 | 0.0100 |
| AI | 45 | 1 | 5 | 10.0000 |

`default_initial_margin_fraction` is 5000 on the equity perpetuals. Maker and
taker fees are zero across the instance.

## Mark price

`GET /api/v1/orderBookDetails?market_id=15` carries `mark_price`, `index_price`,
and `last_trade_price` for NVDA. Read at 19:46 UTC on 2026-09-04:

```
$ curl -s 'https://api.rh.lighter.xyz/api/v1/orderBookDetails?market_id=15'
{"code":200,"order_book_details":[{"symbol":"NVDA","market_id":15,
"market_type":"perp","status":"active","min_base_amount":"0.0400",
"supported_size_decimals":4,"supported_price_decimals":2,
"default_initial_margin_fraction":5000,"mark_price":"230.87",
"index_price":"230.62","last_trade_price":230.83, ...}],
"spot_order_book_details":[]}
```

`/api/v1/candlesticks` is unavailable to a plain client, as FACTS records, but
nothing here needs it.

## Funding

Two routes, two different units. The desk uses the forward-looking one.

- `GET /api/v1/funding-rates` returns a row per venue it tracks, so the row has
  to be filtered to `exchange: "lighter"`. `rate` is a fraction of notional over
  eight hours. NVDA at the time of the check: `binance 0.00021566`,
  `bybit 0.00000276`, `hyperliquid 0.00005`, `lighter 0.000032`. The desk
  reports the venue row as basis points, so 0.000032 becomes 0.32 bps.
- `GET /api/v1/fundings?market_id=15&resolution=1h&...` returns settled history
  where `rate` is a percent of notional over one hour and `direction` names the
  side that paid. Every row in the window read was `direction: "long"`, so longs
  were paying shorts, which is the side a stock-neutral hedge sits on.

Sign convention through the whole module: positive funding means longs pay
shorts, so a short is paid to hold the position. A rate below the negative of
`config.hedge.maxFundingBps8h` is the case the rebalancer closes.

## Signing chain id

The venue publishes it, and it differs from Lighter mainnet:

- `https://apidocs.rh.lighter.xyz/docs/get-started`: "The relevant Chain IDs for
  the Lighter app-chain are: `466324` (mainnet), `300` (testnet)."
- `https://apidocs.lighter.xyz/docs/get-started`, the same sentence on Lighter's
  own copy: "`304` (mainnet), `300` (testnet)."

So `466324` is the value for this instance, and it is what
`LIGHTER_RH_CHAIN_ID` holds. Two qualifications:

1. No signature has been settled against it, because the desk holds no account
   on this instance. A wrong chain id is rejected by the sequencer, so the cost
   of the value being wrong is a refusal rather than a fill.
2. `https://docs.robinhood.com/chain/lighter-domains` does not carry a chain id
   at all. It gives the API base URL, the public interface at
   `robinhoodchain.lighter.xyz`, and the contract
   `0x94bAB9693Ba2f6358507eFfcbd372b0660AFfF9d`.

Routes that would have answered this directly are closed to a plain client:
`/api/v1/info`, `/api/v1/status`, and `/api/v1/layer2BasicInfo` all return HTTP
403, with and without a browser user agent, on both this host and Lighter
mainnet. `GET /` returns `{"status":200,"network_id":1,"timestamp":...}` on the
Robinhood instance, on Lighter mainnet, and on Lighter testnet, so `network_id`
is not an instance identifier.

## Quote asset

Perpetuals on this instance settle in USDG, not USDC. Mark prices are carried
through the module as `USD` because USDG tracks the dollar and chain 4663
publishes a USDG/USD feed at `0x61B7e5650328764B076A108EFF5fa7282a1B9aD2` to
check that. If the peg ever moves enough to matter, that feed is where the
correction belongs.

## What stays dry-run

No Desk credentials exist for this instance. `canTrade()` refuses in this order,
naming one thing to change each time:

1. the jurisdiction notice has not been acknowledged;
2. the desk is in dry run;
3. `DESK_LIGHTER_RH_PRIVATE_KEY` is absent;
4. `DESK_LIGHTER_RH_ACCOUNT_INDEX` is absent or is not a whole number;
5. `DESK_LIGHTER_RH_API_KEY_INDEX` is absent or is not a whole number;
6. the signing chain id is unset.

`hedge apply` returns the plan with whichever reason stopped it. Nothing in the
test suite signs, and the live tests are reads.
