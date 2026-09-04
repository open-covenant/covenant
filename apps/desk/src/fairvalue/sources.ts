/**
 * Off-chain reference sources.
 *
 * Two prices the desk cannot read from chain 4663: the issuer quote from the
 * Robinhood assets API, and the perpetual mark from the Lighter Robinhood
 * Chain instance. Both are behind small interfaces so tests can supply their
 * own numbers and so the hedge module can share one client later.
 */

import { UpstreamError } from '../core/errors.js';

/** Default Robinhood assets API host. */
export const RHJ_BASE_URL = 'https://api.robinhood.com';
/** How long a quote is reused before the source fetches again, ms. */
const DEFAULT_TTL_MS = 10_000;
/** How long a request waits before it is abandoned, ms. */
const DEFAULT_TIMEOUT_MS = 5_000;
/**
 * How long the last good answer stands in after a failed request, ms. A name
 * whose only reference is the issuer quote would otherwise lose its price
 * entirely because one request out of eighty was refused.
 */
const DEFAULT_GRACE_MS = 5 * 60_000;

/** Minimal fetch shape, so tests can pass a function instead of a server. */
export type FetchLike = (input: string, init?: { signal?: AbortSignal }) => Promise<Response>;

/** One issuer quote. Prices are per share, before the ERC-8056 multiplier. */
export interface RhjQuote {
  readonly symbol: string;
  readonly bid?: number;
  readonly ask?: number;
  /** Midpoint of bid and ask when both are published. */
  readonly mid?: number;
  /** True while the issuer has halted trading in this symbol. */
  readonly halted: boolean;
  /** Publication time, ms since epoch. */
  readonly generatedAt: number;
}

/** Issuer quotes from `GET /rhj/prices/{symbol}`. */
export interface RhjPriceSource {
  quote(symbol: string): Promise<RhjQuote | undefined>;
}

/** One perpetual market on the Lighter Robinhood Chain instance. */
export interface LighterMark {
  readonly symbol: string;
  readonly marketId: number;
  /** Mark price, USD per share. */
  readonly markPrice: number;
  /** Index price, USD per share, when the venue publishes one. */
  readonly indexPrice?: number;
  /** Read time, ms since epoch. */
  readonly asOf: number;
}

/** Perpetual marks from `GET /api/v1/orderBookDetails`. */
export interface LighterMarkSource {
  mark(symbol: string): Promise<LighterMark | undefined>;
  /** Every perpetual market the venue lists, keyed by symbol. */
  marks(): Promise<Map<string, LighterMark>>;
}

export interface SourceOptions {
  readonly baseUrl?: string;
  readonly fetchImpl?: FetchLike;
  readonly ttlMs?: number;
  readonly timeoutMs?: number;
  /** How long the last good answer stands in after a failed request, ms. */
  readonly graceMs?: number;
  readonly now?: () => number;
}

/** Read issuer quotes, holding each symbol for a few seconds between calls. */
export function createRhjPriceSource(options: SourceOptions = {}): RhjPriceSource {
  const baseUrl = (options.baseUrl ?? RHJ_BASE_URL).replace(/\/$/, '');
  const ttlMs = options.ttlMs ?? DEFAULT_TTL_MS;
  const now = options.now ?? Date.now;
  const graceMs = options.graceMs ?? DEFAULT_GRACE_MS;
  const cache = new Map<string, { at: number; quote: RhjQuote | undefined }>();

  return {
    async quote(symbol) {
      const key = symbol.toUpperCase();
      const cached = cache.get(key);
      if (cached && now() - cached.at < ttlMs) return cached.quote;

      let body: unknown;
      try {
        body = await getJson(`${baseUrl}/rhj/prices/${encodeURIComponent(key)}`, 'assets API', options);
      } catch (error) {
        if (cached && now() - cached.at < graceMs) return cached.quote;
        throw error;
      }
      const quotes = Array.isArray((body as { quotes?: unknown }).quotes) ? (body as { quotes: unknown[] }).quotes : [];
      const first = quotes.find(
        (entry): entry is Record<string, unknown> => typeof entry === 'object' && entry !== null,
      );

      let quote: RhjQuote | undefined;
      if (first) {
        const bid = toNumber(first.bid);
        const ask = toNumber(first.ask);
        const generatedAt = Date.parse(String(first.generatedAt ?? ''));
        quote = {
          symbol: typeof first.tokenSymbol === 'string' ? first.tokenSymbol : key,
          ...(bid === undefined ? {} : { bid }),
          ...(ask === undefined ? {} : { ask }),
          ...(bid !== undefined && ask !== undefined ? { mid: (bid + ask) / 2 } : {}),
          halted: first.isTradingHalt === true,
          generatedAt: Number.isFinite(generatedAt) ? generatedAt : now(),
        };
      }

      cache.set(key, { at: now(), quote });
      return quote;
    },
  };
}

/** Read perpetual marks. One call covers every listed market. */
export function createLighterMarkSource(options: SourceOptions = {}): LighterMarkSource {
  const baseUrl = (options.baseUrl ?? 'https://api.rh.lighter.xyz').replace(/\/$/, '');
  const ttlMs = options.ttlMs ?? DEFAULT_TTL_MS;
  const graceMs = options.graceMs ?? DEFAULT_GRACE_MS;
  const now = options.now ?? Date.now;
  let cache: { at: number; marks: Map<string, LighterMark> } | undefined;

  const load = async (): Promise<Map<string, LighterMark>> => {
    if (cache && now() - cache.at < ttlMs) return cache.marks;

    let body: unknown;
    try {
      body = await getJson(`${baseUrl}/api/v1/orderBookDetails`, 'Lighter Robinhood Chain', options);
    } catch (error) {
      if (cache && now() - cache.at < graceMs) return cache.marks;
      throw error;
    }
    const details = (body as { order_book_details?: unknown }).order_book_details;
    const marks = new Map<string, LighterMark>();
    const asOf = now();

    if (Array.isArray(details)) {
      for (const entry of details) {
        if (typeof entry !== 'object' || entry === null) continue;
        const row = entry as Record<string, unknown>;
        if (row.market_type !== 'perp') continue;
        const symbol = typeof row.symbol === 'string' ? row.symbol.toUpperCase() : undefined;
        const marketId = toNumber(row.market_id);
        const markPrice = toNumber(row.mark_price);
        if (symbol === undefined || marketId === undefined || markPrice === undefined || markPrice <= 0) continue;
        const indexPrice = toNumber(row.index_price);
        marks.set(symbol, {
          symbol,
          marketId,
          markPrice,
          ...(indexPrice === undefined ? {} : { indexPrice }),
          asOf,
        });
      }
    }

    cache = { at: asOf, marks };
    return marks;
  };

  return {
    marks: load,
    async mark(symbol) {
      return (await load()).get(symbol.toUpperCase());
    },
  };
}

async function getJson(url: string, service: string, options: SourceOptions): Promise<unknown> {
  const fetchImpl = options.fetchImpl ?? (globalThis.fetch as FetchLike | undefined);
  if (!fetchImpl) throw new UpstreamError(service, 'This runtime has no fetch implementation.');

  let response: Response;
  try {
    response = await fetchImpl(url, { signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_TIMEOUT_MS) });
  } catch (error) {
    throw new UpstreamError(service, `Request to ${url} failed: ${(error as Error).message}`, { url });
  }
  if (!response.ok) {
    throw new UpstreamError(service, `${url} answered ${response.status}.`, { url, status: response.status });
  }
  try {
    return await response.json();
  } catch (error) {
    throw new UpstreamError(service, `${url} did not return JSON: ${(error as Error).message}`, { url });
  }
}

function toNumber(value: unknown): number | undefined {
  if (typeof value === 'number') return Number.isFinite(value) ? value : undefined;
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}
