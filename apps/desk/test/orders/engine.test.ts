import { afterEach, describe, expect, it } from 'vitest';
import { isDeskError } from '../../src/core/errors.js';
import type { Execution, Hex32, Order } from '../../src/core/types.js';
import { usd } from '../../src/orders/support.js';
import { AI, buyAi, harness, HUNDRED_USDG, POOL, T0, USDG, type Harness } from './harness.js';

const open: Harness[] = [];

function desk(overrides: Parameters<typeof harness>[0] = {}): Harness {
  const created = harness(overrides);
  open.push(created);
  return created;
}

afterEach(() => {
  for (const created of open.splice(0)) created.close();
});

const ONE_AI = 10n ** 18n;

/** Fifty AI out for a hundred USDG in, priced at two dollars: a hundred dollar fill. */
function fiftyAiOut(d: Harness): void {
  d.quote = { amountOut: 50n * ONE_AI, gasEstimate: 210_000n, route: [POOL] };
  d.snapshot = { usdOnchain: 2, usdFair: 2, premiumBps: 0 };
}

async function only(d: Harness, input: Record<string, unknown> = {}): Promise<Order> {
  const [order] = await d.orders.book.create(buyAi(input));
  if (!order) throw new Error('the order was not created');
  return order;
}

function refusalOf(error: unknown): { code: string; reason: string } {
  if (!isDeskError(error)) throw error;
  return { code: error.code, reason: error.reason };
}

async function caught(promise: Promise<unknown>): Promise<{ code: string; reason: string }> {
  try {
    await promise;
    throw new Error('the call was accepted');
  } catch (error) {
    return refusalOf(error);
  }
}

describe('prices in a message', () => {
  it('names a sub-cent price instead of rounding it to nothing', () => {
    expect(usd(0.00000005874)).toBe('0.00000005874');
    expect(usd(1000)).toBe('1000.00');
    expect(usd(0)).toBe('0.00');
  });

  it('says why an order fired, at the size the trader can read', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.snapshot = { usdOnchain: 0.00000005874, usdFair: 0.00000005874, premiumBps: 4 };
    const order = await only(d, { trigger: { priceLte: 0.0000001 } });

    await d.orders.engine.tick();
    const triggered = d.store
      .listEvents({ subject: order.id })
      .find((event) => event.kind === 'order_triggered');
    const reason = String(triggered?.detail?.reason);
    expect(reason).toContain('0.00000005874');
    expect(reason).toContain('0.0000001000');
  });
});

describe('a dry run fill', () => {
  it('triggers on the pass, records a simulated execution from the quote, and signs nothing', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d);

    const fired = await d.orders.engine.tick();

    expect(fired.map((row) => row.id)).toEqual([order.id]);
    expect(d.sent).toHaveLength(0);

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('filled');
    expect(stored?.reason).toContain('Dry run');

    const executions = d.store.listExecutions({ orderId: order.id });
    expect(executions).toHaveLength(1);
    const execution = executions[0] as Execution;
    expect(execution.live).toBe(false);
    expect(execution.status).toBe('simulated');
    expect(execution.amountIn).toBe(HUNDRED_USDG);
    expect(execution.amountOut).toBe(50n * ONE_AI);
    expect(execution.quotedAmountOut).toBe(50n * ONE_AI);
    expect(execution.slippageBps).toBe(0);
    expect(execution.notionalUsd.value).toBeCloseTo(100, 6);
    expect(execution.txHash).toBeUndefined();
  });

  it('writes the whole status trail as events', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d);
    await d.orders.engine.tick();

    const kinds = d.store
      .listEvents({ subject: order.id })
      .sort((a, b) => (a.id ?? 0) - (b.id ?? 0))
      .map((event) => event.kind);
    expect(kinds).toEqual(['order_created', 'order_triggered', 'order_filled']);
  });

  it('leaves an order alone while its condition is not met', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.snapshot = { usdOnchain: 9, usdFair: 9, premiumBps: 0 };
    const order = await only(d);

    expect(await d.orders.engine.tick()).toEqual([]);
    expect(d.store.getOrder(order.id)?.status).toBe('open');
    expect(d.store.listExecutions({ orderId: order.id })).toHaveLength(0);
  });

  it('counts a dry run against the rolling daily total', async () => {
    const d = desk();
    fiftyAiOut(d);
    await only(d);
    await d.orders.engine.tick();
    expect(d.orders.engine.dailyNotionalUsd()).toBeCloseTo(100, 6);

    d.setNow(T0 + 25 * 60 * 60 * 1000);
    expect(d.orders.engine.dailyNotionalUsd()).toBe(0);
  });
});

describe('expiry', () => {
  it('expires an order that ran out of time and never quotes it', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d, { expiresAt: T0 + 60_000 });

    d.setNow(T0 + 60_001);
    expect(await d.orders.engine.tick()).toEqual([]);

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('expired');
    expect(stored?.reason).toContain('Expired at');
    expect(d.store.listExecutions({ orderId: order.id })).toHaveLength(0);
  });

  it('keeps an order that is still inside its window', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.snapshot = { usdOnchain: 9 };
    const order = await only(d, { expiresAt: T0 + 60_000 });

    d.setNow(T0 + 59_999);
    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('open');
  });
});

describe('time triggers', () => {
  it('fires an at-open order once the offset after the open has passed', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.nextOpen = T0 + 3_600_000;
    const order = await only(d, { kind: 'atOpen', trigger: { atNextOpenOffsetSec: 300 } });

    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('open');

    d.setNow(T0 + 3_600_000 + 300_000);
    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('filled');
  });

  it('waits when the calendar cannot answer', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.nextOpen = new Error('fairvalue.session.nextOpen is not implemented yet.');
    const order = await only(d, { kind: 'atOpen', trigger: { atNextOpenOffsetSec: 0 } });

    d.setNow(T0 + 86_400_000);
    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('open');
  });

  it('fires a wall clock order at its time', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d, { kind: 'atOpen', trigger: { at: T0 + 10_000 } });

    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('open');

    d.setNow(T0 + 10_000);
    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('filled');
  });
});

describe('an OCO pair on the engine', () => {
  async function pair(d: Harness) {
    const created = await d.orders.book.create({
      kind: 'oco',
      side: 'sell',
      tokenIn: AI,
      tokenOut: USDG,
      amountIn: 10n * ONE_AI,
      trigger: {},
      legs: [
        { side: 'sell', tokenIn: AI, tokenOut: USDG, amountIn: 10n * ONE_AI, trigger: { priceGte: 4 } },
        { side: 'sell', tokenIn: AI, tokenOut: USDG, amountIn: 10n * ONE_AI, trigger: { priceLte: 1 } },
      ],
    });
    const [parent, takeProfit, stop] = created;
    if (!parent || !takeProfit || !stop) throw new Error('the pair was not created');
    return { parent, takeProfit, stop };
  }

  it('fills one leg, cancels the other, and closes the pair', async () => {
    const d = desk();
    d.quote = { amountOut: 40n * 10n ** 6n, gasEstimate: 210_000n, route: [POOL] };
    d.snapshot = { usdOnchain: 4, usdFair: 4, premiumBps: 0 };
    const { parent, takeProfit, stop } = await pair(d);

    const fired = await d.orders.engine.tick();
    expect(fired.map((row) => row.id)).toEqual([takeProfit.id]);

    expect(d.store.getOrder(takeProfit.id)?.status).toBe('filled');
    expect(d.store.getOrder(stop.id)?.status).toBe('cancelled');
    expect(d.store.getOrder(stop.id)?.reason).toContain('filled first');
    expect(d.store.getOrder(parent.id)?.status).toBe('filled');
    expect(d.store.listExecutions({ orderId: stop.id })).toHaveLength(0);
  });

  it('never executes the pair itself', async () => {
    const d = desk();
    const { parent } = await pair(d);
    const { reason } = await caught(d.orders.engine.execute(parent));
    expect(reason).toContain('Its legs execute');
  });
});

describe('bounds', () => {
  it('refuses a fill above the per-order cap and names both numbers', async () => {
    const d = desk();
    d.quote = { amountOut: 200n * ONE_AI, gasEstimate: 210_000n, route: [POOL] };
    d.snapshot = { usdOnchain: 2, usdFair: 2, premiumBps: 0 };
    const order = await only(d);

    await d.orders.engine.tick();

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.reason).toContain('250.00');
    expect(stored?.reason).toContain('400.00');
    expect(d.sent).toHaveLength(0);
    expect(d.store.listExecutions({ orderId: order.id })).toHaveLength(0);

    const refused = d.store.listEvents({ subject: order.id }).find((event) => event.kind === 'order_refused');
    expect(refused?.detail).toMatchObject({ bound: 'maxOrderNotionalUsd', limit: 250, actual: 400 });
  });

  it('refuses a fill that would break the daily cap and names what is already used', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.store.insertExecution(priorFill(d.now(), 950));
    const order = await only(d);

    await d.orders.engine.tick();

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.reason).toContain('1000.00');
    expect(stored?.reason).toContain('950.00');
    expect(stored?.reason).toContain('1050.00');
  });

  it('refuses an order whose slippage bound is wider than the configured cap', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d);
    widen(d, order.id, { maxSlippageBps: 900, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 });

    const { code, reason } = await caught(d.orders.engine.execute(d.store.getOrder(order.id) as Order));
    expect(code).toBe('bounds_exceeded');
    expect(reason).toContain('100');
    expect(reason).toContain('900');
    expect(d.store.getOrder(order.id)?.status).toBe('failed');
  });

  it('refuses a buy paying more premium than the cap allows', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.snapshot = { usdOnchain: 2, usdFair: 1.85, premiumBps: 810 };
    const order = await only(d);

    await d.orders.engine.tick();

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.reason).toContain('500 bps');
    expect(stored?.reason).toContain('810 bps');
  });

  it('lets a sell through at a premium the cap would refuse on a buy', async () => {
    const d = desk();
    d.quote = { amountOut: 20n * 10n ** 6n, gasEstimate: 210_000n, route: [POOL] };
    d.snapshot = { usdOnchain: 2, usdFair: 1.85, premiumBps: 810 };
    const [order] = await d.orders.book.create({
      kind: 'takeProfit',
      side: 'sell',
      tokenIn: AI,
      tokenOut: USDG,
      amountIn: 10n * ONE_AI,
      trigger: { priceGte: 1 },
    });
    if (!order) throw new Error('the order was not created');

    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('filled');
  });

  it('refuses a buy when the stock leg premium could not be measured', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.snapshot = { usdOnchain: 2, usdFair: 2 };
    const order = await only(d);

    await d.orders.engine.tick();

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.reason).toContain('premium could not be measured');
  });

  it('refuses an order on a token whose decimals nothing can confirm', async () => {
    const d = desk();
    const unknown = '0x00000000000000000000000000000000000000ff';
    fiftyAiOut(d);
    const [order] = await d.orders.book.create(buyAi({ tokenOut: unknown }));
    if (!order) throw new Error('the order was not created');

    const { code, reason } = await caught(d.orders.engine.execute(order));
    expect(code).toBe('bounds_exceeded');
    expect(reason).toContain('decimals');
    expect(d.store.getOrder(order.id)?.status).toBe('failed');
  });

  it('sizes a fill from the decimals the chain reports for an unnamed token', async () => {
    const d = desk();
    const usdgOut = '0x00000000000000000000000000000000000000fe';
    d.chainDecimals.set(usdgOut, 6);
    // Fifty thousand tokens of a six-decimal token, at one dollar each.
    d.quote = { amountOut: 50_000n * 10n ** 6n, gasEstimate: 210_000n, route: [POOL] };
    d.snapshot = { usdOnchain: 1, usdFair: 1, premiumBps: 0 };
    const [order] = await d.orders.book.create(buyAi({ tokenOut: usdgOut }));
    if (!order) throw new Error('the order was not created');

    const { code, reason } = await caught(d.orders.engine.execute(order));
    expect(code).toBe('bounds_exceeded');
    expect(reason).toContain('50000');
  });

  it('refuses to fill what it cannot measure in USD', async () => {
    const d = desk();
    fiftyAiOut(d);
    const order = await only(d);
    d.snapshot = {};

    const { code, reason } = await caught(d.orders.engine.execute(order));
    expect(code).toBe('bounds_exceeded');
    expect(reason).toContain('could not be measured');
    expect(d.store.getOrder(order.id)?.status).toBe('failed');
  });

  it('reports the daily total from filled and simulated rows alike', () => {
    const d = desk();
    d.store.insertExecution(priorFill(d.now(), 40));
    d.store.insertExecution({ ...priorFill(d.now(), 60), id: 'exe_two', status: 'confirmed' });
    d.store.insertExecution({ ...priorFill(d.now(), 500), id: 'exe_three', status: 'reverted' });
    expect(d.orders.engine.dailyNotionalUsd()).toBeCloseTo(100, 6);
  });
});

describe('live execution', () => {
  it('cannot happen while the desk is in dry run, even for an order marked live', async () => {
    const d = desk({ live: false, acknowledgedRestrictions: true });
    fiftyAiOut(d);
    const order = await only(d);
    d.store.db.prepare('UPDATE orders SET live = 1 WHERE id = ?').run(order.id);

    const { code, reason } = await caught(d.orders.engine.execute(d.store.getOrder(order.id) as Order));
    expect(code).toBe('live_disabled');
    expect(reason).toContain('dry run');
    expect(d.sent).toHaveLength(0);
    expect(d.store.getOrder(order.id)?.status).toBe('failed');
  });

  it('cannot happen while the restrictions are unacknowledged', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: false });
    fiftyAiOut(d);
    const order = await only(d);
    d.store.db.prepare('UPDATE orders SET live = 1 WHERE id = ?').run(order.id);

    const { code } = await caught(d.orders.engine.execute(d.store.getOrder(order.id) as Order));
    expect(code).toBe('live_disabled');
    expect(d.sent).toHaveLength(0);
  });

  it('stays a dry run when the desk is live but the order is not', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    fiftyAiOut(d);
    const order = await only(d);

    await d.orders.engine.tick();

    expect(d.sent).toHaveLength(0);
    const execution = d.store.listExecutions({ orderId: order.id })[0] as Execution;
    expect(execution.status).toBe('simulated');
    expect(execution.live).toBe(false);
  });

  it('sends the swap when the desk is live, the order is live, and the restrictions are acknowledged', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    fiftyAiOut(d);
    const order = await only(d, { live: true });

    await d.orders.engine.tick();

    expect(d.sent).toHaveLength(1);
    const request = d.sent[0];
    expect(request?.live).toBe(true);
    expect(request?.amountIn).toBe(HUNDRED_USDG);
    // One hundred basis points under the quote, from the configured slippage cap.
    expect(request?.minAmountOut).toBe((50n * ONE_AI * 9900n) / 10_000n);

    const execution = d.store.listExecutions({ orderId: order.id })[0] as Execution;
    expect(execution.live).toBe(true);
    expect(execution.status).toBe('confirmed');
    expect(execution.txHash).toBeDefined();
    expect(d.store.getOrder(order.id)?.status).toBe('filled');
  });

  it('sends the whole quoted route, so a two-hop fill uses the pools it was priced on', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    const second = '0xbb'.padEnd(66, 'b') as Hex32;
    fiftyAiOut(d);
    d.quote = { amountOut: 50n * ONE_AI, gasEstimate: 210_000n, route: [POOL, second] };
    await only(d, { live: true });

    await d.orders.engine.tick();

    const request = d.sent[0];
    expect(request?.route).toEqual([POOL, second]);
    // A pinned single pool would refuse the second hop, so it is left unset.
    expect(request?.poolId).toBeUndefined();
  });

  it('pins the pool when the route is a single hop', async () => {
    const d = desk({ live: true, acknowledgedRestrictions: true });
    fiftyAiOut(d);
    await only(d, { live: true });

    await d.orders.engine.tick();

    expect(d.sent[0]?.poolId).toBe(POOL);
    expect(d.sent[0]?.route).toEqual([POOL]);
  });
});

describe('when an upstream fails', () => {
  it('fails the order with the reason the quoter gave', async () => {
    const d = desk();
    fiftyAiOut(d);
    d.quote = { ...d.quote, fail: 'no pool for this pair' };
    const order = await only(d);

    await d.orders.engine.tick();

    const stored = d.store.getOrder(order.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.reason).toContain('no pool for this pair');
    expect(d.store.listExecutions({ orderId: order.id })).toHaveLength(0);
  });

  it('fails the order when the quoter returns nothing', async () => {
    const d = desk();
    d.quote = { amountOut: 0n, gasEstimate: 0n, route: [] };
    const order = await only(d);

    await d.orders.engine.tick();
    expect(d.store.getOrder(order.id)?.status).toBe('failed');
  });
});

describe('the loop', () => {
  it('reports whether it is running', () => {
    const d = desk();
    expect(d.orders.engine.running()).toBe(false);
    d.orders.engine.start();
    expect(d.orders.engine.running()).toBe(true);
    d.orders.engine.start();
    d.orders.engine.stop();
    expect(d.orders.engine.running()).toBe(false);
  });
});

function priorFill(now: number, notionalUsd: number): Execution {
  return {
    id: 'exe_prior',
    orderId: 'ord_prior',
    live: true,
    amountIn: 1n,
    amountOut: 1n,
    effectivePrice: { value: 1, unit: 'token', source: 'pool', asOf: now },
    quotedAmountOut: 1n,
    notionalUsd: { value: notionalUsd, unit: 'USD', source: 'derived', asOf: now },
    slippageBps: 0,
    status: 'confirmed',
    createdAt: now,
  };
}

/** Widen an order's bounds behind the book's back, the way an edited database would. */
function widen(d: Harness, id: string, bounds: Record<string, number>): void {
  d.store.db.prepare('UPDATE orders SET bounds = ? WHERE id = ?').run(JSON.stringify(bounds), id);
}
