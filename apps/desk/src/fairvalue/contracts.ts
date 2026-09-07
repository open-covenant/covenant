/**
 * Interfaces the rest of the desk codes against.
 *
 * The implementations live beside this file; other modules import from
 * `fairvalue/index.js` and never reach past it.
 */

import type {
  Address,
  FairValue,
  Hex32,
  PairedQuote,
  Quantity,
  ReferenceCandidate,
  SessionState,
  Source,
} from '../core/types.js';
import type { Config } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import type { Store } from '../core/store.js';
import type { ChainModule } from '../chain/index.js';

export interface FairValueDeps {
  readonly config: Config;
  readonly logger: Logger;
  readonly store: Store;
  readonly chain: ChainModule;
}

/** Where the calendar stands, and when the next regular open is. */
export interface SessionInfo {
  readonly state: SessionState;
  /** Next United States regular open, ms since epoch. */
  readonly nextOpen: number;
  /** End of the current session, ms since epoch, when one is running. */
  readonly sessionEnd?: number;
  /** Set when the day is a holiday or an early close. */
  readonly note?: string;
}

/**
 * United States equities calendar.
 *
 * The Chainlink 24/5 session runs Sunday 20:00 America/New_York to Friday
 * 20:00 America/New_York. Regular hours are 09:30 to 16:00 America/New_York on
 * a trading day. Holidays close the whole day.
 */
export interface Session {
  /** State at a point in time. `now` is ms since epoch. */
  state(now: number): SessionState;
  /** Full calendar answer at a point in time. */
  info(now: number): SessionInfo;
  /** Next regular open at or after `now`, ms since epoch. */
  nextOpen(now: number): number;
  /** True when the date falls on a market holiday. */
  isHoliday(now: number): boolean;
  /** True when `updatedAt` falls inside the session that contains `now`. */
  isWithinCurrentSession(now: number, updatedAt: number): boolean;
}

/** Selection for one symbol, with the token it belongs to and what could not answer. */
export interface ResolvedReference {
  readonly symbol: string;
  readonly token?: Address;
  readonly chosen?: ReferenceCandidate;
  readonly candidates: ReferenceCandidate[];
  /** Sources that were read and could not answer, with the reason. */
  readonly unavailable: readonly { readonly source: Source; readonly reason: string }[];
}

/** Reference selection for one stock token, USD per whole token. */
export interface Reference {
  /**
   * Selection order:
   *   1. Chainlink, when the session is not closed and the round was published
   *      inside the current session.
   *   2. Lighter Robinhood Chain perpetual mark, when the symbol is listed.
   *   3. Assets API ask times `currentMultiplier`, when trading is not halted
   *      and the quote is under 24 hours old.
   *   4. Last Chainlink price, flagged `chainlink-stale`.
   */
  select(symbol: string, now?: number): Promise<{ chosen?: ReferenceCandidate; candidates: ReferenceCandidate[] }>;
  /** Selection plus the token address and the sources that could not answer. */
  resolve(symbol: string, now?: number): Promise<ResolvedReference>;
  /** Every candidate the desk can read, whether or not it would be chosen. */
  candidates(symbol: string, now?: number): Promise<ReferenceCandidate[]>;
}

/** What a given size would pay or receive, from a live quote through one pool. */
export interface SizedQuote {
  /** USD per whole stock token for the requested size and direction. */
  readonly price: Quantity;
  readonly amountUsd: number;
  readonly side: 'buy' | 'sell';
  readonly pool: Hex32;
  readonly note: string;
}

/** On-chain price against reference price. */
export interface Premium {
  /** Fair value for one stock token. */
  forSymbol(symbol: string): Promise<FairValue>;
  /** Fair value for every stock token with a USDG pool, deepest pool first. */
  all(options?: { limit?: number }): Promise<FairValue[]>;
  /** `(onchainMid / reference - 1) * 10000`, in basis points. */
  computeBps(onchainMid: number, reference: number, asOf?: number): Quantity;
  /**
   * Price a size through the USDG pool the fair value was read from, so the
   * pool fee and the price impact of that size are in the answer. Undefined
   * when the pool or the quoter cannot answer.
   */
  sized(
    fair: FairValue,
    options: { amountUsd: number; side?: 'buy' | 'sell' },
  ): Promise<SizedQuote | undefined>;
}

/** Tokens quoted in a stock token rather than in a stablecoin. */
export interface Paired {
  /**
   * Price a paired token through its stock leg.
   * `usdOnchain = ratio x onchainMid(stock)`, `usdFair = ratio x reference(stock)`.
   * With a size, the ratio comes from a quote for that size in that direction,
   * so the pool fee and the price impact are in the number. Reports the ETH
   * route as well when an ETH pool exists, and names the cheaper entry and the
   * better exit, measured the same way on both routes.
   */
  quote(
    token: Address,
    options?: {
      stockSymbol?: string;
      /** Size to price, USD. Without it the answer is the pool mid. */
      amountUsd?: number;
      /** Direction being priced. Default buy. */
      side?: 'buy' | 'sell';
    },
  ): Promise<PairedQuote>;
  /** Every stock pool quoting this token, with a liquidity-weighted fair value. */
  quoteAll(token: Address): Promise<PairedQuote[]>;
}

/** Interval writer producing the premium dataset. */
export interface Recorder {
  /** Start the loop. Idempotent. */
  start(): void;
  stop(): void;
  running(): boolean;
  /** Run one pass now and return the number of rows written. */
  tick(): Promise<number>;
  /** Write observations to CSV. Returns the path. */
  export(target: string, options?: { since?: number; symbol?: string }): Promise<string>;
}

export interface FairValueModule {
  readonly session: Session;
  readonly reference: Reference;
  readonly premium: Premium;
  readonly paired: Paired;
  readonly recorder: Recorder;
}
