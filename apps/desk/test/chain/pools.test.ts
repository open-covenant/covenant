import { describe, expect, it } from 'vitest';
import { silentLogger } from '../../src/core/logger.js';
import { openStore } from '../../src/core/store.js';
import { defaultConfig } from '../../src/core/config.js';
import { createRegistry } from '../../src/chain/registry.js';
import { createPools, poolIdOf, readCursor, writeCursor } from '../../src/chain/pools.js';
import type { CallOutcome, ContractCall, DeskChainClient } from '../../src/chain/client.js';
import type { Address, Hex32, Pool } from '../../src/core/types.js';

const USDG: Address = '0x5fc5360d0400a0fd4f2af552add042d716f1d168';
const AAPL: Address = '0xaf3d76f1834a1d425780943c99ea8a608f8a93f9';
const HOOK: Address = '0x70a9a88402989226847ec122043ce5e7ff462080';
const STATE_VIEW: Address = '0xf3334192d15450cdd385c8b70e03f9a6bd9e673b';

/** Live reading of the AAPL/USDG reference pool at block 54,444,943. */
const SQRT_PRICE = 4417497546894993845498601652811337n;
const LIQUIDITY = 635708552491308211n;

const referencePool: Pool = {
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

/** A client that answers `getSlot0` and `getLiquidity` from a fixed table. */
function stubClient(
  states: Map<string, { sqrtPriceX96: bigint; tick: number; lpFee: number; liquidity: bigint }>,
) {
  return {
    chainId: 4663,
    blockNumber: async () => 54_444_943n,
    multicall: async () => [],
    async multicallAllowFailure<T>(calls: readonly ContractCall[]): Promise<CallOutcome<T>[]> {
      return calls.map((call) => {
        const poolId = String(call.args?.[0] ?? '').toLowerCase();
        const state = states.get(poolId);
        if (!state) return { status: 'failure' as const };
        const result =
          call.functionName === 'getSlot0'
            ? ([state.sqrtPriceX96, state.tick, 0, state.lpFee] as const)
            : state.liquidity;
        return { status: 'success' as const, result: result as T };
      });
    },
    request: async () => {
      throw new Error('not used');
    },
    publicClient: undefined as never,
    account: () => undefined,
    walletClient: () => undefined,
  } satisfies DeskChainClient;
}

function poolsFor(
  states: Map<string, { sqrtPriceX96: bigint; tick: number; lpFee: number; liquidity: bigint }>,
) {
  const store = openStore({ path: ':memory:' });
  const logger = silentLogger();
  const config = defaultConfig();
  const registry = createRegistry({
    config,
    logger,
    store,
    fetchImpl: (() => {}) as unknown as typeof fetch,
  });
  const client = stubClient(states);
  const tokens = {
    read: async () => [],
    balanceOf: async () => {
      throw new Error('not used');
    },
    balanceOfUi: async () => {
      throw new Error('not used');
    },
    decimalsOf: async () => new Map<string, number>(),
  };
  return {
    store,
    pools: createPools({ client, logger, store, registry, tokens, stateViewAddress: STATE_VIEW }),
  };
}

describe('pool state', () => {
  it('prices the reference pool from slot0 and marks it routable', async () => {
    const states = new Map([
      [
        referencePool.poolId,
        { sqrtPriceX96: SQRT_PRICE, tick: 218_585, lpFee: 0, liquidity: LIQUIDITY },
      ],
    ]);
    const { store, pools } = poolsFor(states);
    store.upsertPool(referencePool);

    const [refreshed] = await pools.state([referencePool.poolId]);
    expect(refreshed?.sqrtPriceX96).toBe(SQRT_PRICE);
    expect(refreshed?.liquidity).toBe(LIQUIDITY);
    expect(refreshed?.lpFee).toBe(0);
    expect(refreshed?.trap).toBe(false);
    // currency0 is USDG, so the mid is AAPL per USDG.
    expect(refreshed?.midPrice?.value).toBeCloseTo(0.003108804891087395, 15);
    expect(refreshed?.midPrice?.source).toBe('pool');
    expect(refreshed?.midPrice?.blockNumber).toBe(54_444_943n);

    const stored = store.getPool(referencePool.poolId);
    expect(stored?.liquidity).toBe(LIQUIDITY);
    expect(stored?.sqrtPriceX96).toBe(SQRT_PRICE);
  });

  it('flags a pool charging more than 300 basis points', async () => {
    const trapKey = {
      currency0: USDG,
      currency1: AAPL,
      fee: 0x800000,
      tickSpacing: 60,
      hooks: HOOK,
    };
    const trap: Pool = { ...referencePool, poolId: poolIdOf(trapKey), tickSpacing: 60 };
    const states = new Map([
      // 900,000 hundredths of a basis point is 90%, the worst of the USDG traps.
      [
        trap.poolId,
        { sqrtPriceX96: SQRT_PRICE, tick: 218_585, lpFee: 900_000, liquidity: LIQUIDITY },
      ],
    ]);
    const { store, pools } = poolsFor(states);
    store.upsertPool(trap);

    const [refreshed] = await pools.state([trap.poolId]);
    expect(refreshed?.trap).toBe(true);
    expect(pools.isTrap(refreshed as Pool)).toBe(true);
    // The store leaves traps out of routing lists unless they are asked for.
    expect(store.listPools({ currency: AAPL })).toHaveLength(0);
    expect(store.listPools({ currency: AAPL, excludeTraps: false })).toHaveLength(1);
  });

  it('keeps the stored row when a pool does not answer', async () => {
    const { store, pools } = poolsFor(new Map());
    store.upsertPool(referencePool);
    const [refreshed] = await pools.state([referencePool.poolId]);
    expect(refreshed?.poolId).toBe(referencePool.poolId);
    expect(refreshed?.sqrtPriceX96).toBeUndefined();
  });

  it('refuses to price a pool it has never read', () => {
    const { pools } = poolsFor(new Map());
    expect(() => pools.midPrice(referencePool)).toThrow(/no price yet/);
  });

  it('ranks pools by liquidity and leaves traps out', async () => {
    const deep: Pool = {
      ...referencePool,
      poolId: poolIdOf({ ...referencePool, tickSpacing: 1 }),
      tickSpacing: 1,
    };
    const shallow: Pool = {
      ...referencePool,
      poolId: poolIdOf({ ...referencePool, tickSpacing: 2 }),
      tickSpacing: 2,
    };
    const trap: Pool = {
      ...referencePool,
      poolId: poolIdOf({ ...referencePool, tickSpacing: 3 }),
      tickSpacing: 3,
    };
    const states = new Map([
      [deep.poolId, { sqrtPriceX96: SQRT_PRICE, tick: 0, lpFee: 3000, liquidity: 5_000n }],
      [shallow.poolId, { sqrtPriceX96: SQRT_PRICE, tick: 0, lpFee: 3000, liquidity: 10n }],
      [trap.poolId, { sqrtPriceX96: SQRT_PRICE, tick: 0, lpFee: 650_000, liquidity: 9_000_000n }],
    ]);
    const { store, pools } = poolsFor(states);
    for (const pool of [deep, shallow, trap]) store.upsertPool(pool);

    const ranked = await pools.forToken(AAPL);
    expect(ranked.map((pool) => pool.poolId)).toEqual([deep.poolId, shallow.poolId]);
    const withTraps = await pools.forToken(AAPL, { includeTraps: true });
    expect(withTraps[0]?.poolId).toBe(trap.poolId);
  });
});

describe('scan cursor', () => {
  it('remembers where the last scan reached', () => {
    const store = openStore({ path: ':memory:' });
    expect(readCursor(store)).toBeUndefined();
    writeCursor(store, 54_000_000n);
    expect(readCursor(store)).toBe(54_000_000n);
    writeCursor(store, 54_444_943n);
    expect(readCursor(store)).toBe(54_444_943n);
  });
});

describe('pool ids', () => {
  it('hashes the key the same way the PoolManager does', () => {
    const id: Hex32 = poolIdOf({
      currency0: USDG,
      currency1: AAPL,
      fee: 0x800000,
      tickSpacing: 10,
      hooks: HOOK,
    });
    expect(id).toBe('0xa2347ba69167e5602f74640ffbf737ee7cdd825e4726d3462564fc6533070147');
  });
});

describe('forToken', () => {
  it('reads only the pools that hold the token against a named currency', async () => {
    const meme: Address = '0x25e27b4824bcf9ef0e89fa99af184cfdbf265504';
    const usdgPool: Pool = { ...referencePool, initialBlock: 41_258_956n };
    const memePool: Pool = {
      ...referencePool,
      poolId: poolIdOf({ currency0: AAPL, currency1: meme, fee: 3000, tickSpacing: 60, hooks: HOOK }),
      currency0: AAPL,
      currency1: meme,
      decimals0: 18,
      decimals1: 18,
      fee: 3000,
      tickSpacing: 60,
      initialBlock: 54_000_000n,
    };
    const states = new Map([
      [usdgPool.poolId, { sqrtPriceX96: SQRT_PRICE, tick: 218_585, lpFee: 0, liquidity: LIQUIDITY }],
      [memePool.poolId, { sqrtPriceX96: SQRT_PRICE, tick: 218_585, lpFee: 3000, liquidity: LIQUIDITY * 1000n }],
    ]);
    const { store, pools } = poolsFor(states);
    store.upsertPool(usdgPool);
    store.upsertPool(memePool);

    const everything = await pools.forToken(AAPL, { limit: 1 });
    // The memecoin pool is a thousand times deeper, so an unfiltered lookup
    // returns it and the USDG pool never comes into view.
    expect(everything.map((pool) => pool.poolId)).toEqual([memePool.poolId]);

    const quoted = await pools.forToken(AAPL, { limit: 1, counterparties: [USDG] });
    expect(quoted.map((pool) => pool.poolId)).toEqual([usdgPool.poolId]);
  });

  it('returns nothing when the token has no pool against the named currency', async () => {
    const { store, pools } = poolsFor(new Map());
    store.upsertPool(referencePool);
    expect(await pools.forToken(AAPL, { counterparties: ['0x'.padEnd(42, '9') as Address] })).toEqual([]);
  });
});
