/** Test doubles for the chain module and the two off-chain price sources. */

import type { Address, Hex32, Pool, Quantity, Token } from '../../src/core/types.js';
import { quantity } from '../../src/core/types.js';
import type { ChainModule, FeedRound, Pools, Quoter, Registry, Feeds } from '../../src/chain/index.js';
import { NotImplementedError } from '../../src/core/errors.js';
import type { LighterMark, LighterMarkSource, RhjPriceSource, RhjQuote } from '../../src/fairvalue/sources.js';
import { orientMid, sameAddress } from '../../src/fairvalue/pools.js';

export const USDG = '0x5fc5360d0400a0fd4f2af552add042d716f1d168' as Address;
export const WETH = '0x0bd7d308f8e1639fab988df18a8011f41eacad73' as Address;
export const NVDA = '0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec' as Address;
export const AAPL = '0xaf3d76f1834a1d425780943c99ea8a608f8a93f9' as Address;
export const AI = '0x00000000000000000000000000000000000000a1' as Address;

export interface PoolInput {
  poolId: string;
  currency0: Address;
  currency1: Address;
  /** Price of currency0 in currency1, decimals applied. */
  mid: number;
  liquidity?: bigint;
  trap?: boolean;
  decimals0?: number;
  decimals1?: number;
}

export function makePool(input: PoolInput): Pool {
  return {
    poolId: input.poolId as Hex32,
    currency0: input.currency0,
    currency1: input.currency1,
    decimals0: input.decimals0 ?? 18,
    decimals1: input.decimals1 ?? 18,
    fee: 3000,
    tickSpacing: 60,
    hooks: '0x0000000000000000000000000000000000000000' as Address,
    initialBlock: 1n,
    lpFee: input.trap ? 9000 : 3000,
    liquidity: input.liquidity ?? 1_000_000n,
    sqrtPriceX96: 1n,
    midPrice: quantity(input.mid, 'token', 'pool', 1_700_000_000_000),
    ...(input.trap ? { trap: true } : {}),
  };
}

export function makeToken(input: Partial<Token> & { address: Address; symbol: string }): Token {
  return {
    name: input.symbol,
    decimals: 18,
    isStockToken: true,
    ...input,
  } as Token;
}

export interface ChainStub {
  registry?: Partial<Registry>;
  feeds?: Partial<Feeds>;
  pools?: Partial<Pools>;
  quoter?: Partial<Quoter>;
}

/** A chain module where anything the test did not supply refuses to answer. */
export function makeChain(stub: ChainStub = {}): ChainModule {
  const missing = (what: string) => () => {
    throw new NotImplementedError(`chain.${what}`);
  };

  return {
    client: { chainId: 4663, blockNumber: missing('client.blockNumber'), multicall: missing('client.multicall') },
    registry: {
      refresh: missing('registry.refresh'),
      stockTokens: () => [],
      token: () => undefined,
      feedFor: () => undefined,
      lighterPerpFor: () => undefined,
      isStockToken: () => false,
      lastRefreshedAt: () => undefined,
      ...stub.registry,
    },
    tokens: {
      read: missing('tokens.read'),
      balanceOf: missing('tokens.balanceOf'),
      balanceOfUi: missing('tokens.balanceOfUi'),
    },
    feeds: {
      latestRoundData: missing('feeds.latestRoundData'),
      latestRoundDataBatch: missing('feeds.latestRoundDataBatch'),
      ethUsd: missing('feeds.ethUsd'),
      usdgUsd: async () => makeRound(1, 1_700_000_000_000),
      ...stub.feeds,
    },
    pools: {
      discover: missing('pools.discover'),
      state: async () => [],
      midPrice: (pool: Pool) => pool.midPrice ?? quantity(1, 'token', 'pool', 0),
      forToken: async () => [],
      isTrap: (pool: Pool) => pool.trap === true,
      ...stub.pools,
    },
    quoter: {
      quoteExactInputSingle: missing('quoter.quoteExactInputSingle'),
      quoteExactInput: missing('quoter.quoteExactInput'),
      ...stub.quoter,
    },
    swap: {
      encode: missing('swap.encode'),
      ensureApprovals: missing('swap.ensureApprovals'),
      execute: missing('swap.execute'),
    },
  } as unknown as ChainModule;
}

export function makeRound(price: number, updatedAt: number): FeedRound {
  return {
    price: quantity(price, 'USD', 'chainlink', updatedAt),
    updatedAt,
    decimals: 8,
    roundId: 1n,
  };
}

export function rhjSource(quotes: Record<string, RhjQuote | undefined>): RhjPriceSource {
  return { quote: async (symbol) => quotes[symbol.toUpperCase()] };
}

export function lighterSource(marks: Record<string, LighterMark | undefined>): LighterMarkSource {
  return {
    mark: async (symbol) => marks[symbol.toUpperCase()],
    marks: async () =>
      new Map(Object.entries(marks).filter((entry): entry is [string, LighterMark] => entry[1] !== undefined)),
  };
}

/** Pools keyed by the token they quote, for `pools.forToken`. */
export function poolsFor(pools: readonly Pool[]) {
  return async (token: Address) =>
    pools.filter((pool) => sameAddress(pool.currency0, token) || sameAddress(pool.currency1, token));
}

/** Units of `quote` one whole `base` buys, mirroring the production helper. */
export function midBetween(pool: Pool, base: Address, quote: Address): number {
  const mid: Quantity = pool.midPrice ?? quantity(1, 'token', 'pool', 0);
  return orientMid(pool, base, quote, mid.value);
}
