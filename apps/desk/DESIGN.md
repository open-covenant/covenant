# Covenant Desk: design

Local, always-on execution desk for Robinhood Chain stock tokens and the
memecoins paired with them. One installable Node package. The user's agent
(Claude, Codex, Hermes, anything that speaks MCP or HTTP) connects to it; keys
never leave the machine; Covenant never sees a trade.

Package `@covenant-org/desk`, bin `covenant-desk`, path `apps/desk` in the
monorepo. Read `FACTS.md` first; every address and endpoint you need is there.

## 1. What it does

1. **Fair value.** For any stock token or stock-paired token on 4663, the USD
   price the chain is charging, the USD price it should be at, and the premium
   between them, with the reference source named. Works when Wall Street is
   closed (Chainlink frozen), which is when it matters.
2. **Conditional orders.** Limit, stop, take-profit, OCO, "at next US open",
   and premium-conditional orders on stock tokens and paired tokens, executed
   locally against Uniswap v4 with bounded slippage and notional.
3. **Stock-neutral positions.** Size and maintain a short on Lighter's
   Robinhood Chain instance that cancels the stock leg embedded in a
   stock-paired position, so the holder keeps the meme and drops the stock.
4. **Premium recorder.** Logs on-chain price vs reference for every liquid
   stock token and the largest paired pools on an interval, producing the
   dataset that proves the product.

Non-goals for v1: hosted keeper, custody of any kind, cross-chain, options,
lending, a Robinhood brokerage bridge, anything on Lighter mainnet.

## 2. Architecture

```
apps/desk/
  src/
    core/        shared types, config, keystore, logger, sqlite store, errors
    chain/       viem client, registry (rhj + chainlink + lighter-rh markets),
                 stock token reads, feed reads, v4 pool discovery + state,
                 v4 quoting, v4 swap encoding + execution (UniversalRouter)
    fairvalue/   US session calendar, reference selection, premium, paired
                 token USD math, route comparison, recorder loop
    orders/      order model, trigger engine, executor bridge, OCO, intents
    hedge/       lighter-rh client, hedge sizing, rebalancer, unwind
    surfaces/
      http/      local JSON API (127.0.0.1:46631, bearer token)
      mcp/       stdio MCP server exposing the desk tools
      ui/        static single page served by the daemon
      cli/       covenant-desk commands
    daemon.ts    lifecycle: store, registry refresh, loops, surfaces
  data/          chainlink-feeds-4663.json, rhj-assets-snapshot.json
  service/       launchd plist, systemd unit, Dockerfile
  test/          vitest; live tests gated by DESK_LIVE=1
```

Rules:
- ESM TypeScript, strict. `viem` for EVM. `node:sqlite` (`DatabaseSync`) for
  storage, no native deps. `@modelcontextprotocol/sdk` 1.26.0 + zod 4 for MCP.
  No UI framework, no bundler: the UI is one HTML file plus one JS file.
- Deterministic loops, no LLM anywhere in the hot path.
- Every number that leaves the process carries its unit and its source
  (`{ value, unit: 'USD'|'USDG'|'raw'|'bps', source: 'chainlink'|'lighter-rh'|
  'rhj'|'pool', asOf }`).
- Nothing executes live unless `config.live === true` AND the order was
  created with `live: true`. Default is dry-run, which quotes and records what
  would have happened.

## 3. Module contracts

### core
- `Config` (JSON at `~/.config/covenant-desk/config.json`): rpcUrl, port,
  bearer token, live flag, bounds `{ maxOrderNotionalUsd: 250,
  maxDailyNotionalUsd: 1000, maxSlippageBps: 100, maxBuyPremiumBps: 500 }`,
  recorder interval, hedge thresholds, acknowledgedRestrictions boolean.
- Keys in `~/.config/covenant-desk/keys.env`, mode 0600:
  `DESK_EVM_PRIVATE_KEY`, `DESK_LIGHTER_RH_PRIVATE_KEY`,
  `DESK_LIGHTER_RH_ACCOUNT_INDEX`, `DESK_LIGHTER_RH_API_KEY_INDEX`. Never
  logged, never returned by any surface. Optional macOS Keychain read via the
  `security` CLI when the env var is absent.
- Store: sqlite at `~/.config/covenant-desk/desk.sqlite`. Tables: `tokens`,
  `pools`, `observations` (recorder), `orders`, `executions`, `hedges`,
  `events`. Migrations in code, idempotent.

### chain
- `Registry`: merges rhj assets (symbol → address, multiplier, tradability),
  Chainlink equity feeds (symbol → feed address) from `data/`, and Lighter RH
  markets (symbol → market id, perp/spot). Refresh rhj + Lighter hourly;
  Chainlink from file. Exposes `stockTokens()`, `feedFor(symbol)`,
  `lighterPerpFor(symbol)`, `isStockToken(address)`.
- `StockToken`: `uiMultiplier`, `oraclePaused`, `tokenPaused`, `decimals`,
  ERC-20 reads via Multicall3.
- `Feeds`: `latestRoundData` → `{ price, updatedAt, decimals }`.
- `Pools`: discovery via `Initialize` logs filtered by stock-token addresses on
  topics[2] then topics[3] (see FACTS for topic0 and the 10k-log limit; split
  ranges on the limit error). Persist poolId, currencies, fee, tickSpacing,
  hooks, initial block. State via `StateView.getSlot0` + `getLiquidity`; mid
  price from sqrtPriceX96 with correct decimals on both currencies. Rank pools
  by liquidity; flag pools whose lpFee > 300 bps as `trap`.
- `Quoter`: `V4Quoter.quoteExactInputSingle` and `quoteExactInput` via
  `simulateContract`; returns amountOut and gas estimate.
- `Swap`: UniversalRouter `execute(commands, inputs, deadline)` with
  `V4_SWAP` (0x10): actions `SWAP_EXACT_IN_SINGLE` (0x06) or `SWAP_EXACT_IN`
  (0x07) + `SETTLE_ALL` (0x0c) + `TAKE_ALL` (0x0f). Permit2: ERC-20 approve to
  Permit2 once, `Permit2.approve(token, router, amount, expiration)` per
  token. `minAmountOut` from the quote minus slippage. Simulate before send.
  Return tx hash, amounts in/out, effective price.

### fairvalue
- `Session`: US equities calendar. Chainlink 24/5 session = Sunday 20:00 ET to
  Friday 20:00 ET, with NYSE holidays (encode the 2026 list) as closed. Expose
  `state(now)`: `open` (regular 09:30-16:00 ET), `extended`, `overnight`,
  `closed`, plus `nextOpen(now)`.
- `Reference` for a stock token (USD per raw token):
  1. Chainlink if session is not `closed` and `updatedAt` is within the current
     session; source `chainlink`.
  2. Else Lighter RH perp mark for the symbol if listed; source `lighter-rh`.
  3. Else rhj ask × currentMultiplier if not halted and generated within 24 h;
     source `rhj`.
  4. Else last Chainlink price, source `chainlink-stale`, flagged.
  Always return all available candidates so the UI can show the spread
  between them.
- `Premium`: `onchainMid / reference - 1` in bps, for every stock token with a
  USDG pool.
- `Paired`: for token X quoted in stock token S: ratio r (S per X, from the
  deepest non-trap pool), `usdOnchain = r × onchainMid(S)`, `usdFair = r ×
  reference(S)`, `stockLegPremiumBps = premium(S)`. If an X/WETH pool exists,
  compute `usdViaWeth = r_w × ETH/USD` and report the cheaper entry route and
  the better exit route. For multipool tokens (several stock quotes), report
  each and a liquidity-weighted fair value.
- `Recorder`: every `config.recorderIntervalSec` (default 60), write an
  observation row per liquid stock token (top 30 by pool liquidity) and per
  tracked paired pool (top 50 by liquidity): onchain mid, each reference
  candidate, premium, session state, block number. Never stops on a single
  failure; logs and continues.

### orders
- `Order`: `{ id, kind: 'limit'|'stop'|'takeProfit'|'oco'|'atOpen'|'premium',
  side, tokenIn, tokenOut, amountIn (raw), trigger, bounds, live, status,
  createdAt, expiresAt, parentId }`. Triggers: `priceLte/priceGte` on
  `usdOnchain` or `usdFair`, `premiumLteBps/premiumGteBps` on the stock leg,
  `atNextOpen(+seconds)`, `at(timestamp)`. OCO = two children, first fill
  cancels the sibling.
- `Engine`: evaluates open orders every 5 s against the latest fairvalue
  snapshot; on trigger, quotes, checks bounds (notional, slippage, premium cap
  for buys, daily cap), then executes (live) or records a simulated execution
  (dry-run). Status transitions: `open → triggered → filled | failed |
  cancelled | expired`. Every transition is an `events` row.
- `Intents` (spec + signing only in v1): EIP-712 `DeskIntent { tokenIn,
  tokenOut, maxAmountIn, minAmountOut, deadline, conditionsHash }` signed by
  the desk key so an external keeper could fill it within bounds later.
  Document the format; do not implement a keeper.

### hedge
- `LighterRh`: client over `https://api.rh.lighter.xyz` using
  `lighter-agent-sdk` with the endpoint override if it works for this host,
  else a thin fetch client with the same shapes. Reads: markets, mark/last
  price, funding, positions, account. Writes: place/cancel limit and market
  orders through the SDK `TradingClient` with `maxNotionalUsd` from config.
  Signing chain id for this instance is unverified (FACTS); if it cannot be
  confirmed, writes stay dry-run and `hedge apply` returns the plan with a
  named reason.
- `Sizer`: for a position of `qtyX` paired with S at ratio r: stock leg USD =
  `qtyX × r × reference(S)`; target short = that notional on the S perp,
  rounded to `size_decimals` and `min_base_amount`. For direct stock-token
  holdings: `qty × reference(S)`.
- `Rebalancer`: every 60 s compare target vs current short; act when drift >
  `config.hedgeDriftBps` (default 500) or funding flips against the position
  by more than `config.hedgeMaxFundingBps8h`. Unwind on demand.

### surfaces
- HTTP (127.0.0.1 only, port 46631, `Authorization: Bearer <token>` on every
  route except `GET /v1/health`): `GET /v1/status`, `GET /v1/tokens`,
  `GET /v1/pools?stock=NVDA`, `GET /v1/quote?token=<addr|symbol>&amountUsd=100&
  side=buy`, `GET /v1/premium`, `POST /v1/orders`, `GET /v1/orders`,
  `DELETE /v1/orders/:id`, `GET /v1/hedge/plan?...`, `POST /v1/hedge/apply`,
  `GET /v1/hedge`, `GET /v1/recorder?since=`. JSON errors `{ error, reason }`.
- MCP (stdio, `covenant-desk mcp`): tools `desk_status`, `desk_quote`,
  `desk_premium`, `desk_pools`, `desk_order_create`, `desk_orders`,
  `desk_order_cancel`, `desk_hedge_plan`, `desk_hedge_apply`,
  `desk_hedge_status`, `desk_recorder_recent`. The MCP process talks to the
  running daemon over the local HTTP API (reads the token from config); if no
  daemon is running it says so instead of starting one. Tool descriptions
  state units, defaults, and that dry-run is the default.
- UI (`GET /`): status header (session state, block, daemon uptime, live or
  dry-run), premium table (symbol, on-chain mid, reference, source, premium
  bps, liquidity, auto-refresh 15 s), quote panel, orders table with cancel,
  hedge panel, recorder sparkline (last 24 h premium for a chosen symbol).
- CLI: `init`, `start [--foreground]`, `stop`, `status`, `quote <token>
  [--usd 100] [--side buy|sell]`, `premium [--top 20]`, `pools [--stock
  NVDA]`, `order create|list|cancel`, `hedge plan|apply|status|unwind`,
  `record start|stop|export`, `mcp`, `service install --launchd|--systemd`,
  `service uninstall`. `init` prints the jurisdiction notice and requires
  `--acknowledge-restrictions` before `live` can ever be set.

## 4. Safety bounds (product first, but these ship in v1)

- Dry-run by default. `live` requires config flag + per-order flag +
  acknowledged restrictions.
- Per-order and per-day notional caps, slippage cap, buy premium cap, all in
  config, all reported in every refusal with the exact number that tripped.
- Trap-pool detection (lpFee > 300 bps) blocks routing through that pool.
- Never sign anything but a swap or a Lighter order. No transfers, no
  approvals beyond Permit2 for the router.
- Keys are read once at start, held in memory, redacted from logs and every
  surface.

## 5. Acceptance (each must be demonstrated, not asserted)

1. `pnpm --filter @covenant-org/desk build && pnpm --filter @covenant-org/desk
   test` green; unit tests cover sqrtPrice→price with mixed decimals,
   multiplier handling, session calendar edges (Friday 20:00 ET, holidays),
   reference selection order, premium math, order triggers incl. OCO and
   atOpen, hedge sizing rounding, and swap calldata encoding against a fixed
   vector.
2. `DESK_LIVE=1` tests: registry loads 194 tokens; NVDA feed reads; pool
   discovery finds the AAPL/USDG reference pool from FACTS and at least 100
   stock-quoted pools; StateView mid for AAPL/USDG within 1% of Chainlink
   during session; V4Quoter returns a quote for 10 USDG → AAPL.
3. `covenant-desk premium --top 10` prints a table with a named reference
   source per row against mainnet.
4. `covenant-desk quote AI --usd 100` (Artificial Inu, quoted in NVDA; find its
   pool by liquidity) prints usdOnchain, usdFair, stock-leg premium, and both
   routes.
5. A dry-run limit order triggers in the engine and records a simulated
   execution with a real V4Quoter quote.
6. `covenant-desk hedge plan` prints a sized NVDA short from live Lighter RH
   marks for a hypothetical 1,000 USD AI position.
7. `covenant-desk mcp` lists the tools and answers `desk_premium` over stdio
   while the daemon runs.
8. `covenant-desk record start` runs for 3 minutes and produces observation
   rows; `record export` writes CSV.
9. README reads to the public copy standard.
