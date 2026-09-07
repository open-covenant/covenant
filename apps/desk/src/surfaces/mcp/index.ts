/**
 * MCP server over stdio.
 *
 * The MCP process is a client of the running desk: it reads the bearer token
 * from the config file and calls the local HTTP API. When no desk is running it
 * says so rather than starting one.
 *
 * Tool descriptions state units and defaults, and say that orders are dry runs
 * unless the desk is live and the order asks for a live fill.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  type CallToolResult,
  type Tool,
} from '@modelcontextprotocol/sdk/types.js';
import { z } from 'zod';
import type { Config } from '../../core/config.js';
import { isDeskError } from '../../core/errors.js';

/** Tools exposed to the agent. */
export const TOOL_NAMES = [
  'desk_status',
  'desk_quote',
  'desk_premium',
  'desk_pools',
  'desk_order_create',
  'desk_orders',
  'desk_order_cancel',
  'desk_hedge_plan',
  'desk_hedge_apply',
  'desk_hedge_status',
  'desk_recorder_recent',
] as const;

export type ToolName = (typeof TOOL_NAMES)[number];

/** One line per tool, shown to the agent choosing between them. */
export const TOOL_SUMMARIES: Record<ToolName, string> = {
  desk_status: 'Report what the desk is doing: session state, block height, dry run or live, and loop health.',
  desk_quote: 'Price a token in USD, on chain and at fair value, with the premium its stock leg carries.',
  desk_premium: 'List stock tokens by the gap between the on-chain price and the reference price, in basis points.',
  desk_pools: 'List Uniswap v4 pools for a stock symbol, deepest first, with the fee each pool charges.',
  desk_order_create: 'Create a conditional order. Orders are dry runs unless the desk is live and the order asks to be live.',
  desk_orders: 'List orders with their status and the reason for it.',
  desk_order_cancel: 'Cancel an open order by id.',
  desk_hedge_plan: 'Size the short that cancels the stock exposure inside a position.',
  desk_hedge_apply: 'Send a hedge plan. Returns the plan with a reason when it cannot be sent.',
  desk_hedge_status: 'Report current hedge positions, mark prices, and funding.',
  desk_recorder_recent: 'Return recorded on-chain prices against reference prices for a symbol.',
};

/** What each tool answers, in the words the agent needs to choose between them. */
export const TOOL_DETAILS: Record<ToolName, string> = {
  desk_status:
    'Fields: session state of the United States equities calendar, block height on chain 4663, seconds since the desk started, whether signed execution is on, tracked tokens and pools, open orders, observations in the last 24 hours.',
  desk_quote:
    'Give a stock symbol such as NVDA, or the address of a token quoted in a stock token. Prices are USD per whole token. The answer carries the pool mid and, when a pool can price the requested size, what that size would pay or receive with the fee and the price impact included. Size defaults to 100 USD and side defaults to buy. Reading a price never places an order.',
  desk_premium:
    'Premium is (on-chain price / reference price - 1) in basis points, where 100 bps is 1 percent. Each row names the reference source: chainlink, lighter-rh, rhj, or chainlink-stale when Wall Street is closed.',
  desk_pools:
    'Fees are reported in hundredths of a basis point as the pool stores them. Pools charging more than 300 bps are flagged and left out of routing unless includeTraps is true.',
  desk_order_create:
    'Sizes are smallest units of tokenIn, or whole tokens through amountInTokens with the token decimals. Price conditions are USD per whole token. Premium conditions are basis points on the stock leg. An OCO order carries its conditions on the two legs and leaves the top-level trigger empty. The order stays a dry run unless the desk config has live execution on and live is true here.',
  desk_orders: 'Status is one of open, triggered, filled, failed, cancelled, expired.',
  desk_order_cancel: 'Cancelling one side of an OCO pair cancels the other side as well.',
  desk_hedge_plan:
    'Give the stock exposure to cancel as stockLegUsd in dollars, or as qty of the paired token with the ratio of stock tokens per paired token. Sizes come back in base units of the perpetual market, rounded to the size the venue accepts.',
  desk_hedge_apply:
    'Sending needs credentials for the Lighter Robinhood Chain instance and a funded account. Without them the plan comes back with the reason it was not sent.',
  desk_hedge_status:
    'Sizes are base units and negative means short. Funding is basis points over eight hours. An empty list with a reason means the desk has no sub-account configured and cannot see any position.',
  desk_recorder_recent:
    'Observations come from the local recorder, so the window only holds what the desk has already recorded. Times are milliseconds since the Unix epoch.',
};

export interface McpSurface {
  /** Serve MCP on stdio until the transport closes. */
  serve(): Promise<void>;
  /** Tool names in the order they are advertised. */
  tools(): readonly ToolName[];
  /** Tool list as the client sees it, with JSON Schema built from the zod schemas. */
  definitions(): Tool[];
  /** Call one tool directly. Used by tests and by anything embedding the desk. */
  call(name: string, args: unknown): Promise<CallResult>;
}

/** What a tool answers: one text block holding JSON, or a plain reason it could not. */
export type CallResult = CallToolResult;

export interface McpOptions {
  readonly config: Config;
  /** Base URL of the running desk. Defaults to the configured loopback port. */
  readonly baseUrl?: string;
}

const TriggerSchema = z.object({
  priceLte: z.number().optional().describe('Fire when the price is at or below this level, USD per whole token.'),
  priceGte: z.number().optional().describe('Fire when the price is at or above this level, USD per whole token.'),
  priceBasis: z
    .enum(['usdOnchain', 'usdFair'])
    .optional()
    .describe('Which price the conditions read. Default usdOnchain.'),
  premiumLteBps: z.number().optional().describe('Fire when the stock leg premium is at or below this, basis points.'),
  premiumGteBps: z.number().optional().describe('Fire when the stock leg premium is at or above this, basis points.'),
  atNextOpenOffsetSec: z
    .number()
    .int()
    .optional()
    .describe('Fire this many seconds after the next United States regular open.'),
  at: z.number().int().optional().describe('Fire at this time, milliseconds since the Unix epoch.'),
});

/** One side of an order. An OCO carries two of these. */
const LegSchema = z.object({
  side: z.enum(['buy', 'sell']),
  tokenIn: z.string().describe('Address of the token being sold.'),
  tokenOut: z.string().describe('Address of the token being bought.'),
  amountIn: z.string().optional().describe('Amount to sell in the smallest units of tokenIn.'),
  amountInTokens: z.string().optional().describe('Amount to sell in whole tokens. Needs decimals.'),
  decimals: z.number().int().min(0).max(36).optional().describe('Decimals of tokenIn. Default 18.'),
  trigger: TriggerSchema.optional().describe('Conditions that arm this leg. All present conditions must hold.'),
  live: z.boolean().optional().describe('Ask for a signed transaction on this leg. Default false.'),
  expiresAt: z.number().int().nullable().optional().describe('Milliseconds since the Unix epoch. Null never expires.'),
});

const SCHEMAS = {
  desk_status: z.object({}),
  desk_quote: z.object({
    token: z.string().describe('Stock symbol such as NVDA, or a token address on chain 4663.'),
    amountUsd: z.number().positive().optional().describe('Size of the order in USD. Default 100.'),
    side: z.enum(['buy', 'sell']).optional().describe('Default buy.'),
  }),
  desk_premium: z.object({
    limit: z.number().int().min(1).max(200).optional().describe('How many stock tokens to return. Default 20.'),
  }),
  desk_pools: z.object({
    stock: z.string().describe('Stock symbol such as NVDA, or its token address.'),
    includeTraps: z
      .boolean()
      .optional()
      .describe('Include pools charging more than 300 bps, which routing refuses. Default false.'),
    limit: z.number().int().min(1).max(200).optional().describe('How many pools to return, deepest first. Default 25.'),
  }),
  desk_order_create: z.object({
    kind: z.enum(['limit', 'stop', 'takeProfit', 'oco', 'atOpen', 'premium']),
    side: z.enum(['buy', 'sell']),
    tokenIn: z.string().describe('Address of the token being sold.'),
    tokenOut: z.string().describe('Address of the token being bought.'),
    amountIn: z.string().optional().describe('Amount to sell in the smallest units of tokenIn.'),
    amountInTokens: z.string().optional().describe('Amount to sell in whole tokens. Needs decimals.'),
    decimals: z.number().int().min(0).max(36).optional().describe('Decimals of tokenIn. Default 18.'),
    trigger: TriggerSchema.optional().describe('Conditions that arm the order. All present conditions must hold.'),
    legs: z
      .tuple([LegSchema, LegSchema])
      .optional()
      .describe('The two sides of an OCO order. The first one to fill cancels the other.'),
    live: z
      .boolean()
      .optional()
      .describe('Ask for a signed transaction. Needs live execution in the desk config too. Default false.'),
    expiresAt: z.number().int().nullable().optional().describe('Milliseconds since the Unix epoch. Null never expires.'),
  }),
  desk_orders: z.object({
    status: z.enum(['open', 'triggered', 'filled', 'failed', 'cancelled', 'expired']).optional(),
    limit: z.number().int().min(1).max(500).optional().describe('Default 100.'),
  }),
  desk_order_cancel: z.object({
    id: z.string().describe('Order id from desk_orders.'),
    reason: z.string().optional().describe('Recorded with the cancellation.'),
  }),
  desk_hedge_plan: z.object({
    symbol: z.string().describe('Stock symbol the position carries, such as NVDA.'),
    stockLegUsd: z.number().positive().optional().describe('Stock exposure to cancel, USD.'),
    qty: z.number().positive().optional().describe('Quantity of the paired token held, whole tokens.'),
    ratio: z.number().positive().optional().describe('Stock tokens per paired token, from the pool.'),
  }),
  desk_hedge_apply: z.object({
    symbol: z.string().describe('Stock symbol the position carries, such as NVDA.'),
    stockLegUsd: z.number().positive().optional().describe('Stock exposure to cancel, USD.'),
    qty: z.number().positive().optional().describe('Quantity of the paired token held, whole tokens.'),
    ratio: z.number().positive().optional().describe('Stock tokens per paired token, from the pool.'),
  }),
  desk_hedge_status: z.object({}),
  desk_recorder_recent: z.object({
    symbol: z.string().optional().describe('Stock symbol. Leave it out for every symbol.'),
    sinceMinutes: z.number().int().min(1).max(20_160).optional().describe('How far back to read. Default 60 minutes.'),
    limit: z.number().int().min(1).max(5_000).optional().describe('Default 200 rows.'),
  }),
} satisfies Record<ToolName, z.ZodType>;

type Args<N extends ToolName> = z.infer<(typeof SCHEMAS)[N]>;

interface HttpCall {
  (method: 'GET' | 'POST' | 'DELETE', route: string, body?: unknown): Promise<unknown>;
}

const RUNNERS: { [N in ToolName]: (args: Args<N>, call: HttpCall) => Promise<unknown> } = {
  desk_status: (_args, call) => call('GET', '/v1/status'),
  desk_quote: (args, call) =>
    call(
      'GET',
      `/v1/quote?token=${encodeURIComponent(args.token)}&amountUsd=${args.amountUsd ?? 100}&side=${args.side ?? 'buy'}`,
    ),
  desk_premium: (args, call) => call('GET', `/v1/premium?limit=${args.limit ?? 20}`),
  desk_pools: (args, call) =>
    call(
      'GET',
      `/v1/pools?stock=${encodeURIComponent(args.stock)}&includeTraps=${args.includeTraps ?? false}&limit=${args.limit ?? 25}`,
    ),
  desk_order_create: (args, call) => call('POST', '/v1/orders', args),
  desk_orders: (args, call) =>
    call('GET', `/v1/orders?limit=${args.limit ?? 100}${args.status ? `&status=${args.status}` : ''}`),
  desk_order_cancel: (args, call) =>
    call(
      'DELETE',
      `/v1/orders/${encodeURIComponent(args.id)}${args.reason ? `?reason=${encodeURIComponent(args.reason)}` : ''}`,
    ),
  desk_hedge_plan: (args, call) => call('GET', `/v1/hedge/plan?${hedgeQuery(args)}`),
  desk_hedge_apply: (args, call) => call('POST', '/v1/hedge/apply', args),
  desk_hedge_status: (_args, call) => call('GET', '/v1/hedge'),
  desk_recorder_recent: (args, call) => {
    const query = new URLSearchParams({
      since: String(Date.now() - (args.sinceMinutes ?? 60) * 60_000),
      limit: String(args.limit ?? 200),
    });
    if (args.symbol) query.set('symbol', args.symbol);
    return call('GET', `/v1/recorder?${query.toString()}`);
  },
};

function hedgeQuery(args: Args<'desk_hedge_plan'>): string {
  const query = new URLSearchParams({ symbol: args.symbol });
  if (args.stockLegUsd !== undefined) query.set('stockLegUsd', String(args.stockLegUsd));
  if (args.qty !== undefined) query.set('qty', String(args.qty));
  if (args.ratio !== undefined) query.set('ratio', String(args.ratio));
  return query.toString();
}

export function createMcpSurface(options: McpOptions): McpSurface {
  const baseUrl = options.baseUrl ?? `http://127.0.0.1:${options.config.port}`;
  const notRunning = `No desk is answering on ${baseUrl}. Start one with "covenant-desk start", then ask again.`;

  const call: HttpCall = async (method, route, body) => {
    let response: Response;
    try {
      response = await fetch(`${baseUrl}${route}`, {
        method,
        headers: {
          authorization: `Bearer ${options.config.token}`,
          ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new OfflineError(notRunning);
    }
    const text = await response.text();
    const parsed = text.trim() === '' ? {} : (JSON.parse(text) as Record<string, unknown>);
    if (!response.ok) {
      const reason = typeof parsed.reason === 'string' ? parsed.reason : `The desk answered ${response.status}.`;
      throw new ToolFailure(reason);
    }
    return parsed;
  };

  const definitions = (): Tool[] =>
    TOOL_NAMES.map((name) => ({
      name,
      description: `${TOOL_SUMMARIES[name]} ${TOOL_DETAILS[name]}`,
      inputSchema: toInputSchema(SCHEMAS[name]),
    }));

  const runTool = async (name: string, rawArgs: unknown): Promise<CallResult> => {
    if (!isToolName(name)) {
      return errorResult(`This desk has no tool called "${name}". Its tools are ${TOOL_NAMES.join(', ')}.`);
    }
    const parsed = SCHEMAS[name].safeParse(rawArgs ?? {});
    if (!parsed.success) {
      const issue = parsed.error.issues[0];
      const where = issue && issue.path.length > 0 ? issue.path.join('.') : 'the arguments';
      return errorResult(`${where} ${issue?.message ?? 'is invalid'}.`);
    }
    try {
      const runner = RUNNERS[name] as (args: unknown, http: HttpCall) => Promise<unknown>;
      const result = await runner(parsed.data, call);
      return { content: [{ type: 'text', text: JSON.stringify(result, bigintReplacer, 2) }] };
    } catch (error) {
      if (error instanceof OfflineError) return errorResult(error.message);
      if (error instanceof ToolFailure) return errorResult(error.message);
      if (isDeskError(error)) return errorResult(error.reason);
      return errorResult(error instanceof Error ? error.message : String(error));
    }
  };

  return {
    tools: () => TOOL_NAMES,
    definitions,
    call: runTool,

    async serve() {
      const server = new Server(
        { name: 'covenant-desk', version: '0.1.0' },
        { capabilities: { tools: {} } },
      );
      server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: definitions() }));
      server.setRequestHandler(CallToolRequestSchema, async (request) =>
        runTool(request.params.name, request.params.arguments ?? {}),
      );
      await server.connect(new StdioServerTransport());
      await new Promise<void>((resolve) => {
        server.onclose = () => resolve();
      });
    },
  };
}

class OfflineError extends Error {}
class ToolFailure extends Error {}

function errorResult(text: string): CallResult {
  return { content: [{ type: 'text', text }], isError: true };
}

function isToolName(name: string): name is ToolName {
  return (TOOL_NAMES as readonly string[]).includes(name);
}

function toInputSchema(schema: z.ZodType): Tool['inputSchema'] {
  const json = z.toJSONSchema(schema, { io: 'input' }) as Record<string, unknown>;
  delete json.$schema;
  return { ...json, type: 'object' } as Tool['inputSchema'];
}

function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === 'bigint' ? value.toString() : value;
}
