import { describe, expect, it } from 'vitest';
import { silentLogger } from '../../src/core/logger.js';
import { createSession } from '../../src/fairvalue/session.js';
import { createReference } from '../../src/fairvalue/reference.js';
import type { Address } from '../../src/core/types.js';
import { AAPL, lighterSource, makeChain, makeRound, makeToken, rhjSource } from './helpers.js';

const FEED = '0x6b22a786baa607d76728168703a39ea9c99f2cd0' as Address;
const MULTIPLIER = 1.000566;

/** Tuesday 2026-09-15, 11:00 New York time. The feed is publishing. */
const DURING_SESSION = Date.UTC(2026, 8, 15, 15);
/** Saturday 2026-09-19, 08:00 New York time. The feed is frozen. */
const WEEKEND = Date.UTC(2026, 8, 19, 12);
/** Friday 2026-09-18, 16:00 New York time. The last round of the week. */
const LAST_ROUND = Date.UTC(2026, 8, 18, 20);

const session = createSession();

interface Fixture {
  feed?: Address;
  round?: { price: number; updatedAt: number };
  feedFails?: boolean;
  mark?: number;
  ask?: number;
  halted?: boolean;
  quotedAt?: number;
}

function build(fixture: Fixture) {
  const chain = makeChain({
    registry: {
      token: (symbolOrAddress) =>
        symbolOrAddress.toUpperCase() === 'AAPL'
          ? makeToken({ address: AAPL, symbol: 'AAPL', uiMultiplier: MULTIPLIER })
          : undefined,
      feedFor: () => fixture.feed,
    },
    feeds: {
      latestRoundData: async () => {
        if (fixture.feedFails) throw new Error('RPC timed out');
        const round = fixture.round ?? { price: 320.52, updatedAt: LAST_ROUND };
        return makeRound(round.price, round.updatedAt);
      },
    },
  });

  return createReference({
    logger: silentLogger(),
    chain,
    session,
    rhj: rhjSource({
      AAPL:
        fixture.ask === undefined
          ? undefined
          : {
              symbol: 'AAPL',
              bid: fixture.ask - 0.03,
              ask: fixture.ask,
              mid: fixture.ask - 0.015,
              halted: fixture.halted ?? false,
              generatedAt: fixture.quotedAt ?? WEEKEND - 60_000,
            },
    }),
    lighter: lighterSource({
      AAPL:
        fixture.mark === undefined
          ? undefined
          : { symbol: 'AAPL', marketId: 10, markPrice: fixture.mark, asOf: WEEKEND - 5_000 },
    }),
  });
}

describe('reference selection', () => {
  it('takes Chainlink while the feed is publishing', async () => {
    const reference = build({ feed: FEED, round: { price: 320.52, updatedAt: DURING_SESSION - 60_000 }, mark: 321, ask: 321.34 });
    const { chosen, candidates } = await reference.select('AAPL', DURING_SESSION);

    expect(chosen?.source).toBe('chainlink');
    expect(chosen?.price.value).toBe(320.52);
    expect(chosen?.stale).toBe(false);
    expect(candidates.map((entry) => entry.source)).toEqual(['chainlink', 'lighter-rh', 'rhj']);
  });

  it('takes the perpetual mark once the feed is frozen', async () => {
    const reference = build({ feed: FEED, mark: 318.4, ask: 321.34 });
    const { chosen, candidates } = await reference.select('AAPL', WEEKEND);

    expect(chosen?.source).toBe('lighter-rh');
    expect(chosen?.price.value).toBeCloseTo(318.4 * MULTIPLIER, 6);
    expect(candidates[0]?.source).toBe('chainlink-stale');
    expect(candidates[0]?.stale).toBe(true);
  });

  it('falls to the issuer ask when the venue does not list the symbol', async () => {
    const reference = build({ feed: FEED, ask: 321.34 });
    const resolved = await reference.resolve('AAPL', WEEKEND);

    expect(resolved.chosen?.source).toBe('rhj');
    expect(resolved.chosen?.price.value).toBeCloseTo(321.34 * MULTIPLIER, 6);
    expect(resolved.unavailable.map((entry) => entry.source)).toContain('lighter-rh');
  });

  it('refuses a halted issuer quote and holds the last Chainlink price', async () => {
    const reference = build({ feed: FEED, ask: 321.34, halted: true });
    const { chosen } = await reference.select('AAPL', WEEKEND);

    expect(chosen?.source).toBe('chainlink-stale');
    expect(chosen?.stale).toBe(true);
    expect(chosen?.note).toContain('Held from the last session');
  });

  it('refuses an issuer quote older than 24 hours', async () => {
    const reference = build({ feed: FEED, ask: 321.34, quotedAt: WEEKEND - 30 * 60 * 60 * 1000 });
    const { chosen, candidates } = await reference.select('AAPL', WEEKEND);

    expect(chosen?.source).toBe('chainlink-stale');
    expect(candidates.find((entry) => entry.source === 'rhj')?.note).toContain('24 hours');
  });

  it('leaves a feed answering zero out and takes the perpetual mark instead', async () => {
    const reference = build({
      feed: FEED,
      round: { price: 0, updatedAt: DURING_SESSION - 60_000 },
      mark: 318.4,
      ask: 321.34,
    });
    const resolved = await reference.resolve('AAPL', DURING_SESSION);

    expect(resolved.chosen?.source).toBe('lighter-rh');
    expect(resolved.candidates.map((entry) => entry.source)).not.toContain('chainlink');
    expect(resolved.unavailable.find((entry) => entry.source === 'chainlink')?.reason).toContain('0');
  });

  it('returns no choice when nothing can answer', async () => {
    const reference = build({ ask: 321.34, halted: true });
    const resolved = await reference.resolve('AAPL', WEEKEND);

    expect(resolved.chosen).toBeUndefined();
    expect(resolved.candidates).toHaveLength(1);
    expect(resolved.unavailable.map((entry) => entry.source)).toEqual(['chainlink', 'lighter-rh']);
  });

  it('keeps one failing source from hiding the others', async () => {
    const reference = build({ feed: FEED, feedFails: true, mark: 318.4, ask: 321.34 });
    const resolved = await reference.resolve('AAPL', WEEKEND);

    expect(resolved.chosen?.source).toBe('lighter-rh');
    expect(resolved.candidates).toHaveLength(2);
    expect(resolved.unavailable[0]).toMatchObject({ source: 'chainlink', reason: 'RPC timed out' });
  });

  it('reports the age of every candidate', async () => {
    const reference = build({ feed: FEED, mark: 318.4, ask: 321.34 });
    const candidates = await reference.candidates('AAPL', WEEKEND);
    const chainlink = candidates.find((entry) => entry.source === 'chainlink-stale');

    expect(chainlink?.ageSec).toBe(Math.round((WEEKEND - LAST_ROUND) / 1000));
    expect(candidates.every((entry) => entry.ageSec >= 0)).toBe(true);
  });
});
