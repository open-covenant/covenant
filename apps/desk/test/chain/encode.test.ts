import { describe, expect, it } from 'vitest';
import { decodeAbiParameters, decodeFunctionData } from 'viem';
import { universalRouterAbi, swapActionAbi } from '../../src/chain/abis.js';
import { poolIdOf } from '../../src/chain/pools.js';
import {
  encodeV4Swap,
  SETTLE_ALL,
  SWAP_EXACT_IN,
  SWAP_EXACT_IN_SINGLE,
  TAKE_ALL,
  V4_SWAP,
} from '../../src/chain/swap.js';
import type { Address, Pool } from '../../src/core/types.js';

/**
 * The fixed vector is built from the AAPL/USDG reference pool on chain 4663,
 * recorded in FACTS.md: currency0 USDG with six decimals, currency1 AAPL with
 * eighteen, the dynamic-fee flag 0x800000, tick spacing 10, and the hook at
 * 0x70a9a884. Selling 10 USDG for at least 0.03 AAPL with a deadline of
 * 1800000000 produces the calldata frozen below. Every field is also decoded
 * back out of that calldata, so a change to the layout fails the comparison and
 * says which field moved.
 */
const USDG: Address = '0x5fc5360d0400a0fd4f2af552add042d716f1d168';
const AAPL: Address = '0xaf3d76f1834a1d425780943c99ea8a608f8a93f9';
const HOOK: Address = '0x70a9a88402989226847ec122043ce5e7ff462080';
const MEME: Address = '0x00000000000000000000000000000000000dec0d';
const NO_HOOK: Address = '0x0000000000000000000000000000000000000000';
const ROUTER: Address = '0x8876789976dEcBfCbBbe364623C63652db8C0904';

const AMOUNT_IN = 10_000_000n;
const MIN_OUT = 30_000_000_000_000_000n;
const DEADLINE = 1_800_000_000n;

const aaplUsdg: Pool = {
  poolId: poolIdOf({
    currency0: USDG,
    currency1: AAPL,
    fee: 0x800000,
    tickSpacing: 10,
    hooks: HOOK,
  }),
  currency0: USDG,
  currency1: AAPL,
  decimals0: 6,
  decimals1: 18,
  fee: 0x800000,
  tickSpacing: 10,
  hooks: HOOK,
  initialBlock: 41_258_956n,
};

const memeAapl: Pool = {
  poolId: poolIdOf({
    currency0: MEME,
    currency1: AAPL,
    fee: 10_000,
    tickSpacing: 200,
    hooks: NO_HOOK,
  }),
  currency0: MEME,
  currency1: AAPL,
  decimals0: 18,
  decimals1: 18,
  fee: 10_000,
  tickSpacing: 200,
  hooks: NO_HOOK,
  initialBlock: 42_000_000n,
};

const MULTI_HOP_CALLDATA =
  '0x3593564c000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000011000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000460000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000003070c0f00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000000000000000000000000000000000000360000000000000000000000000000000000000000000000000000000000000028000000000000000000000000000000000000000000000000000000000000000200000000000000000000000005fc5360d0400a0fd4f2af552add042d716f1d168000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000009896800000000000000000000000000000000000000000000000000de0b6b3a7640000000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000100000000000000000000000000af3d76f1834a1d425780943c99ea8a608f8a93f90000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000070a9a88402989226847ec122043ce5e7ff46208000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000dec0d000000000000000000000000000000000000000000000000000000000000271000000000000000000000000000000000000000000000000000000000000000c8000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000400000000000000000000000005fc5360d0400a0fd4f2af552add042d716f1d1680000000000000000000000000000000000000000000000000000000000989680000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000dec0d0000000000000000000000000000000000000000000000000de0b6b3a7640000';

describe('poolIdOf', () => {
  it('reproduces the AAPL/USDG pool id recorded in FACTS', () => {
    expect(aaplUsdg.poolId).toBe(
      '0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147',
    );
  });

  it('changes when any field of the key changes', () => {
    const withOtherSpacing = poolIdOf({
      currency0: USDG,
      currency1: AAPL,
      fee: 0x800000,
      tickSpacing: 60,
      hooks: HOOK,
    });
    expect(withOtherSpacing).not.toBe(aaplUsdg.poolId);
  });
});

describe('encodeV4Swap, one pool', () => {
  const encoded = encodeV4Swap(
    {
      hops: [{ pool: aaplUsdg, tokenIn: USDG, tokenOut: AAPL }],
      tokenIn: USDG,
      tokenOut: AAPL,
      amountIn: AMOUNT_IN,
      minAmountOut: MIN_OUT,
      deadline: DEADLINE,
    },
    ROUTER,
  );

  it('lays the single swap out the way the router on 4663 decodes it', () => {
    expect(encoded.to).toBe(ROUTER);
    expect(encoded.value).toBe(0n);
    // Mirrors a settled UniversalRouter trade on chain 4663: pool key (5 words),
    // zeroForOne, amountIn, amountOutMinimum, sqrtPriceLimitX96 = 0, then the
    // hookData offset 0x140 and an empty hookData.
    const [swapParams] = decodeAbiParameters(
      [{ type: 'bytes' }, { type: 'bytes[]' }],
      encoded.inputs[0] as `0x${string}`,
    )[1];
    const words = (swapParams as string).slice(2).match(/.{64}/g) ?? [];
    expect(words).toHaveLength(12);
    expect(BigInt(`0x${words[0]}`)).toBe(0x20n);
    expect(BigInt(`0x${words[6]}`)).toBe(1n);
    expect(BigInt(`0x${words[7]}`)).toBe(AMOUNT_IN);
    expect(BigInt(`0x${words[8]}`)).toBe(MIN_OUT);
    expect(BigInt(`0x${words[9]}`)).toBe(0n);
    expect(BigInt(`0x${words[10]}`)).toBe(0x140n);
    expect(BigInt(`0x${words[11]}`)).toBe(0n);
  });

  it('sends one command, V4_SWAP', () => {
    expect(encoded.commands).toBe('0x10');
    expect(Number.parseInt(encoded.commands.slice(2), 16)).toBe(V4_SWAP);
    expect(encoded.inputs).toHaveLength(1);
  });

  it('sends the swap, then the payment, then the collection', () => {
    expect(encoded.actions).toBe('0x060c0f');
    expect([...Buffer.from(encoded.actions.slice(2), 'hex')]).toEqual([
      SWAP_EXACT_IN_SINGLE,
      SETTLE_ALL,
      TAKE_ALL,
    ]);
  });

  it('decodes back to the pool key, the direction, and both bounds', () => {
    const { functionName, args } = decodeFunctionData({
      abi: universalRouterAbi,
      data: encoded.data,
    });
    expect(functionName).toBe('execute');
    const [commands, inputs, deadline] = args as [`0x${string}`, `0x${string}`[], bigint];
    expect(commands).toBe(encoded.commands);
    expect(deadline).toBe(DEADLINE);
    const [actions, params] = decodeAbiParameters(
      [{ type: 'bytes' }, { type: 'bytes[]' }],
      inputs[0] as `0x${string}`,
    ) as [`0x${string}`, `0x${string}`[]];
    expect(actions).toBe(encoded.actions);
    expect(params).toHaveLength(3);

    const [swap] = decodeAbiParameters(
      swapActionAbi.exactInputSingle,
      params[0] as `0x${string}`,
    ) as [
      {
        poolKey: {
          currency0: Address;
          currency1: Address;
          fee: number;
          tickSpacing: number;
          hooks: Address;
        };
        zeroForOne: boolean;
        amountIn: bigint;
        amountOutMinimum: bigint;
        sqrtPriceLimitX96: bigint;
        hookData: `0x${string}`;
      },
    ];
    expect(swap.poolKey.currency0.toLowerCase()).toBe(USDG);
    expect(swap.poolKey.currency1.toLowerCase()).toBe(AAPL);
    expect(swap.poolKey.fee).toBe(0x800000);
    expect(swap.poolKey.tickSpacing).toBe(10);
    expect(swap.poolKey.hooks.toLowerCase()).toBe(HOOK);
    // USDG is currency0, so selling USDG is a zero-for-one swap.
    expect(swap.zeroForOne).toBe(true);
    expect(swap.amountIn).toBe(AMOUNT_IN);
    expect(swap.amountOutMinimum).toBe(MIN_OUT);
    expect(swap.hookData).toBe('0x');

    const settle = decodeAbiParameters(
      swapActionAbi.currencyAndAmount,
      params[1] as `0x${string}`,
    ) as [Address, bigint];
    expect(settle[0].toLowerCase()).toBe(USDG);
    expect(settle[1]).toBe(AMOUNT_IN);

    const take = decodeAbiParameters(
      swapActionAbi.currencyAndAmount,
      params[2] as `0x${string}`,
    ) as [Address, bigint];
    expect(take[0].toLowerCase()).toBe(AAPL);
    expect(take[1]).toBe(MIN_OUT);
  });

  it('sells the other side of the pool as a one-for-zero swap', () => {
    const reverse = encodeV4Swap(
      {
        hops: [{ pool: aaplUsdg, tokenIn: AAPL, tokenOut: USDG }],
        tokenIn: AAPL,
        tokenOut: USDG,
        amountIn: 10n ** 18n,
        minAmountOut: 300_000_000n,
        deadline: DEADLINE,
      },
      ROUTER,
    );
    const [, inputs] = decodeFunctionData({ abi: universalRouterAbi, data: reverse.data }).args as [
      `0x${string}`,
      `0x${string}`[],
      bigint,
    ];
    const [, params] = decodeAbiParameters(
      [{ type: 'bytes' }, { type: 'bytes[]' }],
      inputs[0] as `0x${string}`,
    ) as [`0x${string}`, `0x${string}`[]];
    const [swap] = decodeAbiParameters(
      swapActionAbi.exactInputSingle,
      params[0] as `0x${string}`,
    ) as unknown as [{ zeroForOne: boolean }];
    expect(swap.zeroForOne).toBe(false);
  });
});

describe('encodeV4Swap, two pools', () => {
  const encoded = encodeV4Swap(
    {
      hops: [
        { pool: aaplUsdg, tokenIn: USDG, tokenOut: AAPL },
        { pool: memeAapl, tokenIn: AAPL, tokenOut: MEME },
      ],
      tokenIn: USDG,
      tokenOut: MEME,
      amountIn: AMOUNT_IN,
      minAmountOut: 10n ** 18n,
      deadline: DEADLINE,
    },
    ROUTER,
  );

  it('matches the frozen calldata', () => {
    expect(encoded.data).toBe(MULTI_HOP_CALLDATA);
  });

  it('switches the swap action and keeps the settle and take actions', () => {
    expect(encoded.actions).toBe('0x070c0f');
    expect([...Buffer.from(encoded.actions.slice(2), 'hex')]).toEqual([
      SWAP_EXACT_IN,
      SETTLE_ALL,
      TAKE_ALL,
    ]);
  });

  it('writes one path entry per hop, each naming the currency it lands in', () => {
    const [, inputs] = decodeFunctionData({ abi: universalRouterAbi, data: encoded.data }).args as [
      `0x${string}`,
      `0x${string}`[],
      bigint,
    ];
    const [, params] = decodeAbiParameters(
      [{ type: 'bytes' }, { type: 'bytes[]' }],
      inputs[0] as `0x${string}`,
    ) as [`0x${string}`, `0x${string}`[]];
    const [swap] = decodeAbiParameters(
      swapActionAbi.exactInput,
      params[0] as `0x${string}`,
    ) as unknown as [
      {
        currencyIn: Address;
        path: { intermediateCurrency: Address; fee: number; tickSpacing: number; hooks: Address }[];
        amountIn: bigint;
        amountOutMinimum: bigint;
      },
    ];
    expect(swap.currencyIn.toLowerCase()).toBe(USDG);
    expect(swap.path).toHaveLength(2);
    expect(swap.path[0]?.intermediateCurrency.toLowerCase()).toBe(AAPL);
    expect(swap.path[0]?.fee).toBe(0x800000);
    expect(swap.path[0]?.tickSpacing).toBe(10);
    expect(swap.path[1]?.intermediateCurrency.toLowerCase()).toBe(MEME);
    expect(swap.path[1]?.fee).toBe(10_000);
    expect(swap.path[1]?.tickSpacing).toBe(200);
    expect(swap.amountIn).toBe(AMOUNT_IN);
    expect(swap.amountOutMinimum).toBe(10n ** 18n);
  });
});

describe('encodeV4Swap, native ether in', () => {
  it('carries the amount as call value', () => {
    const nativePool: Pool = { ...aaplUsdg, currency0: NO_HOOK, decimals0: 18 };
    const encoded = encodeV4Swap(
      {
        hops: [{ pool: nativePool, tokenIn: NO_HOOK, tokenOut: AAPL }],
        tokenIn: NO_HOOK,
        tokenOut: AAPL,
        amountIn: 10n ** 17n,
        minAmountOut: 1n,
        deadline: DEADLINE,
      },
      ROUTER,
    );
    expect(encoded.value).toBe(10n ** 17n);
  });

  it('refuses a route with no hops', () => {
    expect(() =>
      encodeV4Swap(
        {
          hops: [],
          tokenIn: USDG,
          tokenOut: AAPL,
          amountIn: 1n,
          minAmountOut: 0n,
          deadline: DEADLINE,
        },
        ROUTER,
      ),
    ).toThrow(/at least one hop/);
  });
});
