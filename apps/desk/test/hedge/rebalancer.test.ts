import { afterEach, describe, expect, it } from 'vitest';
import type { Config } from '../../src/core/config.js';
import { quantity, type HedgePlan } from '../../src/core/types.js';
import type { Store } from '../../src/core/store.js';
import { HedgeRebalancer } from '../../src/hedge/rebalancer.js';
import { LighterRhClient, type TradingLike } from '../../src/hedge/lighter-rh.js';
import { HedgeSizer } from '../../src/hedge/sizer.js';
import {
  FULL_CREDENTIALS,
  fixtureConfig,
  fixtureFairValue,
  fixtureFetchJson,
  fixtureKeystore,
  fixtureLogger,
  fixtureStore,
  fixtureTransport,
  NVDA_MARKET_ID,
  shortPosition,
  type FixtureState,
} from './fixtures.js';

const stores: Store[] = [];

afterEach(() => {
  for (const store of stores.splice(0)) store.close();
});

interface Sent {
  market: number;
  side: 'buy' | 'sell';
  size: string;
  reduceOnly?: boolean;
}

function build(
  options: { credentials?: boolean; state?: FixtureState; config?: Config; sent?: Sent[] } = {},
) {
  const config = options.config ?? fixtureConfig();
  const state: FixtureState = options.state ?? { positions: [], collateral: 5000 };
  const sent = options.sent ?? [];
  const store = fixtureStore();
  stores.push(store);

  const trading: TradingLike = {
    async placeOrder(request) {
      sent.push(request as Sent);
      return { txHash: '0xfeed', clientOrderIndex: 1n };
    },
    async cancelOrder() {
      return undefined;
    },
  };

  const client = new LighterRhClient({
    config,
    logger: fixtureLogger(),
    keystore: fixtureKeystore(options.credentials === true ? FULL_CREDENTIALS : {}),
    transport: fixtureTransport(state),
    fetchJson: fixtureFetchJson,
    createTradingClient: () => trading,
  });
  const sizer = new HedgeSizer({
    config,
    logger: fixtureLogger(),
    fairvalue: fixtureFairValue({ NVDA: 231.5 }),
    client,
  });
  const rebalancer = new HedgeRebalancer({ config, logger: fixtureLogger(), store, client, sizer });
  return { client, sizer, rebalancer, store, sent, config };
}

function plan(overrides: Partial<HedgePlan> = {}): HedgePlan {
  const asOf = 1_788_543_000_000;
  return {
    symbol: 'NVDA',
    marketId: NVDA_MARKET_ID,
    stockLegNotionalUsd: quantity(1000, 'USD', 'derived', asOf),
    referencePrice: quantity(230.72, 'USD', 'lighter-rh', asOf),
    targetShortBase: quantity(4.334, 'token', 'derived', asOf),
    currentShortBase: quantity(-4.3, 'token', 'lighter-rh', asOf),
    driftBps: quantity(78, 'bps', 'derived', asOf),
    action: 'none',
    fundingBps8h: quantity(0.32, 'bps', 'lighter-rh', asOf),
    executable: false,
    asOf,
    ...overrides,
  };
}

describe('when the rebalancer acts', () => {
  it('holds a short that sits inside the drift band', () => {
    const { rebalancer } = build();
    const decision = rebalancer.shouldAct(plan());
    expect(decision.act).toBe(false);
    expect(decision.reason).toContain('within 500 bps');
  });

  it('acts on a short that has drifted past the band', () => {
    const { rebalancer } = build();
    const decision = rebalancer.shouldAct(
      plan({ action: 'increaseShort', driftBps: quantity(1800, 'bps', 'derived', 0) }),
    );
    expect(decision.act).toBe(true);
    expect(decision.reason).toContain('1800 bps from target');
  });

  it('acts when funding turns against the short past the limit', () => {
    const { rebalancer } = build();
    const decision = rebalancer.shouldAct(plan({ fundingBps8h: quantity(-120, 'bps', 'lighter-rh', 0) }));
    expect(decision.act).toBe(true);
    expect(decision.reason).toContain('against the short');
    expect(decision.reason).toContain('50 bps limit');
  });

  it('leaves a short alone while funding pays it', () => {
    const { rebalancer } = build();
    expect(rebalancer.shouldAct(plan({ fundingBps8h: quantity(140, 'bps', 'lighter-rh', 0) })).act).toBe(false);
  });

  it('ignores funding when no short is held', () => {
    const { rebalancer } = build();
    const decision = rebalancer.shouldAct(
      plan({
        action: 'none',
        currentShortBase: quantity(0, 'token', 'lighter-rh', 0),
        fundingBps8h: quantity(-400, 'bps', 'lighter-rh', 0),
      }),
    );
    expect(decision.act).toBe(false);
  });

  it('respects a wider band from config', () => {
    const config = fixtureConfig();
    const { rebalancer } = build({ config: { ...config, hedge: { ...config.hedge, driftBps: 2500 } } });
    const decision = rebalancer.shouldAct(
      plan({ action: 'increaseShort', driftBps: quantity(1800, 'bps', 'derived', 0) }),
    );
    expect(decision.act).toBe(false);
  });
});

describe('applying a plan', () => {
  it('sends nothing and names the blocker while the desk is in dry run', async () => {
    const { rebalancer, sent, store } = build({
      credentials: true,
      config: fixtureConfig({ live: false }),
    });
    const applied = await rebalancer.apply(
      plan({ action: 'open', actionSizeBase: quantity(4.334, 'token', 'derived', 0) }),
    );
    expect(applied.executable).toBe(false);
    expect(applied.reason).toContain('dry run');
    expect(sent).toEqual([]);
    expect(store.listEvents().map((event) => event.kind)).toContain('hedge.blocked');
  });

  it('sends nothing when no key is present', async () => {
    const { rebalancer, sent } = build();
    const applied = await rebalancer.apply(
      plan({ action: 'open', actionSizeBase: quantity(4.334, 'token', 'derived', 0) }),
    );
    expect(applied.executable).toBe(false);
    expect(applied.reason).toContain('DESK_LIGHTER_RH_PRIVATE_KEY');
    expect(sent).toEqual([]);
  });

  it('sends a sell to open or grow the short', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    const applied = await rebalancer.apply(
      plan({ action: 'open', actionSizeBase: quantity(4.334, 'token', 'derived', 0) }),
    );
    expect(applied.executable).toBe(true);
    expect(sent).toEqual([
      { market: NVDA_MARKET_ID, side: 'sell', size: '4.3340', type: 'market', reduceOnly: false },
    ]);
  });

  it('sends a reduce-only buy to cut or close the short', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    await rebalancer.apply(plan({ action: 'unwind', actionSizeBase: quantity(4.3, 'token', 'derived', 0) }));
    expect(sent).toEqual([
      { market: NVDA_MARKET_ID, side: 'buy', size: '4.3000', type: 'market', reduceOnly: true },
    ]);
  });

  it('refuses a gap the venue would not accept as an order', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    const applied = await rebalancer.apply(
      // NVDA takes 0.0400 or nothing.
      plan({ action: 'increaseShort', actionSizeBase: quantity(0.02, 'token', 'derived', 0) }),
    );
    expect(applied.executable).toBe(false);
    expect(applied.reason).toContain('0.0400');
    expect(sent).toEqual([]);
  });

  it('truncates the size to the market step rather than rounding it up', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    await rebalancer.apply(
      plan({ action: 'increaseShort', actionSizeBase: quantity(4.33459999, 'token', 'derived', 0) }),
    );
    expect(sent[0]?.size).toBe('4.3345');
  });

  it('keeps the reason the sizer already wrote instead of sending anyway', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    const applied = await rebalancer.apply(
      plan({
        action: 'increaseShort',
        actionSizeBase: quantity(4.334, 'token', 'derived', 0),
        executable: false,
        reason: 'The NVDA short is within 12 bps of target, so no order is needed.',
      }),
    );
    expect(applied.executable).toBe(false);
    expect(applied.reason).toContain('within 12 bps');
    expect(sent).toEqual([]);
  });

  it('has nothing to send for a plan that asks for nothing', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    const applied = await rebalancer.apply(plan());
    expect(applied.executable).toBe(false);
    expect(sent).toEqual([]);
  });
});

describe('a pass over the target book', () => {
  it('records the venue positions it read', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    const { rebalancer, store } = build({ credentials: true, state });
    await rebalancer.tick();
    const held = store.listHedges();
    expect(held).toHaveLength(1);
    expect(held[0]?.sizeBase.value).toBeCloseTo(-4.33, 4);
  });

  it('opens a short for a newly tracked stock leg', async () => {
    const { rebalancer, sent } = build({ credentials: true });
    rebalancer.setTarget('nvda', 1000);
    expect(rebalancer.targets()).toEqual([
      { symbol: 'NVDA', stockLegUsd: 1000, updatedAt: expect.any(Number) },
    ]);

    const acted = await rebalancer.tick();
    expect(acted).toHaveLength(1);
    expect(acted[0]?.action).toBe('open');
    expect(sent[0]?.side).toBe('sell');
  });

  it('leaves a short that is already the right size alone', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    const { rebalancer, sent } = build({ credentials: true, state });
    rebalancer.setTarget('NVDA', 1000);
    expect(await rebalancer.tick()).toEqual([]);
    expect(sent).toEqual([]);
  });

  it('closes a short that funding has turned against', async () => {
    const config = fixtureConfig();
    const state: FixtureState = { positions: [shortPosition('AAPL', 10, 3.87, 258)], collateral: 5000 };
    const { rebalancer, sent, sizer } = build({
      credentials: true,
      state,
      config: { ...config, hedge: { ...config.hedge, maxFundingBps8h: 0.5 } },
    });
    rebalancer.setTarget('AAPL', 1000);
    const target = await sizer.plan({ symbol: 'AAPL', stockLegUsd: 1000 });
    expect(target.fundingBps8h?.value).toBeCloseTo(-1, 6);
    expect(target.action).toBe('none');

    const acted = await rebalancer.tick();
    expect(acted).toHaveLength(1);
    expect(acted[0]?.action).toBe('unwind');
    expect(sent[0]).toEqual({ market: 10, side: 'buy', size: '3.8700', type: 'market', reduceOnly: true });
  });

  it('keeps going when one symbol cannot be planned', async () => {
    const { rebalancer, store } = build({ credentials: true });
    rebalancer.setTarget('HIMS', 1000);
    rebalancer.setTarget('NVDA', 1000);
    const acted = await rebalancer.tick();
    expect(acted.map((entry) => entry.symbol)).toEqual(['NVDA']);
    expect(store.listEvents({ subject: 'HIMS' }).map((event) => event.kind)).toContain('hedge.plan_failed');
  });
});

describe('unwinding', () => {
  it('closes every short and stops tracking it', async () => {
    const state: FixtureState = {
      positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229), shortPosition('AAPL', 10, 3.87, 258)],
      collateral: 5000,
    };
    const { rebalancer, sent } = build({ credentials: true, state });
    rebalancer.setTarget('NVDA', 1000);
    const plans = await rebalancer.unwind();
    expect(plans.map((entry) => entry.symbol)).toEqual(['NVDA', 'AAPL']);
    expect(sent.every((order) => order.side === 'buy' && order.reduceOnly === true)).toBe(true);
    expect(rebalancer.targets()).toEqual([]);
  });

  it('closes only the symbol it was given', async () => {
    const state: FixtureState = {
      positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229), shortPosition('AAPL', 10, 3.87, 258)],
      collateral: 5000,
    };
    const { rebalancer, sent } = build({ credentials: true, state });
    const plans = await rebalancer.unwind('aapl');
    expect(plans.map((entry) => entry.symbol)).toEqual(['AAPL']);
    expect(sent).toHaveLength(1);
    expect(sent[0]?.market).toBe(10);
  });

  it('returns the plan with its reason when the desk cannot sign', async () => {
    const state: FixtureState = { positions: [shortPosition('NVDA', NVDA_MARKET_ID, 4.33, 229)], collateral: 5000 };
    const { rebalancer, sent } = build({ credentials: true, state, config: fixtureConfig({ live: false }) });
    const plans = await rebalancer.unwind();
    expect(plans).toHaveLength(1);
    expect(plans[0]?.executable).toBe(false);
    expect(plans[0]?.reason).toContain('dry run');
    expect(sent).toEqual([]);
  });
});

describe('the loop', () => {
  it('starts and stops once', () => {
    const { rebalancer } = build();
    expect(rebalancer.running()).toBe(false);
    rebalancer.start();
    rebalancer.start();
    expect(rebalancer.running()).toBe(true);
    rebalancer.stop();
    expect(rebalancer.running()).toBe(false);
  });
});
