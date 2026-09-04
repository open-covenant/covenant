/**
 * Uniswap v4 pool discovery and state on chain 4663.
 *
 * Discovery reads `Initialize` logs from the PoolManager. Both currencies are
 * indexed, so a filter on the currency topic finds every pool holding a given
 * token without scanning the chain. Two passes are needed because a token can
 * sort either side of its counterpart: one filtering the currency0 topic and
 * one filtering currency1.
 *
 * State comes from `StateView`, which reads the PoolManager's transient storage
 * without going through the manager itself. `getSlot0` also carries the fee the
 * pool is charging right now, which is the only way to see through a
 * dynamic-fee pool whose key says nothing but "the hook decides".
 */

import { decodeAbiParameters, encodeAbiParameters, keccak256, pad } from 'viem';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import { UpstreamError } from '../core/errors.js';
import { quantity, type Address, type Hex32, type Pool, type Quantity } from '../core/types.js';
import { poolKeyAbi, stateViewAbi } from './abis.js';
import type { ContractCall, DeskChainClient } from './client.js';
import { chunkRange, isRangeTooWideError, splitRange, type BlockRange } from './logs.js';
import { currentFeeBps, sqrtPriceX96ToPrice } from './price.js';
import type { DeskRegistry } from './registry.js';
import type { DeskTokenReader } from './tokens.js';

/** `Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)`. */
export const INITIALIZE_TOPIC0: Hex32 =
  '0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438';

/** Pools charging more than this are refused for routing, in basis points. */
export const TRAP_LP_FEE_BPS = 300;

/** The zero address is native ether in a v4 pool key. */
export const NATIVE_CURRENCY: Address = '0x0000000000000000000000000000000000000000';

/** Where the scan cursor is kept, so a restart resumes instead of rescanning. */
const SCAN_SUBJECT = 'chain.pool-scan';

/** Attempts made on a range the node refused and that cannot be narrowed. */
const MAX_RANGE_RETRIES = 3;

/** The five fields of a v4 pool key. */
export interface PoolKey {
  readonly currency0: Address;
  readonly currency1: Address;
  readonly fee: number;
  readonly tickSpacing: number;
  readonly hooks: Address;
}

/** A pool id is the hash of its key, so a key can always be checked against one. */
export function poolIdOf(key: PoolKey): Hex32 {
  return keccak256(
    encodeAbiParameters(poolKeyAbi, [
      key.currency0,
      key.currency1,
      key.fee,
      key.tickSpacing,
      key.hooks,
    ]),
  );
}

/** The key a stored pool was created with. */
export function poolKeyOf(pool: Pool): PoolKey {
  return {
    currency0: pool.currency0,
    currency1: pool.currency1,
    fee: pool.fee,
    tickSpacing: pool.tickSpacing,
    hooks: pool.hooks,
  };
}

export interface PoolDiscoveryOptions {
  /** First block to scan. Defaults to the block after the last completed scan. */
  readonly fromBlock?: bigint;
  readonly toBlock?: bigint;
  /** Only pools holding one of these addresses. Defaults to every stock token. */
  readonly currencies?: readonly Address[];
  /** Blocks per query before the scan starts splitting. */
  readonly span?: bigint;
  /** Write what was found to the store. Default true. */
  readonly persist?: boolean;
  /** Read decimals for currencies the desk has not seen. Default true. */
  readonly resolveDecimals?: boolean;
  /** Called after each answered query, for progress output. */
  readonly onProgress?: (progress: { readonly range: BlockRange; readonly found: number }) => void;
}

export interface PoolStateOptions {
  /** Write refreshed state to the store. Default true. */
  readonly persist?: boolean;
}

/** Pool discovery, state, and pricing. */
export interface DeskPools {
  discover(options?: PoolDiscoveryOptions): Promise<Pool[]>;
  state(poolIds: readonly Hex32[], options?: PoolStateOptions): Promise<Pool[]>;
  midPrice(pool: Pool): Quantity;
  forToken(
    token: Address,
    options?: {
      includeTraps?: boolean;
      limit?: number;
      stateBudget?: number;
      /**
       * Only pools that hold the token against one of these currencies. A stock
       * token sits in thousands of pools, so a caller that wants the USDG pool
       * says so here instead of reading the whole book and discarding it.
       */
      counterparties?: readonly Address[];
    },
  ): Promise<Pool[]>;
  isTrap(pool: Pool): boolean;
  /** Block the last completed scan reached. */
  scanCursor(): bigint | undefined;
}

export interface PoolsDeps {
  readonly client: DeskChainClient;
  readonly logger: Logger;
  readonly store: Store;
  readonly registry: DeskRegistry;
  readonly tokens: DeskTokenReader;
  readonly stateViewAddress: Address;
}

export function createPools(deps: PoolsDeps): DeskPools {
  const logger = deps.logger.child({ component: 'chain.pools' });

  const midPrice = (pool: Pool): Quantity => {
    if (pool.sqrtPriceX96 === undefined) {
      throw new UpstreamError(
        'pool',
        `pool ${pool.poolId} has no price yet; read its state first`,
        {
          poolId: pool.poolId,
        },
      );
    }
    const value = sqrtPriceX96ToPrice(pool.sqrtPriceX96, pool.decimals0, pool.decimals1);
    return quantity(value, 'token', 'pool', Date.now());
  };

  const isTrap = (pool: Pool): boolean => {
    const bps = currentFeeBps(pool);
    return bps !== undefined && bps > TRAP_LP_FEE_BPS;
  };

  const readState = async (
    poolIds: readonly Hex32[],
    options: PoolStateOptions = {},
  ): Promise<Pool[]> => {
    if (poolIds.length === 0) return [];
    const calls: ContractCall[] = [];
    for (const poolId of poolIds) {
      calls.push({
        address: deps.stateViewAddress,
        abi: stateViewAbi,
        functionName: 'getSlot0',
        args: [poolId],
      });
      calls.push({
        address: deps.stateViewAddress,
        abi: stateViewAbi,
        functionName: 'getLiquidity',
        args: [poolId],
      });
    }
    const results = await deps.client.multicallAllowFailure<unknown>(calls);
    const blockNumber = await deps.client.blockNumber();

    const out: Pool[] = [];
    poolIds.forEach((poolId, index) => {
      const slot0 = results[index * 2];
      const liquidity = results[index * 2 + 1];
      const stored = deps.store.getPool(poolId);
      if (!stored) {
        logger.debug('pool state was requested for a pool the desk has not discovered', { poolId });
        return;
      }
      if (slot0?.status !== 'success') {
        logger.debug('pool did not answer getSlot0', { poolId });
        out.push(stored);
        return;
      }
      const [sqrtPriceX96, tick, , lpFee] = slot0.result as readonly [
        bigint,
        number,
        number,
        number,
      ];
      const next: Pool = {
        ...stored,
        sqrtPriceX96,
        tick: Number(tick),
        lpFee: Number(lpFee),
        liquidity:
          liquidity?.status === 'success' ? (liquidity.result as bigint) : stored.liquidity,
      };
      const priced: Pool = {
        ...next,
        trap: isTrap(next),
        midPrice: sqrtPriceX96 > 0n ? { ...midPrice(next), blockNumber } : undefined,
      };
      out.push(priced);
      if (options.persist !== false) deps.store.upsertPool(priced);
    });
    return out;
  };

  return {
    midPrice,
    isTrap,
    state: readState,
    scanCursor: () => readCursor(deps.store),

    async discover(options = {}) {
      const currencies = (options.currencies ?? deps.registry.stockTokenAddresses()).map(
        (address) => address.toLowerCase() as Address,
      );
      if (currencies.length === 0) {
        logger.warn('discovery was asked to scan with no currencies, nothing to look for');
        return [];
      }

      const tip = options.toBlock ?? (await deps.client.blockNumber());
      const cursor = readCursor(deps.store);
      const from = options.fromBlock ?? (cursor === undefined ? 0n : cursor + 1n);
      if (from > tip) return [];

      const topics = currencies.map((address) => pad(address, { size: 32 }) as Hex32);
      const found = new Map<Hex32, DiscoveredPool>();

      for (const position of [2, 3] as const) {
        for (const range of chunkRange(from, tip, options.span)) {
          await scanRange(deps.client, range, position, topics, (logs) => {
            for (const log of logs) {
              const decoded = decodeInitializeLog(log);
              if (decoded) found.set(decoded.poolId, decoded);
            }
            options.onProgress?.({ range, found: found.size });
          });
        }
      }

      const decimals =
        options.resolveDecimals === false
          ? new Map<string, number>()
          : await deps.tokens.decimalsOf(uniqueCurrencies(found.values()));

      const pools: Pool[] = [];
      for (const entry of found.values()) {
        const pool: Pool = {
          poolId: entry.poolId,
          currency0: entry.currency0,
          currency1: entry.currency1,
          decimals0: decimalsFor(entry.currency0, decimals),
          decimals1: decimalsFor(entry.currency1, decimals),
          fee: entry.fee,
          tickSpacing: entry.tickSpacing,
          hooks: entry.hooks,
          initialBlock: entry.blockNumber,
          sqrtPriceX96: entry.sqrtPriceX96,
          tick: entry.tick,
        };
        pools.push(pool);
        if (options.persist !== false) deps.store.upsertPool(pool);
      }

      if (
        options.persist !== false &&
        options.currencies === undefined &&
        options.fromBlock === undefined
      ) {
        writeCursor(deps.store, tip);
      }
      logger.info('pool discovery finished', {
        fromBlock: from,
        toBlock: tip,
        currencies: currencies.length,
        pools: pools.length,
      });
      return pools;
    },

    async forToken(token, forTokenOptions = {}) {
      const limit = forTokenOptions.limit ?? 20;
      const budget = forTokenOptions.stateBudget ?? DEFAULT_STATE_BUDGET;
      const counterparties = forTokenOptions.counterparties?.map(
        (address) => address.toLowerCase() as Address,
      );
      const candidates = candidatePools(
        deps.store,
        token.toLowerCase() as Address,
        budget,
        counterparties,
      );
      if (candidates.length === 0) return [];
      const refreshed = await readState(candidates);
      const usable = forTokenOptions.includeTraps
        ? refreshed
        : refreshed.filter((pool) => !isTrap(pool));
      return usable
        .filter((pool) => (pool.liquidity ?? 0n) > 0n)
        .sort((a, b) => compareBigint(b.liquidity ?? 0n, a.liquidity ?? 0n))
        .slice(0, limit);
    },
  };
}

/** Pools whose state one `forToken` call will refresh. */
const DEFAULT_STATE_BUDGET = 600;

/**
 * Choose which pools to spend a state read on.
 *
 * A token on 4663 can sit in thousands of pools, nearly all of them empty, so
 * reading every one on every call is not affordable. Half the budget goes to
 * the pools already known to hold liquidity, deepest first, and half to pools
 * whose liquidity has never been read, newest first, because a stock-paired
 * launch is recent by definition. Every read is written back, so the second
 * call already knows what the first one found and the deep pools stay in view.
 */
function candidatePools(
  store: Store,
  currency: Address,
  budget: number,
  counterparties?: readonly Address[],
): Hex32[] {
  const pair =
    counterparties && counterparties.length > 0
      ? {
          where: `((currency0 = ? AND currency1 IN (${placeholders(counterparties.length)})
                   ) OR (currency1 = ? AND currency0 IN (${placeholders(counterparties.length)})))`,
          args: [currency, ...counterparties, currency, ...counterparties],
        }
      : { where: `(currency0 = ? OR currency1 = ?)`, args: [currency, currency] };

  const known = store.db
    .prepare(
      `SELECT pool_id FROM pools
       WHERE ${pair.where} AND liquidity IS NOT NULL
       ORDER BY CAST(liquidity AS REAL) DESC LIMIT ?`,
    )
    .all(...pair.args, Math.ceil(budget / 2)) as { pool_id: string }[];
  const unread = store.db
    .prepare(
      `SELECT pool_id FROM pools
       WHERE ${pair.where} AND liquidity IS NULL
       ORDER BY CAST(initial_block AS INTEGER) DESC LIMIT ?`,
    )
    .all(...pair.args, budget - known.length) as { pool_id: string }[];
  return [...known, ...unread].map((row) => row.pool_id as Hex32);
}

function placeholders(count: number): string {
  return new Array(count).fill('?').join(', ');
}

/** A pool as the `Initialize` log describes it, before decimals are attached. */
interface DiscoveredPool {
  readonly poolId: Hex32;
  readonly currency0: Address;
  readonly currency1: Address;
  readonly fee: number;
  readonly tickSpacing: number;
  readonly hooks: Address;
  readonly sqrtPriceX96: bigint;
  readonly tick: number;
  readonly blockNumber: bigint;
}

/** A raw log as the node returns it. */
interface RawLog {
  readonly topics: readonly string[];
  readonly data: string;
  readonly blockNumber: string;
}

/**
 * Read one block range, halving it whenever the node refuses.
 *
 * The node answers a query matching more than 10,000 logs, or one that runs too
 * long, with a refusal rather than a partial result. Both are answered by
 * splitting, which is why the span is a starting guess and not a limit.
 */
async function scanRange(
  client: DeskChainClient,
  range: BlockRange,
  topicPosition: 2 | 3,
  addressTopics: readonly Hex32[],
  onLogs: (logs: readonly RawLog[]) => void,
): Promise<void> {
  const topics: (Hex32 | readonly Hex32[] | null)[] = [INITIALIZE_TOPIC0, null, null];
  topics[topicPosition] = addressTopics;

  const pending: { range: BlockRange; tries: number }[] = [{ range, tries: 0 }];
  while (pending.length > 0) {
    const current = pending.pop();
    if (!current) break;
    try {
      const logs = await client.request<RawLog[]>('eth_getLogs', [
        {
          fromBlock: toHex(current.range.from),
          toBlock: toHex(current.range.to),
          topics,
        },
      ]);
      onLogs(logs);
    } catch (error) {
      const halves = isRangeTooWideError(error) ? splitRange(current.range) : null;
      if (halves) {
        pending.push({ range: halves[1], tries: 0 }, { range: halves[0], tries: 0 });
        continue;
      }
      // A range that cannot be narrowed any further is retried, because a
      // single block that timed out once usually answers on the next attempt.
      if (current.tries < MAX_RANGE_RETRIES) {
        pending.push({ range: current.range, tries: current.tries + 1 });
        continue;
      }
      throw new UpstreamError(
        'rpc',
        `eth_getLogs failed for blocks ${current.range.from} to ${current.range.to}`,
        {
          fromBlock: current.range.from.toString(),
          toBlock: current.range.to.toString(),
          reason: error instanceof Error ? error.message : String(error),
        },
      );
    }
  }
}

/** Decode one `Initialize` log. Returns undefined for a log that does not fit. */
export function decodeInitializeLog(log: RawLog): DiscoveredPool | undefined {
  const [topic0, poolId, currency0, currency1] = log.topics;
  if (topic0?.toLowerCase() !== INITIALIZE_TOPIC0 || !poolId || !currency0 || !currency1)
    return undefined;
  const [fee, tickSpacing, hooks, sqrtPriceX96, tick] = decodeAbiParameters(
    [
      { type: 'uint24' },
      { type: 'int24' },
      { type: 'address' },
      { type: 'uint160' },
      { type: 'int24' },
    ],
    log.data as `0x${string}`,
  );
  return {
    poolId: poolId.toLowerCase() as Hex32,
    currency0: topicToAddress(currency0),
    currency1: topicToAddress(currency1),
    fee: Number(fee),
    tickSpacing: Number(tickSpacing),
    hooks: hooks.toLowerCase() as Address,
    sqrtPriceX96,
    tick: Number(tick),
    blockNumber: BigInt(log.blockNumber),
  };
}

function topicToAddress(topic: string): Address {
  return `0x${topic.slice(-40)}`.toLowerCase() as Address;
}

function uniqueCurrencies(pools: Iterable<DiscoveredPool>): Address[] {
  const seen = new Set<string>();
  for (const pool of pools) {
    seen.add(pool.currency0);
    seen.add(pool.currency1);
  }
  seen.delete(NATIVE_CURRENCY);
  return [...seen].map((address) => address as Address);
}

/**
 * Decimals for one side of a discovered pool.
 *
 * A currency that has not answered yet gets the ERC-20 default so the row can
 * be written, and `chain.tokens` corrects every pool holding that currency the
 * moment the token itself is read.
 */
function decimalsFor(currency: Address, decimals: Map<string, number>): number {
  if (currency === NATIVE_CURRENCY) return 18;
  return decimals.get(currency.toLowerCase()) ?? 18;
}

function compareBigint(a: bigint, b: bigint): number {
  return a === b ? 0 : a > b ? 1 : -1;
}

function toHex(value: bigint): `0x${string}` {
  return `0x${value.toString(16)}`;
}

/**
 * Highest block a completed scan has covered.
 *
 * Ordered by row id rather than by timestamp: two scans that finish in the same
 * millisecond would otherwise tie, and the older one could win.
 */
export function readCursor(store: Store): bigint | undefined {
  const row = store.db
    .prepare('SELECT detail FROM events WHERE subject = ? ORDER BY id DESC LIMIT 1')
    .get(SCAN_SUBJECT) as { detail?: string } | undefined;
  if (!row?.detail) return undefined;
  const value = (JSON.parse(row.detail) as { toBlock?: string | number }).toBlock;
  if (typeof value === 'string' || typeof value === 'number') return BigInt(value);
  return undefined;
}

/** Record how far a scan reached, so the next one starts from there. */
export function writeCursor(store: Store, toBlock: bigint): void {
  store.appendEvent({
    ts: Date.now(),
    kind: 'pool-scan',
    subject: SCAN_SUBJECT,
    detail: { toBlock: toBlock.toString() },
  });
}
