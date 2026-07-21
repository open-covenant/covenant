import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js';
import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { readFileSync } from 'node:fs';
import { z } from 'zod';

type TextContent = { type: 'text'; text: string };
export type ToolResult = { content: TextContent[]; isError?: boolean };
type Fetch = typeof fetch;
type ToolInputSchema = Tool['inputSchema'];

const DEFAULT_DAEMON_URL = 'http://127.0.0.1:8421';

// Every tool is proxied to covenantd, which runs the capability gate and signs
// the request in the covenant-robinhood-signer sidecar. The private key never
// reaches this process; reads and the governed order take the same path.
const symbolsSchema = z.object({ symbols: z.array(z.string().min(1)).min(1) });
const holdingsSchema = z.object({ asset_codes: z.array(z.string().min(1)).default([]) });
const estimateSchema = z.object({
  symbol: z.string().min(1),
  side: z.enum(['buy', 'sell']),
  quantities: z.array(z.number().positive()).min(1),
});
const orderSchema = z.object({
  symbol: z.string().min(1),
  side: z.enum(['buy', 'sell']),
  type: z.enum(['market', 'limit']).default('market'),
  quantity: z.number().positive(),
  limit_price: z.number().positive().optional(),
});
const cancelSchema = z.object({ order_id: z.string().min(1) });
const receiptsSchema = z.object({ limit: z.number().int().positive().max(200).optional() });
const reputationSchema = z.object({ agent: z.string().min(1).optional() });

function describeTool(name: string, description: string, inputSchema: ToolInputSchema): Tool {
  return { name, description, inputSchema };
}

function asText(value: unknown): ToolResult {
  return { content: [{ type: 'text', text: JSON.stringify(value, null, 2) }] };
}

function asError(message: string): ToolResult {
  return { content: [{ type: 'text', text: message }], isError: true };
}

function daemonUrl(env: NodeJS.ProcessEnv): string {
  return (env.COVENANT_HTTP_URL ?? DEFAULT_DAEMON_URL).replace(/\/+$/, '');
}

function authToken(env: NodeJS.ProcessEnv): string | undefined {
  const explicit = env.COVENANT_AUTH_TOKEN?.trim();
  if (explicit) return explicit;
  if (env.COVENANT_HOME) {
    try {
      return readFileSync(`${env.COVENANT_HOME}/peers/operator.token`, 'utf8').trim();
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function redactSensitive(input: string, env: NodeJS.ProcessEnv): string {
  let out = input;
  for (const [value, label] of [
    [authToken(env), '<redacted-token>'],
    [env.COVENANT_HOME, '$COVENANT_HOME'],
    [env.HOME, '$HOME'],
  ] as const) {
    if (value) out = out.split(value).join(label);
  }
  return out;
}

async function daemonCall(tool: string, args: unknown, deps: { env: NodeJS.ProcessEnv; fetchImpl: Fetch }): Promise<unknown> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  const token = authToken(deps.env);
  if (token) headers.Authorization = `Bearer ${token}`;

  const response = await deps.fetchImpl(`${daemonUrl(deps.env)}/tools/call`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ name: tool, arguments: args }),
  });

  const text = await response.text();
  let payload: unknown = null;
  if (text.trim()) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = text;
    }
  }

  if (!response.ok) {
    throw new Error(`covenantd ${response.status}: ${redactSensitive(text, deps.env)}`);
  }
  return payload;
}

export const robinhoodTools = [
  describeTool('robinhood_account', 'Read the Robinhood crypto trading account.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('robinhood_holdings', 'Read crypto holdings, optionally filtered by asset code.', {
    type: 'object',
    properties: { asset_codes: { type: 'array', items: { type: 'string' } } },
  }),
  describeTool('robinhood_quote', 'Best bid/ask for one or more symbols (e.g. BTC-USD).', {
    type: 'object',
    properties: { symbols: { type: 'array', items: { type: 'string' } } },
    required: ['symbols'],
  }),
  describeTool('robinhood_estimated_price', 'Estimated fill price for a side and quantities.', {
    type: 'object',
    properties: {
      symbol: { type: 'string' },
      side: { type: 'string', enum: ['buy', 'sell'] },
      quantities: { type: 'array', items: { type: 'number' } },
    },
    required: ['symbol', 'side', 'quantities'],
  }),
  describeTool(
    'robinhood_governed_order',
    'Place an order through the Covenant policy gate. Rejected if it breaks policy; every decision is attested.',
    {
      type: 'object',
      properties: {
        symbol: { type: 'string' },
        side: { type: 'string', enum: ['buy', 'sell'] },
        type: { type: 'string', enum: ['market', 'limit'] },
        quantity: { type: 'number' },
        limit_price: { type: 'number' },
      },
      required: ['symbol', 'side', 'quantity'],
    },
  ),
  describeTool('robinhood_cancel_order', 'Cancel an open order by id.', {
    type: 'object',
    properties: { order_id: { type: 'string' } },
    required: ['order_id'],
  }),
  describeTool('robinhood_receipts', 'Recent Covenant trade receipts (placed and blocked).', {
    type: 'object',
    properties: { limit: { type: 'number' } },
  }),
  describeTool('robinhood_reputation', 'Trading reputation derived from the receipt history.', {
    type: 'object',
    properties: { agent: { type: 'string' } },
  }),
];

export async function callRobinhoodTool(
  name: string,
  args: unknown,
  deps: { env?: NodeJS.ProcessEnv; fetchImpl?: Fetch } = {},
): Promise<ToolResult | null> {
  const env = deps.env ?? process.env;
  const fetchImpl = deps.fetchImpl ?? fetch;
  const call = (tool: string, parsed: unknown) => daemonCall(tool, parsed, { env, fetchImpl });

  try {
    switch (name) {
      case 'robinhood_account':
        return asText(await call('robinhood.account', {}));
      case 'robinhood_holdings':
        return asText(await call('robinhood.holdings', holdingsSchema.parse(args)));
      case 'robinhood_quote':
        return asText(await call('robinhood.quote', symbolsSchema.parse(args)));
      case 'robinhood_estimated_price':
        return asText(await call('robinhood.estimated_price', estimateSchema.parse(args)));
      case 'robinhood_governed_order':
        return asText(await call('robinhood.place_order', orderSchema.parse(args)));
      case 'robinhood_cancel_order':
        return asText(await call('robinhood.cancel_order', cancelSchema.parse(args)));
      case 'robinhood_receipts':
        return asText(await call('robinhood.receipts', receiptsSchema.parse(args)));
      case 'robinhood_reputation':
        return asText(await call('robinhood.reputation', reputationSchema.parse(args)));
      default:
        return null;
    }
  } catch (error) {
    if (error instanceof z.ZodError) {
      return asError(error.issues.map((issue) => issue.message).join('; '));
    }
    if (error instanceof Error) return asError(redactSensitive(error.message, env));
    return asError('Unknown Robinhood bridge failure');
  }
}

const server = new Server(
  { name: 'covenant-robinhood', version: '0.1.0' },
  { capabilities: { tools: {} } },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: robinhoodTools }));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  return (
    (await callRobinhoodTool(request.params.name, request.params.arguments ?? {})) ??
    asError(`Unknown tool: ${request.params.name}`)
  );
});

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on('unhandledRejection', (reason) => {
    process.stderr.write(`robinhood-mcp-bridge: unhandled rejection: ${reason instanceof Error ? reason.message : String(reason)}\n`);
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    process.stderr.write(`robinhood-mcp-bridge: uncaught exception: ${err.message}\n`);
    process.exit(1);
  });
  const transport = new StdioServerTransport();
  await server.connect(transport);
}
