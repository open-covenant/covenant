import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { OrderTrigger } from '../../src/core/types.js';
import type { TriggerContext } from '../../src/orders/index.js';
import { harness, T0, type Harness } from './harness.js';

let desk: Harness;

beforeAll(() => {
  desk = harness();
});

afterAll(() => desk.close());

function fires(trigger: OrderTrigger, context: Partial<TriggerContext>): boolean {
  return desk.orders.engine.isTriggered(trigger, { now: T0, ...context });
}

describe('trigger evaluation', () => {
  it('never fires on a trigger with no conditions', () => {
    expect(fires({}, { usdOnchain: 1 })).toBe(false);
    expect(fires({ priceBasis: 'usdFair' }, { usdFair: 1 })).toBe(false);
  });

  it('fires a limit buy when the on-chain price reaches the level', () => {
    expect(fires({ priceLte: 2.5 }, { usdOnchain: 2.5 })).toBe(true);
    expect(fires({ priceLte: 2.5 }, { usdOnchain: 2.49 })).toBe(true);
    expect(fires({ priceLte: 2.5 }, { usdOnchain: 2.51 })).toBe(false);
  });

  it('fires a take profit when the price reaches the level from below', () => {
    expect(fires({ priceGte: 3 }, { usdOnchain: 3 })).toBe(true);
    expect(fires({ priceGte: 3 }, { usdOnchain: 2.99 })).toBe(false);
  });

  it('reads the basis the trigger names', () => {
    expect(fires({ priceLte: 2.5, priceBasis: 'usdFair' }, { usdOnchain: 1, usdFair: 4 })).toBe(false);
    expect(fires({ priceLte: 2.5, priceBasis: 'usdFair' }, { usdOnchain: 4, usdFair: 1 })).toBe(true);
  });

  it('holds when the price it needs is missing', () => {
    expect(fires({ priceLte: 2.5 }, {})).toBe(false);
    expect(fires({ priceGte: 2.5 }, {})).toBe(false);
    expect(fires({ priceLte: 2.5, priceBasis: 'usdFair' }, { usdOnchain: 1 })).toBe(false);
  });

  it('fires on a premium below the level and holds when the premium is missing', () => {
    expect(fires({ premiumLteBps: -200 }, { premiumBps: -250 })).toBe(true);
    expect(fires({ premiumLteBps: -200 }, { premiumBps: -150 })).toBe(false);
    expect(fires({ premiumLteBps: -200 }, {})).toBe(false);
  });

  it('fires on a premium above the level', () => {
    expect(fires({ premiumGteBps: 400 }, { premiumBps: 460 })).toBe(true);
    expect(fires({ premiumGteBps: 400 }, { premiumBps: 399.9 })).toBe(false);
  });

  it('needs every condition present to hold', () => {
    const trigger: OrderTrigger = { priceLte: 2.5, premiumLteBps: 0 };
    expect(fires(trigger, { usdOnchain: 2, premiumBps: -10 })).toBe(true);
    expect(fires(trigger, { usdOnchain: 2, premiumBps: 10 })).toBe(false);
    expect(fires(trigger, { usdOnchain: 3, premiumBps: -10 })).toBe(false);
  });

  it('treats a price band as both ends at once', () => {
    const band: OrderTrigger = { priceGte: 2, priceLte: 3 };
    expect(fires(band, { usdOnchain: 2.5 })).toBe(true);
    expect(fires(band, { usdOnchain: 1.9 })).toBe(false);
    expect(fires(band, { usdOnchain: 3.1 })).toBe(false);
  });

  it('fires at a wall clock time', () => {
    expect(fires({ at: T0 + 1000 }, {})).toBe(false);
    expect(fires({ at: T0 }, {})).toBe(true);
    expect(fires({ at: T0 - 1 }, {})).toBe(true);
  });

  it('fires the given number of seconds after the next open', () => {
    const open = T0 + 60_000;
    expect(fires({ atNextOpenOffsetSec: 0 }, { nextOpen: open })).toBe(false);
    expect(fires({ atNextOpenOffsetSec: 0 }, { nextOpen: T0 })).toBe(true);
    expect(fires({ atNextOpenOffsetSec: 300 }, { nextOpen: T0 - 299_000 })).toBe(false);
    expect(fires({ atNextOpenOffsetSec: 300 }, { nextOpen: T0 - 300_000 })).toBe(true);
  });

  it('holds when the calendar has no next open to offer', () => {
    expect(fires({ atNextOpenOffsetSec: 0 }, {})).toBe(false);
  });
});
