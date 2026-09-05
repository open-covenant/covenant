/**
 * Local HTTP API.
 *
 * Binds to 127.0.0.1 only. Every route except `GET /v1/health` and the page
 * itself requires `Authorization: Bearer <token>` from the config file. Errors
 * return `{ error, reason }` with the code from `core/errors`.
 */

import { timingSafeEqual } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { z } from 'zod';
import type { Desk } from '../../daemon.js';
import { DeskError, NotFoundError, isDeskError, toErrorResponse, type DeskErrorCode } from '../../core/errors.js';
import { toRawAmount, type Address, type HedgePlan, type OrderStatus, type Pool } from '../../core/types.js';
import { version } from '../../core/version.js';
import { createUiSurface } from '../ui/index.js';

/** Every route the API answers. */
export const ROUTES = [
  { method: 'GET', path: '/v1/health', auth: false, description: 'Liveness. Returns version and uptime.' },
  { method: 'GET', path: '/v1/status', auth: true, description: 'Session state, block, live or dry run, loop health.' },
  { method: 'GET', path: '/v1/tokens', auth: true, description: 'Stock tokens with multiplier, feed, and pause flags.' },
  { method: 'GET', path: '/v1/pools', auth: true, description: 'Pools for a stock symbol, deepest first, traps flagged.' },
  { method: 'GET', path: '/v1/quote', auth: true, description: 'Price a buy or a sell of a given USD size.' },
  { method: 'GET', path: '/v1/premium', auth: true, description: 'On-chain price against reference for every stock token.' },
  { method: 'POST', path: '/v1/orders', auth: true, description: 'Create a conditional order. Dry run by default.' },
  { method: 'GET', path: '/v1/orders', auth: true, description: 'List orders and their status.' },
  { method: 'GET', path: '/v1/orders/:id', auth: true, description: 'One order, with every fill recorded against it.' },
  { method: 'DELETE', path: '/v1/orders/:id', auth: true, description: 'Cancel an open order.' },
  { method: 'GET', path: '/v1/hedge/plan', auth: true, description: 'Size the short that cancels a stock leg.' },
  { method: 'POST', path: '/v1/hedge/apply', auth: true, description: 'Send a hedge plan, when sending is possible.' },
  { method: 'POST', path: '/v1/hedge/unwind', auth: true, description: 'Close the short for one symbol, or for all of them.' },
  { method: 'GET', path: '/v1/hedge', auth: true, description: 'Current hedge positions and funding.' },
  { method: 'GET', path: '/v1/recorder', auth: true, description: 'Recorded premium observations since a timestamp.' },
  { method: 'POST', path: '/v1/recorder/start', auth: true, description: 'Start recording on-chain prices against reference prices.' },
  { method: 'POST', path: '/v1/recorder/stop', auth: true, description: 'Stop recording.' },
  { method: 'GET', path: '/', auth: false, description: 'The desk page.' },
  { method: 'GET', path: '/app.js', auth: false, description: 'The script the page loads.' },
  { method: 'GET', path: '/app.css', auth: false, description: 'The stylesheet the page loads.' },
] as const;

export type Route = (typeof ROUTES)[number];

export interface HttpSurface {
  /** Start listening on 127.0.0.1 and the configured port. */
  start(): Promise<{ url: string }>;
  stop(): Promise<void>;
  readonly url: string;
  readonly listening: boolean;
}

/** Query parameters accepted by `GET /v1/quote`. */
export interface QuoteQuery {
  /** Token address or symbol. */
  readonly token: string;
  /** Size of the order in USD. Default 100. */
  readonly amountUsd?: number;
  readonly side?: 'buy' | 'sell';
}

export interface HttpOptions {
  /** Overrides `config.port`. Port 0 asks the operating system for a free port. */
  readonly port?: number;
}

const HOST = '127.0.0.1';
const MAX_BODY_BYTES = 1_000_000;

const STATUS_BY_CODE: Record<DeskErrorCode, number> = {
  not_implemented: 501,
  config_invalid: 400,
  invalid_request: 400,
  invalid_state: 409,
  keystore_missing: 400,
  keystore_permissions: 400,
  not_found: 404,
  bounds_exceeded: 422,
  trap_pool: 422,
  no_reference: 503,
  upstream_failed: 502,
  live_disabled: 409,
  store_failed: 500,
  unauthorized: 401,
};

interface Ctx {
  readonly desk: Desk;
  readonly url: URL;
  readonly params: Readonly<Record<string, string>>;
  body<T>(schema: z.ZodType<T>): Promise<T>;
}

interface Asset {
  readonly contentType: string;
  readonly text: string;
}

type Handler = (ctx: Ctx) => Promise<unknown>;

interface Entry {
  readonly method: string;
  readonly segments: readonly string[];
  readonly auth: boolean;
  readonly handle: Handler;
}

export function createHttpSurface(desk: Desk, options: HttpOptions = {}): HttpSurface {
  const ui = createUiSurface();
  const entries = routeTable(ui);
  let server: Server | undefined;
  let boundUrl = `http://${HOST}:${options.port ?? desk.config.port}`;

  const onRequest = (req: IncomingMessage, res: ServerResponse) => {
    void handle(desk, entries, req, res).catch((error: unknown) => {
      desk.logger.error('the API could not answer a request', {
        method: req.method,
        path: req.url,
        reason: error instanceof Error ? error.message : String(error),
      });
      if (!res.headersSent) sendJson(res, 500, toErrorResponse(error));
      else res.end();
    });
  };

  return {
    get url() {
      return boundUrl;
    },
    get listening() {
      return server?.listening ?? false;
    },

    start() {
      if (server) return Promise.resolve({ url: boundUrl });
      const next = createServer(onRequest);
      next.keepAliveTimeout = 5_000;
      return new Promise<{ url: string }>((resolve, reject) => {
        const onError = (error: NodeJS.ErrnoException) => {
          next.close();
          reject(
            error.code === 'EADDRINUSE'
              ? new DeskError(
                  'config_invalid',
                  `Port ${options.port ?? desk.config.port} on ${HOST} is already in use. Stop the desk that holds it, or set another port in config.json.`,
                  { port: options.port ?? desk.config.port },
                )
              : error,
          );
        };
        next.once('error', onError);
        next.listen(options.port ?? desk.config.port, HOST, () => {
          next.off('error', onError);
          const address = next.address();
          const port = typeof address === 'object' && address ? address.port : desk.config.port;
          boundUrl = `http://${HOST}:${port}`;
          server = next;
          resolve({ url: boundUrl });
        });
      });
    },

    stop() {
      const running = server;
      server = undefined;
      if (!running) return Promise.resolve();
      running.closeIdleConnections();
      return new Promise<void>((resolve) => running.close(() => resolve()));
    },
  };
}

async function handle(desk: Desk, entries: readonly Entry[], req: IncomingMessage, res: ServerResponse): Promise<void> {
  const url = new URL(req.url ?? '/', `http://${HOST}`);
  if (!isLoopbackHost(req.headers.host)) {
    sendJson(res, 403, {
      error: 'unauthorized',
      reason: 'The desk answers requests addressed to 127.0.0.1 only.',
    });
    return;
  }

  const method = (req.method ?? 'GET').toUpperCase();
  const segments = url.pathname.split('/').filter((part) => part !== '');
  const match = matchRoute(entries, method, segments);

  if (!match) {
    const pathExists = entries.some((entry) => matchSegments(entry.segments, segments));
    sendJson(res, pathExists ? 405 : 404, {
      error: 'not_found',
      reason: pathExists
        ? `${method} is not accepted on ${url.pathname}.`
        : `${url.pathname} is not a route on this desk. See GET /v1/health for the version.`,
    });
    return;
  }

  if (match.entry.auth && !authorized(req, desk.config.token)) {
    res.setHeader('WWW-Authenticate', 'Bearer realm="covenant-desk"');
    sendJson(res, 401, {
      error: 'unauthorized',
      reason: 'This route needs the bearer token from config.json in an Authorization header.',
    });
    return;
  }

  const ctx: Ctx = {
    desk,
    url,
    params: match.params,
    body: async <T,>(schema: z.ZodType<T>) => parseBody(schema, await readBody(req)),
  };

  const started = Date.now();
  try {
    const result = await match.entry.handle(ctx);
    if (isAsset(result)) {
      res.writeHead(200, { 'content-type': result.contentType, 'cache-control': 'no-store' });
      res.end(result.text);
    } else {
      sendJson(res, 200, result);
    }
    desk.logger.debug('request answered', { method, path: url.pathname, ms: Date.now() - started });
  } catch (error) {
    const status = isDeskError(error) ? STATUS_BY_CODE[error.code] : 500;
    if (status >= 500) {
      desk.logger.warn('request failed', {
        method,
        path: url.pathname,
        reason: error instanceof Error ? error.message : String(error),
      });
    }
    sendJson(res, status, toErrorResponse(error));
  }
}

function routeTable(ui: ReturnType<typeof createUiSurface>): Entry[] {
  const route = (method: string, path: string, auth: boolean, handle: Handler): Entry => ({
    method,
    segments: path.split('/').filter((part) => part !== ''),
    auth,
    handle,
  });

  return [
    // Liveness answers from constants. It needs no token, so it must never
    // read the database or the chain.
    route('GET', '/v1/health', false, async ({ desk }) => ({
      ok: true,
      name: 'covenant-desk',
      version: version(),
      uptimeSec: Math.floor((Date.now() - desk.startedAt) / 1000),
      chainId: desk.config.chainId,
    })),

    route('GET', '/v1/status', true, ({ desk }) => desk.status()),

    route('GET', '/v1/tokens/:key', true, async ({ desk, params }) => {
      const key = params.key ?? '';
      const known = findToken(desk, key);
      if (known) return { token: known, source: 'desk' };
      if (/^0x[0-9a-fA-F]{40}$/.test(key)) {
        const read = await desk.chain.tokens.decimalsOf([key as Address]);
        const decimals = read.get(key.toLowerCase());
        if (decimals !== undefined) {
          return {
            token: { address: key.toLowerCase(), decimals, symbol: `${key.slice(0, 6)}...${key.slice(-4)}` },
            source: 'chain',
          };
        }
      }
      throw new NotFoundError(`Token ${key}`, { token: key });
    }),

    route('GET', '/v1/tokens', true, async ({ desk, url }) => {
      const stockOnly = boolParam(url, 'stockOnly') ?? true;
      const tokens = stockOnly ? desk.chain.registry.stockTokens() : desk.store.listTokens();
      const limit = intParam(url, 'limit') ?? tokens.length;
      return { tokens: tokens.slice(0, limit), count: tokens.length };
    }),

    route('GET', '/v1/pools', true, async ({ desk, url }) => {
      const stock = url.searchParams.get('stock') ?? url.searchParams.get('token');
      if (!stock) throw new DeskError('config_invalid', 'Name the stock token: /v1/pools?stock=NVDA.');
      const token = resolveToken(desk, stock);
      const pools = await desk.chain.pools.forToken(token.address, {
        includeTraps: boolParam(url, 'includeTraps') ?? false,
        limit: intParam(url, 'limit') ?? 25,
      });
      return { stock: token.symbol, token: token.address, pools, names: nameCurrencies(desk, pools) };
    }),

    route('GET', '/v1/quote', true, async ({ desk, url }) => {
      const key = url.searchParams.get('token');
      if (!key) throw new DeskError('config_invalid', 'Name the token: /v1/quote?token=NVDA&amountUsd=100.');
      const amountUsd = numberParam(url, 'amountUsd') ?? 100;
      const side = sideParam(url) ?? 'buy';
      const token = findToken(desk, key);

      if (token?.isStockToken || (!token && !key.startsWith('0x'))) {
        const symbol = token?.symbol ?? key.toUpperCase();
        const fairValue = await desk.fairvalue.premium.forSymbol(symbol);
        const sized = await desk.fairvalue.premium.sized(fairValue, { amountUsd, side });
        return {
          kind: 'stock',
          side,
          amountUsd,
          symbol,
          fairValue,
          ...(sized
            ? { sizedPrice: sized.price, note: sized.note }
            : {
                note: `No pool could price ${amountUsd} USD of ${symbol} right now, so these are mid prices with no price impact.`,
              }),
        };
      }

      const address = (token?.address ?? key) as Address;
      const quote = await desk.fairvalue.paired.quote(address, { amountUsd, side });
      return { kind: 'paired', side, amountUsd, token: address, quote };
    }),

    route('GET', '/v1/premium', true, async ({ desk, url }) => {
      const limit = intParam(url, 'limit') ?? intParam(url, 'top') ?? 20;
      const rows = await desk.fairvalue.premium.all({ limit });
      const since = Date.now() - 6 * 60 * 60 * 1000;
      const premium = rows.map((row) => {
        const recent = desk.store.listObservations({ symbol: row.symbol, since, limit: 400 })
          .filter((entry) => entry.pool === row.pool && entry.onchainMidUsd !== undefined)
          .map((entry) => entry.onchainMidUsd as number);
        const stalePool = recent.length >= 10 && Math.min(...recent) === Math.max(...recent);
        return {
          ...row,
          liquidity: row.pool ? desk.store.getPool(row.pool)?.liquidity?.toString() : undefined,
          stalePool,
        };
      });
      return { premium, asOf: Date.now() };
    }),

    route('POST', '/v1/orders', true, async ({ desk, body }) => {
      const input = toCreateOrderInput(await body(OrderBodySchema));
      const orders = await desk.orders.book.create(input);
      return { orders };
    }),

    route('GET', '/v1/orders', true, async ({ desk, url }) => {
      const status = statusParam(url);
      const orders = desk.orders.book.list({ status, limit: intParam(url, 'limit') ?? 100 });
      return { orders };
    }),

    route('GET', '/v1/orders/:id', true, async ({ desk, params }) => {
      const id = params.id;
      if (!id) throw new NotFoundError('The order');
      const order = desk.orders.book.get(id);
      if (!order) throw new NotFoundError(`Order ${id}`, { order: id });
      return { order, executions: desk.store.listExecutions({ orderId: id, limit: 50 }) };
    }),

    route('DELETE', '/v1/orders/:id', true, async ({ desk, params, url }) => {
      const id = params.id;
      if (!id) throw new NotFoundError('The order');
      const order = await desk.orders.book.cancel(id, url.searchParams.get('reason') ?? undefined);
      return { order };
    }),

    route('GET', '/v1/hedge/plan', true, async ({ desk, url }) => {
      const plan = await hedgePlan(desk, {
        symbol: url.searchParams.get('symbol') ?? undefined,
        stockSymbol: url.searchParams.get('stockSymbol') ?? undefined,
        stockLegUsd: numberParam(url, 'stockLegUsd'),
        qty: numberParam(url, 'qty'),
        ratio: numberParam(url, 'ratio'),
      });
      return { plan };
    }),

    // The plan is always sized here, from the exposure, so a posted plan
    // cannot ask the desk to send a size the sizer would refuse.
    route('POST', '/v1/hedge/apply', true, async ({ desk, body }) => {
      const request = await body(HedgeApplySchema);
      const plan = await hedgePlan(desk, fromPostedPlan(request));
      const applied = await desk.hedge.rebalancer.apply(plan);
      return { plan: applied };
    }),

    route('POST', '/v1/hedge/unwind', true, async ({ desk, body }) => {
      const request = await body(HedgeUnwindSchema);
      const plans = await desk.hedge.rebalancer.unwind(request.symbol);
      return { plans };
    }),

    route('GET', '/v1/hedge', true, async ({ desk }) => {
      try {
        const positions = await desk.hedge.client.positions();
        const account = desk.hedge.client.readiness().accountIndex;
        return {
          positions,
          source: 'lighter-rh',
          // An empty list means one of two things, and they are not the same
          // answer, so a desk with no account says so.
          ...(account === undefined
            ? {
                reason:
                  'No Lighter Robinhood Chain sub-account is configured, so the desk cannot see any position. Add DESK_LIGHTER_RH_ACCOUNT_INDEX to keys.env.',
              }
            : {}),
          asOf: Date.now(),
        };
      } catch (error) {
        return {
          positions: desk.store.listHedges(),
          source: 'store',
          reason: `The hedge venue could not be read, so these are the last recorded positions: ${
            isDeskError(error) ? error.reason : String(error)
          }`,
          asOf: Date.now(),
        };
      }
    }),

    route('GET', '/v1/recorder', true, async ({ desk, url }) => {
      const since = intParam(url, 'since') ?? Date.now() - 24 * 60 * 60 * 1000;
      const observations = desk.store.listObservations({
        since,
        until: intParam(url, 'until'),
        symbol: url.searchParams.get('symbol') ?? undefined,
        limit: intParam(url, 'limit') ?? 5_000,
      });
      return { since, running: recorderRunning(desk), observations };
    }),

    route('POST', '/v1/recorder/start', true, async ({ desk }) => {
      desk.fairvalue.recorder.start();
      return { running: recorderRunning(desk), intervalSec: desk.config.recorderIntervalSec };
    }),

    route('POST', '/v1/recorder/stop', true, async ({ desk }) => {
      desk.fairvalue.recorder.stop();
      return { running: recorderRunning(desk) };
    }),

    route('GET', '/', false, async () => asset('text/html; charset=utf-8', ui.html())),
    route('GET', '/app.js', false, async () => asset('text/javascript; charset=utf-8', ui.script())),
    route('GET', '/app.css', false, async () => asset('text/css; charset=utf-8', ui.styles())),
  ];
}

const AddressSchema = z
  .string()
  .regex(/^0x[0-9a-fA-F]{40}$/, 'must be a 20-byte address such as 0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC');

const TriggerSchema = z.object({
  priceLte: z.number().optional(),
  priceGte: z.number().optional(),
  priceBasis: z.enum(['usdOnchain', 'usdFair']).optional(),
  premiumLteBps: z.number().optional(),
  premiumGteBps: z.number().optional(),
  atNextOpenOffsetSec: z.number().int().optional(),
  at: z.number().int().optional(),
});

const BoundsSchema = z.object({
  maxSlippageBps: z.number().int().min(0).max(10_000).optional(),
  maxOrderNotionalUsd: z.number().positive().optional(),
  maxBuyPremiumBps: z.number().int().min(0).max(100_000).optional(),
});

const LegSchema = z.object({
  side: z.enum(['buy', 'sell']),
  tokenIn: AddressSchema,
  tokenOut: AddressSchema,
  /** Smallest units of `tokenIn`. */
  amountIn: z.union([z.string().regex(/^[0-9]+$/, 'must be a whole number of smallest units'), z.number().int().nonnegative()]).optional(),
  /** Whole tokens. Converted with `decimals`, which defaults to 18. */
  amountInTokens: z.union([z.string(), z.number()]).optional(),
  decimals: z.number().int().min(0).max(36).optional(),
  trigger: TriggerSchema.default({}),
  bounds: BoundsSchema.optional(),
  live: z.boolean().optional(),
  expiresAt: z.number().int().nullable().optional(),
});

const OrderBodySchema = LegSchema.extend({
  kind: z.enum(['limit', 'stop', 'takeProfit', 'oco', 'atOpen', 'premium']),
  legs: z.tuple([LegSchema, LegSchema]).optional(),
});

const HedgeApplySchema = z.object({
  /** A plan from `/v1/hedge/plan`. Only the symbol and the exposure are used. */
  plan: z
    .object({
      symbol: z.string().optional(),
      stockLegNotionalUsd: z.object({ value: z.number() }).partial().optional(),
    })
    .loose()
    .optional(),
  symbol: z.string().optional(),
  stockSymbol: z.string().optional(),
  stockLegUsd: z.number().optional(),
  qty: z.number().optional(),
  ratio: z.number().optional(),
});

const HedgeUnwindSchema = z.object({ symbol: z.string().optional() });

type OrderBody = z.infer<typeof OrderBodySchema>;
type Leg = z.infer<typeof LegSchema>;

function toCreateOrderInput(body: OrderBody) {
  const base = toLeg(body);
  return {
    ...base,
    kind: body.kind,
    legs: body.legs ? ([toLeg(body.legs[0]), toLeg(body.legs[1])] as const) : undefined,
  };
}

function toLeg(leg: Leg) {
  return {
    side: leg.side,
    tokenIn: leg.tokenIn as Address,
    tokenOut: leg.tokenOut as Address,
    amountIn: rawAmount(leg),
    trigger: leg.trigger,
    bounds: leg.bounds,
    live: leg.live ?? false,
    expiresAt: leg.expiresAt ?? null,
  };
}

function rawAmount(leg: Leg): bigint {
  if (leg.amountIn !== undefined) return BigInt(leg.amountIn);
  if (leg.amountInTokens !== undefined) return toRawAmount(leg.amountInTokens, leg.decimals ?? 18);
  throw new DeskError(
    'config_invalid',
    'Set amountIn in smallest units, or amountInTokens in whole tokens with the token decimals.',
  );
}

interface HedgeRequest {
  readonly symbol?: string | undefined;
  readonly stockSymbol?: string | undefined;
  readonly stockLegUsd?: number | undefined;
  readonly qty?: number | undefined;
  readonly ratio?: number | undefined;
}

/** Read the exposure out of a posted plan, so it can be sized again here. */
function fromPostedPlan(request: z.infer<typeof HedgeApplySchema>): HedgeRequest {
  if (!request.plan) return request;
  return {
    symbol: request.symbol ?? request.plan.symbol,
    stockSymbol: request.stockSymbol,
    stockLegUsd: request.stockLegUsd ?? request.plan.stockLegNotionalUsd?.value,
    qty: request.qty,
    ratio: request.ratio,
  };
}

async function hedgePlan(desk: Desk, request: HedgeRequest): Promise<HedgePlan> {
  const stockSymbol = request.stockSymbol ?? request.symbol;
  if (!stockSymbol) {
    throw new DeskError('config_invalid', 'Name the stock the position carries, for example symbol=NVDA.');
  }
  if (request.stockLegUsd !== undefined) {
    return desk.hedge.sizer.plan({ symbol: stockSymbol, stockLegUsd: request.stockLegUsd });
  }
  if (request.qty !== undefined && request.ratio !== undefined) {
    const sized = await desk.hedge.sizer.stockLegForPaired({ qtyX: request.qty, ratio: request.ratio, stockSymbol });
    return desk.hedge.sizer.plan({ symbol: stockSymbol, stockLegUsd: sized.stockLegUsd.value });
  }
  if (request.qty !== undefined) {
    const sized = await desk.hedge.sizer.stockLegForToken({ symbol: stockSymbol, qty: request.qty });
    return desk.hedge.sizer.plan({ symbol: stockSymbol, stockLegUsd: sized.stockLegUsd.value });
  }
  throw new DeskError(
    'config_invalid',
    'Give the stock exposure to cancel: stockLegUsd in dollars, or qty with the ratio of stock tokens per paired token.',
  );
}

/** Symbol per currency address, for the currencies these pools hold. */
function nameCurrencies(desk: Desk, pools: readonly Pool[]): Record<string, string> {
  const names: Record<string, string> = {};
  for (const pool of pools) {
    for (const currency of [pool.currency0, pool.currency1]) {
      const key = currency.toLowerCase();
      if (names[key]) continue;
      const known = findToken(desk, key);
      if (known) names[key] = known.symbol;
    }
  }
  return names;
}

function resolveToken(desk: Desk, key: string) {
  const token = findToken(desk, key);
  if (!token) throw new NotFoundError(`Token ${key}`, { token: key });
  return token;
}

function findToken(desk: Desk, key: string) {
  try {
    const fromRegistry = desk.chain.registry.token(key);
    if (fromRegistry) return fromRegistry;
  } catch {
    // The registry has not loaded yet. The store still answers from the last refresh.
  }
  return desk.store.getToken(key);
}

function recorderRunning(desk: Desk): boolean {
  try {
    return desk.fairvalue.recorder.running();
  } catch {
    return false;
  }
}

function parseBody<T>(schema: z.ZodType<T>, text: string): T {
  let raw: unknown;
  try {
    raw = text.trim() === '' ? {} : JSON.parse(text);
  } catch (error) {
    throw new DeskError('config_invalid', `The request body is not valid JSON: ${(error as Error).message}`);
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    const issue = parsed.error.issues[0];
    const where = issue && issue.path.length > 0 ? issue.path.join('.') : 'the request body';
    throw new DeskError('config_invalid', `${where} ${issue?.message ?? 'is invalid'}.`, {
      issues: parsed.error.issues,
    });
  }
  return parsed.data;
}

async function readBody(req: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const buffer = chunk as Buffer;
    size += buffer.length;
    if (size > MAX_BODY_BYTES) throw new DeskError('config_invalid', 'The request body is larger than 1 MB.');
    chunks.push(buffer);
  }
  return Buffer.concat(chunks).toString('utf8');
}

function authorized(req: IncomingMessage, token: string): boolean {
  const header = req.headers.authorization;
  if (!header || !header.startsWith('Bearer ')) return false;
  const supplied = Buffer.from(header.slice(7).trim());
  const expected = Buffer.from(token);
  return supplied.length === expected.length && timingSafeEqual(supplied, expected);
}

function isLoopbackHost(host: string | undefined): boolean {
  if (!host) return true;
  const name = host.startsWith('[') ? host.slice(1, host.indexOf(']')) : (host.split(':')[0] ?? '');
  return name === '127.0.0.1' || name === 'localhost' || name === '::1';
}

function matchRoute(entries: readonly Entry[], method: string, segments: readonly string[]) {
  for (const entry of entries) {
    if (entry.method !== method) continue;
    const params = matchSegments(entry.segments, segments);
    if (params) return { entry, params };
  }
  return undefined;
}

function matchSegments(
  pattern: readonly string[],
  segments: readonly string[],
): Record<string, string> | undefined {
  if (pattern.length !== segments.length) return undefined;
  const params: Record<string, string> = {};
  for (let i = 0; i < pattern.length; i += 1) {
    const expected = pattern[i] ?? '';
    const actual = segments[i] ?? '';
    if (expected.startsWith(':')) params[expected.slice(1)] = decodeURIComponent(actual);
    else if (expected !== actual) return undefined;
  }
  return params;
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const text = JSON.stringify(body, bigintReplacer);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(text),
    'cache-control': 'no-store',
  });
  res.end(text);
}

function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === 'bigint' ? value.toString() : value;
}

function asset(contentType: string, text: string): Asset {
  return { contentType, text };
}

function isAsset(value: unknown): value is Asset {
  return typeof value === 'object' && value !== null && 'contentType' in value && 'text' in value;
}

function numberParam(url: URL, name: string): number | undefined {
  const raw = url.searchParams.get(name);
  if (raw === null || raw.trim() === '') return undefined;
  const value = Number(raw);
  if (!Number.isFinite(value)) throw new DeskError('config_invalid', `${name} must be a number, got "${raw}".`);
  return value;
}

function intParam(url: URL, name: string): number | undefined {
  const value = numberParam(url, name);
  if (value === undefined) return undefined;
  if (!Number.isInteger(value)) throw new DeskError('config_invalid', `${name} must be a whole number, got "${value}".`);
  return value;
}

function boolParam(url: URL, name: string): boolean | undefined {
  const raw = url.searchParams.get(name);
  if (raw === null) return undefined;
  if (['1', 'true', 'yes'].includes(raw.toLowerCase())) return true;
  if (['0', 'false', 'no'].includes(raw.toLowerCase())) return false;
  throw new DeskError('config_invalid', `${name} must be true or false, got "${raw}".`);
}

function sideParam(url: URL): 'buy' | 'sell' | undefined {
  const raw = url.searchParams.get('side');
  if (raw === null) return undefined;
  if (raw === 'buy' || raw === 'sell') return raw;
  throw new DeskError('config_invalid', `side must be buy or sell, got "${raw}".`);
}

function statusParam(url: URL): OrderStatus | undefined {
  const raw = url.searchParams.get('status');
  if (raw === null) return undefined;
  const allowed: OrderStatus[] = ['open', 'triggered', 'filled', 'failed', 'cancelled', 'expired'];
  if (!allowed.includes(raw as OrderStatus)) {
    throw new DeskError('config_invalid', `status must be one of ${allowed.join(', ')}, got "${raw}".`);
  }
  return raw as OrderStatus;
}
