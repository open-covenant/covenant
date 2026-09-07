/**
 * Local storage: one SQLite file in the desk home, opened with `node:sqlite`.
 *
 * No native dependency and no ORM. Migrations run in code and are idempotent,
 * so opening an existing database is always safe. Amounts that do not fit a
 * double (raw token amounts, block numbers, liquidity) are stored as text and
 * returned as `bigint`.
 */

import { DatabaseSync } from 'node:sqlite';
import type { StatementSync } from 'node:sqlite';
import { mkdirSync } from 'node:fs';
import path from 'node:path';
import { DeskError } from './errors.js';
import type {
  Address,
  DeskEvent,
  Execution,
  Hex32,
  HedgePosition,
  Observation,
  Order,
  OrderStatus,
  Pool,
  Quantity,
  Source,
  Token,
} from './types.js';

/** Applied in order, once each. Never edit a shipped statement; append a new one. */
const MIGRATIONS: readonly { readonly id: string; readonly sql: string }[] = [
  {
    id: '0001_initial',
    sql: `
      CREATE TABLE IF NOT EXISTS tokens (
        address TEXT PRIMARY KEY,
        symbol TEXT NOT NULL,
        name TEXT NOT NULL DEFAULT '',
        decimals INTEGER NOT NULL,
        is_stock_token INTEGER NOT NULL DEFAULT 0,
        ui_multiplier REAL,
        pending_multiplier REAL,
        token_paused INTEGER,
        oracle_paused INTEGER,
        feed TEXT,
        lighter_market_id INTEGER,
        capabilities TEXT,
        isin TEXT,
        logo_url TEXT,
        updated_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS tokens_symbol ON tokens(symbol);

      CREATE TABLE IF NOT EXISTS pools (
        pool_id TEXT PRIMARY KEY,
        currency0 TEXT NOT NULL,
        currency1 TEXT NOT NULL,
        decimals0 INTEGER NOT NULL,
        decimals1 INTEGER NOT NULL,
        fee INTEGER NOT NULL,
        tick_spacing INTEGER NOT NULL,
        hooks TEXT NOT NULL,
        initial_block TEXT NOT NULL,
        lp_fee INTEGER,
        liquidity TEXT,
        sqrt_price_x96 TEXT,
        tick INTEGER,
        mid_price REAL,
        trap INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS pools_currency0 ON pools(currency0);
      CREATE INDEX IF NOT EXISTS pools_currency1 ON pools(currency1);

      CREATE TABLE IF NOT EXISTS observations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        symbol TEXT NOT NULL,
        token TEXT NOT NULL,
        pool TEXT,
        onchain_mid_usd REAL,
        reference_usd REAL,
        reference_source TEXT,
        premium_bps REAL,
        session_state TEXT NOT NULL,
        block_number TEXT,
        candidates TEXT,
        stock_symbol TEXT,
        liquidity TEXT
      );
      CREATE INDEX IF NOT EXISTS observations_ts ON observations(ts);
      CREATE INDEX IF NOT EXISTS observations_symbol_ts ON observations(symbol, ts);

      CREATE TABLE IF NOT EXISTS orders (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        side TEXT NOT NULL,
        token_in TEXT NOT NULL,
        token_out TEXT NOT NULL,
        amount_in TEXT NOT NULL,
        trigger TEXT NOT NULL,
        bounds TEXT NOT NULL,
        live INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER,
        parent_id TEXT,
        reason TEXT
      );
      CREATE INDEX IF NOT EXISTS orders_status ON orders(status);
      CREATE INDEX IF NOT EXISTS orders_parent ON orders(parent_id);

      CREATE TABLE IF NOT EXISTS executions (
        id TEXT PRIMARY KEY,
        order_id TEXT NOT NULL,
        live INTEGER NOT NULL DEFAULT 0,
        tx_hash TEXT,
        amount_in TEXT NOT NULL,
        amount_out TEXT NOT NULL,
        quoted_amount_out TEXT NOT NULL,
        effective_price REAL NOT NULL,
        effective_price_unit TEXT NOT NULL,
        effective_price_source TEXT NOT NULL,
        notional_usd REAL NOT NULL,
        slippage_bps REAL NOT NULL,
        gas_used TEXT,
        block_number TEXT,
        status TEXT NOT NULL,
        reason TEXT,
        created_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS executions_order ON executions(order_id);
      CREATE INDEX IF NOT EXISTS executions_created ON executions(created_at);

      CREATE TABLE IF NOT EXISTS hedges (
        symbol TEXT PRIMARY KEY,
        market_id INTEGER,
        size_base REAL NOT NULL,
        entry_price REAL,
        mark_price REAL,
        unrealized_pnl_usd REAL,
        funding_bps_8h REAL,
        updated_at INTEGER NOT NULL
      );

      CREATE TABLE IF NOT EXISTS events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        kind TEXT NOT NULL,
        subject TEXT,
        detail TEXT
      );
      CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
      CREATE INDEX IF NOT EXISTS events_subject ON events(subject);
    `,
  },
];

export interface OpenStoreOptions {
  /** File path, or `:memory:` for a throwaway database. */
  path: string;
  /** Skip migrations. Only useful when a caller runs them itself. */
  migrate?: boolean;
}

export interface PoolFilter {
  /** Pools with this address on either side. */
  readonly currency?: Address;
  readonly minLiquidity?: bigint;
  /** Leave pools charging more than 300 bps out of the result. Default true. */
  readonly excludeTraps?: boolean;
  readonly limit?: number;
}

export interface ObservationFilter {
  /** ms since epoch, inclusive. */
  readonly since?: number;
  readonly until?: number;
  readonly symbol?: string;
  readonly limit?: number;
}

/** Everything the desk persists. */
export interface Store {
  readonly db: DatabaseSync;
  close(): void;

  upsertToken(token: Token): void;
  getToken(addressOrSymbol: string): Token | undefined;
  listTokens(options?: { stockOnly?: boolean }): Token[];
  countTokens(): number;

  upsertPool(pool: Pool): void;
  getPool(poolId: Hex32): Pool | undefined;
  listPools(filter?: PoolFilter): Pool[];
  countPools(): number;
  /**
   * Write a token's decimals into every pool that holds it, and drop the mid
   * price of the rows that change so the next state read prices them again.
   * Returns how many rows were corrected.
   */
  applyTokenDecimals(address: string, decimals: number): number;

  insertObservation(observation: Observation): number;
  listObservations(filter?: ObservationFilter): Observation[];
  countObservationsSince(since: number): number;

  insertOrder(order: Order): void;
  updateOrderStatus(id: string, status: OrderStatus, reason?: string): void;
  getOrder(id: string): Order | undefined;
  listOrders(filter?: { status?: OrderStatus; parentId?: string; limit?: number }): Order[];

  insertExecution(execution: Execution): void;
  listExecutions(filter?: { orderId?: string; since?: number; limit?: number }): Execution[];
  /**
   * Sum of execution notional over a rolling window, USD. Feeds the daily cap.
   * Counts sent and confirmed fills unless another set of statuses is named;
   * a desk in dry run passes `['simulated']` so the cap stays visible.
   */
  filledNotionalUsdSince(since: number, statuses?: readonly Execution['status'][]): number;

  upsertHedge(position: HedgePosition): void;
  listHedges(): HedgePosition[];

  appendEvent(event: Omit<DeskEvent, 'id'>): number;
  listEvents(filter?: { since?: number; subject?: string; kind?: string; limit?: number }): DeskEvent[];
}

/** Open the database, run migrations, and return the typed accessors. */
export function openStore(options: OpenStoreOptions): Store {
  const file = options.path;
  if (file !== ':memory:') mkdirSync(path.dirname(path.resolve(file)), { recursive: true, mode: 0o700 });

  let db: DatabaseSync;
  try {
    db = new DatabaseSync(file);
  } catch (error) {
    throw new DeskError('store_failed', `Could not open ${file}: ${(error as Error).message}`, { path: file });
  }
  db.exec('PRAGMA journal_mode = WAL');
  db.exec('PRAGMA foreign_keys = ON');
  db.exec('PRAGMA busy_timeout = 5000');

  if (options.migrate !== false) migrate(db);

  const prepared = new Map<string, StatementSync>();
  const stmt = (sql: string): StatementSync => {
    let found = prepared.get(sql);
    if (!found) {
      found = db.prepare(sql);
      prepared.set(sql, found);
    }
    return found;
  };

  return {
    db,
    close: () => db.close(),

    upsertToken(token) {
      stmt(`
        INSERT INTO tokens (address, symbol, name, decimals, is_stock_token, ui_multiplier, pending_multiplier,
          token_paused, oracle_paused, feed, lighter_market_id, capabilities, isin, logo_url, updated_at)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(address) DO UPDATE SET
          symbol=excluded.symbol, name=excluded.name, decimals=excluded.decimals,
          is_stock_token=excluded.is_stock_token, ui_multiplier=excluded.ui_multiplier,
          pending_multiplier=excluded.pending_multiplier, token_paused=excluded.token_paused,
          oracle_paused=excluded.oracle_paused, feed=excluded.feed,
          lighter_market_id=excluded.lighter_market_id, capabilities=excluded.capabilities,
          isin=excluded.isin, logo_url=excluded.logo_url, updated_at=excluded.updated_at
      `).run(
        token.address.toLowerCase(),
        token.symbol,
        token.name,
        token.decimals,
        token.isStockToken ? 1 : 0,
        token.uiMultiplier ?? null,
        token.pendingMultiplier ?? null,
        boolOrNull(token.tokenPaused),
        boolOrNull(token.oraclePaused),
        token.feed?.toLowerCase() ?? null,
        token.lighterMarketId ?? null,
        token.tradingCapabilities ? JSON.stringify(token.tradingCapabilities) : null,
        token.isin ?? null,
        token.logoUrl ?? null,
        Date.now(),
      );
    },

    getToken(addressOrSymbol) {
      const key = addressOrSymbol.trim();
      // A symbol is not unique on chain: anyone can deploy a token called AI.
      // A stock token wins, and among the rest the one with the deepest pool,
      // because that is the one a trader means.
      const row = key.startsWith('0x')
        ? stmt('SELECT * FROM tokens WHERE address = ?').get(key.toLowerCase())
        : stmt(
            `SELECT * FROM tokens WHERE symbol = ? COLLATE NOCASE
             ORDER BY is_stock_token DESC,
               (SELECT MAX(CAST(COALESCE(liquidity, '0') AS REAL)) FROM pools
                 WHERE currency0 = tokens.address OR currency1 = tokens.address) DESC
             LIMIT 1`,
          ).get(key);
      return row ? rowToToken(row as SqlRow) : undefined;
    },

    listTokens(listOptions) {
      const rows = listOptions?.stockOnly
        ? stmt('SELECT * FROM tokens WHERE is_stock_token = 1 ORDER BY symbol').all()
        : stmt('SELECT * FROM tokens ORDER BY symbol').all();
      return rows.map((row) => rowToToken(row as SqlRow));
    },

    upsertPool(pool) {
      stmt(`
        INSERT INTO pools (pool_id, currency0, currency1, decimals0, decimals1, fee, tick_spacing, hooks,
          initial_block, lp_fee, liquidity, sqrt_price_x96, tick, mid_price, trap, updated_at)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(pool_id) DO UPDATE SET
          decimals0=excluded.decimals0, decimals1=excluded.decimals1,
          lp_fee=excluded.lp_fee, liquidity=excluded.liquidity, sqrt_price_x96=excluded.sqrt_price_x96,
          tick=excluded.tick, mid_price=excluded.mid_price, trap=excluded.trap, updated_at=excluded.updated_at
      `).run(
        pool.poolId.toLowerCase(),
        pool.currency0.toLowerCase(),
        pool.currency1.toLowerCase(),
        pool.decimals0,
        pool.decimals1,
        pool.fee,
        pool.tickSpacing,
        pool.hooks.toLowerCase(),
        pool.initialBlock.toString(),
        pool.lpFee ?? null,
        pool.liquidity?.toString() ?? null,
        pool.sqrtPriceX96?.toString() ?? null,
        pool.tick ?? null,
        pool.midPrice?.value ?? null,
        pool.trap ? 1 : 0,
        Date.now(),
      );
    },

    countTokens() {
      const row = stmt('SELECT COUNT(*) AS n FROM tokens').get() as { n: number } | undefined;
      return Number(row?.n ?? 0);
    },

    getPool(poolId) {
      const row = stmt('SELECT * FROM pools WHERE pool_id = ?').get(poolId.toLowerCase());
      return row ? rowToPool(row as SqlRow) : undefined;
    },

    countPools() {
      const row = stmt('SELECT COUNT(*) AS n FROM pools').get() as { n: number } | undefined;
      return Number(row?.n ?? 0);
    },

    applyTokenDecimals(address, decimals) {
      const key = address.toLowerCase();
      const first = stmt(
        `UPDATE pools SET decimals0 = ?, mid_price = NULL WHERE currency0 = ? AND decimals0 <> ?`,
      ).run(decimals, key, decimals);
      const second = stmt(
        `UPDATE pools SET decimals1 = ?, mid_price = NULL WHERE currency1 = ? AND decimals1 <> ?`,
      ).run(decimals, key, decimals);
      return Number(first.changes) + Number(second.changes);
    },

    listPools(filter = {}) {
      const clauses: string[] = [];
      const args: (string | number)[] = [];
      if (filter.currency) {
        clauses.push('(currency0 = ? OR currency1 = ?)');
        args.push(filter.currency.toLowerCase(), filter.currency.toLowerCase());
      }
      if (filter.excludeTraps !== false) clauses.push('trap = 0');
      if (filter.minLiquidity !== undefined) {
        clauses.push('liquidity IS NOT NULL AND CAST(liquidity AS REAL) >= ?');
        args.push(Number(filter.minLiquidity));
      }
      const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
      const limit = filter.limit ?? 500;
      const rows = stmt(
        `SELECT * FROM pools ${where} ORDER BY CAST(COALESCE(liquidity,'0') AS REAL) DESC LIMIT ?`,
      ).all(...args, limit);
      return rows.map((row) => rowToPool(row as SqlRow));
    },

    insertObservation(observation) {
      const result = stmt(`
        INSERT INTO observations (ts, symbol, token, pool, onchain_mid_usd, reference_usd, reference_source,
          premium_bps, session_state, block_number, candidates, stock_symbol, liquidity)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        observation.ts,
        observation.symbol,
        observation.token.toLowerCase(),
        observation.pool?.toLowerCase() ?? null,
        observation.onchainMidUsd ?? null,
        observation.referenceUsd ?? null,
        observation.referenceSource ?? null,
        observation.premiumBps ?? null,
        observation.sessionState,
        observation.blockNumber?.toString() ?? null,
        observation.candidates ? JSON.stringify(observation.candidates) : null,
        observation.stockSymbol ?? null,
        observation.liquidity?.toString() ?? null,
      );
      return Number(result.lastInsertRowid);
    },

    listObservations(filter = {}) {
      const clauses: string[] = [];
      const args: (string | number)[] = [];
      if (filter.since !== undefined) {
        clauses.push('ts >= ?');
        args.push(filter.since);
      }
      if (filter.until !== undefined) {
        clauses.push('ts <= ?');
        args.push(filter.until);
      }
      if (filter.symbol) {
        clauses.push('symbol = ? COLLATE NOCASE');
        args.push(filter.symbol);
      }
      const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
      const rows = stmt(`SELECT * FROM observations ${where} ORDER BY ts ASC LIMIT ?`).all(
        ...args,
        filter.limit ?? 10_000,
      );
      return rows.map((row) => rowToObservation(row as SqlRow));
    },

    countObservationsSince(since) {
      const row = stmt('SELECT COUNT(*) AS n FROM observations WHERE ts >= ?').get(since) as { n: number } | undefined;
      return Number(row?.n ?? 0);
    },

    insertOrder(order) {
      stmt(`
        INSERT INTO orders (id, kind, side, token_in, token_out, amount_in, trigger, bounds, live, status,
          created_at, expires_at, parent_id, reason)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        order.id,
        order.kind,
        order.side,
        order.tokenIn.toLowerCase(),
        order.tokenOut.toLowerCase(),
        order.amountIn.toString(),
        JSON.stringify(order.trigger),
        JSON.stringify(order.bounds),
        order.live ? 1 : 0,
        order.status,
        order.createdAt,
        order.expiresAt,
        order.parentId,
        order.reason ?? null,
      );
    },

    updateOrderStatus(id, status, reason) {
      stmt('UPDATE orders SET status = ?, reason = COALESCE(?, reason) WHERE id = ?').run(status, reason ?? null, id);
    },

    getOrder(id) {
      const row = stmt('SELECT * FROM orders WHERE id = ?').get(id);
      return row ? rowToOrder(row as SqlRow) : undefined;
    },

    listOrders(filter = {}) {
      const clauses: string[] = [];
      const args: (string | number)[] = [];
      if (filter.status) {
        clauses.push('status = ?');
        args.push(filter.status);
      }
      if (filter.parentId) {
        clauses.push('parent_id = ?');
        args.push(filter.parentId);
      }
      const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
      const rows = stmt(`SELECT * FROM orders ${where} ORDER BY created_at DESC LIMIT ?`).all(
        ...args,
        filter.limit ?? 500,
      );
      return rows.map((row) => rowToOrder(row as SqlRow));
    },

    insertExecution(execution) {
      stmt(`
        INSERT INTO executions (id, order_id, live, tx_hash, amount_in, amount_out, quoted_amount_out,
          effective_price, effective_price_unit, effective_price_source, notional_usd, slippage_bps,
          gas_used, block_number, status, reason, created_at)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        execution.id,
        execution.orderId,
        execution.live ? 1 : 0,
        execution.txHash ?? null,
        execution.amountIn.toString(),
        execution.amountOut.toString(),
        execution.quotedAmountOut.toString(),
        execution.effectivePrice.value,
        execution.effectivePrice.unit,
        execution.effectivePrice.source,
        execution.notionalUsd.value,
        execution.slippageBps,
        execution.gasUsed?.toString() ?? null,
        execution.blockNumber?.toString() ?? null,
        execution.status,
        execution.reason ?? null,
        execution.createdAt,
      );
    },

    listExecutions(filter = {}) {
      const clauses: string[] = [];
      const args: (string | number)[] = [];
      if (filter.orderId) {
        clauses.push('order_id = ?');
        args.push(filter.orderId);
      }
      if (filter.since !== undefined) {
        clauses.push('created_at >= ?');
        args.push(filter.since);
      }
      const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
      const rows = stmt(`SELECT * FROM executions ${where} ORDER BY created_at DESC LIMIT ?`).all(
        ...args,
        filter.limit ?? 500,
      );
      return rows.map((row) => rowToExecution(row as SqlRow));
    },

    filledNotionalUsdSince(since, statuses = ['sent', 'confirmed']) {
      if (statuses.length === 0) return 0;
      const placeholders = statuses.map(() => '?').join(',');
      const row = stmt(
        `SELECT COALESCE(SUM(notional_usd), 0) AS total FROM executions
         WHERE created_at >= ? AND status IN (${placeholders})`,
      ).get(since, ...statuses) as { total: number } | undefined;
      return Number(row?.total ?? 0);
    },

    upsertHedge(position) {
      stmt(`
        INSERT INTO hedges (symbol, market_id, size_base, entry_price, mark_price, unrealized_pnl_usd,
          funding_bps_8h, updated_at)
        VALUES (?,?,?,?,?,?,?,?)
        ON CONFLICT(symbol) DO UPDATE SET
          market_id=excluded.market_id, size_base=excluded.size_base, entry_price=excluded.entry_price,
          mark_price=excluded.mark_price, unrealized_pnl_usd=excluded.unrealized_pnl_usd,
          funding_bps_8h=excluded.funding_bps_8h, updated_at=excluded.updated_at
      `).run(
        position.symbol,
        position.marketId,
        position.sizeBase.value,
        position.entryPrice?.value ?? null,
        position.markPrice.value,
        position.unrealizedPnlUsd?.value ?? null,
        position.fundingBps8h?.value ?? null,
        position.asOf,
      );
    },

    listHedges() {
      const rows = stmt('SELECT * FROM hedges ORDER BY symbol').all();
      return rows.map((row) => rowToHedge(row as SqlRow));
    },

    appendEvent(event) {
      const result = stmt('INSERT INTO events (ts, kind, subject, detail) VALUES (?,?,?,?)').run(
        event.ts,
        event.kind,
        event.subject ?? null,
        event.detail ? JSON.stringify(event.detail, bigintReplacer) : null,
      );
      return Number(result.lastInsertRowid);
    },

    listEvents(filter = {}) {
      const clauses: string[] = [];
      const args: (string | number)[] = [];
      if (filter.since !== undefined) {
        clauses.push('ts >= ?');
        args.push(filter.since);
      }
      if (filter.subject) {
        clauses.push('subject = ?');
        args.push(filter.subject);
      }
      if (filter.kind) {
        clauses.push('kind = ?');
        args.push(filter.kind);
      }
      const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
      const rows = stmt(`SELECT * FROM events ${where} ORDER BY ts DESC LIMIT ?`).all(...args, filter.limit ?? 200);
      return rows.map((row) => {
        const r = row as SqlRow;
        return {
          id: numberOrUndefined(r.id),
          ts: Number(r.ts),
          kind: String(r.kind),
          subject: stringOrUndefined(r.subject),
          detail: r.detail ? (JSON.parse(String(r.detail)) as Record<string, unknown>) : undefined,
        } satisfies DeskEvent;
      });
    },
  };
}

/** Apply pending migrations. Safe to call on every open. */
export function migrate(db: DatabaseSync): string[] {
  db.exec('CREATE TABLE IF NOT EXISTS schema_migrations (id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL)');
  const applied = new Set(
    (db.prepare('SELECT id FROM schema_migrations').all() as { id: string }[]).map((row) => row.id),
  );
  const ran: string[] = [];
  for (const migration of MIGRATIONS) {
    if (applied.has(migration.id)) continue;
    db.exec('BEGIN');
    try {
      db.exec(migration.sql);
      db.prepare('INSERT INTO schema_migrations (id, applied_at) VALUES (?, ?)').run(migration.id, Date.now());
      db.exec('COMMIT');
      ran.push(migration.id);
    } catch (error) {
      db.exec('ROLLBACK');
      throw new DeskError('store_failed', `Migration ${migration.id} failed: ${(error as Error).message}`, {
        migration: migration.id,
      });
    }
  }
  return ran;
}

type SqlValue = string | number | bigint | Uint8Array | null;
type SqlRow = Record<string, SqlValue>;

function boolOrNull(value: boolean | undefined): number | null {
  return value === undefined ? null : value ? 1 : 0;
}

function stringOrUndefined(value: SqlValue | undefined): string | undefined {
  return value === null || value === undefined ? undefined : String(value);
}

function numberOrUndefined(value: SqlValue | undefined): number | undefined {
  return value === null || value === undefined ? undefined : Number(value);
}

function bigintOrUndefined(value: SqlValue | undefined): bigint | undefined {
  return value === null || value === undefined ? undefined : BigInt(String(value));
}

function boolOrUndefined(value: SqlValue | undefined): boolean | undefined {
  return value === null || value === undefined ? undefined : Number(value) !== 0;
}

function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === 'bigint' ? value.toString() : value;
}

function rowToToken(row: SqlRow): Token {
  return {
    address: String(row.address) as Address,
    symbol: String(row.symbol),
    name: String(row.name ?? ''),
    decimals: Number(row.decimals),
    isStockToken: Number(row.is_stock_token) !== 0,
    uiMultiplier: numberOrUndefined(row.ui_multiplier),
    pendingMultiplier: numberOrUndefined(row.pending_multiplier),
    tokenPaused: boolOrUndefined(row.token_paused),
    oraclePaused: boolOrUndefined(row.oracle_paused),
    feed: row.feed ? (String(row.feed) as Address) : undefined,
    lighterMarketId: numberOrUndefined(row.lighter_market_id),
    tradingCapabilities: row.capabilities
      ? (JSON.parse(String(row.capabilities)) as Token['tradingCapabilities'])
      : undefined,
    isin: stringOrUndefined(row.isin),
    logoUrl: stringOrUndefined(row.logo_url),
  };
}

function rowToPool(row: SqlRow): Pool {
  const midPrice = numberOrUndefined(row.mid_price);
  return {
    poolId: String(row.pool_id) as Hex32,
    currency0: String(row.currency0) as Address,
    currency1: String(row.currency1) as Address,
    decimals0: Number(row.decimals0),
    decimals1: Number(row.decimals1),
    fee: Number(row.fee),
    tickSpacing: Number(row.tick_spacing),
    hooks: String(row.hooks) as Address,
    initialBlock: BigInt(String(row.initial_block)),
    lpFee: numberOrUndefined(row.lp_fee),
    liquidity: bigintOrUndefined(row.liquidity),
    sqrtPriceX96: bigintOrUndefined(row.sqrt_price_x96),
    tick: numberOrUndefined(row.tick),
    midPrice:
      midPrice === undefined
        ? undefined
        : { value: midPrice, unit: 'token', source: 'pool', asOf: Number(row.updated_at) },
    trap: Number(row.trap) !== 0,
  };
}

function rowToObservation(row: SqlRow): Observation {
  return {
    id: numberOrUndefined(row.id),
    ts: Number(row.ts),
    symbol: String(row.symbol),
    token: String(row.token) as Address,
    pool: row.pool ? (String(row.pool) as Hex32) : undefined,
    onchainMidUsd: numberOrUndefined(row.onchain_mid_usd),
    referenceUsd: numberOrUndefined(row.reference_usd),
    referenceSource: row.reference_source ? (String(row.reference_source) as Source) : undefined,
    premiumBps: numberOrUndefined(row.premium_bps),
    sessionState: String(row.session_state) as Observation['sessionState'],
    blockNumber: bigintOrUndefined(row.block_number),
    candidates: row.candidates ? (JSON.parse(String(row.candidates)) as Observation['candidates']) : undefined,
    stockSymbol: stringOrUndefined(row.stock_symbol),
    liquidity: bigintOrUndefined(row.liquidity),
  };
}

function rowToOrder(row: SqlRow): Order {
  return {
    id: String(row.id),
    kind: String(row.kind) as Order['kind'],
    side: String(row.side) as Order['side'],
    tokenIn: String(row.token_in) as Address,
    tokenOut: String(row.token_out) as Address,
    amountIn: BigInt(String(row.amount_in)),
    trigger: JSON.parse(String(row.trigger)) as Order['trigger'],
    bounds: JSON.parse(String(row.bounds)) as Order['bounds'],
    live: Number(row.live) !== 0,
    status: String(row.status) as OrderStatus,
    createdAt: Number(row.created_at),
    expiresAt: row.expires_at === null || row.expires_at === undefined ? null : Number(row.expires_at),
    parentId: row.parent_id === null || row.parent_id === undefined ? null : String(row.parent_id),
    reason: stringOrUndefined(row.reason),
  };
}

function rowToExecution(row: SqlRow): Execution {
  const asOf = Number(row.created_at);
  const price: Quantity = {
    value: Number(row.effective_price),
    unit: String(row.effective_price_unit) as Quantity['unit'],
    source: String(row.effective_price_source) as Source,
    asOf,
  };
  return {
    id: String(row.id),
    orderId: String(row.order_id),
    live: Number(row.live) !== 0,
    txHash: row.tx_hash ? (String(row.tx_hash) as Hex32) : undefined,
    amountIn: BigInt(String(row.amount_in)),
    amountOut: BigInt(String(row.amount_out)),
    quotedAmountOut: BigInt(String(row.quoted_amount_out)),
    effectivePrice: price,
    notionalUsd: { value: Number(row.notional_usd), unit: 'USD', source: 'derived', asOf },
    slippageBps: Number(row.slippage_bps),
    gasUsed: bigintOrUndefined(row.gas_used),
    blockNumber: bigintOrUndefined(row.block_number),
    status: String(row.status) as Execution['status'],
    reason: stringOrUndefined(row.reason),
    createdAt: asOf,
  };
}

function rowToHedge(row: SqlRow): HedgePosition {
  const asOf = Number(row.updated_at);
  const entry = numberOrUndefined(row.entry_price);
  const pnl = numberOrUndefined(row.unrealized_pnl_usd);
  const funding = numberOrUndefined(row.funding_bps_8h);
  return {
    symbol: String(row.symbol),
    marketId: Number(row.market_id),
    sizeBase: { value: Number(row.size_base), unit: 'token', source: 'lighter-rh', asOf },
    entryPrice: entry === undefined ? undefined : { value: entry, unit: 'USD', source: 'lighter-rh', asOf },
    markPrice: { value: Number(row.mark_price), unit: 'USD', source: 'lighter-rh', asOf },
    unrealizedPnlUsd: pnl === undefined ? undefined : { value: pnl, unit: 'USD', source: 'lighter-rh', asOf },
    fundingBps8h: funding === undefined ? undefined : { value: funding, unit: 'bps', source: 'lighter-rh', asOf },
    asOf,
  };
}
