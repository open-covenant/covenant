# Plain stock-quoted pools on chain 4663

Checked live on 2026-09-07 against `https://rpc.mainnet.chain.robinhood.com`
(block 56,828,562) and `https://robinhoodchain.blockscout.com`. Pool inventory
read from the desk store, state and every quote read from chain.

**Headline: outside LONG, no adapter is needed.** The 150 deepest
stock-quoted pools that are not on Clanker's hook were simulated at 0.03 USDG,
USDG into the stock token and the stock token into the paired token, sent as
`eth_call` from the desk wallet with the desk's own calldata. All 150 filled.
The launchpad makes no difference to execution: Doppler, PAIR, Pons, Flaunch
and unhooked pools all behave like ordinary Uniswap v4, and between them they
cover 53,210 of the 53,333 stock-quoted pools on the chain.

The one thing that did have to be fixed first is the desk's multi-hop
encoding. This chain runs a fork of the v4 periphery that carries a per-hop
price floor, `minHopPriceX36`, which is an array on `SWAP_EXACT_IN`. Mainline
calldata reverts with no reason data, which is what every two-hop simulation
returned until `src/chain/abis.ts` was corrected. Single-hop calldata was
unaffected because the field lands in the same word as `sqrtPriceLimitX96` and
both are sent as zero. Same finding as `PAIR-NOTES.md`, reached from the other
side.

## The set

A stock-quoted pool holds exactly one Robinhood stock token; the other side is
neither USDG nor wrapped ether. There are 53,333 of them. Removing the 123 on
Clanker's hook leaves **53,210 plain pools, 37,453 of which hold liquidity**,
against 194 stock tokens.

## Which launchpad owns which hook

Attribution is by the contract that deployed the paired token, read from the
token's creation trace, not by name matching.

| hook | contract | launchpad | pools | funded |
|---|---|---|---|---|
| `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | `DopplerHookInitializer` | Doppler, through Airlock `0xeb7C034704eF8Dcd2D32324c1545f62fB4aD0862` and `DopplerERC20V1Factory` `0x1B37D3a72082029c44B35B604Ea473617580b69a` | 33,932 | 23,707 |
| `0x16d1560630ce74af4478d9b8ad46548a092a2000` | `PairV4Hook` | PAIR (pair.fund), launchpad `0x8660A7F019C7943b0b0A91B8E39AFf3b6DB6Ae62` | 4,164 | 2,711 |
| `0x0000000000000000000000000000000000000000` | none | no launchpad; tokens deployed by ordinary accounts | 3,386 | 2,542 |
| `0x778b0c4eea7d35d66513b587ba87fc9084b0eacc` | `LaunchHook` | `LaunchTokenDeployer` `0x6544AF3524a8d9135Eb5765CECE6E514d85D615b` | 3,221 | 1,567 |
| `0x0310cfebe1d7a69f2414f6595bbe9d17c5342acc` | `LaunchHook` | `LaunchTokenDeployer` `0xf86dfDb678D8E5d932100Ef479A59fa65a82a5Eb` | 2,906 | 2,305 |
| `0xe5e702641ea86f4ae6cc3cdaed2b886f976be044` | `PonsV2MemeHook` | Pons, `PonsV2LaunchDeployer` `0x3711ceA4feaDE896C913C68F01Eda97Cb06D1A42` | 2,006 | 2,006 |
| `0x8aa375f7186f86bbac7b13ab01db189ebe50c0c4` | unverified, self-deploying | unattributed | 996 | 710 |
| `0x4eb1976978756bd56802d8162f2271844924e0cc` | `ERC1967Proxy` | unattributed | 726 | 499 |
| `0xd2f759a1cf13c30127c551c3aee04629aea200c0` | unverified | PAIR second generation, `PairTokenV5LaunchV2Factory` | 334 | 316 |
| `0xd8e101ca5a6dc06382536b26c7ca7bc1a8a6a8cc` | unverified | `EquityTokenFactory` `0x9A92FA82466Ef93853b28d3C2719081C89D68db3` | 75 | 56 |
| `0x8d346f24278c5cd786309161aac0fc2bbe4c25dc` | unverified | Flaunch | 68 | 14 |
| `0x3a319a4769d2a473ed93861766ce217775412044` | unverified | `RobinhoodLaunchTokenFactory` | 19 | 19 |
| `0x72d25ed431210a29ac637319efd8adf8aa4820c0` | unverified | `SockLaunchpad` | 19 | 5 |
| `0xd7d3d9ceec4e8f295bbb87ce7bd46c7c6224a0cc` | unverified | `LunchV4PairLauncherImmutable` | 19 | 16 |

Two corrections to what the desk assumed.

**The hook at `0x48b8f6ad3a1b4aa477314c9a23035b8f84dde8cc` is not LONG's.** It
is `ClankerHookStaticFeeV2`, verified, deployed through the deterministic
deployer. The paired tokens under it come from the Clanker factory
`0xD3f2cC1731b7Fd17f28798835C2E02f0a1839A94`. LONG is a signed-route front end
over Clanker pools, so the restriction is LONG's router, not the hook itself.
It covers 123 stock-quoted pools, 0.2% of the set.

**Bankr sits on Clanker, not on its own hook.** No Bankr-specific hook or
factory appears anywhere in the stock-quoted set. Bankr launches land in the
same 123-pool Clanker family, which is the family the desk cannot reach
directly.

## USDG entry legs

Every route starts USDG into the stock token. The deepest USDG pool is rarely
the best one: for SPCX, GME and NVDA the best fill comes from a set of
zero-fee, dynamic-fee pools all deployed by
`0xD9d96fE2aE5f30fD39df4Df177FBb7DA5d8A71dA`, one per stock, which beat the
100 bps unhooked pools by a wide margin. AAPL is the exception documented in
`FACTS.md`: its entry is the `FablesRWA` oracle pool
`0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147`.

The desk's quoter picks by liquidity, not by fill, so it will route AAPL
correctly and route SPCX, GME and NVDA into a 100 bps pool it did not have to
pay. That is a routing defect worth fixing separately; it is not a blocker.

## Simulation

Each row was quoted through `V4Quoter.quoteExactInput` and then simulated as a
real fill: calldata from `encodeV4Swap` in `src/chain/swap.ts`, sent as
`eth_call` from the desk wallet `0xEB262a96A796aeE8fb42478E37c86f32069993BE`
with `minAmountOut` zero, so the result reports whether the route executes
rather than whether it prices well. Permit2 allowances for USDG were already
in place, so a settle failure would have shown up as a revert.

### Ranked by pool liquidity

| # | token | token address | stock leg | pool id | hook | lp fee | liquidity | 0.03 USDG sim |
|---|---|---|---|---|---|---|---|---|
| 1 | PLS | `0xed2736d9ada10ef50fa1c4db118d4108db45fd98` | NVDA | `0x85613e20f26fdca2…` | `0x0000000000000000000000000000000000000000` | 30 bps | 1.55e+27 | pass |
| 2 | APE | `0x104ea0e4747405f588961cf5b0460cafb9b88788` | GME | `0xa6aa8c617a1a68f9…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 250 bps | 1.19e+27 | pass |
| 3 | TESTMAGNET | `0x8466420cc715a4fb917ad08da2b956e3db434f61` | SPY | `0x4dcbaa30a4441201…` | `0x3654b9a3c452c095103ad4e8bc32efaccf6ed0c4` | 0 bps | 1.00e+27 | pass |
| 4 | NRWA | `0xd3fa5fd735105b40aacb917d0d2edea24f1b8c9c` | NVDA | `0xabf1dc9e2ae50150…` | `0xcb4d62a616729d1f27e31f341a1527b77a37f0c4` | 0 bps | 1.00e+27 | pass |
| 5 | CRWQ | `0xadff88f04f1450b9d5866a5de16d2e22a6dc2e74` | NVDA | `0x13350444ea3b94d9…` | `0xcb4d62a616729d1f27e31f341a1527b77a37f0c4` | 0 bps | 1.00e+27 | pass |
| 6 | LARRY | `0x7f98d4ec9c905928727c8c0fc68d02489e2f1e18` | ORCL | `0xca1356c31e598790…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 10 bps | 7.01e+26 | pass |
| 7 | MOASS | `0xae7546cb9cebd0fb27e59333314ff41371e709c1` | GME | `0xc6eced01eaa697df…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 250 bps | 4.10e+26 | pass |
| 8 | BA | `0xe6c0c41edd8662b6a9b806b94972ff4b2e8b6da2` | BA | `0x28faede7f52a7a6c…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 250 bps | 4.05e+26 | pass |
| 9 | CHILL | `0x4aaba07e09d7844a3a7f9215441e0d2d971fb154` | NFLX | `0x3e6fd3bb00227579…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 250 bps | 2.58e+26 | pass |
| 10 | STRAT | `0xc8cd8e78f406ba201fc492c33de1de83a188e811` | MSTR | `0x62dd135893b682b9…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 250 bps | 2.10e+26 | pass |
| 11 | STLSPCXMTGIN | `0xc2bf9ee39f8d912b5a96e66b4766f7f2a45d1413` | SPCX | `0x2b1e2dac51623380…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 10 bps | 1.76e+25 | pass |
| 12 | tornadoes | `0x807533834164b770fc5150dff67a7392c717cd1e` | COIN | `0x599d3c46c8c06163…` | `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` | 0 bps | 1.73e+25 | pass |
| 13 | tornadoes | `0x807533834164b770fc5150dff67a7392c717cd1e` | PLTR | `0xdf48baba6b288956…` | `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` | 0 bps | 1.73e+25 | pass |
| 14 | tornadoes | `0x807533834164b770fc5150dff67a7392c717cd1e` | MSFT | `0xb6f7abb00c0f773f…` | `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` | 0 bps | 1.73e+25 | pass |
| 15 | tornadoes | `0x807533834164b770fc5150dff67a7392c717cd1e` | SGOV | `0x0e9fc1cfc513ba7c…` | `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` | 0 bps | 1.73e+25 | pass |
| 16 | tornadoes | `0x807533834164b770fc5150dff67a7392c717cd1e` | USAR | `0xc2016ac1ed62f45c…` | `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` | 0 bps | 1.73e+25 | pass |
| 17 | DOGE1 | `0x3582c0ed4324cb742266401e56c68d026118eba3` | SPCX | `0x4b8b7245a1fef270…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 70 bps | 1.29e+25 | pass |
| 18 | TESTASDASASDDAA | `0xfc4a09f240766fb63f25fde0b40d6742d4603333` | NVDA | `0x52fe80dc0a3765ee…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 50 bps | 1.11e+25 | pass |
| 19 | TESTASDASDASD | `0x8ffaf28a3192fac276b78401555c566a69473333` | NVDA | `0xcd81d036c99c123e…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 50 bps | 1.11e+25 | pass |
| 20 | TESTASDASDASDD | `0xaba49face8283ae075018cc6da1718024c723333` | NVDA | `0x42e6384214b2b619…` | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | 50 bps | 1.11e+25 | pass |

### Ranked by on-chain use (holders and transfers)

| token | token address | stock leg | pool id | hook | lp fee | liquidity | holders | transfers | round trip | 0.03 USDG sim |
|---|---|---|---|---|---|---|---|---|---|---|
| MARSCOIN | `0xd8ec6474c02e5f913a8fd566648a2df4b18dbba3` | SPCX | `0x94d40a947551b068…` | `0x4e3468951d…` | 70 bps | 5.58e+24 | 2231 | 334149 | 96.64% | pass |
| ZAIBATSU | `0x5dbaca8327b0baa57eb6c872a333bf8d6f642ba3` | GME | `0x1cbac474c81b584c…` | `0x4e3468951d…` | 70 bps | 8.51e+24 | 1044 | 168609 | 96.54% | pass |
| NVDA | `0xdecf74e4aa6ff30b1612e65665aaf650bedecba3` | NVDA | `0xd9ea532d4b342ae5…` | `0x4e3468951d…` | 70 bps | 4.55e+24 | 648 | 126798 | 96.64% | pass |
| DOGE1 | `0x3582c0ed4324cb742266401e56c68d026118eba3` | SPCX | `0x4b8b7245a1fef270…` | `0x4e3468951d…` | 70 bps | 1.29e+25 | 180 | 59058 | 96.64% | pass |
| WSB | `0x8000e44986a7892caba486349e7291da388e7ba3` | GME | `0xa52eed541c3aaf7b…` | `0x4e3468951d…` | 70 bps | 1.04e+25 | 144 | 50954 | 96.64% | pass |
| HODL | `0x2fff19fdfcf9d74a3a2e63fcad5f930d451b0ba3` | GME | `0x7bb8f56bee52af44…` | `0x4e3468951d…` | 70 bps | 4.45e+24 | 122 | 44748 | 96.64% | pass |
| WSB | `0xee75df3387eff2a73405d26496934b9e9abd1ba3` | GME | `0xb96be94cdf94becf…` | `0x4e3468951d…` | 70 bps | 1.11e+25 | 175 | 41618 | 96.64% | pass |
| RKT | `0x68685e9dc6dcfe17962450399aa2d84a54adcba3` | GME | `0xe89b807b9d581725…` | `0x4e3468951d…` | 70 bps | 4.57e+24 | 119 | 41055 | 96.64% | pass |
| COIN | `0xf65d23387c4fe9150eb82d1cd57876e99969cba3` | GME | `0x21c36879303b5ed0…` | `0x4e3468951d…` | 70 bps | 5.43e+24 | 72 | 22526 | 96.64% | pass |
| SPCX | `0xc40a8118fd33330772de5be4c6e2587a07489ba3` | SPCX | `0x0a15fd7500462f06…` | `0x4e3468951d…` | 70 bps | 4.49e+24 | 77 | 20901 | 96.64% | pass |
| BTC | `0xeb5e79262b9d96dcdb2ceb1a2b5b0ed26bff2ba3` | MSTR | `0xa13583435298df81…` | `0x4e3468951d…` | 70 bps | 4.76e+24 | 38 | 15537 | 96.08% | pass |
| MSTR | `0x19edc1e8c597e054593dddd5d12be7a9cafe8ba3` | MSTR | `0x4d17eab492ef6752…` | `0x4e3468951d…` | 70 bps | 5.52e+24 | 86 | 15526 | 96.08% | pass |
| DOGE | `0x4d61f21ca2fa4002e038d86ad186d9cb1f74eba3` | SPCX | `0xcded3b01bf601755…` | `0x4e3468951d…` | 70 bps | 5.07e+24 | 52 | 14184 | 96.64% | pass |
| FART | `0xcf57098451c03c0ed8d31cbf55f06c55be104ba3` | GME | `0xdeb4c16867caeacc…` | `0x4e3468951d…` | 70 bps | 1.03e+25 | 19 | 4032 | 96.64% | pass |

Raw liquidity ranks freshly seeded launches, not traded ones. Most of the top
20 above has three to nine holders and fewer than 200 transfers: a launch
deposit and nothing since. Four of the top 20 are named `TEST…`. The second
table is the list worth trading.

## Depth and cost

Thirty-six pools were quoted at 0.003, 0.03 and 0.30 USDG, and then quoted
back, selling the received amount into USDG through the same two pools.

- Price impact at 0.03 USDG is below measurement resolution everywhere: the
  0.03 quote matched ten times the 0.003 quote to within 2 bps, and the 0.30
  quote matched a hundred times it to the same tolerance. Size is not the
  constraint at this notional.
- Round-trip cost is fees, and only fees. 96.6% back on a 70 bps Doppler pool,
  99.3% on the unhooked 30 bps PLS/NVDA pool, 92.2% on a 300 bps pool. The
  loss splits about evenly between the paired pool and the USDG entry leg.
- Five of the thirty-six price a 0.03 USDG buy and then refuse to sell the
  amount that buy delivered, reverting with `0x6190b2b0`. Under
  `0x49a446e0dc7a4f998ac2cb4e5ce861cc79e16054` (the `tornadoes` pools, 26 of
  them) and `0x3f27fb70d91ad868fd6b94db67cb919b38b7e8cc` (7) the refusal is a
  size ceiling: a tenth or a hundredth of the same amount sells fine, so the
  liquidity sits on one side of the tick and the exit is smaller than the
  entry. Under `0xfa8e2b6a377016b5e21e458215fc2125d789d080` (3) every size
  tested reverts, down to a thousandth. The desk should quote the exit at the
  entry size before it takes a position in any pool it has not traded, whatever
  the hook says.

## Best candidate for a live proof

**MARSCOIN, `0xd8ec6474c02e5f913a8fd566648a2df4b18dbba3`, quoted in SPCX.**

| | |
|---|---|
| paired pool | `0x94d40a947551b06802705277bcedbb7c2ea2d789b1aba763e208fd5141dccab6` |
| pool key | SPCX `0x4a0e65a3eccec6dbe60ae065f2e7bb85fae35eea` / MARSCOIN, fee `0x800000`, tickSpacing 200, hook `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` |
| current lp fee | 70 bps |
| liquidity | 5.58e24 |
| USDG entry leg | `0xafeca386a0cf3b6df4ad9c202b8de61e82ea3b188492b44e23bb0541ee50a21c`, fee `0x800000`, tickSpacing 2, hook `0xeb4f11e1320a3bd53c1c9070a4f0ce61692245c7`, 0 bps |
| 0.03 USDG buys | 71,081.87 MARSCOIN |
| sell straight back | 0.028992 USDG, 96.64% |
| price impact | 0 bps at this size |
| holders / transfers | 2,231 / 334,149 |

It is the most traded paired token in the plain set by a wide margin, and its
pool is deep enough that 0.03 USDG does not move it. The stock leg is the
reason to prefer it over the deeper but quieter pools: SPCX is the only stock
in this list carrying both a Chainlink feed
(`0xb265810950ba6c5c0ff821c9963014a56fd8bffb`) and a Lighter RH perp
(market 18), so a fill here exercises premium and hedge sizing as well as
execution. Doppler carries 64% of the plain set, so a fill on this hook stands
for most of the market.

Runners-up, both proven in simulation and both quoting in each direction:
ZAIBATSU/GME (`0x1cbac474c81b584caff0f1aa23eb38fb39beab5c173eb6d8486036f4f29808ca`,
1,044 holders, deeper at 8.51e24, but GME has no Lighter market) and
PLS/NVDA (`0x85613e20f26fdca2193ef6cd77fa3d3e872a37c7f25e2ed68190e2e713ed8bc7`,
the cheapest round trip at 99.3% and no hook at all, but a dormant token with
three holders).

## Reproducing

Pool inventory:

```sql
SELECT p.* FROM pools p
LEFT JOIN tokens s0 ON s0.address = p.currency0 AND s0.is_stock_token = 1
LEFT JOIN tokens s1 ON s1.address = p.currency1 AND s1.is_stock_token = 1
WHERE (s0.address IS NOT NULL) != (s1.address IS NOT NULL)
  AND CAST(p.liquidity AS REAL) > 0
ORDER BY CAST(p.liquidity AS REAL) DESC;
```

Drop the USDG, wrapped ether and native sides from the non-stock currency, and
drop `hooks = 0x48b8f6ad3a1b4aa477314c9a23035b8f84dde8cc`.

Simulation, per pool: build two `RouteHop`s, USDG into the stock token through
the entry leg and the stock token into the paired token through the pool under
test, pass them to `encodeV4Swap` with `amountIn` 30000 and `minAmountOut` 0,
and `eth_call` the result to the UniversalRouter from the desk wallet. A pass
means the router settled the input through Permit2, took both hops, and
delivered the output.
