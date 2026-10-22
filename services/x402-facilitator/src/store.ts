import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { DatabaseSync, type SQLInputValue } from 'node:sqlite';

export interface Settlement {
  network: string;
  transaction: string;
  payTo: string;
  payer: string;
  amount: string;
  metered: boolean;
  /** A broadcast that failed on chain still cost the signer its fee. */
  succeeded: boolean;
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
  return normalAddress(a) === normalAddress(b);
}

/** The form an address is stored and counted in, so case variants share one quota. */
export function normalAddress(address: string): string {
  return address.startsWith('0x') ? address.toLowerCase() : address;
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
        succeeded INTEGER NOT NULL,
        day TEXT NOT NULL,
        settled_at INTEGER NOT NULL,
        PRIMARY KEY (network, transaction_id)
      );
      CREATE INDEX IF NOT EXISTS settlements_by_day ON settlements (day, network, metered, pay_to);
      CREATE TABLE IF NOT EXISTS catalog (
        key TEXT PRIMARY KEY,
        item TEXT NOT NULL,
        type TEXT NOT NULL,
        network TEXT NOT NULL,
        scheme TEXT NOT NULL,
        pay_to TEXT NOT NULL,
        extension_keys TEXT NOT NULL,
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
           (network, transaction_id, pay_to, payer, amount, metered, succeeded, day, settled_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        settlement.network,
        settlement.transaction,
        normalAddress(settlement.payTo),
        settlement.payer,
        settlement.amount,
        settlement.metered ? 1 : 0,
        settlement.succeeded ? 1 : 0,
        utcDay(settlement.settledAtMs),
        settlement.settledAtMs,
      );
  }

  /** Metered broadcasts on one UTC day, failed ones included. */
  meteredSettlements(network: string, day: string, payTo?: string): number {
    const row = payTo
      ? this.db
          .prepare(
            'SELECT COUNT(*) AS n FROM settlements WHERE day = ? AND network = ? AND metered = 1 AND pay_to = ?',
          )
          .get(day, network, normalAddress(payTo))
      : this.db
          .prepare('SELECT COUNT(*) AS n FROM settlements WHERE day = ? AND network = ? AND metered = 1')
          .get(day, network);
    return Number((row as { n: number | bigint }).n);
  }

  settlementTotals(): { network: string; settlements: number }[] {
    return (
      this.db
        .prepare(
          'SELECT network, COUNT(*) AS n FROM settlements WHERE succeeded = 1 GROUP BY network ORDER BY network',
        )
        .all() as { network: string; n: number | bigint }[]
    ).map((row) => ({ network: row.network, settlements: Number(row.n) }));
  }

  /** One entry per resource and receiving rail, so a payment to one seller cannot rewrite another's. */
  upsertCatalogItem(item: CatalogItem, method: string, updatedAtMs: number): void {
    const [accepted] = item.accepts;
    const network = String(accepted?.network ?? '');
    const payTo = normalAddress(String(accepted?.payTo ?? ''));
    const key = [item.type, method, item.resource, network, payTo].join(' ');
    const search = [item.resource, item.serviceName, item.description, ...(item.tags ?? [])]
      .filter(Boolean)
      .join(' ')
      .toLowerCase();
    const extensionKeys = `,${Object.keys(item.extensions ?? {}).join(',')},`;
    this.db
      .prepare(
        `INSERT INTO catalog (key, item, type, network, scheme, pay_to, extension_keys, search, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET item = excluded.item, search = excluded.search,
           extension_keys = excluded.extension_keys, updated_at = excluded.updated_at`,
      )
      .run(key, JSON.stringify(item), item.type, network, String(accepted?.scheme ?? ''), payTo, extensionKeys, search, updatedAtMs);
    this.db
      .prepare(
        `DELETE FROM catalog WHERE key IN (
           SELECT key FROM catalog ORDER BY updated_at DESC LIMIT -1 OFFSET ?)`,
      )
      .run(MAX_CATALOG_ITEMS);
  }

  /** Newest first. Every search term must appear in the entry's text. */
  catalog(
    filter: CatalogFilter,
    page: { limit: number; offset: number },
    terms: string[] = [],
  ): { items: CatalogItem[]; total: number } {
    const clauses: string[] = [];
    const values: SQLInputValue[] = [];
    const filterBy = (clause: string, value: SQLInputValue) => {
      clauses.push(clause);
      values.push(value);
    };
    if (filter.type) filterBy('type = ?', filter.type);
    if (filter.network) filterBy('network = ?', filter.network);
    if (filter.scheme) filterBy('scheme = ?', filter.scheme);
    if (filter.payTo) filterBy('pay_to = ?', normalAddress(filter.payTo));
    if (filter.extensions) filterBy('instr(extension_keys, ?) > 0', `,${filter.extensions},`);
    for (const term of terms) filterBy('instr(search, ?) > 0', term);
    const where = clauses.length ? `WHERE ${clauses.join(' AND ')}` : '';
    const total = this.db.prepare(`SELECT COUNT(*) AS n FROM catalog ${where}`).get(...values) as {
      n: number | bigint;
    };
    const rows = this.db
      .prepare(`SELECT item FROM catalog ${where} ORDER BY updated_at DESC LIMIT ? OFFSET ?`)
      .all(...values, page.limit, page.offset) as { item: string }[];
    return { items: rows.map((row) => JSON.parse(row.item) as CatalogItem), total: Number(total.n) };
  }

  close(): void {
    this.db.close();
  }
}
