# PAIR (pair.fund) on chain 4663

Checked live on 2026-09-07 against `https://rpc.mainnet.chain.robinhood.com`,
`https://robinhoodchain.blockscout.com`, and `https://pair.fund/api`.

**Headline: PAIR pools need no adapter.** They are ordinary Uniswap v4 pools.
The desk can quote them with the V4Quoter and fill them through the
UniversalRouter. Proven with a live two-hop fill of 0.02 USDG into the PAIR
token, tx
`0xcdb8b95a80c9feaeec85f6064b7daeafcf2784c2e2df81589d5e20b121cc76c6`.

Getting there did surface a real defect in the desk's own multi-hop calldata.
See "The router fork" below; `src/chain/abis.ts` and `src/chain/swap.ts` are
fixed.

## Contracts

| contract | address | verified |
|---|---|---|
| PairLaunchpadV5Upgradeable (proxy) | `0x8660A7F019C7943b0b0A91B8E39AFf3b6DB6Ae62` | yes, as `PairERC1967Proxy` |
| launchpad implementation | `0x8000B64B62837a1511E302c62354E1Bc39b5641a` | no |
| PairV4Hook (canonical, `launchpad.pairHook()`) | `0x16D1560630Ce74af4478d9b8AD46548A092A2000` | yes |
| PairV4Locker (owns every launch LP position) | `0xeFcF476E8870fB3eb8680f039414fdcCE6C2a117` | yes |
| PairV5MultiPoolAggregator | `0x9d7741776098aFA315e4D576ede4F2c67a21d8Ce` | yes |
| token factory | `0xEAcFbf0DBC0AbF560f89416D13B6bb3e6b2cD017` | not checked |
| SwapRouter02 (v3, used by the aggregator) | `0xCaf681a66D020601342297493863E78C959E5cb2` | not checked |

Reads confirmed on the aggregator: `launchpad()`, `poolManager()`
(`0x8366a39CC670B4001A1121B8F6A443A643e40951`), `universalRouter()`
(`0x8876789976dEcBfCbBbe364623C63652db8C0904`), `permit2()`, `usdg()`,
`weth()`, `MAX_LEGS() = 5`, `MAX_DEADLINE_WINDOW() = 86400`,
`dependenciesConsistent() = true`.

A second, newer generation exists: the "native fee V2" manifests in the
pair.fund bundle name launchpad `0x8660A7F0…` with hooks
`0x539b1aEdc83eDD55125405af90b0FB476803c0C0`,
`0x29A5B61d16BB087e138da3391f1B3bbF0269C0c0` and
`0x465c784a59dbFD81bCa6E2A669d3D1837cd880C0`, all mined for permission bits
`0x00c0` (beforeSwap and afterSwap). Every graduated token on that generation
carries hook `0xd2f759a1cf13c30127c551c3aee04629aea200c0`, which is not
verified on Blockscout. Those pools quote and swap through the UniversalRouter
too (checked below), and none of them holds material size yet.

## Discovery

Two sources, both usable.

**On chain, no API needed.** The launchpad is the registry:

```
function pairHook() view returns (address)
function getLaunchPoolCount(address projectToken) view returns (uint256)
function getLaunchPool(address projectToken, uint256 index) view returns (
  address quoteToken, uint16 weightBps, bytes32 poolId, uint256 positionId,
  uint256 initialProjectTokenAmount, int24 tickLower, int24 tickUpper,
  uint256 quoteUsdAtLaunchE8, address quotePriceFeed, uint8 quoteDecimals)
```

Every canonical pool key is `(sorted(projectToken, quoteToken), fee 10000,
tickSpacing 200, hooks = pairHook())`, so the key can be rebuilt from the
project token and the quote token alone. `quotePriceFeed` is the Chainlink
equity feed for the stock leg, which the desk already reads.

**Over HTTP.** `https://pair.fund/api/tokens/graduated?limit=200`,
`/api/stats/trending`, `/api/tokens/:address`, `/api/tokens/:address/trades`,
`/api/tokens/:address/candles`, `/api/stock-tokens`. No auth, browser
User-Agent not required. Each `pairs[]` entry carries `poolId`, `hookAddress`,
`poolFee`, `tickSpacing`, `weightBps`, `liquidityUsd` and `impliedPriceUsd`.

## The token that was tested

PAIR, the launchpad's own token, `0x6b1d42927b1a84ec28fa88d4fc6fa7af404966be`.
Graduated, one pool, the deepest book on the venue: 149,371 USD of liquidity
and 2.5M USD of 24-hour volume when checked.

Pool `0xf224a070c8626c890a085b258cf562ee4bf052b6d1d59104b3b44d722640c001`

| field | value |
|---|---|
| currency0 | SPY `0x117cc2133c37b721f49de2a7a74833232b3b4c0c`, 18 decimals |
| currency1 | PAIR `0x6b1d42927b1a84ec28fa88d4fc6fa7af404966be`, 18 decimals |
| fee | 10000 (static 1%) |
| tickSpacing | 200 |
| hooks | `0x16D1560630Ce74af4478d9b8AD46548A092A2000` |
| `StateView.getSlot0` | sqrtPriceX96 15667749320359575094380565788875, tick 105745, lpFee 10000 |
| `StateView.getLiquidity` | 69371163157332782983095 |
| LP position | 1152094, owned by PairV4Locker |

The desk's `poolIdOf` reproduces that pool id from the key, so the pool the
desk stores and the pool the launchpad registered are the same one. The row is
already in the desk database with `trap = 0`: lpFee 10000 hundredths of a bip
is 100 bps, under the 300 bps trap threshold.

Multi-pool example, for the adapter's benefit: CHIPS
`0xbcd09284dffe06f868a4650546233e3e9abe4d97` runs four pools at 2500 weightBps
each, quoted in NVDA, AMD, INTC and MU.

## The hook does not gate swaps

`PairV4Hook` is verified. Its permissions are one flag:

```solidity
function getHookPermissions() public pure override returns (Hooks.Permissions memory p) {
    p.beforeInitialize = true;
}
```

`_beforeInitialize` refuses any caller that is not the launchpad and any pool
the launchpad has not pre-authorised, which is how PAIR keeps its pool keys
canonical. There is no `beforeSwap`, no `afterSwap`, no fee override and no
delta return. The contract comment says it outright: "It changes neither swap
math, fees, balances nor liquidity accounting." The address ends in `2000`,
which is `BEFORE_INITIALIZE_FLAG` and nothing else.

So PAIR is the opposite of LONG. LONG routes every trade through a signed
router because its hook demands it. PAIR's pools are open to anyone holding
the quote token.

## Simulation result

Route: 0.02 USDG through the deepest hookless SPY/USDG pool
(`0xfe2a80bb5618fd14984b92ca6d45bf5ba67443ddb1435e28b2e48df2fc1526cd`, fee
3000, tickSpacing 60, no hook) into the PAIR/SPY pool. Caller
`0xEB262a96A796aeE8fb42478E37c86f32069993BE`, which already holds the ERC-20
approval to Permit2 and the Permit2 allowance to the router, so a revert would
have been a genuine refusal rather than a missing allowance.

- `V4Quoter.quoteExactInputSingle` leg 1: 20000 USDG raw to 25803031846599 SPY raw.
- `V4Quoter.quoteExactInputSingle` leg 2: that SPY to 1001256760370455570 PAIR raw.
- `V4Quoter.quoteExactInput` over both hops: the same 1001256760370455570, gas estimate 75423.
- `encodeV4Swap` plus `eth_call` from the agent address, before the fix: reverted with empty data.
- After the fix: `eth_call` returns, `eth_estimateGas` returns 202,542.
- Live fill: tx `0xcdb8b95a80c9feaeec85f6064b7daeafcf2784c2e2df81589d5e20b121cc76c6`, block 56825124, status success, gas used 197,490, 0.02 USDG in, 0.962981873338953373 PAIR out.

Control on the newer generation: the same corrected two-hop encoding through
`0xd2f759a1cf13c30127c551c3aee04629aea200c0` (asd/SPY, pool
`0xdb74a3f17d82b06290a4f8f4c8d196e8995488f8cffb325fd93da562cbf8a5c9`) also
passes `eth_call`, and `quoteExactInputSingle` on it returns a quote. Neither
PAIR generation gates the router.

One control does revert, and it is expected: a single-hop SPY to PAIR swap
from the agent address, because the agent holds no SPY and `SETTLE_ALL` cannot
pull what is not there.

## The router fork, which is why the first simulation failed

The UniversalRouter at `0x8876789976dEcBfCbBbe364623C63652db8C0904` is
verified on Blockscout and its sources are a fork of the v4 periphery, not the
mainline. Every swap struct carries an extra per-hop price floor:

```solidity
struct ExactInputSingleParams {
    PoolKey poolKey; bool zeroForOne; uint128 amountIn;
    uint128 amountOutMinimum; uint256 minHopPriceX36; bytes hookData;
}
struct ExactInputParams {
    Currency currencyIn; PathKey[] path; uint256[] minHopPriceX36;
    uint128 amountIn; uint128 amountOutMinimum;
}
```

`PathKey` is unchanged, and the action bytes are unchanged.

On a single swap the extra field sits exactly where mainline puts
`sqrtPriceLimitX96`, and both are sent as zero, so the desk's single-hop
calldata was byte-identical and always worked. The note in `swap.ts` that this
router "still carries `sqrtPriceLimitX96`" was the wrong explanation for the
right bytes.

On a multi-hop swap the array sits between the path and the amounts. The desk
was writing the mainline layout, so `currencyIn` decoded to an offset word and
the call reverted with no reason data. Every two-hop route the desk could
build was dead, on any venue, not only PAIR. A hookless USDG to SPY to USDG
control reverted the same way, which is how the cause was isolated.

The fix sends an empty array, which turns the check off. Route protection
stays where the desk already puts it: `amountOutMinimum` and `TAKE_ALL`.
`V4Router._swapExactInput` reverts with `InvalidHopPriceLength()` unless the
array is empty or exactly as long as the path, so the empty array is the only
safe default for a caller that does not want per-hop floors.

`PairV5MultiPoolAggregator` confirms the layout independently: it declares its
own `RobinhoodExactInputSingleParams` with `minHopPriceX36` in the same
position and comments that "Robinhood's proven exact-input settlement requires
zero minima in both router-level fields."

Files changed: `src/chain/abis.ts` (`swapActionAbi.exactInputSingle` and
`.exactInput`), `src/chain/swap.ts` (`encodeV4Swap` and the header note),
`test/chain/encode.test.ts` (frozen multi-hop vector regenerated, field names,
one assertion added).

## Adapter plan

No swap adapter is needed. What is needed is discovery and routing, so that
`covenant-desk quote PAIR --usd 100` finds the pool and the two-hop route
without being told.

1. **`src/chain/venues/pair.ts`, a read-only venue module.** Wrap the
   launchpad reads above behind
   `launchPools(projectToken): Promise<PairLaunchPool[]>` returning
   `{ quoteToken, weightBps, poolId, positionId, quoteUsdAtLaunchE8,
   quotePriceFeed, quoteDecimals }`, and `isPairPool(pool)` as
   `pool.hooks === launchpad.pairHook()`. Rebuild each pool key locally and
   assert it hashes to the `poolId` the launchpad returned; refuse the pool if
   it does not, so a launchpad upgrade cannot silently move the desk onto a
   different pool. Cache `pairHook()` per process, refresh with the registry.

2. **Route through the stock leg.** `QuoterDeps.intermediates` is
   `[USDG, WETH]` today, so a USDG to PAIR route only works if the SPY/USDG
   pool happens to fall inside the first fifty USDG pools the store returns.
   Make the intermediate list per-request: when the target token is
   stock-paired, put its quote tokens, in `weightBps` order, at the front of
   the list. The launchpad read gives them directly; `pools.forToken(token,
   { counterparties })` already exists for the other side.

3. **Split across pools for multi-pool tokens.** For CHIPS-style launches,
   quote each `(USDG, quoteToken_i, projectToken)` route separately and either
   fill through the best one or split by `weightBps`. Fill the legs as
   separate UniversalRouter calls; the desk's per-order bounds then apply per
   leg, and one failed leg does not take the rest down.

4. **Fair value.** `quotePriceFeed` from the launchpad is the Chainlink feed
   for the stock leg, which is exactly the input `fairvalue/paired` needs.
   Prefer it over matching by symbol.

5. **Leave `PairV5MultiPoolAggregator` alone for now.** It is permissionless
   and it would save one round trip on a multi-pool token, but it costs
   control: it sends zero minima to the router and enforces slippage from its
   own balance delta afterwards, it converts USDG to the quote token through
   Uniswap v3 (`SwapRouter02`, fee tier per stock token fixed in
   `PairV4RoutePolicy`, 500 for AAPL, NVDA, SPY, SPCX and GOOGL, 3000 for most
   others, 10000 for SNDK) rather than through the v4 pools the desk already
   prices, and it requires a fresh Permit2 approval to itself. Worth revisiting
   only if splitting five legs through the plain router proves expensive.

If it is ever needed, the ABI is on Blockscout and the two entry points are:

```
buyExactInput(address projectToken, address fundingToken, address recipient,
  (uint8 poolIndex, PoolKey poolKey, uint128 amountIn, uint128 minAmountOut)[] legs,
  uint256 aggregateMinOut, uint256 deadline) returns (uint256 amountOut)
sellExactInput(address projectToken, address outputToken, address recipient,
  (uint8 poolIndex, PoolKey poolKey, uint128 amountIn, uint128 minAmountOut)[] legs,
  uint256 aggregateMinOut, uint256 deadline) returns (uint256 amountOut)
```

`fundingToken` and `outputToken` must be USDG, WETH, or a stock token listed in
`PairV4RoutePolicy`. `deadline` must be within 86400 seconds. Errors:
`InvalidAddress`, `InvalidAmount`, `ExpiredOrUnboundedDeadline`,
`DuplicatePool(uint8)`, `InvalidPoolKey(uint8)`, `UnsupportedRoute(address)`,
`Slippage(uint256,uint256)`, `BalanceNotRestored(address,uint256,uint256)`.

## Corrections to FACTS.md

- USDG has **6 decimals**, read from the token. FACTS carries this as VERIFY.
- The 1% figure in the FACTS launchpad note is the pool fee, and it is static
  (`fee = 10000`, not the dynamic flag). PAIR pools are not trap pools by the
  desk's 300 bps rule.
- The claim that the aggregator "rejects >15% impact" is not in the deployed
  contract. `PairV5MultiPoolAggregator` enforces `MAX_LEGS`, a bounded
  deadline, no duplicate pool index, a pool key that matches the launchpad's
  registration, an aggregate minimum out, and a zero residual balance. There is
  no price-impact ceiling.
