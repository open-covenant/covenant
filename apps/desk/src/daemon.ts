/**
 * Daemon lifecycle.
 *
 * Opens the store, loads keys, builds every module, starts the loops, and
 * serves the API and the page. The loops are the desk: the registry keeps the
 * token list current, the pool scan finds the pools that were created since the
 * last pass, the recorder writes the premium dataset, the engine watches the
 * open orders, and the rebalancer keeps a hedge on target. A loop that fails is
 * reported in `status` and tried again on its next turn.
 */

import type { Config } from './core/config.js';
import { ensureDeskHome, loadConfig, liveBlockedReason } from './core/config.js';
import { loadKeystore, type Keystore } from './core/keystore.js';
import { createLogger, type Logger } from './core/logger.js';
import { openStore, type Store } from './core/store.js';
import { deskPaths } from './core/config.js';
import type { Address, DeskStatus, Hex32 } from './core/types.js';
import {
  chunkRange,
  createChainModule,
  readPoolScanCursor,
  writePoolScanCursor,
  type ChainModule,
} from './chain/index.js';
import { createFairValueModule, type FairValueModule } from './fairvalue/index.js';
import { createOrdersModule, type OrdersModule } from './orders/index.js';
import { createHedgeModule, type HedgeModule } from './hedge/index.js';
import { createHttpSurface, type HttpSurface } from './surfaces/http/index.js';
import { version } from './core/version.js';

/** Pools priced per pass. Two chain calls each, batched into one round trip. */
const PRICING_BATCH = 2000;
/** How often the desk prices pools it has found but never read, ms. */
const PRICING_INTERVAL_MS = 15_000;
/** Tokens named per pass. One batched read of seven fields each. */
const NAMING_BATCH = 500;
/** How often the desk names the tokens in pools it has just found, ms. */
const NAMING_INTERVAL_MS = 20_000;
/** How long a block height reading stands in for a fresh one in status, ms. */
const BLOCK_FRESHNESS_MS = 10_000;

/** Everything a surface needs. Built once per process. */
export interface Desk {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly keystore: Keystore;
  readonly chain: ChainModule;
  readonly fairvalue: FairValueModule;
  readonly orders: OrdersModule;
  readonly hedge: HedgeModule;
  /** ms since epoch. */
  readonly startedAt: number;
  /** Start the loops and the API. */
  start(): Promise<{ url: string }>;
  /** Stop the loops, close the API, close the database. */
  stop(): Promise<void>;
  status(): Promise<DeskStatus>;
  /**
   * Look for pools created since the last pass. Runs on its own interval; call
   * it to force a pass, for example straight after a registry refresh.
   */
  scanPools(): Promise<number>;
  /**
   * Read the state of pools the desk has found but never priced. Returns how
   * many were read.
   */
  pricePools(limit?: number): Promise<number>;
  /**
   * Read the symbol and decimals of the tokens in the deepest pools the desk
   * has not named yet. Returns how many were read.
   */
  nameTokens(limit?: number): Promise<number>;
}

export interface CreateDeskOptions {
  /** Loaded config. Read from the desk home when absent. */
  config?: Config;
  logger?: Logger;
  store?: Store;
  keystore?: Keystore;
  env?: NodeJS.ProcessEnv;
}

/** Build the desk. Nothing starts until {@link Desk.start} is called. */
export function createDesk(options: CreateDeskOptions = {}): Desk {
  const env = options.env ?? process.env;
  const config = options.config ?? loadConfig(env);
  const paths = ensureDeskHome(env);

  const keystore = options.keystore ?? loadKeystore({ env });
  const logger =
    options.logger ??
    createLogger({
      level: config.logLevel,
      component: 'desk',
      // The bearer token opens the local API, so it is masked wherever it
      // appears rather than only under a field with that name.
      secrets: [...keystore.secrets(), config.token],
    });

  const store = options.store ?? openStore({ path: paths.database });

  const chain = createChainModule({
    config,
    logger: logger.child({ module: 'chain' }),
    store,
    keystore,
  });
  const fairvalue = createFairValueModule({
    config,
    logger: logger.child({ module: 'fairvalue' }),
    store,
    chain,
  });
  const orders = createOrdersModule({
    config,
    logger: logger.child({ module: 'orders' }),
    store,
    chain,
    fairvalue,
    keystore,
  });
  const hedge = createHedgeModule({
    config,
    logger: logger.child({ module: 'hedge' }),
    store,
    keystore,
    fairvalue,
  });

  const startedAt = Date.now();
  /** Last failure per component, cleared when the component next succeeds. */
  const degraded = new Map<string, string>();
  const timers = new Set<NodeJS.Timeout>();
  let http: HttpSurface | undefined;
  let scanning = false;
  let stopped = false;
  /** Highest block any read has seen, so status has an answer under load. */
  let lastBlock: bigint | undefined;
  let lastBlockAt = 0;
  /** How far down the book the pricing pass has reached. Undefined means the top. */
  let priceCursor: bigint | undefined;

  const blockNumber = async (): Promise<bigint> => {
    lastBlock = await chain.client.blockNumber();
    lastBlockAt = Date.now();
    return lastBlock;
  };

  const failed = (component: string, error: unknown) => {
    const reason = error instanceof Error ? error.message : String(error);
    degraded.set(component, reason);
    logger.warn('a loop did not finish its pass', { component, reason });
  };

  /**
   * Run `pass` now and then on an interval, never twice at once. A failure is
   * recorded and the loop carries on, because one bad pass is not a reason to
   * stop watching prices.
   */
  const loop = (component: string, intervalMs: number, pass: () => Promise<unknown>): void => {
    let running = false;
    const turn = async () => {
      if (running || stopped) return;
      running = true;
      try {
        await pass();
        degraded.delete(component);
      } catch (error) {
        failed(component, error);
      } finally {
        running = false;
      }
    };
    const timer = setInterval(() => void turn(), intervalMs);
    timer.unref();
    timers.add(timer);
    void turn();
  };

  /**
   * Look for pools the desk has not seen.
   *
   * The first pass covers the whole chain, which is minutes of work and tens of
   * thousands of pools, so it advances in slices and records how far it reached
   * after each one. Later passes start from that mark and cost one query.
   */
  const scanPools = async (): Promise<number> => {
    if (scanning) return 0;
    scanning = true;
    try {
      const tip = await blockNumber();
      const cursor = readPoolScanCursor(store);
      const from = cursor === undefined ? 0n : cursor + 1n;
      let found = 0;

      for (const range of chunkRange(from, tip, BigInt(config.poolScanChunkBlocks))) {
        if (stopped) break;
        const pools = await chain.pools.discover({ fromBlock: range.from, toBlock: range.to });
        found += pools.length;
        writePoolScanCursor(store, range.to);
        logger.info('pool scan advanced', {
          fromBlock: range.from.toString(),
          toBlock: range.to.toString(),
          tip: tip.toString(),
          pools: pools.length,
        });
      }
      return found;
    } finally {
      scanning = false;
    }
  };

  /**
   * Price the pools the desk has found but never read.
   *
   * Discovery records that a pool exists. Only a state read says whether it
   * holds anything, and depth is what decides which pool prices a token, so
   * every pool is read once, newest first, and refreshed after that by the
   * loops that use it. When there is nothing left the pass starts again from
   * the newest, which picks up the pools the last chain scan added.
   */
  const pricePools = async (limit = PRICING_BATCH): Promise<number> => {
    const from = priceCursor === undefined ? Number.MAX_SAFE_INTEGER : Number(priceCursor);
    const rows = store.db
      .prepare(
        `SELECT pool_id, CAST(initial_block AS INTEGER) AS block FROM pools
         WHERE liquidity IS NULL AND CAST(initial_block AS INTEGER) <= ?
         ORDER BY block DESC LIMIT ?`,
      )
      .all(from, limit) as { pool_id: string; block: number }[];

    if (rows.length === 0) {
      priceCursor = undefined;
      return 0;
    }
    const last = rows[rows.length - 1];
    priceCursor = last && last.block > 0 ? BigInt(last.block) - 1n : undefined;

    const priced = await chain.pools.state(rows.map((row) => row.pool_id as Hex32));
    logger.info('priced pools the desk had not read', {
      pools: priced.length,
      throughBlock: priceCursor?.toString(),
    });
    return priced.length;
  };

  /**
   * Name the tokens in the deepest pools.
   *
   * Discovery finds pools, not names: a pool holds two addresses. Until the
   * other side of a stock pool is read, the desk can price it but cannot say
   * what it is, and nobody can ask for it by symbol. Each pass reads the
   * deepest unnamed tokens, so the names arrive in the order they matter.
   */
  const nameTokens = async (limit = NAMING_BATCH): Promise<number> => {
    const rows = store.db
      .prepare(
        `SELECT side AS address, MAX(depth) AS depth FROM (
           SELECT currency0 AS side, CAST(COALESCE(liquidity, '0') AS REAL) AS depth FROM pools WHERE trap = 0
           UNION ALL
           SELECT currency1 AS side, CAST(COALESCE(liquidity, '0') AS REAL) AS depth FROM pools WHERE trap = 0
         )
         WHERE side NOT IN (SELECT address FROM tokens)
         GROUP BY side HAVING depth > 0
         ORDER BY depth DESC LIMIT ?`,
      )
      .all(limit) as { address: string }[];
    if (rows.length === 0) return 0;

    const named = await chain.tokens.read(rows.map((row) => row.address as Address));
    logger.info('named the tokens in the deepest new pools', { tokens: named.length });
    return named.length;
  };

  return {
    config,
    logger,
    store,
    keystore,
    chain,
    fairvalue,
    orders,
    hedge,
    startedAt,
    scanPools,
    pricePools,
    nameTokens,

    async start() {
      const blocked = liveBlockedReason(config);
      logger.info('desk starting', {
        version: version(),
        port: config.port,
        mode: blocked ? 'dry-run' : 'live',
        reason: blocked ?? undefined,
        home: paths.home,
      });

      stopped = false;
      degraded.clear();

      // The token list is what every other loop reads, so the first refresh is
      // awaited. A failure leaves the desk on the snapshot in `data/`.
      try {
        logger.info('registry loaded', await chain.registry.refresh());
      } catch (error) {
        failed('registry', error);
      }
      loop('registry', config.registryRefreshSec * 1000, () => chain.registry.refresh());

      if (config.poolScanEnabled) {
        loop('pools', config.poolScanIntervalSec * 1000, scanPools);
        loop('pool prices', PRICING_INTERVAL_MS, () => pricePools());
        loop('tokens', NAMING_INTERVAL_MS, () => nameTokens());
      }
      if (config.recorderEnabled) {
        try {
          fairvalue.recorder.start();
        } catch (error) {
          failed('recorder', error);
        }
      }
      try {
        orders.engine.start();
      } catch (error) {
        failed('orders', error);
      }
      if (config.hedgeEnabled) {
        try {
          hedge.rebalancer.start();
        } catch (error) {
          failed('hedge', error);
        }
      }

      http = createHttpSurface(this);
      const started = await http.start();
      logger.info('desk listening', { url: started.url });
      return started;
    },

    async stop() {
      stopped = true;
      for (const timer of timers) clearInterval(timer);
      timers.clear();

      for (const [component, halt] of [
        ['recorder', () => fairvalue.recorder.stop()],
        ['orders', () => orders.engine.stop()],
        ['hedge', () => hedge.rebalancer.stop()],
      ] as const) {
        try {
          halt();
        } catch (error) {
          logger.debug('component was not running', {
            component,
            reason: error instanceof Error ? error.message : String(error),
          });
        }
      }
      if (http) {
        try {
          await http.stop();
        } catch (error) {
          logger.warn('the API did not close cleanly', {
            reason: error instanceof Error ? error.message : String(error),
          });
        }
        http = undefined;
      }
      store.close();
      logger.info('desk stopped');
    },

    async status() {
      const now = Date.now();
      const dayAgo = now - 24 * 60 * 60 * 1000;
      // The endpoint is serialised behind one gate, so status answers from the
      // last reading and only goes to the chain when that reading has aged out.
      // A page refreshing every fifteen seconds costs at most one read.
      let block: bigint | undefined = lastBlock;
      if (block === undefined || now - lastBlockAt > BLOCK_FRESHNESS_MS) {
        try {
          block = await blockNumber();
        } catch (error) {
          logger.debug('the block height could not be read', {
            reason: error instanceof Error ? error.message : String(error),
          });
        }
      }
      let sessionState: DeskStatus['sessionState'] = 'closed';
      let nextOpen: number | undefined;
      try {
        const info = fairvalue.session.info(now);
        sessionState = info.state;
        nextOpen = info.nextOpen;
      } catch {
        sessionState = 'closed';
      }
      let recorderRunning = false;
      try {
        recorderRunning = fairvalue.recorder.running();
      } catch {
        recorderRunning = false;
      }
      let poolScanBlock: bigint | undefined;
      try {
        poolScanBlock = readPoolScanCursor(store);
      } catch {
        poolScanBlock = undefined;
      }
      return {
        version: version(),
        uptimeSec: Math.floor((now - startedAt) / 1000),
        live: liveBlockedReason(config) === null,
        acknowledgedRestrictions: config.acknowledgedRestrictions,
        chainId: config.chainId,
        blockNumber: block,
        sessionState,
        nextOpen,
        tokensTracked: store.countTokens(),
        poolsTracked: store.countPools(),
        poolScanRunning: scanning,
        poolScanBlock,
        openOrders: store.listOrders({ status: 'open' }).length,
        observations24h: store.countObservationsSince(dayAgo),
        recorderRunning,
        hedgeEnabled: config.hedgeEnabled,
        degraded: [...degraded].map(([component, reason]) => ({ component, reason })),
        asOf: now,
      } satisfies DeskStatus;
    },
  };
}

/** Start a desk and stop it cleanly on SIGINT or SIGTERM. */
export async function runDaemon(options: CreateDeskOptions = {}): Promise<Desk> {
  const desk = createDesk(options);
  await desk.start();
  const shutdown = () => {
    void desk.stop().finally(() => process.exit(0));
  };
  process.once('SIGINT', shutdown);
  process.once('SIGTERM', shutdown);
  return desk;
}

/** Paths the daemon reads and writes, for the CLI and the service files. */
export { deskPaths };
