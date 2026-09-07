/**
 * Fixtures for the hedge tests.
 *
 * The market rows are the shapes `https://api.rh.lighter.xyz` returned on
 * 2026-09-04, trimmed to the markets the tests use. Serving them through the
 * read client's own transport keeps the parsing under test rather than mocked.
 */

import type { HttpTransport } from 'lighter-agent-sdk';
import { defaultConfig, type Config } from '../../src/core/config.js';
import type { KeyName, Keystore } from '../../src/core/keystore.js';
import { silentLogger } from '../../src/core/logger.js';
import { openStore, type Store } from '../../src/core/store.js';
import { quantity } from '../../src/core/types.js';
import { createFairValueModule, type FairValueModule } from '../../src/fairvalue/index.js';

export const NVDA_MARKET_ID = 15;
export const AAPL_MARKET_ID = 10;
export const SPY_MARKET_ID = 26;
export const AI_MARKET_ID = 45;

interface MarketRow {
  symbol: string;
  market_id: number;
  market_type: 'perp' | 'spot';
  status: string;
  min_base_amount: string;
  min_quote_amount: string;
  order_quote_limit: string;
  supported_size_decimals: number;
  supported_price_decimals: number;
  supported_quote_decimals: number;
  taker_fee: string;
  maker_fee: string;
  size_decimals: number;
  price_decimals: number;
}

function market(
  symbol: string,
  marketId: number,
  sizeDecimals: number,
  priceDecimals: number,
  minBaseAmount: string,
  marketType: 'perp' | 'spot' = 'perp',
): MarketRow {
  return {
    symbol,
    market_id: marketId,
    market_type: marketType,
    status: 'active',
    min_base_amount: minBaseAmount,
    min_quote_amount: '10.000000',
    order_quote_limit: '25000000.000000',
    supported_size_decimals: sizeDecimals,
    supported_price_decimals: priceDecimals,
    supported_quote_decimals: 6,
    taker_fee: '0.0000',
    maker_fee: '0.0000',
    size_decimals: sizeDecimals,
    price_decimals: priceDecimals,
  };
}

export const MARKET_ROWS: MarketRow[] = [
  market('NVDA', NVDA_MARKET_ID, 4, 2, '0.0400'),
  market('AAPL', AAPL_MARKET_ID, 4, 2, '0.0200'),
  market('SPY', SPY_MARKET_ID, 4, 2, '0.0100'),
  market('AI', AI_MARKET_ID, 1, 5, '10.0000'),
  market('AAPL/USDG', 2049, 4, 2, '0.0200', 'spot'),
];

export const MARK_PRICES: Record<number, number> = {
  [NVDA_MARKET_ID]: 230.72,
  [AAPL_MARKET_ID]: 258.4,
  [SPY_MARKET_ID]: 682.15,
  [AI_MARKET_ID]: 0.10422,
};

/** Eight-hour funding as a fraction of notional, keyed by market id. */
export const FUNDING_FRACTIONS: Record<number, number> = {
  [NVDA_MARKET_ID]: 0.000032,
  [AAPL_MARKET_ID]: -0.0001,
  [SPY_MARKET_ID]: 0.00002979,
};

export interface AccountPositionRow {
  market_id: number;
  symbol: string;
  /** `1` for a long, `-1` for a short. */
  sign: number;
  /** Magnitude, as the venue reports it. */
  position: string;
  avg_entry_price: string;
  position_value: string;
  unrealized_pnl: string;
  realized_pnl: string;
  liquidation_price: string;
}

export interface FixtureState {
  positions: AccountPositionRow[];
  collateral: number;
}

export function shortPosition(
  symbol: string,
  marketId: number,
  size: number,
  entry: number,
): AccountPositionRow {
  const mark = MARK_PRICES[marketId] ?? entry;
  return {
    market_id: marketId,
    symbol,
    sign: -1,
    position: size.toFixed(4),
    avg_entry_price: entry.toFixed(2),
    position_value: (size * mark).toFixed(6),
    unrealized_pnl: ((entry - mark) * size).toFixed(6),
    realized_pnl: '0.000000',
    liquidation_price: '0.000000',
  };
}

/**
 * A transport that answers the routes the hedge client calls.
 *
 * `state` is read on every request, so a test can move a position and call the
 * client again without rebuilding anything.
 */
export function fixtureTransport(state: FixtureState = { positions: [], collateral: 5000 }): HttpTransport {
  return async (request) => {
    const url = new URL(request.url);
    const json = (body: unknown) => ({
      status: 200,
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });

    if (url.pathname.endsWith('/orderBooks')) {
      return json({ code: 200, order_books: MARKET_ROWS });
    }

    if (url.pathname.endsWith('/orderBookDetails')) {
      const requested = url.searchParams.get('market_id');
      const rows = MARKET_ROWS.filter(
        (row) =>
          row.market_type === 'perp' && (requested === null || row.market_id === Number(requested)),
      ).map((row) => ({
        ...row,
        default_initial_margin_fraction: 5000,
        mark_price: (MARK_PRICES[row.market_id] ?? 1).toFixed(row.price_decimals),
        index_price: (MARK_PRICES[row.market_id] ?? 1).toFixed(row.price_decimals),
        last_trade_price: MARK_PRICES[row.market_id] ?? 1,
        open_interest: 1000,
        daily_trades_count: 100,
        daily_base_token_volume: 1000,
        daily_quote_token_volume: 100000,
        daily_price_change: 0.1,
      }));
      return json({ code: 200, order_book_details: rows, spot_order_book_details: [] });
    }

    if (url.pathname.endsWith('/funding-rates')) {
      const rates = Object.entries(FUNDING_FRACTIONS).flatMap(([marketId, rate]) => {
        const row = MARKET_ROWS.find((entry) => entry.market_id === Number(marketId));
        if (row === undefined) return [];
        return [
          { market_id: Number(marketId), exchange: 'binance', symbol: row.symbol, rate: rate * 2 },
          { market_id: Number(marketId), exchange: 'lighter', symbol: row.symbol, rate },
        ];
      });
      return json({ code: 200, funding_rates: rates });
    }

    if (url.pathname.endsWith('/account')) {
      return json({
        code: 200,
        accounts: [
          {
            account_index: Number(url.searchParams.get('value') ?? 0),
            l1_address: '0x0000000000000000000000000000000000000001',
            collateral: state.collateral.toFixed(6),
            available_balance: (state.collateral * 0.8).toFixed(6),
            total_asset_value: state.collateral.toFixed(6),
            cross_initial_margin_requirement: '0.000000',
            cross_maintenance_margin_requirement: '0.000000',
            positions: state.positions,
          },
        ],
      });
    }

    return { status: 404, headers: {}, body: JSON.stringify({ code: 404, message: url.pathname }) };
  };
}

/** Margin fractions in the shape the details route publishes them. */
export async function fixtureFetchJson(): Promise<unknown> {
  return {
    order_book_details: MARKET_ROWS.filter((row) => row.market_type === 'perp').map((row) => ({
      market_id: row.market_id,
      default_initial_margin_fraction: 5000,
    })),
  };
}

/** A keystore holding exactly the values a test supplies. */
export function fixtureKeystore(values: Partial<Record<KeyName, string>> = {}): Keystore {
  const entries = new Map(Object.entries(values) as [KeyName, string][]);
  return {
    get: (name) => entries.get(name),
    require: (name) => {
      const value = entries.get(name);
      if (value === undefined) throw new Error(`${name} is not set`);
      return value;
    },
    has: (name) => entries.has(name),
    names: () => [...entries.keys()],
    secrets: () => [...entries.values()],
  };
}

/** Credentials that look real enough to reach the signing path. */
export const FULL_CREDENTIALS: Partial<Record<KeyName, string>> = {
  DESK_LIGHTER_RH_PRIVATE_KEY: `0x${'11'.repeat(40)}`,
  DESK_LIGHTER_RH_ACCOUNT_INDEX: '4242',
  DESK_LIGHTER_RH_API_KEY_INDEX: '4',
};

export function fixtureConfig(overrides: Partial<Config> = {}): Config {
  return defaultConfig({
    token: 'x'.repeat(32),
    live: true,
    acknowledgedRestrictions: true,
    ...overrides,
  });
}

export function fixtureStore(): Store {
  return openStore({ path: ':memory:' });
}

export const fixtureLogger = silentLogger;

/**
 * A fair value module that answers reference prices from a table and refuses
 * everything else. Only `reference.select` is reachable from the hedge module.
 */
export function fixtureFairValue(prices: Record<string, number>): FairValueModule {
  const base = createFairValueModule({
    config: fixtureConfig(),
    logger: silentLogger(),
    store: {} as never,
    chain: {} as never,
  });
  return {
    ...base,
    reference: {
      async candidates(symbol: string, now = Date.now()) {
        const price = prices[symbol.toUpperCase()];
        if (price === undefined) return [];
        return [
          {
            symbol: symbol.toUpperCase(),
            price: quantity(price, 'USD', 'chainlink', now),
            source: 'chainlink' as const,
            updatedAt: now,
            ageSec: 0,
            stale: false,
          },
        ];
      },
      async select(symbol: string, now = Date.now()) {
        const candidates = await this.candidates(symbol, now);
        return { ...(candidates[0] === undefined ? {} : { chosen: candidates[0] }), candidates };
      },
      async resolve(symbol: string, now = Date.now()) {
        const candidates = await this.candidates(symbol, now);
        return {
          symbol: symbol.toUpperCase(),
          ...(candidates[0] === undefined ? {} : { chosen: candidates[0] }),
          candidates,
          unavailable: [],
        };
      },
    },
  };
}
