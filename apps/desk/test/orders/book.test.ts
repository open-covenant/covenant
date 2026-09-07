import { afterEach, describe, expect, it } from 'vitest';
import { isDeskError } from '../../src/core/errors.js';
import { createCachedSnapshotProvider, subjectToken } from '../../src/orders/index.js';
import type { Order } from '../../src/core/types.js';
import type { SnapshotProvider } from '../../src/orders/index.js';
import { AI, buyAi, harness, HUNDRED_USDG, NVDA, T0, USDG, type Harness } from './harness.js';

/** A stored order, only as much of one as a snapshot provider reads. */
const ORDER_FOR_CACHE: Order = {
  id: 'ord_cache',
  kind: 'limit',
  side: 'buy',
  tokenIn: USDG,
  tokenOut: AI,
  amountIn: HUNDRED_USDG,
  trigger: { priceLte: 1 },
  bounds: { maxSlippageBps: 100, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 },
  live: false,
  status: 'open',
  createdAt: T0,
  expiresAt: null,
  parentId: null,
};

const open: Harness[] = [];

function desk(overrides: Parameters<typeof harness>[0] = {}): Harness {
  const created = harness(overrides);
  open.push(created);
  return created;
}

afterEach(() => {
  for (const created of open.splice(0)) created.close();
});

/** The error code a refusal carries, so a test can name it without matching prose. */
function code(error: unknown): string {
  return isDeskError(error) ? error.code : `not a desk error: ${String(error)}`;
}

async function refusal(promise: Promise<unknown>): Promise<{ code: string; reason: string }> {
  try {
    await promise;
    throw new Error('the order was accepted');
  } catch (error) {
    if (!isDeskError(error)) throw error;
    return { code: error.code, reason: error.reason };
  }
}

describe('creating an order', () => {
  it('stores the order, the bounds, and an audit row', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi());
    expect(order).toBeDefined();
    if (!order) return;

    expect(order.status).toBe('open');
    expect(order.kind).toBe('limit');
    expect(order.live).toBe(false);
    expect(order.parentId).toBeNull();
    expect(order.amountIn).toBe(HUNDRED_USDG);
    expect(order.bounds).toEqual({
      maxSlippageBps: 100,
      maxOrderNotionalUsd: 250,
      maxBuyPremiumBps: 500,
    });
    expect(d.store.getOrder(order.id)?.trigger).toEqual({ priceLte: 2.5 });

    const events = d.store.listEvents({ subject: order.id });
    expect(events.map((event) => event.kind)).toEqual(['order_created']);
  });

  it('accepts a raw amount written as a string, because JSON has no bigint', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi({ amountIn: '100000000' }) as never);
    expect(order?.amountIn).toBe(100_000_000n);
  });

  it('applies a tighter bound from the caller', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi({ bounds: { maxOrderNotionalUsd: 50 } }));
    expect(order?.bounds.maxOrderNotionalUsd).toBe(50);
  });

  it('refuses a bound wider than the configured cap and names both numbers', async () => {
    const d = desk();
    const { code: errorCode, reason } = await refusal(
      d.orders.book.create(buyAi({ bounds: { maxOrderNotionalUsd: 5000 } })),
    );
    expect(errorCode).toBe('bounds_exceeded');
    expect(reason).toContain('250');
    expect(reason).toContain('5000');
  });

  it('refuses a swap of a token for itself', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ tokenOut: USDG })));
    expect(reason).toContain('tokenOut');
    expect(reason).toContain('two different tokens');
  });

  it('refuses an amount of zero', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ amountIn: 0n })));
    expect(reason).toContain('amountIn');
  });

  it('refuses a trigger with nothing in it', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ trigger: {} })));
    expect(reason).toContain('at least one condition');
  });

  it('refuses a limit order with no price condition', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ trigger: { premiumLteBps: 0 } })));
    expect(reason).toContain('priceLte or priceGte');
  });

  it('refuses a band no price can satisfy', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ trigger: { priceGte: 4, priceLte: 2 } })));
    expect(reason).toContain('no price can satisfy both');
  });

  it('refuses an expiry that has already passed', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(buyAi({ expiresAt: T0 - 1000 })));
    expect(reason).toContain('already passed');
  });

  it('refuses an order that would expire before it could fire', async () => {
    const d = desk();
    const { reason } = await refusal(
      d.orders.book.create(
        buyAi({ kind: 'atOpen', trigger: { at: T0 + 20_000 }, expiresAt: T0 + 10_000 }),
      ),
    );
    expect(reason).toContain('expire');
  });
});

describe('live orders', () => {
  it('refuses a live order while the desk is in dry run', async () => {
    const d = desk({ live: false, acknowledgedRestrictions: true });
    const { code: errorCode, reason } = await refusal(d.orders.book.create(buyAi({ live: true })));
    expect(errorCode).toBe('live_disabled');
    expect(reason).toContain('dry run');
  });

  it('refuses a live order while the restrictions have not been acknowledged', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: false });
    const { code: errorCode, reason } = await refusal(d.orders.book.create(buyAi({ live: true })));
    expect(errorCode).toBe('live_disabled');
    expect(reason).toContain('acknowledge-restrictions');
  });

  it('accepts a live order once the desk is live and the restrictions are acknowledged', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    const [order] = await d.orders.book.create(buyAi({ live: true }));
    expect(order?.live).toBe(true);
  });

  it('defaults to a dry run when the caller says nothing', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    const [order] = await d.orders.book.create(buyAi());
    expect(order?.live).toBe(false);
  });
});

describe('OCO pairs', () => {
  const legs = [
    { side: 'sell' as const, tokenIn: AI, tokenOut: USDG, amountIn: 10n ** 18n, trigger: { priceGte: 4 } },
    { side: 'sell' as const, tokenIn: AI, tokenOut: USDG, amountIn: 10n ** 18n, trigger: { priceLte: 1 } },
  ] as const;

  function pair(overrides: Record<string, unknown> = {}) {
    return {
      kind: 'oco' as const,
      side: 'sell' as const,
      tokenIn: AI,
      tokenOut: USDG,
      amountIn: 10n ** 18n,
      trigger: {},
      legs,
      ...overrides,
    };
  }

  it('creates a parent and two legs', async () => {
    const d = desk();
    const created = await d.orders.book.create(pair());
    expect(created).toHaveLength(3);
    const [parent, first, second] = created;
    expect(parent?.kind).toBe('oco');
    expect(parent?.parentId).toBeNull();
    expect(first?.parentId).toBe(parent?.id);
    expect(second?.parentId).toBe(parent?.id);
    expect(first?.kind).toBe('takeProfit');
    expect(second?.kind).toBe('stop');
  });

  it('refuses a pair with no legs', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(pair({ legs: undefined })));
    expect(reason).toContain('two legs');
  });

  it('refuses conditions on the parent, because each leg carries its own', async () => {
    const d = desk();
    const { reason } = await refusal(d.orders.book.create(pair({ trigger: { priceLte: 2 } })));
    expect(reason).toContain('each leg');
  });

  it('cancels the sibling and closes the pair when one leg is cancelled', async () => {
    const d = desk();
    const [parent, first, second] = await d.orders.book.create(pair());
    if (!parent || !first || !second) throw new Error('the pair was not created');

    await d.orders.book.cancel(first.id, 'Changed my mind.');

    expect(d.store.getOrder(first.id)?.status).toBe('cancelled');
    expect(d.store.getOrder(second.id)?.status).toBe('cancelled');
    expect(d.store.getOrder(second.id)?.reason).toContain('other leg');
    expect(d.store.getOrder(parent.id)?.status).toBe('cancelled');
  });

  it('cancels both legs when the pair itself is cancelled', async () => {
    const d = desk();
    const [parent, first, second] = await d.orders.book.create(pair());
    if (!parent || !first || !second) throw new Error('the pair was not created');

    await d.orders.book.cancel(parent.id);

    expect(d.store.getOrder(first.id)?.status).toBe('cancelled');
    expect(d.store.getOrder(second.id)?.status).toBe('cancelled');
    expect(d.store.getOrder(parent.id)?.status).toBe('cancelled');
  });
});

describe('cancelling', () => {
  it('reports an id that does not exist', async () => {
    const d = desk();
    await expect(d.orders.book.cancel('ord_missing')).rejects.toSatisfy(
      (error: unknown) => code(error) === 'not_found',
    );
  });

  it('writes a cancelled event and returns the stored order', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi());
    if (!order) throw new Error('the order was not created');

    const cancelled = await d.orders.book.cancel(order.id, 'No longer wanted.');
    expect(cancelled.status).toBe('cancelled');
    expect(cancelled.reason).toBe('No longer wanted.');
    expect(d.store.listEvents({ subject: order.id }).map((event) => event.kind)).toContain('order_cancelled');
  });

  it('is quiet when the same order is cancelled twice', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi());
    if (!order) throw new Error('the order was not created');
    await d.orders.book.cancel(order.id);
    const again = await d.orders.book.cancel(order.id);
    expect(again.status).toBe('cancelled');
  });

  it('refuses to cancel an order that already filled', async () => {
    const d = desk();
    const [order] = await d.orders.book.create(buyAi());
    if (!order) throw new Error('the order was not created');
    d.store.updateOrderStatus(order.id, 'filled', 'Filled.');

    const { reason } = await refusal(d.orders.book.cancel(order.id));
    expect(reason).toContain('filled');
  });
});

describe('reading the book', () => {
  it('lists by status', async () => {
    const d = desk();
    const [first] = await d.orders.book.create(buyAi());
    await d.orders.book.create(buyAi({ tokenOut: NVDA, trigger: { priceLte: 180 } }));
    if (!first) throw new Error('the order was not created');
    await d.orders.book.cancel(first.id);

    expect(d.orders.book.list({ status: 'open' })).toHaveLength(1);
    expect(d.orders.book.list({ status: 'cancelled' })).toHaveLength(1);
    expect(d.orders.book.get(first.id)?.status).toBe('cancelled');
  });
});

describe('reusing a reading', () => {
  it('asks once for the token two orders share, then asks again when it goes stale', async () => {
    let reads = 0;
    const inner: SnapshotProvider = {
      async snapshot(order, now) {
        reads += 1;
        return { token: subjectToken(order), usdOnchain: 1, asOf: now };
      },
    };
    const cached = createCachedSnapshotProvider(inner, 4_000);
    const order = { ...ORDER_FOR_CACHE };

    await cached.snapshot(order, 1_000);
    await cached.snapshot(order, 2_000);
    expect(reads).toBe(1);

    await cached.snapshot(order, 6_000);
    expect(reads).toBe(2);
  });

  it('does not keep a failed reading', async () => {
    let reads = 0;
    const inner: SnapshotProvider = {
      async snapshot() {
        reads += 1;
        throw new Error('the pool could not be read');
      },
    };
    const cached = createCachedSnapshotProvider(inner, 4_000);
    const order = { ...ORDER_FOR_CACHE };

    await expect(cached.snapshot(order, 1_000)).rejects.toThrow('the pool could not be read');
    await expect(cached.snapshot(order, 1_100)).rejects.toThrow('the pool could not be read');
    expect(reads).toBe(2);
  });
});
