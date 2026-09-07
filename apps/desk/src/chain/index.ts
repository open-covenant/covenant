/**
 * Chain access for Robinhood Chain (chain id 4663).
 *
 * Every read returns a `Quantity` carrying its unit, its source, and the block
 * it was read at. Addresses below were checked live on 2026-09-04 and are
 * recorded in `FACTS.md`.
 *
 * Nothing in this module signs anything unless the desk is live and the caller
 * asked for it. Quoting, pool discovery, and state reads are all `eth_call` and
 * `eth_getLogs`.
 */

import type { Config } from '../core/config.js';
import type { Keystore } from '../core/keystore.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { Address } from '../core/types.js';
import { createChainClient, type DeskChainClient } from './client.js';
import { createFeeds, type DeskFeeds } from './feeds.js';
import { createPools, TRAP_LP_FEE_BPS as TRAP_FEE_BPS, type DeskPools } from './pools.js';
import { createQuoter, type DeskQuoter } from './quoter.js';
import { createRegistry, type DeskRegistry } from './registry.js';
import { createSwap, type DeskSwap } from './swap.js';
import { createTokenReader, type DeskTokenReader } from './tokens.js';

/** Contracts on chain 4663. */
export const CONTRACTS = {
  poolManager: '0x8366a39cc670b4001a1121b8f6a443a643e40951',
  positionManager: '0x58daec3116aae6d93017baaea7749052e8a04fa7',
  v4Quoter: '0x8dc178efb8111bb0973dd9d722ebeff267c98f94',
  stateView: '0xf3334192d15450cdd385c8b70e03f9a6bd9e673b',
  universalRouter: '0x8876789976dEcBfCbBbe364623C63652db8C0904',
  permit2: '0x000000000022D473030F116dDEE9F6B43aC78BA3',
  multicall3: '0xcA11bde05977b3631167028862bE2a173976CA11',
  usdg: '0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168',
  weth9: '0x0Bd7D308f8E1639FAb988df18A8011f41EAcAD73',
} as const satisfies Record<string, Address>;

/** USDG carries six decimals. Read from the token on 2026-09-04. */
export const USDG_DECIMALS = 6;

/**
 * `eth_getLogs` on 4663 accepts any block range and refuses above this many
 * matching logs. It also refuses a query that runs too long. Both are answered
 * by halving the range.
 */
export const MAX_LOGS_PER_QUERY = 10_000;

/** Pools charging more than this are refused for routing, in basis points. */
export const TRAP_LP_FEE_BPS = TRAP_FEE_BPS;

export { INITIALIZE_TOPIC0 } from './pools.js';

export type { BlockRange } from './logs.js';
export type { FeedEntry, LighterMarketSummary } from './registry.js';
export type { PoolKey, PoolDiscoveryOptions } from './pools.js';
export type { RouteHop } from './quoter.js';
export type { EncodeInput, EncodedSwap } from './swap.js';

export {
  chunkRange,
  splitRange,
  isLogLimitError,
  isRangeTooWideError,
  isRateLimitError,
} from './logs.js';
export { currentFeeBps, effectivePriceOf, isDynamicFee, sqrtPriceX96ToPrice } from './price.js';
export { poolIdOf, poolKeyOf } from './pools.js';
export { readCursor as readPoolScanCursor, writeCursor as writePoolScanCursor } from './pools.js';
export { encodeV4Swap } from './swap.js';

/** Read-only chain access. The write path lives in {@link Swap}. */
export type ChainClient = DeskChainClient;
/** Merged token registry: assets API, Chainlink feed list, Lighter markets. */
export type Registry = DeskRegistry;
/** ERC-20 plus the ERC-8056 fields a Robinhood stock token carries. */
export type StockTokenReader = DeskTokenReader;
/** Chainlink feed reads on 4663. */
export type Feeds = DeskFeeds;
/** One Chainlink round. */
export type { FeedRound } from './feeds.js';
/** Uniswap v4 pool discovery and state. */
export type Pools = DeskPools;
/** `V4Quoter` reads through `simulateContract`. No state is changed. */
export type Quoter = DeskQuoter;
export type { QuoteRequest, QuoteResult } from './quoter.js';
/** Swap execution through the UniversalRouter. */
export type Swap = DeskSwap;
export type { SwapRequest, SwapResult } from './swap.js';

export interface ChainDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  /**
   * Keys held in memory. Reads work without one; signing does not, so a desk
   * with no key stays in dry run and says so.
   */
  readonly keystore?: Keystore;
  /** Overrides for tests. Defaults to the live endpoints. */
  readonly fetchImpl?: typeof fetch;
}

/** Everything the chain module hands to the rest of the desk. */
export interface ChainModule {
  readonly client: ChainClient;
  readonly registry: Registry;
  readonly tokens: StockTokenReader;
  readonly feeds: Feeds;
  readonly pools: Pools;
  readonly quoter: Quoter;
  readonly swap: Swap;
}

/** Wire the chain module. Nothing here touches the network until it is called. */
export function createChainModule(deps: ChainDeps): ChainModule {
  const logger = deps.logger.child({ component: 'chain' });
  const privateKey = deps.keystore?.get('DESK_EVM_PRIVATE_KEY');
  const client = createChainClient(deps.config, logger, privateKey);

  const registry = createRegistry({
    config: deps.config,
    logger,
    store: deps.store,
    fetchImpl: deps.fetchImpl,
  });

  const tokens = createTokenReader({ client, logger, store: deps.store, registry });
  const feeds = createFeeds({ client, logger, registry });
  const pools = createPools({
    client,
    logger,
    store: deps.store,
    registry,
    tokens,
    stateViewAddress: CONTRACTS.stateView,
  });
  const quoter = createQuoter({
    client,
    logger,
    store: deps.store,
    pools,
    quoterAddress: CONTRACTS.v4Quoter,
    intermediates: [CONTRACTS.usdg, CONTRACTS.weth9],
  });
  const swap = createSwap({
    client,
    config: deps.config,
    logger,
    quoter,
    routerAddress: CONTRACTS.universalRouter,
    permit2Address: CONTRACTS.permit2,
  });

  return { client, registry, tokens, feeds, pools, quoter, swap };
}
