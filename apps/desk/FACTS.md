# Verified facts for Covenant Desk (as of 2026-09-04)

Every item below was checked live on 2026-09-04 unless marked VERIFY. Do not
"correct" these from memory; if a value looks wrong, re-check it live and record
what you found.

## Robinhood Chain (chain id 4663)

- RPC `https://rpc.mainnet.chain.robinhood.com` (eth_chainId 0x1237). Arbitrum
  Nitro Orbit, ~100 ms blocks, ETH gas. Tip at check: 54,423,221.
- Explorer `https://robinhoodchain.blockscout.com` (Cloudflare-gated, needs a
  browser user agent).
- `eth_getLogs`: block range is NOT limited; the limit is **10,000 matching logs
  per query** (error -32000 "logs matched by query exceeds limit of 10000").
  Split the range on that error.
- USDG `0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168`, 6 decimals (read on chain 2026-09-05).
- WETH9 `0x0Bd7D308f8E1639FAb988df18A8011f41EAcAD73`.
- Multicall3 `0xcA11bde05977b3631167028862bE2a173976CA11` (has code).
- Permit2 `0x000000000022D473030F116dDEE9F6B43aC78BA3` (has code).

## Uniswap v4 on 4663 (from developers.uniswap.org deployments, PoolManager
## also confirmed via UniversalRouter.poolManager())

| contract | address |
|---|---|
| PoolManager | `0x8366a39cc670b4001a1121b8f6a443a643e40951` |
| PositionManager | `0x58daec3116aae6d93017baaea7749052e8a04fa7` |
| PositionDescriptor | `0x9639443158e8c5efa35bd45287bf2effd3d8dc06` |
| V4Quoter | `0x8dc178efb8111bb0973dd9d722ebeff267c98f94` |
| StateView | `0xf3334192d15450cdd385c8b70e03f9a6bd9e673b` |
| UniversalRouter | `0x8876789976dEcBfCbBbe364623C63652db8C0904` |
| Permit2 | `0x000000000022D473030F116dDEE9F6B43aC78BA3` |

- `Initialize(bytes32 indexed id, address indexed currency0, address indexed
  currency1, uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96,
  int24 tick)` topic0 =
  `0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438`.
  currency0/currency1 are indexed, so pools involving stock tokens can be found
  with topic filters (topics[2] OR-list, then topics[3] OR-list) instead of a
  full scan. Pool creation rate is high: 683 Initialize logs in the last 50k
  blocks (~1.4 h), 9,893 in the last 500k blocks.
- Known reference pool: AAPL/USDG poolId
  `0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147`,
  dynamic fee (0x800000), tickSpacing 10, hook
  `0x70a9a88402989226847ec122043ce5e7ff462080`, prices at the oracle. Other
  USDG/AAPL pools are traps with 65-90% lpFee. Always read `lpFee` and
  liquidity before routing through a pool.
- Prior art for swap execution against this router:
  `~/Projects/covenant/covenant-rwa-pr/agent-os/evm/contracts/GuardedTradeExecutor.sol`
  and `~/Projects/covenant/covenant-rwa-pr/agent-os/crates/covenant-rwa-firewall/src/lib.rs`
  (Permit2-primed on both legs; refuses off-hours with StalePriceFeed, which is
  the behaviour Desk must NOT copy).

## The UniversalRouter on 4663 is a fork (verified 2026-09-07)

Both v4 swap structs carry an extra per-hop price floor that mainline does not:
`ExactInputSingleParams { poolKey, zeroForOne, amountIn, amountOutMinimum,
uint256 minHopPriceX36, hookData }` and `ExactInputParams { currencyIn,
PathKey[] path, uint256[] minHopPriceX36, amountIn, amountOutMinimum }`. The
array must be empty or path-length (`InvalidHopPriceLength()`). Omitting it
reverts with empty data. `SWAP_EXACT_OUT*` carry the same layout. Encoded in
`src/chain/swap.ts`; tests carry a vector from a settled transaction.

## Paired-token venues (verified 2026-09-07, see src/chain/venues/*.md)

- LONG-listed tokens (Artificial Inu etc.) sit in Clanker pools with hook
  `ClankerHookStaticFeeV2` `0x48b8f6ad…` which never checks the caller. The desk
  fills them through the UniversalRouter (AI fill tx `0x6184cf25…` via the order
  engine). LONG's own router `0x6F6F5E1b…` (selector `0x39ecce49`) requires a
  backend-signed order and is not needed. Bankr launches on Clanker too.
- PAIR pools carry `PairV4Hook` `0x16D15606…` (beforeInitialize only): plain.
  Fill tx `0xcdb8b95a…`. Launch data via `PairLaunchpadV5Upgradeable`
  `0x8660A7F0…` (`getLaunchPool`). The aggregator does NOT enforce a 15% impact
  cap; PAIR's 1% is a static pool fee.
- Doppler, Pons, LaunchToken and hookless pools: 150 of 150 deepest simulate
  and fill through the same path.

## Robinhood Stock Tokens

- 194 tokens, all deployed on 4663 only. Registry:
  `GET https://api.robinhood.com/rhj/assets` (no auth, 60 rps). Response shape:
  `{ "assets": [ { id, tokenSymbol, tokenName, deployments:[{contractAddress,
  chainId, networkName}], currentMultiplier, pendingMultiplier, status,
  logoUrl, tradingCapabilities:{market,extended,overnight}, tokenDecimals:18,
  isin } ] }`. Snapshot in `data/rhj-assets-snapshot.json`.
- Quotes: `GET https://api.robinhood.com/rhj/prices/{SYMBOL}` returns
  `{ quotes:[{ tokenSymbol, deployments, bid, ask, currency, dailyTradingVolume,
  isTradingHalt, generatedAt, dailyHigh, dailyLow, mintBurnTokenVolume,
  mintBurnUsdVolume }] }`. bid/ask are per share, NOT multiplier-adjusted
  (VERIFY against Chainlink for a token with multiplier > 1, e.g. AAPL
  1.000566).
- Corporate actions: `GET https://api.robinhood.com/rhj/corporate-actions`.
- Contract: BeaconProxy → `Stock` implementation. ABI = ERC-20 + EIP-2612
  `permit` + ERC-8056 (`uiMultiplier()`, `balanceOfUI(address)`,
  `newUIMultiplier()`, `effectiveAt()`) + issuer `mint/burn/adminBurn` +
  `pause()/tokenPaused()` + `pauseOracle()/oraclePaused()`. **No allowlist or
  blocklist in the ABI.** Geo restriction is front-end plus a sequencer-level
  restricted address list.
- Issuer: Robinhood Assets (Jersey) Ltd. Only one Authorised Participant
  (BBVI) mints/redeems, and only while the underlying market is open. Trading
  on chain is 24/7. This mismatch is the product.
- Excluded jurisdictions for stock tokens: US, Canada, UK, Switzerland.
- Examples: NVDA `0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC` (supply ~62.6k),
  AAPL `0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9`, CRM
  `0xd95B44124e475743a7589e68F3D74008A5536D44`.

## Chainlink feeds on 4663

- Full list in `data/chainlink-feeds-4663.json` (57 feeds; 35 are
  "Robinhood X / USD" equity feeds, plus ETH/USD `0x78F3556b67E17Df817D51Ef5a990cDaF09E8d3A9`,
  USDG/USD `0x61B7e5650328764B076A108EFF5fa7282a1B9aD2`, USDC/USD, BTC/USD).
- Equity feeds: 8 decimals, heartbeat 86400 s, 0.5% deviation, market hours
  `us_equities_24/5`. Off-hours they **hold the last published price with no
  heartbeat**. Prices already incorporate the ERC-8056 multiplier (VERIFY by
  comparing feed vs rhj ask × currentMultiplier for AAPL).
- Key addresses: NVDA `0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15`, SPY
  `0x319724394D3A0e3669269846abE664Cd621f9f6A`, AAPL
  `0x6B22A786bAa607d76728168703a39Ea9C99f2cD0`, TSLA
  `0x4A1166a659A55625345e9515b32adECea5547C38`, QQQ
  `0x80901d846d5D7B030F26B480776EE3b29374C2ae`, SPCX
  `0xB265810950ba6c5C0Ff821c9963014a56fD8Bffb`.
- No Chainlink L2 sequencer uptime feed is published on 4663.
- Tokens WITHOUT a feed (e.g. HIMS, AMC) have no on-chain reference; rhj
  bid/ask is the only reference for them.

## Lighter, Robinhood Chain instance (24/7 hedge venue and off-hours reference)

- REST `https://api.rh.lighter.xyz` (same API shape as Lighter mainnet
  `https://mainnet.zklighter.elliot.ai`): `/api/v1/orderBooks`,
  `/api/v1/orderBookDetails`, `/api/v1/fundings?market_id=&resolution=1h&
  start_timestamp=&end_timestamp=&count_back=`. `/api/v1/candlesticks` returned
  empty/403 to curl; treat as unavailable.
- 83 books: perps on ~37 equities/ETFs and spot `X/USDG` pairs. Perp market
  ids: NVDA 15, AAPL 10, SPY 26 (others: read from orderBookDetails). Memecoin
  perps exist too: AI, CASHCAT, PONS. Zero maker/taker fees.
  `default_initial_margin_fraction` 5000 on equities. `size_decimals` 4,
  `price_decimals` 2 for NVDA/AAPL/SPY.
- Funding accrues hourly through the weekend (verified on Lighter mainnet NVDA
  for 2026-08-29/30), so the perp mark is a live 24/7 reference when the
  Chainlink feed is frozen.
- Lighter mainnet also lists NVDA, AAPL, TSLA, SPY, QQQ, SPCX, HOOD perps and
  `rhSPY/USDC`, `rhQQQ/USDC` spot. Not used by Desk v1.
- In-house SDK: `~/Projects/lighter-agent/packages/sdk` (npm
  `lighter-agent-sdk@0.1.1`, ESM, Node ≥20). `LighterRestClient` for reads,
  `TradingClient` for signed orders with a mandatory `maxNotionalUsd`.
  `client/endpoints.ts` resolves a custom target URL. The signer is wasm-based
  and bound to Lighter mainnet chain id 304; whether the Robinhood instance
  uses a different signing chain id is VERIFY (`/api/v1/info` or the Lighter
  RH docs at docs.robinhood.com/chain/lighter-domains). No Desk credentials
  exist for the Robinhood instance yet; hedge execution ships dry-run until an
  account is funded.

## Stock-paired market structure (why Desk exists)

- Launchpads pairing memecoins with stock tokens: LONG (long.xyz, since
  07-14), Bankr (07-20), Flap, PAIR (pair.fund, multipool up to 5 stock
  pools per token, Uniswap v4, locked liquidity, 1% fee). Public read API at
  pair.fund/docs (tokens, trades, OHLCV).
- 2026-09-02: $217M memecoin/stock-pair volume vs $127M direct stock-token
  volume. 432 stock-quoted pools hold 17.2% of the on-chain supply of the 19
  liquid stock tokens.
- Weekend 2026-08-29: HIMS token traded at 4.6x its NYSE close (BONER pool held
  >50% of HIMS float); collapsed Monday when ~4,000 tokens were minted.
- Denar (Morpho wrapper) freezes off-hours; Arcus raises margin off-hours;
  Uniswap does nothing; Stockhood only displays premium. Nobody executes at
  fair value off-hours.

## Monorepo conventions (open-covenant/covenant)

- pnpm 10.31 workspaces + turbo; Node 24 locally, engines `>=22`. TS 6.0.3,
  vitest 4, zod 4.3.6, `@modelcontextprotocol/sdk` 1.26.0. ESM everywhere.
  Template: `services/mcp-bridge` (package.json, tsconfig extending
  `../../tsconfig.base.json`).
- Worktree for this build: `~/Projects/covenant/covenant-desk`, branch
  `feat/desk` off `origin/main` (343dffc4e). Package lives at `apps/desk`.
- Public copy standard (README, UI, CLI help): see `~/.claude/CLAUDE.md`. No
  em dashes, no "not X but Y", no internal vocabulary, no unsupported claims.
