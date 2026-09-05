/**
 * What one stock token is worth in USD right now.
 *
 * Three venues answer that question and they disagree, especially when the
 * exchange is shut. The desk reads all three, names the one it used, and hands
 * back the rest so the difference between them stays visible.
 *
 * Order:
 *   1. Chainlink, while the feed is inside its publishing session.
 *   2. The Lighter Robinhood Chain perpetual mark, which prices around the
 *      clock.
 *   3. The issuer ask from the assets API, while trading is not halted and the
 *      quote is under 24 hours old.
 *   4. The last Chainlink price, flagged as held over from the last session.
 */

import type { Address, ReferenceCandidate, Source } from '../core/types.js';
import { quantity } from '../core/types.js';
import type { Logger } from '../core/logger.js';
import type { ChainModule } from '../chain/index.js';
import type { Reference, ResolvedReference, Session } from './contracts.js';
import type { LighterMarkSource, RhjPriceSource } from './sources.js';

/** An issuer quote older than this is not used as a reference. */
export const RHJ_MAX_SPREAD_BPS = 200;
const RHJ_MAX_AGE_MS = 24 * 60 * 60 * 1000;

export interface ReferenceDeps {
  readonly logger: Logger;
  readonly chain: ChainModule;
  readonly session: Session;
  readonly rhj: RhjPriceSource;
  readonly lighter: LighterMarkSource;
  readonly now?: () => number;
}

/** Reference selection with the full candidate list. */
export type DeskReference = Reference;

/** Build the reference reader. */
export function createReference(deps: ReferenceDeps): DeskReference {
  const clock = deps.now ?? Date.now;

  const resolve = async (symbol: string, at?: number): Promise<ResolvedReference> => {
    const now = at ?? clock();
    const upper = symbol.toUpperCase();
    const candidates: ReferenceCandidate[] = [];
    const unavailable: { source: Source; reason: string }[] = [];

    const token = safely(() => deps.chain.registry.token(upper));
    /**
     * Shares per token. Chainlink already publishes the adjusted price, so the
     * multiplier is applied to the two per-share sources only.
     */
    const multiplier = token?.uiMultiplier ?? 1;

    const chainlink = await readChainlink(deps, upper, now);
    if (chainlink.candidate) candidates.push(chainlink.candidate);
    else if (chainlink.reason) unavailable.push({ source: 'chainlink', reason: chainlink.reason });

    const perp = await readLighter(deps, upper, multiplier, now);
    if (perp.candidate) candidates.push(perp.candidate);
    else if (perp.reason) unavailable.push({ source: 'lighter-rh', reason: perp.reason });

    const issuer = await readRhj(deps, upper, multiplier, now);
    if (issuer.candidate) candidates.push(issuer.candidate);
    else if (issuer.reason) unavailable.push({ source: 'rhj', reason: issuer.reason });

    const fresh = candidates.find((entry) => entry.source === 'chainlink');
    const mark = candidates.find((entry) => entry.source === 'lighter-rh');
    const ask = candidates.find((entry) => entry.source === 'rhj' && !entry.stale);
    const held = candidates.find((entry) => entry.source === 'chainlink-stale');
    const chosen = fresh ?? mark ?? ask ?? held;

    return {
      symbol: upper,
      ...(token?.address ? { token: token.address } : {}),
      ...(chosen ? { chosen } : {}),
      candidates,
      unavailable,
    };
  };

  return {
    resolve,
    async select(symbol, now) {
      const resolved = await resolve(symbol, now);
      return { ...(resolved.chosen ? { chosen: resolved.chosen } : {}), candidates: resolved.candidates };
    },
    async candidates(symbol, now) {
      return (await resolve(symbol, now)).candidates;
    },
  };
}

interface CandidateRead {
  candidate?: ReferenceCandidate;
  reason?: string;
}

async function readChainlink(deps: ReferenceDeps, symbol: string, now: number): Promise<CandidateRead> {
  let feed: Address | undefined;
  try {
    feed = deps.chain.registry.feedFor(symbol);
  } catch (error) {
    return { reason: describe(error) };
  }
  if (!feed) return { reason: `No Chainlink equity feed is published on chain 4663 for ${symbol}.` };

  try {
    const round = await deps.chain.feeds.latestRoundData(feed);
    // A round that answers zero or below is not a price. Offering it would put
    // it ahead of the perpetual mark and the issuer ask, which are readable.
    if (!Number.isFinite(round.price.value) || round.price.value <= 0) {
      return {
        reason: `The Chainlink feed for ${symbol} answered ${round.price.value}, which is not a usable price.`,
      };
    }
    const inSession =
      deps.session.state(now) !== 'closed' && deps.session.isWithinCurrentSession(now, round.updatedAt);
    const source: Source = inSession ? 'chainlink' : 'chainlink-stale';
    return {
      candidate: {
        symbol,
        price: quantity(round.price.value, 'USD', source, round.updatedAt),
        source,
        updatedAt: round.updatedAt,
        ageSec: ageSeconds(now, round.updatedAt),
        stale: !inSession,
        note: inSession
          ? 'Published inside the current session.'
          : 'Held from the last session. The feed stops publishing while the market is closed.',
      },
    };
  } catch (error) {
    deps.logger.debug('chainlink reference failed', { symbol, reason: describe(error) });
    return { reason: describe(error) };
  }
}

async function readLighter(
  deps: ReferenceDeps,
  symbol: string,
  multiplier: number,
  now: number,
): Promise<CandidateRead> {
  try {
    const mark = await deps.lighter.mark(symbol);
    if (!mark) return { reason: `The Lighter Robinhood Chain instance does not list a ${symbol} perpetual.` };
    return {
      candidate: {
        symbol,
        price: quantity(mark.markPrice * multiplier, 'USD', 'lighter-rh', mark.asOf),
        source: 'lighter-rh',
        updatedAt: mark.asOf,
        ageSec: ageSeconds(now, mark.asOf),
        stale: false,
        note:
          multiplier === 1
            ? `Perpetual mark on market ${mark.marketId}, priced around the clock.`
            : `Perpetual mark on market ${mark.marketId} times the ${multiplier} share multiplier.`,
      },
    };
  } catch (error) {
    deps.logger.debug('lighter reference failed', { symbol, reason: describe(error) });
    return { reason: describe(error) };
  }
}

async function readRhj(deps: ReferenceDeps, symbol: string, multiplier: number, now: number): Promise<CandidateRead> {
  try {
    const quote = await deps.rhj.quote(symbol);
    if (!quote) return { reason: `The assets API has no quote for ${symbol}.` };
    if (quote.ask === undefined) return { reason: `The assets API quote for ${symbol} carries no ask.` };

    const ageMs = now - quote.generatedAt;
    const tooOld = ageMs > RHJ_MAX_AGE_MS;
    const notes: string[] = [];
    if (quote.halted) notes.push('The issuer has halted trading in this symbol.');
    if (tooOld) notes.push('The quote is more than 24 hours old.');

    let price = quote.ask;
    let side = 'ask';
    if (quote.bid !== undefined && quote.bid > 0) {
      const spreadBps = ((quote.ask - quote.bid) / ((quote.ask + quote.bid) / 2)) * 10_000;
      if (spreadBps <= RHJ_MAX_SPREAD_BPS) {
        price = (quote.ask + quote.bid) / 2;
        side = 'mid';
      } else {
        price = quote.bid;
        side = 'bid';
        notes.push(`The issuer ask sits ${Math.round(spreadBps)} bps above the bid, so only the bid is used.`);
      }
    }
    if (notes.length === 0) {
      notes.push(
        multiplier === 1
          ? `Issuer ${side} from the assets API.`
          : `Issuer ${side} from the assets API times the ${multiplier} share multiplier.`,
      );
    }

    return {
      candidate: {
        symbol,
        price: quantity(price * multiplier, 'USD', 'rhj', quote.generatedAt),
        source: 'rhj',
        updatedAt: quote.generatedAt,
        ageSec: ageSeconds(now, quote.generatedAt),
        stale: quote.halted || tooOld,
        note: notes.join(' '),
      },
    };
  } catch (error) {
    deps.logger.debug('assets API reference failed', { symbol, reason: describe(error) });
    return { reason: describe(error) };
  }
}

/** Age in whole seconds, never negative. */
function ageSeconds(now: number, updatedAt: number): number {
  return Math.max(0, Math.round((now - updatedAt) / 1000));
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
