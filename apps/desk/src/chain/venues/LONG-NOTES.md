# LONG on Robinhood Chain: router calldata and what the desk actually needs

Checked live on 2026-09-07 against `https://rpc.mainnet.chain.robinhood.com`
(chain 4663) and `https://robinhoodchain.blockscout.com`. Every number below
came out of a real transaction or a real call. Re-check before trusting it.

## 1. The headline

The desk does not need LONG's router, and does not need a signature from
LONG's backend, to trade a LONG launchpad token.

The AI/NVDA pool is an ordinary Uniswap v4 pool with a Clanker hook. The
desk's own `encodeV4Swap` two-hop route through the UniversalRouter filled it:

```
tx     0xc46c12a6d151326c3ebf006ffdd9658b84ad5b42d92de9ead9d308dcf4af1ecb
block  56830620   status success   gasUsed 560980
in     30000 raw USDG (0.03)
out    515185548383037527316789 raw AI (515185.548)
route  USDG -> NVDA  pool 0x7990aad9e8fb048f49a155a7df5603db0366f0657035b78eb4196395cccb3dcd
       NVDA -> AI    pool 0x01fcb246e7b957ed4a65b0f8423e973b65a17baef45361fa087c52c0e85e42f4
```

The `V4Quoter` had priced the same route at exactly 515185548383037527316789,
so the fill matched the quote to the wei.

Hook checks that back this up:

- `ClankerHookStaticFeeV2` at `0x48b8f6ad3a1b4aa477314c9a23035b8f84dde8cc`,
  16,405 bytes of runtime code, contains **zero** occurrences of the
  `msgSender()` selector `0xd737d0c7`. It never asks the router who the caller
  is, so it cannot gate on it.
- `V4Quoter.quoteExactInputSingle` on the AI/NVDA pool returns a price for an
  arbitrary caller: 0.001 NVDA quotes 3988985507022446220412961 raw AI.
- `eth_call` of the desk's encoded two-hop swap from the agent EOA
  `0xEB262a96A796aeE8fb42478E37c86f32069993BE` returned success, not a revert,
  before the transaction above was sent.

So the earlier working assumption ("LONG tokens cannot be swapped by calling
the UniversalRouter directly") is wrong. LONG's router is a fee wrapper and a
route planner. It is not a gate.

## 2. Addresses

| what | address | note |
|---|---|---|
| LONG router | `0x6F6F5E1b4669C2e1553E65e6162D9E60172bb7Fe` | unverified, selector `0x39ecce49` unknown to 4byte and openchain |
| router deployer | `0x0bC6E9f2714eF2f3d586e5167dc282119a0edcd8` | creation tx `0xbcfb9166b3fb1e6aebad5e4c124b60350e857e35184a090ae293690049b8bcbd` |
| Clanker launchpad | `0xd3f2cc1731b7fd17f28798835c2e02f0a1839a94` | verified, name `Clanker` |
| Clanker hook | `0x48b8f6ad3a1b4aa477314c9a23035b8f84dde8cc` | `ClankerHookStaticFeeV2` |
| Clanker fee locker | `0x290F735F63824BB5836cDe24a35F5103A5B5Bc99` | takes the pool's fee position |
| UniversalRouter (v4) | `0x8876789976dEcBfCbBbe364623C63652db8C0904` | |
| V4Quoter | `0x8dc178efb8111bb0973dd9d722ebeff267c98f94` | |
| SwapRouter02 (v3) | `0xcaf681a66d020601342297493863e78c959e5cb2` | verified, solc 0.7.6 |
| QuoterV2 (v3) | `0x33e885ed0ec9bf04ecfb19341582aadcb4c8a9e7` | verified, solc 0.7.6 |
| AI token | `0x48c3f51cf851b85e1bbc1c040f51bef0c56aeb07` | Artificial Inu, 18 decimals |

Uniswap v3 is deployed on 4663 and LONG uses it for the stock leg. The desk's
router today is v4 only, so it prices that leg through a v4 pool instead.

## 3. `0x39ecce49` calldata layout

Four top-level arguments. Decoded from seven real transactions, four sells and
three buys, all to the LONG router:

```
sells 0x01b1184f93fc0a946690ac2dc088ed944c63ff48aa9fc3f0eba0e85b5a3f1a16
      0x8ccae92d5ebc918ca453ca2ab18e294d1bad7f0c215192092ed732856204d76f
      0x10fd7f6eaea0af6926d3f4ab61ee10cde5787225e7e36b60ba3ecf6b184667ec
      0x000c65c7dfae1fa2764aa7de1d90701048aad868f05fdd10ab5dd021cee30511
buys  0x55df08b807fddb1559f965fa42cb59eb00b9e835b6715c1fc9f763b686ddee3f
      0xe1ab96e998d175da017614f3521d9e763df647894e749ef292faf3eb3b846f6d
      0x802eb737f1e6a98f969170c74f6bc26a47dbd0f33aa18b626eccfee51cd641de
```

```solidity
// selector 0x39ecce49; the function name is not public
function <unknown>(
    uint256 amountIn,      // 0 on a buy: the input is msg.value
    uint256 minAmountOut,
    Hop[]   route,
    Order   order
) payable;

struct Hop {
    address tokenIn;      // address(0) on the wrap hop
    address tokenOut;     // address(0) on the unwrap hop
    address router;       // UniversalRouter for a v4 hop, SwapRouter02 for a v3 hop, 0 for wrap/unwrap
    address quoter;       // V4Quoter for a v4 hop, QuoterV2 for a v3 hop, 0 for wrap/unwrap
    uint24  fee;          // 0x800000 dynamic on v4 hook pools; 100 / 500 / 3000 on v3
    int24   tickSpacing;  // 200 on the AI/NVDA pool; 1 / 10 / 60 on v3
    address hooks;        // the v4 hook; 0 everywhere else
    bytes   hookData;     // empty in all seven transactions
}

struct Order {
    address feeRecipient0;
    address feeRecipient1;
    address feeRecipient2;
    uint256 feeBps;       // 90 in all seven
    uint256 split0Bps;    // 2000 or 2500
    uint256 split1Bps;    // 3500 in all seven
    uint256 reserved0;    // 0 in all seven
    uint256 reserved1;    // 0
    uint256 reserved2;    // 0
    uint256 deadline;     // unix seconds
    uint256 direction;    // 1 on every sell, 0 on every buy
    uint256 reserved3;    // 0
    bytes   signature;    // exactly 65 bytes, r || s || v, v = 0x1b or 0x1c
}
```

TypeScript, matching `src/chain/` conventions:

```ts
import type { Address, Hex } from 'viem';

export const LONG_ROUTER: Address = '0x6F6F5E1b4669C2e1553E65e6162D9E60172bb7Fe';
export const LONG_SWAP_SELECTOR = '0x39ecce49' as const;

/** address(0) in tokenIn wraps ether; in tokenOut it unwraps. */
export interface LongHop {
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  readonly router: Address;
  readonly quoter: Address;
  readonly fee: number;
  readonly tickSpacing: number;
  readonly hooks: Address;
  readonly hookData: Hex;
}

export interface LongOrder {
  readonly feeRecipient0: Address;
  readonly feeRecipient1: Address;
  readonly feeRecipient2: Address;
  readonly feeBps: bigint;
  readonly split0Bps: bigint;
  readonly split1Bps: bigint;
  readonly reserved0: bigint;
  readonly reserved1: bigint;
  readonly reserved2: bigint;
  readonly deadline: bigint;
  readonly direction: bigint;
  readonly reserved3: bigint;
  /** 65 bytes, produced off chain by LONG. */
  readonly signature: Hex;
}

export const longSwapAbi = [
  {
    type: 'function',
    name: 'swap',
    stateMutability: 'payable',
    inputs: [
      { name: 'amountIn', type: 'uint256' },
      { name: 'minAmountOut', type: 'uint256' },
      {
        name: 'route',
        type: 'tuple[]',
        components: [
          { name: 'tokenIn', type: 'address' },
          { name: 'tokenOut', type: 'address' },
          { name: 'router', type: 'address' },
          { name: 'quoter', type: 'address' },
          { name: 'fee', type: 'uint24' },
          { name: 'tickSpacing', type: 'int24' },
          { name: 'hooks', type: 'address' },
          { name: 'hookData', type: 'bytes' },
        ],
      },
      {
        name: 'order',
        type: 'tuple',
        components: [
          { name: 'feeRecipient0', type: 'address' },
          { name: 'feeRecipient1', type: 'address' },
          { name: 'feeRecipient2', type: 'address' },
          { name: 'feeBps', type: 'uint256' },
          { name: 'split0Bps', type: 'uint256' },
          { name: 'split1Bps', type: 'uint256' },
          { name: 'reserved0', type: 'uint256' },
          { name: 'reserved1', type: 'uint256' },
          { name: 'reserved2', type: 'uint256' },
          { name: 'deadline', type: 'uint256' },
          { name: 'direction', type: 'uint256' },
          { name: 'reserved3', type: 'uint256' },
          { name: 'signature', type: 'bytes' },
        ],
      },
    ],
    outputs: [],
  },
] as const;
```

The name `swap` is a placeholder. `keccak("swap(...)")` over this exact tuple
does not have to equal `0x39ecce49`, so encode by hand with the selector
rather than through `encodeFunctionData` until the real name is known.

### Worked example, sell

`0x01b1184f93fc0a946690ac2dc088ed944c63ff48aa9fc3f0eba0e85b5a3f1a16`, block
16655146, four hops:

```
0  AI    -> NVDA   UniversalRouter / V4Quoter    fee 0x800000  ts 200  hook 0x48b8f6ad...
1  NVDA  -> USDG   SwapRouter02   / QuoterV2     fee 500       ts 10   hook 0
2  USDG  -> WETH   SwapRouter02   / QuoterV2     fee 100       ts 1    hook 0
3  WETH  -> 0x0    0 / 0                         0 / 0 / 0             unwrap
```

A buy is the mirror: hop 0 is `0x0 -> WETH` with every other field zero, then
WETH to USDG to the stock token to the launchpad token, and `direction` is 0.

## 4. The fee, exactly

Both legs charge on the native side, so the numbers reconcile against the
internal transfers rather than the ERC-20 transfers. `gross` is `msg.value` on
a buy and the ether the route produces on a sell.

```
fee    = gross * feeBps / 10_000                 // feeBps = 90
part0  = fee * split0Bps / 10_000                // to feeRecipient0
part1  = (fee - part0) * split1Bps / 10_000      // to feeRecipient1
part2  = fee - part0 - part1                     // to feeRecipient2
extra  = gross * 10 / 10_000                     // 10 bps, always to 0x7e7dF6F60581F13216c5E6FcfC5643B9F4261750
user   = gross - fee - extra                     // 99% of gross
```

Checked against two transactions:

| | buy `0x802eb737...` | sell `0x01b1184f...` |
|---|---|---|
| gross | 0.1 ETH | 0.04511251310664451 ETH |
| split0Bps | 2500 | 2000 |
| `0xB59665f5b091e9342b8217F296b02fE5487E398E` | 0.000225 | 0.00008120252359196 |
| `0x725526b331B4307e8E67b228Cd11B71E13DC0aE8` | 0.00023625 | 0.000113683533028744 |
| `0x7f77BAd9EB06373Fe3AEE84f85a9D701FF820EeB` | 0.00043875 | 0.000211126561339096 |
| `0x7e7dF6F60581F13216c5E6FcfC5643B9F4261750` | 0.0001 | 0.000045112513106644 |
| to the trader | 0.099 into WETH | 0.04466138797557807 |

All four recipients are EOAs. `0x7e7dF6F6...` is constant across all seven
transactions and does not appear in the calldata, so the router holds it.
Trading LONG's own way costs 100 bps on the native leg on top of the pool fee.
Routing the same trade through the UniversalRouter costs the pool fee alone;
the AI/NVDA pool reads `lpFee` 10000, which is 1%.

## 5. The signature

`order.signature` is 65 bytes and differs in every transaction, including two
transactions in adjacent blocks with byte-identical routes and fee splits. The
only fields that move with it are `deadline` and the amounts, so the digest
covers the order. LONG's backend produces it; the router's runtime code
(23,988 bytes) is unverified and exposes no plain getter that returns the
authorised signer through the 80 `PUSH4` selectors found in its bytecode.

Reproducing that signature is not possible without LONG. It is also not
needed: section 1 is a filled trade that never touched the LONG router.

## 6. Is there a public signing endpoint

No endpoint was reachable and no public documentation exists. Evidence:

- `long.xyz` redirects to `app.long.xyz`. Every host under `long.xyz` returns
  a Cloudflare 403 block page to this machine: `app.long.xyz`,
  `api.long.xyz`, `api.long.xyz/v1/graphql`, `docs.long.xyz`,
  `storage.long.xyz`, `longx.long.xyz`, `assets.long.xyz`, `custom.long.xyz`.
  Verified with a full Chrome header set, with headless Chrome 140, and with
  `WebFetch`. Ray ID `a375926dd81d2610`.
- Through a third-party reader the same hosts return the Cloudflare
  "Just a moment" interstitial, so the block is bot management rather than a
  missing service.
- The only archived bundle set (Wayback, 2026-07-20 and 2026-07-26, 38 chunks,
  5.6 MB decompressed) is the Base-era app built on the Doppler SDK. It
  contains one API base, `https://api.long.xyz/v1`, and one GraphQL endpoint,
  `${base}/graphql`, whose named operations are all read-only asset listings:
  `ListAllAssetsForIntegrator`, `ListLiveAssetsForIntegrator`,
  `ListGraduatedAssetsForIntegrator`, `ListIncomingAssetsForIntegrator`,
  `ListAuctionPoolsForIntegrator`, `GetAssetByAddressForIntegrator`,
  `GetAuctionPoolsByAddressAndIntegrator`, `SearchAssetsForIntegrator`,
  `GetTokenImagesByAddresses`, `AttestationsForUsers`. No quote, route, sign,
  swap, or trade operation appears anywhere in those bundles, and neither does
  the router address `0x6F6F5E1b...` or the selector `0x39ecce49`.
- `docs.long.xyz` and `api.long.xyz` have no Wayback snapshots at all, and no
  public developer documentation turned up in search.

The "Integrator" naming in the GraphQL schema says LONG already models
third-party integrators for reads. Route signing for writes would be the ask.

## 7. What the desk should do

Trade LONG tokens through the desk's own v4 encoder. Nothing in this venue
needs a special adapter beyond pool selection, and the desk's route is 100 bps
cheaper on the native leg than LONG's own path.

Two follow-ups worth carrying into the adapter work:

1. The deepest stock leg on 4663 can be a Uniswap v3 pool. LONG routes
   NVDA/USDG through v3 at 5 bps. The desk priced the same leg through a v4
   pool. Compare both before routing, or accept a worse stock leg on size.
2. `direction` and the reserved words are inferred from seven samples that all
   trade AI against NVDA. A route with a different shape could show the
   reserved words carrying something. Re-decode before writing an encoder.
