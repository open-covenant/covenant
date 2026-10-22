import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { PaymentPayload, PaymentRequirements } from '@x402/core/types';
import type { Gate } from './gate.js';
import type { CatalogFilter, Store } from './store.js';

export interface FacilitatorApi {
  gate: Gate;
  store: Store;
  supported(): unknown;
  /** Public facts about this facilitator, served at `/`. */
  about(): Record<string, unknown>;
  /**
   * Whether every configured network can settle right now: its RPC answers
   * and its signer holds enough for gas. A static 200 would keep sellers
   * routing payments here after a signer ran dry.
   */
  readiness(): Promise<{ ok: boolean; networks: Record<string, unknown> }>;
}

type Log = (event: Record<string, unknown>) => void;

class RequestError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

const PAYMENT_LIMIT_PER_MINUTE = 120;
const DISCOVERY_LIMIT_PER_MINUTE = 600;
const MAX_TRACKED_CLIENTS = 50_000;
const READINESS_CACHE_MS = 15_000;

class RateLimit {
  private windows = new Map<string, number>();
  private minute = 0;

  constructor(private readonly perMinute: number) {}

  allow(client: string, now = Date.now()): boolean {
    const minute = Math.floor(now / 60_000);
    // A new minute starts every count over. Past the size cap within one
    // minute (forged addresses), start over too rather than scan the map.
    if (minute !== this.minute || this.windows.size >= MAX_TRACKED_CLIENTS) {
      this.windows = new Map();
      this.minute = minute;
    }
    const count = (this.windows.get(client) ?? 0) + 1;
    this.windows.set(client, count);
    return count <= this.perMinute;
  }
}

/**
 * Cloudflare sets cf-connecting-ip on the custom domain; Render sets
 * x-forwarded-for. Either can be forged by a caller that reaches Render
 * directly, so these limits slow abuse down rather than stop it; the daily
 * settlement caps are what bound the fee payer's spend.
 */
function clientAddress(request: IncomingMessage): string {
  const cloudflare = request.headers['cf-connecting-ip'];
  if (typeof cloudflare === 'string' && cloudflare) return cloudflare;
  const forwarded = request.headers['x-forwarded-for'];
  if (typeof forwarded === 'string' && forwarded) return forwarded.split(',')[0]!.trim();
  return request.socket.remoteAddress ?? 'unknown';
}

async function readJson(request: IncomingMessage, limit: number): Promise<unknown> {
  const chunks: Buffer[] = [];
  let received = 0;
  for await (const chunk of request) {
    received += chunk.length;
    if (received > limit) throw new RequestError(413, 'request body is too large');
    chunks.push(chunk as Buffer);
  }
  if (received === 0) throw new RequestError(400, 'request body is required');
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new RequestError(400, 'request body must be JSON');
  }
}

function writeJson(
  response: ServerResponse,
  status: number,
  body: unknown,
  headers: Record<string, string> = {},
): void {
  const payload = JSON.stringify(body);
  response.writeHead(status, {
    'content-type': 'application/json',
    'cache-control': 'no-store',
    'content-length': Buffer.byteLength(payload),
    ...headers,
  });
  response.end(payload);
}

function paymentBody(body: unknown): { payload: PaymentPayload; requirements: PaymentRequirements } {
  const value = body as { paymentPayload?: unknown; paymentRequirements?: unknown } | null;
  if (
    !value ||
    typeof value !== 'object' ||
    typeof value.paymentPayload !== 'object' ||
    value.paymentPayload === null ||
    typeof value.paymentRequirements !== 'object' ||
    value.paymentRequirements === null
  ) {
    throw new RequestError(400, 'paymentPayload and paymentRequirements are required');
  }
  return {
    payload: value.paymentPayload as PaymentPayload,
    requirements: value.paymentRequirements as PaymentRequirements,
  };
}

function pageSize(query: URLSearchParams): { limit: number; offset: number } {
  const limit = Number(query.get('limit') ?? 20);
  const offset = Number(query.get('offset') ?? 0);
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new RequestError(400, 'limit must be 1 to 100');
  if (!Number.isInteger(offset) || offset < 0) throw new RequestError(400, 'offset must be a whole number');
  return { limit, offset };
}

function catalogFilter(query: URLSearchParams): CatalogFilter {
  const filter: CatalogFilter = {};
  for (const key of ['type', 'payTo', 'scheme', 'network', 'extensions'] as const) {
    const value = query.get(key);
    if (value) filter[key] = value.slice(0, 200);
  }
  return filter;
}

function searchTerms(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter((term) => term.length > 0)
    .slice(0, 8);
}

export function createFacilitatorServer(
  api: FacilitatorApi,
  config: { maxRequestBytes: number },
  log: Log = (event) => console.log(JSON.stringify(event)),
): Server {
  const payments = new RateLimit(PAYMENT_LIMIT_PER_MINUTE);
  const discovery = new RateLimit(DISCOVERY_LIMIT_PER_MINUTE);
  // Readiness asks every network's RPC for a balance; an unthrottled probe
  // would spend RPC quota on anyone's behalf.
  let readiness: { atMs: number; result: Promise<{ ok: boolean; networks: Record<string, unknown> }> } | undefined;
  const ready = () => {
    if (!readiness || Date.now() - readiness.atMs > READINESS_CACHE_MS) {
      readiness = { atMs: Date.now(), result: api.readiness() };
    }
    return readiness.result;
  };
  const server = createServer((request, response) => {
    void handle(request, response, { ...api, readiness: ready }, config, log, payments, discovery).catch((error) => {
      const status = error instanceof RequestError ? error.status : 500;
      const message = error instanceof RequestError ? error.message : 'facilitator error';
      if (status >= 500) log({ event: 'facilitator_error', message: String(error) });
      if (!response.headersSent) writeJson(response, status, { error: message });
    });
  });
  server.headersTimeout = 10_000;
  server.requestTimeout = 15_000;
  return server;
}

async function handle(
  request: IncomingMessage,
  response: ServerResponse,
  api: FacilitatorApi,
  config: { maxRequestBytes: number },
  log: Log,
  payments: RateLimit,
  discovery: RateLimit,
): Promise<void> {
  const url = new URL(request.url ?? '/', 'http://facilitator.invalid');
  const path = url.pathname;
  const cors = { 'access-control-allow-origin': '*' };

  if (request.method === 'GET' && path === '/healthz') return writeJson(response, 200, { ok: true });
  if (request.method === 'GET' && path === '/readyz') {
    try {
      const readiness = await api.readiness();
      return writeJson(response, readiness.ok ? 200 : 503, readiness);
    } catch (error) {
      log({ event: 'facilitator_not_ready', message: String(error) });
      return writeJson(response, 503, { ok: false });
    }
  }

  if (request.method === 'GET') {
    if (!discovery.allow(clientAddress(request))) throw new RequestError(429, 'too many requests');
    if (path === '/') return writeJson(response, 200, api.about(), cors);
    if (path === '/supported') return writeJson(response, 200, api.supported(), cors);
    if (path === '/discovery/resources') {
      const { limit, offset } = pageSize(url.searchParams);
      const { items, total } = api.store.catalog(catalogFilter(url.searchParams), { limit, offset });
      return writeJson(response, 200, { x402Version: 2, items, pagination: { limit, offset, total } }, cors);
    }
    if (path === '/discovery/search') {
      const query = url.searchParams.get('query')?.trim();
      if (!query) throw new RequestError(400, 'query is required');
      const { limit } = pageSize(url.searchParams);
      const { items, total } = api.store.catalog(catalogFilter(url.searchParams), { limit, offset: 0 }, searchTerms(query));
      return writeJson(
        response,
        200,
        { x402Version: 2, resources: items, partialResults: total > items.length, pagination: null },
        cors,
      );
    }
  }

  if (request.method === 'POST' && (path === '/verify' || path === '/settle')) {
    if (!payments.allow(clientAddress(request))) throw new RequestError(429, 'too many requests');
    const { payload, requirements } = paymentBody(await readJson(request, config.maxRequestBytes));
    if (path === '/verify') {
      const result = await api.gate.verify(payload, requirements);
      log({ event: 'facilitator_verify', network: requirements.network, isValid: result.isValid, reason: result.invalidReason });
      return writeJson(response, 200, result);
    }
    const { response: result, bazaar } = await api.gate.settle(payload, requirements);
    log({
      event: 'facilitator_settle',
      network: requirements.network,
      success: result.success,
      reason: result.errorReason,
      transaction: result.transaction || undefined,
      bazaar: bazaar?.status,
    });
    // Extension outcomes go to the seller only, never into the buyer's PAYMENT-RESPONSE.
    const headers: Record<string, string> = bazaar
      ? { 'extension-responses': Buffer.from(JSON.stringify({ bazaar })).toString('base64') }
      : {};
    return writeJson(response, 200, result, headers);
  }

  if (request.method === 'OPTIONS') {
    response.writeHead(204, { ...cors, 'access-control-allow-methods': 'GET', 'access-control-max-age': '86400' });
    response.end();
    return;
  }
  throw new RequestError(404, 'not found');
}
