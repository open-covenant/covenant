import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';

export interface Settlement {
  network: string;
  transaction: string;
  payTo: string;
  payer: string;
  amount: string;
  metered: boolean;
  settledAtMs: number;
}

/** One catalog entry, in the shape `/discovery/resources` returns. */
export interface CatalogItem {
  resource: string;
  type: string;
  x402Version: number;
  accepts: Record<string, unknown>[];
  lastUpdated: string;
  description?: string;
  mimeType?: string;
  serviceName?: string;
  tags?: string[];
  iconUrl?: string;
  extensions?: Record<string, unknown>;
}

export interface CatalogFilter {
  type?: string;
  payTo?: string;
  scheme?: string;
  network?: string;
  extensions?: string;
}

/** Oldest entries are dropped past this, so the catalog cannot grow without bound. */
const MAX_CATALOG_ITEMS = 10_000;

export const utcDay = (ms: number): string => new Date(ms).toISOString().slice(0, 10);

/** EVM addresses compare case-insensitively; Solana addresses are case-sensitive. */
export function sameAddress(a: unknown, b: unknown): boolean {
  if (typeof a !== 'string' || typeof b !== 'string') return false;
  return a.startsWith('0x') && b.startsWith('0x') ? a.toLowerCase() === b.toLowerCase() : a === b;
}

/**
 * Settlement ledger and Bazaar catalog on one SQLite file. The ledger backs the
 * daily caps and survives restarts, so a restart cannot reset a receiver's quota.
 */
export class Store {
  private readonly db: DatabaseSync;

  constructor(dataDir: string) {
    if (dataDir !== ':memory:') mkdirSync(dataDir, { recursive: true });
    this.db = new DatabaseSync(dataDir === ':memory:' ? ':memory:' : join(dataDir, 'facilitator.sqlite'));
    this.db.exec(`
      PRAGMA journal_mode = WAL;
      CREATE TABLE IF NOT EXISTS settlements (
        network TEXT NOT NULL,
        transaction_id TEXT NOT NULL,
        pay_to TEXT NOT NULL,
        payer TEXT NOT NULL,
        amount TEXT NOT NULL,
        metered INTEGER NOT NULL,
        day TEXT NOT NULL,
        settled_at INTEGER NOT NULL,
        PRIMARY KEY (network, transaction_id)
      );
      CREATE INDEX IF NOT EXISTS settlements_by_day ON settlements (day, network, metered, pay_to);
      CREATE TABLE IF NOT EXISTS catalog (
        key TEXT PRIMARY KEY,
        item TEXT NOT NULL,
        search TEXT NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS catalog_by_age ON catalog (updated_at);
    `);
  }

  recordSettlement(settlement: Settlement): void {
    this.db
      .prepare(
        `INSERT OR IGNORE INTO settlements
           (network, transaction_id, pay_to, payer, amount, metered, day, settled_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        settlement.network,
        settlement.transaction,
        settlement.payTo,
        settlement.payer,
        settlement.amount,
        settlement.metered ? 1 : 0,
        utcDay(settlement.settledAtMs),
        settlement.settledAtMs,
      );
  }

  meteredSettlements(network: string, day: string, payTo?: string): number {
    const row = payTo
      ? this.db
          .prepare(
            'SELECT COUNT(*) AS n FROM settlements WHERE day = ? AND network = ? AND metered = 1 AND pay_to = ?',
          )
          .get(day, network, payTo)
      : this.db
          .prepare('SELECT COUNT(*) AS n FROM settlements WHERE day = ? AND network = ? AND metered = 1')
          .get(day, network);
    return Number((row as { n: number | bigint }).n);
  }

  settlementTotals(): { network: string; settlements: number }[] {
    return (
      this.db
        .prepare('SELECT network, COUNT(*) AS n FROM settlements GROUP BY network ORDER BY network')
        .all() as { network: string; n: number | bigint }[]
    ).map((row) => ({ network: row.network, settlements: Number(row.n) }));
  }

  upsertCatalogItem(key: string, item: CatalogItem, updatedAtMs: number): void {
    const existing = this.db.prepare('SELECT item FROM catalog WHERE key = ?').get(key) as
      | { item: string }
      | undefined;
    // A resource paid on several networks keeps one entry with every rail it was paid on.
    const accepts = existing
      ? mergeAccepts((JSON.parse(existing.item) as CatalogItem).accepts, item.accepts)
      : item.accepts;
    const merged = { ...item, accepts };
    const search = [merged.resource, merged.serviceName, merged.description, ...(merged.tags ?? [])]
      .filter(Boolean)
      .join(' ')
      .toLowerCase();
    this.db
      .prepare(
        `INSERT INTO catalog (key, item, search, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET item = excluded.item, search = excluded.search,
           updated_at = excluded.updated_at`,
      )
      .run(key, JSON.stringify(merged), search, updatedAtMs);
    this.db
      .prepare(
        `DELETE FROM catalog WHERE key IN (
           SELECT key FROM catalog ORDER BY updated_at DESC LIMIT -1 OFFSET ?)`,
      )
      .run(MAX_CATALOG_ITEMS);
  }

  catalog(filter: CatalogFilter, terms: string[] = []): CatalogItem[] {
    const rows = this.db.prepare('SELECT item, search FROM catalog ORDER BY updated_at DESC').all() as {
      item: string;
      search: string;
    }[];
    return rows
      .filter((row) => terms.every((term) => row.search.includes(term)))
      .map((row) => JSON.parse(row.item) as CatalogItem)
      .filter((item) => matches(item, filter));
  }

  close(): void {
    this.db.close();
  }
}

function sameRail(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  return (
    a.scheme === b.scheme &&
    a.network === b.network &&
    sameAddress(a.asset, b.asset) &&
    sameAddress(a.payTo, b.payTo)
  );
}

function mergeAccepts(
  previous: Record<string, unknown>[],
  latest: Record<string, unknown>[],
): Record<string, unknown>[] {
  return [...latest, ...previous.filter((old) => !latest.some((fresh) => sameRail(old, fresh)))];
}

function matches(item: CatalogItem, filter: CatalogFilter): boolean {
  if (filter.type && item.type !== filter.type) return false;
  if (filter.extensions && !(item.extensions && filter.extensions in item.extensions)) return false;
  return item.accepts.some(
    (accepted) =>
      (!filter.payTo || sameAddress(accepted.payTo, filter.payTo)) &&
      (!filter.scheme || accepted.scheme === filter.scheme) &&
      (!filter.network || accepted.network === filter.network),
  );
}
