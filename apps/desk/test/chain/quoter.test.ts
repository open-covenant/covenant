import { describe, expect, it } from 'vitest';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import { createQuoter } from '../../src/chain/quoter.js';
import type { DeskPools } from '../../src/chain/pools.js';
import type { DeskChainClient } from '../../src/chain/client.js';
import { currentFeeBps } from '../../src/chain/price.js';
import type { Address, Hex32, Pool } from '../../src/core/types.js';

const USDG = '0x5fc5360d0400a0fd4f2af552add042d716f1d168' as Address;
const NVDA = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
const AI = '0x00000000000000000000000000000000000000a1' as Address;
const QUOTER = '0x8dc178efb8111bb0973dd9d722ebeff267c98f94' as Address;

const USDG_NVDA = `0x${'11'.repeat(32)}` as Hex32;
const NVDA_AI = `0x${'22'.repeat(32)}` as Hex32;
const TRAP = `0x${'33'.repeat(32)}` as Hex32;

function pool(poolId: Hex32, currency0: Address, currency1: Address, fee = 3000): Pool {
  return {
    poolId,
    currency0,
    currency1,
    decimals0: currency0 === USDG ? 6 : 18,
    decimals1: 18,
    fee,
    tickSpacing: 10,
    hooks: '0x0000000000000000000000000000000000000000',
    initialBlock: 1n,
    liquidity: 10n ** 18n,
  };
}

function quoter(store: Store) {
  const pools: DeskPools = {
    discover: async () => [],
    state: async () => [],
    midPrice: () => ({ value: 1, unit: 'token', source: 'pool', asOf: 0 }),
    forToken: async () => [],
    isTrap: (candidate) => {
      const bps = currentFeeBps(candidate);
      return bps !== undefined && bps > 300;
    },
    scanCursor: () => undefined,
  };
  const client = {
    chainId: 4663,
    blockNumber: async () => 54_444_943n,
  } as unknown as DeskChainClient;

  return createQuoter({
    client,
    logger: silentLogger(),
    store,
    pools,
    quoterAddress: QUOTER,
    intermediates: [USDG],
  });
}

function seeded(): Store {
  const store = openStore({ path: ':memory:' });
  store.upsertPool(pool(USDG_NVDA, USDG, NVDA));
  store.upsertPool(pool(NVDA_AI, NVDA, AI));
  store.upsertPool({ ...pool(TRAP, USDG, NVDA, 650_000), lpFee: 650_000, trap: true });
  return store;
}

describe('a route that was already chosen', () => {
  it('walks every hop and names the currency each one lands in', async () => {
    const store = seeded();
    const hops = await quoter(store).route(USDG, AI, undefined, [USDG_NVDA, NVDA_AI]);
    expect(hops.map((hop) => hop.pool.poolId)).toEqual([USDG_NVDA, NVDA_AI]);
    expect(hops.map((hop) => hop.tokenOut)).toEqual([NVDA, AI]);
    store.close();
  });

  it('refuses a route that does not end where the swap needs to', async () => {
    const store = seeded();
    await expect(quoter(store).route(USDG, AI, undefined, [USDG_NVDA])).rejects.toThrow(/route from/);
    store.close();
  });

  it('refuses a hop through a pool charging more than the routing limit', async () => {
    const store = seeded();
    await expect(quoter(store).route(USDG, NVDA, undefined, [TRAP])).rejects.toThrow(/routing limit/);
    store.close();
  });
});
