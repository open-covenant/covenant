/**
 * The token registry: what exists, what prices it, and where it can be hedged.
 *
 * Three sources are merged. The Robinhood assets API lists every stock token
 * and the share multiplier each one carries. The Chainlink feed list shipped in
 * `data/` maps a symbol to its equity price feed on 4663. The Lighter Robinhood
 * Chain market list maps a symbol to a perpetual that keeps quoting after the
 * United States market closes.
 *
 * The registry answers from a local snapshot immediately and replaces it with
 * live data on the first refresh, so a desk that starts without a network still
 * knows what the tokens are.
 */

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { z } from 'zod';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { Address, Token } from '../core/types.js';
import { UpstreamError } from '../core/errors.js';

/** Where the assets API lives. No authentication, 60 requests per second. */
export const RHJ_ASSETS_URL = 'https://api.robinhood.com/rhj/assets';
/** Robinhood Chain, the only chain these tokens are deployed on. */
export const RHJ_CHAIN_ID = 4663;

/** One market on the Lighter Robinhood Chain instance. */
export interface LighterMarketSummary {
  readonly symbol: string;
  readonly marketId: number;
  readonly kind: 'perp' | 'spot';
  readonly sizeDecimals: number;
  readonly priceDecimals: number;
  readonly minBaseAmount: number;
  readonly markPrice?: number;
  readonly initialMarginFraction?: number;
}

const deploymentSchema = z.object({
  contractAddress: z.string(),
  chainId: z.number(),
  networkName: z.string().optional(),
});

const capabilitySchema = z
  .object({ whole: z.string().optional(), fractional: z.string().optional() })
  .optional();

const assetSchema = z.object({
  tokenSymbol: z.string(),
  tokenName: z.string().default(''),
  deployments: z.array(deploymentSchema).default([]),
  currentMultiplier: z.string().default(''),
  pendingMultiplier: z.string().default(''),
  status: z.string().default(''),
  logoUrl: z.string().default(''),
  tokenDecimals: z.number().default(18),
  isin: z.string().default(''),
  tradingCapabilities: z
    .object({ market: capabilitySchema, extended: capabilitySchema, overnight: capabilitySchema })
    .optional(),
});

const assetsResponseSchema = z.object({ assets: z.array(assetSchema) });

const feedSchema = z.object({
  name: z.string(),
  proxyAddress: z.string(),
  decimals: z.number().default(8),
  heartbeat: z.number().optional(),
  docs: z
    .object({ marketHours: z.string().optional(), baseAsset: z.string().optional() })
    .partial()
    .optional(),
});

const lighterBookSchema = z.object({
  symbol: z.string(),
  market_id: z.number(),
  market_type: z.string().default('perp'),
  size_decimals: z.number().optional(),
  supported_size_decimals: z.number().optional(),
  price_decimals: z.number().optional(),
  supported_price_decimals: z.number().optional(),
  min_base_amount: z.string().optional(),
  mark_price: z.string().optional(),
  default_initial_margin_fraction: z.number().optional(),
});

const lighterBooksSchema = z.object({ order_books: z.array(lighterBookSchema).default([]) });
const lighterDetailsSchema = z.object({
  order_book_details: z.array(lighterBookSchema).default([]),
});

/** A Chainlink feed published on 4663. */
export interface FeedEntry {
  readonly symbol: string;
  readonly address: Address;
  readonly decimals: number;
  readonly heartbeatSec?: number;
  /** True when the feed follows the United States equities calendar. */
  readonly equity: boolean;
}

/** The registry as the rest of the desk sees it, plus the reads only chain uses. */
export interface DeskRegistry {
  refresh(): Promise<{ tokens: number; feeds: number; markets: number }>;
  stockTokens(): Token[];
  token(symbolOrAddress: string): Token | undefined;
  feedFor(symbol: string): Address | undefined;
  lighterPerpFor(symbol: string): number | undefined;
  isStockToken(address: Address): boolean;
  lastRefreshedAt(): number | undefined;
  /** Every feed the desk knows about, equity and crypto. */
  feeds(): FeedEntry[];
  /** ETH/USD, for routes that pass through wrapped ether. */
  ethUsdFeed(): Address | undefined;
  /** USDG/USD, so USDG amounts can be reported in dollars. */
  usdgUsdFeed(): Address | undefined;
  /** Every Lighter Robinhood Chain market, perpetual and spot. */
  lighterMarkets(): LighterMarketSummary[];
  /** Addresses of every stock token, lowercased. */
  stockTokenAddresses(): Address[];
}

export interface RegistryDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  /** Overrides for tests. Defaults to the live endpoints. */
  readonly fetchImpl?: typeof fetch;
  readonly assetsUrl?: string;
  readonly lighterBaseUrl?: string;
  /** Skip the shipped snapshot. Used by tests that supply their own data. */
  readonly skipSnapshot?: boolean;
}

/** Build the registry and seed it from the files shipped with the package. */
export function createRegistry(deps: RegistryDeps): DeskRegistry {
  const logger = deps.logger.child({ component: 'chain.registry' });
  const doFetch = deps.fetchImpl ?? fetch;
  const assetsUrl = deps.assetsUrl ?? RHJ_ASSETS_URL;
  const lighterBaseUrl = deps.lighterBaseUrl ?? deps.config.hedge.lighterBaseUrl;

  const feedList = deps.skipSnapshot ? [] : loadFeedFile();
  const feedBySymbol = new Map<string, FeedEntry>();
  for (const feed of feedList) feedBySymbol.set(feed.symbol.toUpperCase(), feed);

  let byAddress = new Map<string, Token>();
  let bySymbol = new Map<string, Token>();
  let markets: LighterMarketSummary[] = [];
  let refreshedAt: number | undefined;

  const rebuild = (assets: z.infer<typeof assetSchema>[]): void => {
    const nextByAddress = new Map<string, Token>();
    const nextBySymbol = new Map<string, Token>();
    for (const asset of assets) {
      const token = toToken(asset, feedBySymbol, markets);
      if (!token) continue;
      nextByAddress.set(token.address.toLowerCase(), token);
      nextBySymbol.set(token.symbol.toUpperCase(), token);
    }
    byAddress = nextByAddress;
    bySymbol = nextBySymbol;
  };

  if (!deps.skipSnapshot) {
    try {
      rebuild(loadAssetSnapshot());
    } catch (error) {
      logger.warn('shipped asset snapshot could not be read', { reason: messageOf(error) });
    }
  }

  const persist = (): void => {
    for (const token of byAddress.values()) {
      try {
        deps.store.upsertToken(token);
      } catch (error) {
        logger.warn('token could not be stored', {
          symbol: token.symbol,
          reason: messageOf(error),
        });
      }
    }
  };

  return {
    async refresh() {
      markets = await loadLighterMarkets(doFetch, lighterBaseUrl, logger);
      let assets: z.infer<typeof assetSchema>[];
      try {
        assets = await loadAssetsLive(doFetch, assetsUrl);
      } catch (error) {
        logger.warn('assets API is unavailable, using the shipped snapshot', {
          reason: messageOf(error),
        });
        assets = deps.skipSnapshot ? [] : loadAssetSnapshot();
      }
      rebuild(assets);
      persist();
      refreshedAt = Date.now();
      logger.info('registry refreshed', {
        tokens: byAddress.size,
        feeds: feedBySymbol.size,
        markets: markets.length,
      });
      return { tokens: byAddress.size, feeds: feedBySymbol.size, markets: markets.length };
    },

    stockTokens: () => [...byAddress.values()].sort((a, b) => a.symbol.localeCompare(b.symbol)),

    stockTokenAddresses: () => [...byAddress.keys()].map((address) => address as Address),

    token(symbolOrAddress) {
      const key = symbolOrAddress.trim();
      if (key.startsWith('0x')) return byAddress.get(key.toLowerCase());
      return bySymbol.get(key.toUpperCase());
    },

    feedFor: (symbol) => feedBySymbol.get(symbol.trim().toUpperCase())?.address,

    lighterPerpFor(symbol) {
      const wanted = symbol.trim().toUpperCase();
      return markets.find(
        (market) => market.kind === 'perp' && market.symbol.toUpperCase() === wanted,
      )?.marketId;
    },

    isStockToken: (address) => byAddress.has(address.toLowerCase()),

    lastRefreshedAt: () => refreshedAt,

    feeds: () => [...feedBySymbol.values()],

    ethUsdFeed: () => feedBySymbol.get('ETH')?.address,

    usdgUsdFeed: () => feedBySymbol.get('USDG')?.address,

    lighterMarkets: () => [...markets],
  };
}

/** Resolve a path inside the package `data/` directory, from source or from `dist`. */
export function dataFile(name: string): string {
  return fileURLToPath(new URL(`../../data/${name}`, import.meta.url));
}

/** Read the Chainlink feed list shipped with the package. */
export function loadFeedFile(path = dataFile('chainlink-feeds-4663.json')): FeedEntry[] {
  const raw = JSON.parse(readFileSync(path, 'utf8')) as unknown;
  return parseFeeds(raw);
}

/**
 * Turn the Chainlink feed list into symbol lookups.
 *
 * Equity feeds are named "Robinhood NVDA / USD" or "Robinhood DELL-USD" and
 * carry `us_equities_24/5` market hours. The symbol is the word after the
 * "Robinhood" prefix, which matches the stock token symbol for all 35 equity
 * feeds published on 4663. Crypto feeds keep their own leading symbol, so
 * "ETH / USD" registers as ETH and "USDG / USD" as USDG.
 */
export function parseFeeds(raw: unknown): FeedEntry[] {
  const parsed = z.array(feedSchema).safeParse(raw);
  if (!parsed.success)
    throw new UpstreamError('chainlink-feeds', 'the feed list could not be read');
  const out: FeedEntry[] = [];
  for (const feed of parsed.data) {
    // Exchange-rate feeds price one token against another, not against the
    // dollar, so they are left out rather than shadowing the USD feed that
    // shares their leading symbol.
    if (/exchange rate/i.test(feed.name)) continue;
    const equity = feed.docs?.marketHours === 'us_equities_24/5';
    const symbol = feedSymbol(feed.name);
    if (!symbol) continue;
    out.push({
      symbol,
      address: feed.proxyAddress.toLowerCase() as Address,
      decimals: feed.decimals,
      heartbeatSec: feed.heartbeat,
      equity,
    });
  }
  return out;
}

function feedSymbol(name: string): string | undefined {
  const withoutIssuer = name.replace(/^Robinhood\s+/i, '').trim();
  const head = withoutIssuer.split(/\s*[/-]\s*/)[0]?.trim();
  if (!head || /\s/.test(head)) return undefined;
  return head.toUpperCase();
}

/** Read the asset snapshot shipped with the package. */
function loadAssetSnapshot(
  path = dataFile('rhj-assets-snapshot.json'),
): z.infer<typeof assetSchema>[] {
  const raw = JSON.parse(readFileSync(path, 'utf8')) as unknown;
  const parsed = assetsResponseSchema.safeParse(raw);
  if (!parsed.success)
    throw new UpstreamError('rhj-assets', 'the asset snapshot could not be read');
  return parsed.data.assets;
}

async function loadAssetsLive(
  doFetch: typeof fetch,
  url: string,
): Promise<z.infer<typeof assetSchema>[]> {
  const response = await doFetch(url, { signal: AbortSignal.timeout(20_000) });
  if (!response.ok) throw new UpstreamError('rhj-assets', `${url} answered ${response.status}`);
  const parsed = assetsResponseSchema.safeParse(await response.json());
  if (!parsed.success) throw new UpstreamError('rhj-assets', `${url} returned an unexpected shape`);
  return parsed.data.assets;
}

/**
 * Read the Lighter Robinhood Chain market list.
 *
 * `/api/v1/orderBooks` lists every market, perpetual and spot. `/api/v1/
 * orderBookDetails` covers the perpetuals only and adds the mark price and the
 * margin fraction, so both are read and merged. A failure here is not fatal:
 * the desk keeps working with Chainlink and the assets API as references.
 */
export async function loadLighterMarkets(
  doFetch: typeof fetch,
  baseUrl: string,
  logger: Logger,
): Promise<LighterMarketSummary[]> {
  const byId = new Map<number, LighterMarketSummary>();

  const ingest = (book: z.infer<typeof lighterBookSchema>): void => {
    const kind = book.market_type === 'spot' ? 'spot' : 'perp';
    const existing = byId.get(book.market_id);
    const summary: LighterMarketSummary = {
      symbol: book.symbol,
      marketId: book.market_id,
      kind,
      sizeDecimals: book.size_decimals ?? book.supported_size_decimals ?? 4,
      priceDecimals: book.price_decimals ?? book.supported_price_decimals ?? 2,
      minBaseAmount: Number(book.min_base_amount ?? '0'),
      markPrice: book.mark_price === undefined ? existing?.markPrice : Number(book.mark_price),
      initialMarginFraction:
        book.default_initial_margin_fraction ?? existing?.initialMarginFraction,
    };
    byId.set(book.market_id, summary);
  };

  for (const [path, schema] of [
    ['/api/v1/orderBooks', lighterBooksSchema],
    ['/api/v1/orderBookDetails', lighterDetailsSchema],
  ] as const) {
    try {
      const response = await doFetch(`${baseUrl}${path}`, { signal: AbortSignal.timeout(20_000) });
      if (!response.ok)
        throw new UpstreamError('lighter-rh', `${path} answered ${response.status}`);
      const parsed = schema.safeParse(await response.json());
      if (!parsed.success)
        throw new UpstreamError('lighter-rh', `${path} returned an unexpected shape`);
      const books =
        'order_books' in parsed.data ? parsed.data.order_books : parsed.data.order_book_details;
      for (const book of books) ingest(book);
    } catch (error) {
      logger.warn('lighter market list is unavailable', { path, reason: messageOf(error) });
    }
  }

  return [...byId.values()].sort((a, b) => a.marketId - b.marketId);
}

function toToken(
  asset: z.infer<typeof assetSchema>,
  feeds: Map<string, FeedEntry>,
  markets: readonly LighterMarketSummary[],
): Token | undefined {
  const deployment = asset.deployments.find((entry) => entry.chainId === RHJ_CHAIN_ID);
  if (!deployment) return undefined;
  const symbol = asset.tokenSymbol.toUpperCase();
  const perp = markets.find(
    (market) => market.kind === 'perp' && market.symbol.toUpperCase() === symbol,
  );
  return {
    address: deployment.contractAddress.toLowerCase() as Address,
    symbol: asset.tokenSymbol,
    name: asset.tokenName,
    decimals: asset.tokenDecimals,
    isStockToken: true,
    uiMultiplier: numberOrUndefined(asset.currentMultiplier),
    pendingMultiplier: numberOrUndefined(asset.pendingMultiplier),
    feed: feeds.get(symbol)?.address,
    lighterMarketId: perp?.marketId,
    tradingCapabilities: {
      market: tradable(asset.tradingCapabilities?.market),
      extended: tradable(asset.tradingCapabilities?.extended),
      overnight: tradable(asset.tradingCapabilities?.overnight),
    },
    isin: asset.isin || undefined,
    logoUrl: asset.logoUrl || undefined,
  };
}

function tradable(capability: { whole?: string; fractional?: string } | undefined): boolean {
  if (!capability) return false;
  return (
    capability.whole === 'TRADING_STATUS_TRADABLE' ||
    capability.fractional === 'TRADING_STATUS_TRADABLE'
  );
}

function numberOrUndefined(value: string): number | undefined {
  if (!value) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
