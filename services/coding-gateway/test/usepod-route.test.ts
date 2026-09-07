import { describe, expect, it } from 'vitest';
import { providerReceipt } from '../src/usepod-http.js';

/**
 * UsePod reports which tier served a request in `x-pod-route`. The gateway asks
 * for `marketplace-only` routing, and this check is the second lock on that:
 * a centralized answer must fail even if the routing header were ignored.
 */
function reply(route: string | undefined): Response {
  const headers = new Headers({ 'x-balance-remaining': '4694193' });
  if (route !== undefined) headers.set('x-pod-route', route);
  return new Response('{}', { status: 200, headers });
}

describe('UsePod route acceptance', () => {
  it('accepts per-request, which is what the marketplace answers today', () => {
    // Observed live on 2026-09-06: marketplace-only routing returns
    // x-pod-route: per-request with x-pod-provider-id: c0mpute.
    expect(() => providerReceipt(reply('per-request'), 'supergemma4-26b', '1')).not.toThrow();
  });

  it('still accepts the older marketplace label', () => {
    expect(() => providerReceipt(reply('marketplace'), 'supergemma4-26b', '1')).not.toThrow();
  });

  it('rejects surplus, which is the centralized tier', () => {
    // Observed live: default routing returns x-pod-route: surplus. That is a
    // hosted provider, not a marketplace seller, and must never be accepted.
    expect(() => providerReceipt(reply('surplus'), 'deepseek-v3-2', '1')).toThrow(
      /unacceptable route: surplus/,
    );
  });

  it('rejects a route it has never seen rather than assuming it is safe', () => {
    expect(() => providerReceipt(reply('something-new'), 'm', '1')).toThrow(/unacceptable route/);
  });

  it('rejects a missing route header', () => {
    expect(() => providerReceipt(reply(undefined), 'm', '1')).toThrow(
      /unacceptable route: missing/,
    );
  });

  it('is case and whitespace insensitive, as headers vary', () => {
    expect(() => providerReceipt(reply('  Per-Request '), 'm', '1')).not.toThrow();
  });
});

describe('a receipt the gateway writes must survive its own reload', () => {
  it('accepts every route it is willing to write', async () => {
    // The write path and the persisted-record validator drifted apart once:
    // providerReceipt() accepted `per-request` while isProviderReceipt() still
    // required `marketplace`, so the gateway wrote a record it then refused to
    // load, and crash-looped on boot. They now share one predicate.
    const { isMarketplaceRoute } = await import('../src/usepod-http.js');
    for (const route of ['marketplace', 'per-request'] as const) {
      const receipt = providerReceipt(
        new Response('{}', {
          status: 200,
          headers: { 'x-pod-route': route, 'x-balance-remaining': '3944191' },
        }),
        'supergemma4-26b',
        '1',
      );
      expect(isMarketplaceRoute(receipt.route)).toBe(true);
    }
  });
});
