/**
 * The two off-chain reference sources: the issuer quote and the perpetual mark.
 * Both are read once per symbol per pass, so the interesting behaviour is what
 * happens when one request in a burst is refused.
 */

import { describe, expect, it } from 'vitest';
import { createLighterMarkSource, createRhjPriceSource } from '../../src/fairvalue/sources.js';
import { UpstreamError } from '../../src/core/errors.js';

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
}

const quoteBody = {
  quotes: [{ tokenSymbol: 'GLD', bid: 406.5, ask: 407, isTradingHalt: false, generatedAt: '2026-09-04T19:00:00.000Z' }],
};

const detailsBody = {
  order_book_details: [
    { symbol: 'NVDA', market_id: 15, market_type: 'perp', mark_price: '230.87', index_price: '230.62' },
    { symbol: 'NVDA/USDG', market_id: 60, market_type: 'spot', mark_price: '230.87' },
  ],
};

describe('issuer quotes', () => {
  it('reads bid, ask, and the midpoint between them', async () => {
    const source = createRhjPriceSource({ fetchImpl: async () => jsonResponse(quoteBody) });
    const quote = await source.quote('gld');
    expect(quote?.symbol).toBe('GLD');
    expect(quote?.bid).toBe(406.5);
    expect(quote?.ask).toBe(407);
    expect(quote?.mid).toBe(406.75);
    expect(quote?.halted).toBe(false);
    expect(quote?.generatedAt).toBe(Date.parse('2026-09-04T19:00:00.000Z'));
  });

  it('holds the last good quote through a refused request', async () => {
    let calls = 0;
    let clock = 0;
    const source = createRhjPriceSource({
      ttlMs: 10,
      graceMs: 1000,
      now: () => clock,
      fetchImpl: async () => {
        calls += 1;
        if (calls === 1) return jsonResponse(quoteBody);
        return new Response('slow down', { status: 429 });
      },
    });

    expect((await source.quote('GLD'))?.ask).toBe(407);
    clock = 500;
    expect((await source.quote('GLD'))?.ask).toBe(407);
    expect(calls).toBe(2);
  });

  it('gives up once the last good quote is older than the grace window', async () => {
    let clock = 0;
    let calls = 0;
    const source = createRhjPriceSource({
      ttlMs: 10,
      graceMs: 1000,
      now: () => clock,
      fetchImpl: async () => {
        calls += 1;
        if (calls === 1) return jsonResponse(quoteBody);
        return new Response('slow down', { status: 429 });
      },
    });

    await source.quote('GLD');
    clock = 5000;
    await expect(source.quote('GLD')).rejects.toBeInstanceOf(UpstreamError);
  });
});

describe('perpetual marks', () => {
  it('keeps the perpetual markets and leaves the spot pairs out', async () => {
    const source = createLighterMarkSource({ fetchImpl: async () => jsonResponse(detailsBody) });
    const marks = await source.marks();
    expect([...marks.keys()]).toEqual(['NVDA']);
    expect((await source.mark('nvda'))?.markPrice).toBe(230.87);
    expect((await source.mark('nvda'))?.marketId).toBe(15);
  });

  it('holds the last good market list through a refused request', async () => {
    let clock = 0;
    let calls = 0;
    const source = createLighterMarkSource({
      ttlMs: 10,
      graceMs: 1000,
      now: () => clock,
      fetchImpl: async () => {
        calls += 1;
        if (calls === 1) return jsonResponse(detailsBody);
        throw new Error('connection reset');
      },
    });

    await source.marks();
    clock = 500;
    expect((await source.mark('NVDA'))?.markPrice).toBe(230.87);
    clock = 5000;
    await expect(source.marks()).rejects.toBeInstanceOf(UpstreamError);
  });
});
