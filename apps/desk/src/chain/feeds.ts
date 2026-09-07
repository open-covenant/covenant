/**
 * Chainlink reads on chain 4663.
 *
 * Equity feeds publish eight decimals, update on a 0.5% move, and follow the
 * United States equities calendar. Outside that calendar they hold the last
 * published price with no heartbeat, so `updatedAt` is the field that decides
 * whether an answer is current. This module reports the price and the time it
 * was published; deciding whether that is still a fair reference belongs to
 * the fairvalue module.
 */

import type { Logger } from '../core/logger.js';
import { NotFoundError, UpstreamError } from '../core/errors.js';
import { quantity, type Address, type Quantity } from '../core/types.js';
import { aggregatorV3Abi } from './abis.js';
import type { ContractCall, DeskChainClient } from './client.js';
import type { DeskRegistry } from './registry.js';

/** One published round. */
export interface FeedRound {
  /** United States dollars per share, the multiplier already applied by the feed. */
  readonly price: Quantity;
  /** Publication time, milliseconds since the Unix epoch. */
  readonly updatedAt: number;
  readonly decimals: number;
  readonly roundId: bigint;
}

/** Chainlink feed reads. */
export interface DeskFeeds {
  latestRoundData(feed: Address): Promise<FeedRound>;
  latestRoundDataBatch(feeds: readonly Address[]): Promise<Map<Address, FeedRound>>;
  ethUsd(): Promise<FeedRound>;
  usdgUsd(): Promise<FeedRound>;
  /** The feed for a symbol, or undefined when 4663 publishes none. */
  forSymbol(symbol: string): Address | undefined;
}

export interface FeedsDeps {
  readonly client: DeskChainClient;
  readonly logger: Logger;
  readonly registry: DeskRegistry;
}

type RoundTuple = readonly [bigint, bigint, bigint, bigint, bigint];

export function createFeeds(deps: FeedsDeps): DeskFeeds {
  const logger = deps.logger.child({ component: 'chain.feeds' });
  const decimalsCache = new Map<string, number>();

  const seedDecimals = (): void => {
    for (const feed of deps.registry.feeds())
      decimalsCache.set(feed.address.toLowerCase(), feed.decimals);
  };
  seedDecimals();

  const decimalsFor = async (feeds: readonly Address[]): Promise<Map<string, number>> => {
    const missing = feeds.filter((feed) => !decimalsCache.has(feed.toLowerCase()));
    if (missing.length > 0) {
      const calls: ContractCall[] = missing.map((address) => ({
        address,
        abi: aggregatorV3Abi,
        functionName: 'decimals',
      }));
      const results = await deps.client.multicallAllowFailure<number>(calls);
      missing.forEach((address, index) => {
        const outcome = results[index];
        decimalsCache.set(
          address.toLowerCase(),
          outcome?.status === 'success' ? Number(outcome.result) : 8,
        );
      });
    }
    const out = new Map<string, number>();
    for (const feed of feeds)
      out.set(feed.toLowerCase(), decimalsCache.get(feed.toLowerCase()) ?? 8);
    return out;
  };

  const batch = async (feeds: readonly Address[]): Promise<Map<Address, FeedRound>> => {
    if (feeds.length === 0) return new Map();
    const decimals = await decimalsFor(feeds);
    const calls: ContractCall[] = feeds.map((address) => ({
      address,
      abi: aggregatorV3Abi,
      functionName: 'latestRoundData',
    }));
    const results = await deps.client.multicallAllowFailure<RoundTuple>(calls);
    const out = new Map<Address, FeedRound>();
    feeds.forEach((address, index) => {
      const outcome = results[index];
      if (outcome?.status !== 'success') {
        logger.warn('feed did not answer', { feed: address });
        return;
      }
      out.set(address, toRound(outcome.result, decimals.get(address.toLowerCase()) ?? 8));
    });
    return out;
  };

  const single = async (feed: Address): Promise<FeedRound> => {
    const rounds = await batch([feed]);
    const round = rounds.get(feed);
    if (!round)
      throw new UpstreamError('chainlink', `feed ${feed} did not answer latestRoundData`, { feed });
    return round;
  };

  const named = async (symbol: string, feed: Address | undefined): Promise<FeedRound> => {
    if (!feed) throw new NotFoundError(`the ${symbol}/USD feed on chain 4663`, { symbol });
    return single(feed);
  };

  return {
    latestRoundData: single,
    latestRoundDataBatch: batch,
    ethUsd: () => named('ETH', deps.registry.ethUsdFeed()),
    usdgUsd: () => named('USDG', deps.registry.usdgUsdFeed()),
    forSymbol: (symbol) => deps.registry.feedFor(symbol),
  };
}

/** Turn a raw round into a dollar price with its publication time. */
export function toRound(tuple: RoundTuple, decimals: number): FeedRound {
  const [roundId, answer, , updatedAtSec] = tuple;
  const updatedAt = Number(updatedAtSec) * 1000;
  const price = Number(answer) / 10 ** decimals;
  return {
    price: quantity(price, 'USD', 'chainlink', updatedAt),
    updatedAt,
    decimals,
    roundId,
  };
}
