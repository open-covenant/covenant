import { afterEach, describe, expect, it } from 'vitest';
import { defaultConfig } from '../../src/core/config.js';
import {
  TOOL_DETAILS,
  TOOL_NAMES,
  TOOL_SUMMARIES,
  createMcpSurface,
} from '../../src/surfaces/mcp/index.js';
import { deskFixture, startDesk, TEST_TOKEN, type RunningDesk } from './fixtures.js';

const config = defaultConfig({ token: TEST_TOKEN });

function surface(baseUrl?: string) {
  return createMcpSurface({ config, baseUrl });
}

function textOf(result: { content: unknown[] }): string {
  const first = result.content[0] as { type: string; text?: string } | undefined;
  return first?.text ?? '';
}

describe('tool list', () => {
  it('advertises the eleven desk tools', () => {
    expect(surface().tools()).toEqual(TOOL_NAMES);
    expect(TOOL_NAMES).toHaveLength(11);
    expect(surface().definitions().map((tool) => tool.name)).toEqual([...TOOL_NAMES]);
  });

  it('gives every tool an object schema an agent can fill in', () => {
    for (const tool of surface().definitions()) {
      expect(tool.inputSchema.type).toBe('object');
      expect(tool.description ?? '').not.toBe('');
    }
    const quote = surface().definitions().find((tool) => tool.name === 'desk_quote');
    const properties = quote?.inputSchema.properties as Record<string, { description?: string }>;
    expect(Object.keys(properties)).toEqual(['token', 'amountUsd', 'side']);
    expect(properties.amountUsd?.description).toContain('USD');
    expect(quote?.inputSchema.required).toEqual(['token']);
  });

  it('states units and defaults, and says orders are dry runs', () => {
    expect(TOOL_DETAILS.desk_premium).toContain('basis points');
    expect(TOOL_DETAILS.desk_quote).toContain('USD per whole token');
    expect(TOOL_DETAILS.desk_hedge_status).toContain('eight hours');
    expect(TOOL_SUMMARIES.desk_order_create).toContain('dry runs');
    expect(TOOL_DETAILS.desk_order_create).toContain('dry run');
  });

  it('reads as product copy', () => {
    const copy = [...Object.values(TOOL_SUMMARIES), ...Object.values(TOOL_DETAILS)].join('\n');
    expect(copy).not.toMatch(/—/);
    expect(copy).not.toMatch(/\b(canary|rollout|feature flag|backfill|refactor)\b/i);
    expect(copy).not.toMatch(/\b(institutional-grade|enterprise-grade|risk-free|guaranteed|production-ready)\b/i);
  });
});

describe('calls', () => {
  it('says no desk is running instead of starting one', async () => {
    const result = await surface('http://127.0.0.1:1').call('desk_status', {});
    expect(result.isError).toBe(true);
    expect(textOf(result)).toContain('No desk is answering on http://127.0.0.1:1');
    expect(textOf(result)).toContain('covenant-desk start');
  });

  it('names the argument that was wrong', async () => {
    const result = await surface('http://127.0.0.1:1').call('desk_quote', {});
    expect(result.isError).toBe(true);
    expect(textOf(result)).toContain('token');
  });

  it('names the tools it has when asked for one it does not', async () => {
    const result = await surface('http://127.0.0.1:1').call('desk_moon', {});
    expect(result.isError).toBe(true);
    expect(textOf(result)).toContain('desk_status');
  });

  describe('against a running desk', () => {
    let running: RunningDesk | undefined;

    afterEach(async () => {
      await running?.close();
      running = undefined;
    });

    it('answers desk_status over the local API', async () => {
      running = await startDesk();
      const result = await surface(running.url).call('desk_status', {});
      expect(result.isError).toBeUndefined();
      expect(JSON.parse(textOf(result))).toMatchObject({ chainId: 4663, sessionState: 'closed' });
    });

    it('answers desk_premium with the reference source per row', async () => {
      const desk = deskFixture();
      desk.fairvalue.premium.all = async () => [
        {
          symbol: 'AAPL',
          token: '0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9',
          onchainMid: { value: 232.1, unit: 'USD', source: 'pool', asOf: 1 },
          reference: { value: 230, unit: 'USD', source: 'lighter-rh', asOf: 1 },
          referenceSource: 'lighter-rh',
          candidates: [],
          premiumBps: { value: 91.3, unit: 'bps', source: 'derived', asOf: 1 },
          sessionState: 'closed',
          asOf: 1,
        },
      ];
      running = await startDesk(desk);

      const result = await surface(running.url).call('desk_premium', { limit: 5 });
      const body = JSON.parse(textOf(result)) as { premium: { symbol: string; referenceSource: string }[] };
      expect(body.premium[0]).toMatchObject({ symbol: 'AAPL', referenceSource: 'lighter-rh' });
    });

    it('creates an OCO order from the two legs an agent sends', async () => {
      running = await startDesk();
      const usdg = '0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168';
      const nvda = '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC';
      const leg = (trigger: Record<string, number>) => ({
        side: 'buy' as const,
        tokenIn: usdg,
        tokenOut: nvda,
        amountIn: '1000000',
        trigger,
      });

      const result = await surface(running.url).call('desk_order_create', {
        kind: 'oco',
        side: 'buy',
        tokenIn: usdg,
        tokenOut: nvda,
        amountIn: '1000000',
        legs: [leg({ priceLte: 200 }), leg({ priceGte: 300 })],
      });

      expect(result.isError).toBeUndefined();
      const body = JSON.parse(textOf(result)) as { orders: { kind: string; parentId: string | null }[] };
      expect(body.orders).toHaveLength(3);
      expect(body.orders.filter((order) => order.parentId !== null)).toHaveLength(2);
    });

    it('passes a refusal from the desk through with its reason', async () => {
      running = await startDesk();
      const result = await surface(running.url).call('desk_pools', { stock: 'NOPE' });
      expect(result.isError).toBe(true);
      expect(textOf(result)).toContain('not found');
    });

    it('rejects a bearer token that does not match the desk', async () => {
      running = await startDesk();
      const wrong = createMcpSurface({ config: defaultConfig({ token: 'b'.repeat(64) }), baseUrl: running.url });
      const result = await wrong.call('desk_status', {});
      expect(result.isError).toBe(true);
      expect(textOf(result)).toContain('bearer token');
    });
  });
});
