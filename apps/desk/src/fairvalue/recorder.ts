/**
 * The premium recorder.
 *
 * On an interval it writes one row per liquid stock token and one per tracked
 * paired pool: what the chain charged, what the reference said, and the gap
 * between them. That dataset is the evidence for the whole product, so a
 * single failed symbol never stops the pass.
 */

import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import type { Address, FairValue, Observation, Pool, ReferenceCandidate, Source } from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';
import type { Paired, Recorder, Session } from './contracts.js';
import type { DeskPremium } from './premium.js';
import { USDG_ADDRESS, WETH_ADDRESS, byLiquidity, otherCurrency, sameAddress } from './pools.js';

/** Columns written by {@link Recorder.export}. */
const CSV_COLUMNS = [
  'ts',
  'iso',
  'symbol',
  'token',
  'stock_symbol',
  'pool',
  'onchain_mid_usd',
  'reference_usd',
  'reference_source',
  'premium_bps',
  'session_state',
  'block_number',
  'liquidity',
] as const;

/** Rows read in one export. */
const EXPORT_LIMIT = 200_000;

export interface RecorderDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
  readonly session: Session;
  readonly premium: DeskPremium;
  readonly paired: Paired;
  readonly now?: () => number;
}

/** One paired pool the recorder tracks. */
interface PairedTarget {
  readonly token: Address;
  readonly stockSymbol: string;
  readonly pool: Pool;
}

export function createRecorder(deps: RecorderDeps): Recorder {
  const clock = deps.now ?? Date.now;
  let timer: ReturnType<typeof setInterval> | undefined;
  let inFlight = false;

  const tick = async (): Promise<number> => {
    const now = clock();
    const sessionState = deps.session.state(now);
    let rows = 0;

    let stocks: FairValue[] = [];
    try {
      stocks = await deps.premium.all({ limit: deps.config.recorderTopStocks });
    } catch (error) {
      deps.logger.warn('recorder could not read stock fair values', { reason: describe(error) });
    }

    const sourceBySymbol = new Map<string, Source>();
    for (const value of stocks) {
      if (value.referenceSource) sourceBySymbol.set(value.symbol, value.referenceSource);
      try {
        deps.store.insertObservation(stockObservation(value, now));
        rows += 1;
      } catch (error) {
        deps.logger.warn('recorder could not write a stock row', {
          symbol: value.symbol,
          reason: describe(error),
        });
      }
    }

    /**
     * Where a paired row's reference came from. The stock leg of a pair is
     * often outside the stock rows this pass wrote, and a price with no named
     * source is not worth recording.
     */
    const sourceOf = async (symbol: string): Promise<Source | undefined> => {
      try {
        const value = await deps.premium.forSymbol(symbol);
        if (value.referenceSource) sourceBySymbol.set(symbol, value.referenceSource);
        return value.referenceSource;
      } catch (error) {
        deps.logger.debug('the reference source for a stock leg could not be read', {
          symbol,
          reason: describe(error),
        });
        return undefined;
      }
    };

    for (const target of pairedTargets(deps)) {
      try {
        const quote = await deps.paired.quote(target.token, { stockSymbol: target.stockSymbol });
        const source = sourceBySymbol.get(target.stockSymbol) ?? (await sourceOf(target.stockSymbol));
        deps.store.insertObservation({
          ts: now,
          symbol: quote.symbol,
          token: target.token,
          pool: quote.pool,
          onchainMidUsd: quote.usdOnchain.value,
          referenceUsd: quote.usdFair.value,
          ...(source ? { referenceSource: source } : {}),
          premiumBps: quote.stockLegPremiumBps.value,
          sessionState,
          stockSymbol: quote.stockSymbol,
          ...(target.pool.liquidity === undefined ? {} : { liquidity: target.pool.liquidity }),
        });
        rows += 1;
      } catch (error) {
        deps.logger.warn('recorder could not write a paired row', {
          token: target.token,
          stock: target.stockSymbol,
          reason: describe(error),
        });
      }
    }

    deps.logger.info('recorder pass complete', { rows, sessionState });
    return rows;
  };

  const runGuarded = () => {
    if (inFlight) {
      deps.logger.debug('recorder pass still running, skipping this interval');
      return;
    }
    inFlight = true;
    void tick()
      .catch((error: unknown) => deps.logger.error('recorder pass failed', { reason: describe(error) }))
      .finally(() => {
        inFlight = false;
      });
  };

  return {
    start() {
      if (timer) return;
      timer = setInterval(runGuarded, deps.config.recorderIntervalSec * 1000);
      timer.unref?.();
      runGuarded();
    },

    stop() {
      if (!timer) return;
      clearInterval(timer);
      timer = undefined;
    },

    running: () => timer !== undefined,
    tick,

    async export(target, options) {
      const rows = deps.store.listObservations({
        ...(options?.since === undefined ? {} : { since: options.since }),
        ...(options?.symbol === undefined ? {} : { symbol: options.symbol }),
        limit: EXPORT_LIMIT,
      });

      const lines = [CSV_COLUMNS.join(',')];
      for (const row of rows) lines.push(csvRow(row));

      const file = path.resolve(target);
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, `${lines.join('\n')}\n`, 'utf8');
      deps.logger.info('recorder export written', { file, rows: rows.length });
      return file;
    },
  };
}

function stockObservation(value: FairValue, now: number): Observation {
  return {
    ts: now,
    symbol: value.symbol,
    token: value.token,
    ...(value.pool ? { pool: value.pool } : {}),
    ...(value.onchainMid ? { onchainMidUsd: value.onchainMid.value } : {}),
    ...(value.reference ? { referenceUsd: value.reference.value } : {}),
    ...(value.referenceSource ? { referenceSource: value.referenceSource } : {}),
    ...(value.premiumBps ? { premiumBps: value.premiumBps.value } : {}),
    sessionState: value.sessionState,
    ...(value.blockNumber === undefined ? {} : { blockNumber: value.blockNumber }),
    candidates: plainCandidates(value.candidates),
  };
}

/**
 * Candidates are stored as JSON and a bigint cannot be serialised, so the
 * block number is dropped here. The observation row carries it separately.
 */
function plainCandidates(candidates: readonly ReferenceCandidate[]): ReferenceCandidate[] {
  return candidates.map((candidate) => {
    const price = { ...candidate.price };
    delete (price as { blockNumber?: bigint }).blockNumber;
    return { ...candidate, price };
  });
}

/** Paired pools worth recording: one stock side, one non-stock side. */
function pairedTargets(deps: RecorderDeps): PairedTarget[] {
  const pools = safely(() => deps.store.listPools({ excludeTraps: true })) ?? [];
  const targets: PairedTarget[] = [];

  for (const pool of byLiquidity(pools)) {
    const stockSide = stockSideOf(deps, pool);
    if (!stockSide) continue;
    const token = otherCurrency(pool, stockSide.address);
    if (!token) continue;
    if (sameAddress(token, USDG_ADDRESS) || sameAddress(token, WETH_ADDRESS)) continue;
    if (stockSymbolOf(deps, token)) continue;
    targets.push({ token, stockSymbol: stockSide.symbol, pool });
    if (targets.length >= deps.config.recorderTopPairs) break;
  }

  return targets;
}

function stockSideOf(deps: RecorderDeps, pool: Pool): { address: Address; symbol: string } | undefined {
  for (const side of [pool.currency0, pool.currency1]) {
    const symbol = stockSymbolOf(deps, side);
    if (symbol) return { address: side.toLowerCase() as Address, symbol };
  }
  return undefined;
}

function stockSymbolOf(deps: RecorderDeps, token: Address): string | undefined {
  const known = safely(() => deps.chain.registry.token(token));
  if (known?.isStockToken) return known.symbol.toUpperCase();
  const stored = safely(() => deps.store.getToken(token));
  if (stored?.isStockToken) return stored.symbol.toUpperCase();
  return undefined;
}

function csvRow(row: Observation): string {
  const values: (string | number | undefined)[] = [
    row.ts,
    new Date(row.ts).toISOString(),
    row.symbol,
    row.token,
    row.stockSymbol,
    row.pool,
    row.onchainMidUsd,
    row.referenceUsd,
    row.referenceSource,
    row.premiumBps,
    row.sessionState,
    row.blockNumber?.toString(),
    row.liquidity?.toString(),
  ];
  return values.map(csvCell).join(',');
}

function csvCell(value: string | number | undefined): string {
  if (value === undefined || value === null) return '';
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function safely<T>(read: () => T): T | undefined {
  try {
    return read();
  } catch {
    return undefined;
  }
}
