import { request as httpRequest } from 'node:http';
import { afterEach, describe, expect, it } from 'vitest';
import { NotFoundError, NotImplementedError, UpstreamError } from '../../src/core/errors.js';
import { ROUTES } from '../../src/surfaces/http/index.js';
import type { FairValue, Order } from '../../src/core/types.js';
import { deskFixture, startDesk, type RunningDesk } from './fixtures.js';

let running: RunningDesk | undefined;

afterEach(async () => {
  await running?.close();
  running = undefined;
});

describe('routes', () => {
  it('covers every route the design names, and asks for a token on all but health and the page', () => {
    const paths = ROUTES.map((route) => `${route.method} ${route.path}`);
    for (const required of [
      'GET /v1/health',
      'GET /v1/status',
      'GET /v1/tokens',
      'GET /v1/pools',
      'GET /v1/quote',
      'GET /v1/premium',
      'POST /v1/orders',
      'GET /v1/orders',
      'DELETE /v1/orders/:id',
      'GET /v1/orders/:id',
      'GET /v1/hedge/plan',
      'POST /v1/hedge/apply',
      'GET /v1/hedge',
      'GET /v1/recorder',
      'GET /',
    ]) {
      expect(paths).toContain(required);
    }
    const open = ROUTES.filter((route) => !route.auth).map((route) => route.path);
    expect(open.sort()).toEqual(['/', '/app.css', '/app.js', '/v1/health']);
  });
});

describe('authorisation', () => {
  it('answers health without a token', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/health', { token: null });
    expect(status).toBe(200);
    expect(body).toMatchObject({ ok: true, name: 'covenant-desk', chainId: 4663 });
  });

  it('refuses a route with no token', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/status', { token: null });
    expect(status).toBe(401);
    expect(body.error).toBe('unauthorized');
    expect(body.reason).toContain('bearer token');
  });

  it('refuses the wrong token', async () => {
    running = await startDesk();
    const { status } = await running.call('/v1/status', { token: 'b'.repeat(64) });
    expect(status).toBe(401);
  });

  it('refuses a token of a different length', async () => {
    running = await startDesk();
    const { status } = await running.call('/v1/status', { token: 'short' });
    expect(status).toBe(401);
  });

  it('accepts the configured token', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/status');
    expect(status).toBe(200);
    expect(body).toMatchObject({ chainId: 4663, sessionState: 'closed', live: false });
    expect(body.blockNumber).toBe('54423221');
  });

  it('answers only requests addressed to the loopback interface', async () => {
    running = await startDesk();
    const { status, body } = await rawGet(running.url, '/v1/health', 'desk.example.com');
    expect(status).toBe(403);
    expect(body.reason).toContain('127.0.0.1');
  });
});

describe('shapes', () => {
  it('names an unknown path and an unaccepted method apart', async () => {
    running = await startDesk();
    const unknown = await running.call('/v1/nothing');
    expect(unknown.status).toBe(404);
    expect(unknown.body.error).toBe('not_found');

    const wrongMethod = await running.call('/v1/premium', { method: 'POST' });
    expect(wrongMethod.status).toBe(405);
    expect(wrongMethod.body.reason).toContain('POST is not accepted');
  });

  it('reports a part of the desk that is not built yet as 501', async () => {
    const desk = deskFixture();
    desk.fairvalue.premium.all = async () => {
      throw new NotImplementedError('fairvalue.premium.all');
    };
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/premium');
    expect(status).toBe(501);
    expect(body).toMatchObject({ error: 'not_implemented' });
  });

  it('returns premium rows with their units', async () => {
    const desk = deskFixture();
    const row: FairValue = {
      symbol: 'NVDA',
      token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
      onchainMid: { value: 182.5, unit: 'USD', source: 'pool', asOf: 1 },
      reference: { value: 180, unit: 'USD', source: 'chainlink', asOf: 1 },
      referenceSource: 'chainlink',
      candidates: [],
      premiumBps: { value: 138.9, unit: 'bps', source: 'derived', asOf: 1 },
      sessionState: 'open',
      asOf: 1,
    };
    desk.fairvalue.premium.all = async () => [row];
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/premium?limit=5');
    expect(status).toBe(200);
    expect(body.premium).toHaveLength(1);
    expect(body.premium[0]).toMatchObject({ symbol: 'NVDA', referenceSource: 'chainlink' });
    expect(body.premium[0].premiumBps.unit).toBe('bps');
  });

  it('refuses a query parameter that is not a number', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/premium?limit=soon');
    expect(status).toBe(400);
    expect(body.reason).toContain('limit must be a number');
  });

  it('asks for the token on a quote', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/quote');
    expect(status).toBe(400);
    expect(body.reason).toContain('/v1/quote?token=');
  });

  it('prices a stock symbol through fair value', async () => {
    const desk = deskFixture();
    desk.fairvalue.premium.forSymbol = async (symbol) =>
      ({
        symbol,
        token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
        candidates: [],
        sessionState: 'closed',
        asOf: 2,
      }) satisfies FairValue;
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/quote?token=NVDA&amountUsd=250&side=sell');
    expect(status).toBe(200);
    expect(body).toMatchObject({ kind: 'stock', side: 'sell', amountUsd: 250, symbol: 'NVDA' });
    expect(body.fairValue.symbol).toBe('NVDA');
  });

  it('prices the size that was asked for and says so', async () => {
    const desk = deskFixture();
    const token = '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC';
    desk.fairvalue.premium.forSymbol = async (symbol) =>
      ({ symbol, token, candidates: [], sessionState: 'closed', asOf: 2 }) satisfies FairValue;
    desk.fairvalue.premium.sized = async (fair, options) => ({
      price: { value: 231.4, unit: 'USD', source: 'derived', asOf: 3 },
      amountUsd: options.amountUsd,
      side: options.side ?? 'buy',
      pool: `0x${'11'.repeat(32)}`,
      note: `What buying ${options.amountUsd} USD of ${fair.symbol} costs per token in this pool, price impact included.`,
    });
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/quote?token=NVDA&amountUsd=250&side=buy');
    expect(status).toBe(200);
    expect(body.sizedPrice.value).toBe(231.4);
    expect(body.note).toContain('250 USD');
  });

  it('says when no pool can price the size', async () => {
    const desk = deskFixture();
    desk.fairvalue.premium.forSymbol = async (symbol) =>
      ({
        symbol,
        token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
        candidates: [],
        sessionState: 'closed',
        asOf: 2,
      }) satisfies FairValue;
    desk.fairvalue.premium.sized = async () => undefined;
    running = await startDesk(desk);

    const { body } = await running.call('/v1/quote?token=NVDA&amountUsd=100');
    expect(body.sizedPrice).toBeUndefined();
    expect(body.note).toContain('mid prices');
  });

  it('answers the decimals of one token, reading the chain when its own records are empty', async () => {
    const desk = deskFixture();
    const address = '0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168';
    desk.chain.tokens.decimalsOf = async () => new Map([[address.toLowerCase(), 6]]);
    running = await startDesk(desk);

    const { status, body } = await running.call(`/v1/tokens/${address}`);
    expect(status).toBe(200);
    expect(body.token.decimals).toBe(6);
    expect(body.source).toBe('chain');

    const missing = await running.call('/v1/tokens/NOPE');
    expect(missing.status).toBe(404);
  });

  it('refuses a side that is not buy or sell', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/quote?token=NVDA&side=hold');
    expect(status).toBe(400);
    expect(body.reason).toContain('side must be buy or sell');
  });

  it('names the missing stock on a pool query', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/pools');
    expect(status).toBe(400);
    expect(body.reason).toContain('/v1/pools?stock=NVDA');
  });

  it('reports an unknown stock as not found', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/pools?stock=NOPE');
    expect(status).toBe(404);
    expect(body).toMatchObject({ error: 'not_found' });
  });
});

describe('orders', () => {
  const order: Order = {
    id: 'ord_1',
    kind: 'limit',
    side: 'buy',
    tokenIn: '0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168',
    tokenOut: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
    amountIn: 100_000_000_000_000_000_000n,
    trigger: { priceLte: 175 },
    bounds: { maxSlippageBps: 100, maxOrderNotionalUsd: 250, maxBuyPremiumBps: 500 },
    live: false,
    status: 'open',
    createdAt: 1,
    expiresAt: null,
    parentId: null,
  };

  it('creates an order from whole tokens and returns amounts as exact strings', async () => {
    const desk = deskFixture();
    let seen: bigint | undefined;
    desk.orders.book.create = async (input) => {
      seen = input.amountIn;
      return [{ ...order, amountIn: input.amountIn }];
    };
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/orders', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        kind: 'limit',
        side: 'buy',
        tokenIn: order.tokenIn,
        tokenOut: order.tokenOut,
        amountInTokens: '25.5',
        decimals: 18,
        trigger: { priceLte: 175 },
      }),
    });

    expect(status).toBe(200);
    expect(seen).toBe(25_500_000_000_000_000_000n);
    expect(body.orders[0].amountIn).toBe('25500000000000000000');
  });

  it('names the field that made the body invalid', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/orders', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ kind: 'limit', side: 'buy', tokenIn: '0x01', tokenOut: '0x02', amountIn: '1' }),
    });
    expect(status).toBe(400);
    expect(body.reason).toContain('tokenIn');
  });

  it('asks for a size when neither form of it is given', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/orders', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ kind: 'limit', side: 'buy', tokenIn: order.tokenIn, tokenOut: order.tokenOut }),
    });
    expect(status).toBe(400);
    expect(body.reason).toContain('amountIn');
  });

  it('lists orders and cancels one by id', async () => {
    const desk = deskFixture();
    desk.orders.book.list = () => [order];
    desk.orders.book.cancel = async (id) => ({ ...order, id, status: 'cancelled', reason: 'asked for' });
    running = await startDesk(desk);

    const list = await running.call('/v1/orders?status=open');
    expect(list.status).toBe(200);
    expect(list.body.orders[0].id).toBe('ord_1');

    const cancelled = await running.call('/v1/orders/ord_1', { method: 'DELETE' });
    expect(cancelled.status).toBe(200);
    expect(cancelled.body.order.status).toBe('cancelled');
  });

  it('returns one order with the fills recorded against it', async () => {
    const desk = deskFixture();
    desk.orders.book.get = (id) => ({ ...order, id, status: 'filled' });
    desk.store.insertExecution({
      id: 'exe_1',
      orderId: 'ord_1',
      live: false,
      amountIn: 50_000_000n,
      amountOut: 215_833_281_345_852_717n,
      quotedAmountOut: 215_833_281_345_852_717n,
      effectivePrice: { value: 0.0043, unit: 'token', source: 'pool', asOf: 1 },
      notionalUsd: { value: 49.83, unit: 'USD', source: 'derived', asOf: 1 },
      slippageBps: 0,
      status: 'simulated',
      reason: 'Dry run. No transaction was signed.',
      createdAt: 2,
    });
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/orders/ord_1');
    expect(status).toBe(200);
    expect(body.order.status).toBe('filled');
    expect(body.executions).toHaveLength(1);
    expect(body.executions[0].amountOut).toBe('215833281345852717');
    expect(body.executions[0].status).toBe('simulated');
  });

  it('reports an order that is not on the book as not found', async () => {
    const desk = deskFixture();
    desk.orders.book.get = () => undefined;
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/orders/missing');
    expect(status).toBe(404);
    expect(body.reason).toContain('missing');
  });

  it('reports an order that does not exist as not found', async () => {
    const desk = deskFixture();
    desk.orders.book.cancel = async (id) => {
      throw new NotFoundError(`Order ${id}`);
    };
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/orders/missing', { method: 'DELETE' });
    expect(status).toBe(404);
    expect(body.reason).toContain('Order missing');
  });

  it('refuses a status filter that is not part of the lifecycle', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/orders?status=pending');
    expect(status).toBe(400);
    expect(body.reason).toContain('status must be one of');
  });
});

describe('hedge and recorder', () => {
  it('falls back to the recorded positions when the venue cannot be read', async () => {
    const desk = deskFixture();
    desk.hedge.client.positions = async () => {
      throw new UpstreamError('lighter-rh', 'the account has no credentials');
    };
    running = await startDesk(desk);
    const { status, body } = await running.call('/v1/hedge');
    expect(status).toBe(200);
    expect(body.source).toBe('store');
    expect(body.positions).toEqual([]);
    expect(body.reason).toContain('last recorded positions');
  });

  it('says why an empty hedge list is empty when no sub-account is configured', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/hedge');
    expect(status).toBe(200);
    expect(body.positions).toEqual([]);
    expect(body.reason).toContain('DESK_LIGHTER_RH_ACCOUNT_INDEX');
  });

  it('asks for the stock a hedge cancels', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/hedge/plan');
    expect(status).toBe(400);
    expect(body.reason).toContain('symbol=NVDA');
  });

  it('asks for the exposure when only the symbol is given', async () => {
    running = await startDesk();
    const { status, body } = await running.call('/v1/hedge/plan?symbol=NVDA');
    expect(status).toBe(400);
    expect(body.reason).toContain('stockLegUsd');
  });

  it('returns recorded observations for a symbol', async () => {
    const desk = deskFixture();
    desk.store.insertObservation({
      ts: Date.now() - 1_000,
      symbol: 'NVDA',
      token: '0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC',
      onchainMidUsd: 182.5,
      referenceUsd: 180,
      referenceSource: 'chainlink',
      premiumBps: 138.9,
      sessionState: 'open',
      blockNumber: 54_423_221n,
    });
    running = await startDesk(desk);

    const { status, body } = await running.call('/v1/recorder?symbol=NVDA');
    expect(status).toBe(200);
    expect(body.observations).toHaveLength(1);
    expect(body.observations[0]).toMatchObject({ symbol: 'NVDA', premiumBps: 138.9 });
    expect(body.observations[0].blockNumber).toBe('54423221');
    expect(body.running).toBe(false);
  });
});

describe('the page', () => {
  it('serves the page and its assets without a token', async () => {
    running = await startDesk();
    const page = await fetch(`${running.url}/`);
    expect(page.status).toBe(200);
    expect(page.headers.get('content-type')).toContain('text/html');
    expect(await page.text()).toContain('Covenant Desk');

    const script = await fetch(`${running.url}/app.js`);
    expect(script.headers.get('content-type')).toContain('javascript');
    expect(await script.text()).toContain('/v1/premium');

    const styles = await fetch(`${running.url}/app.css`);
    expect(await styles.text()).toContain('cursor: pointer');
  });
});

function rawGet(url: string, path: string, host: string): Promise<{ status: number; body: any }> {
  const port = Number(new URL(url).port);
  return new Promise((resolve, reject) => {
    const req = httpRequest({ host: '127.0.0.1', port, path, method: 'GET', headers: { host } }, (res) => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => {
        text += chunk;
      });
      res.on('end', () => resolve({ status: res.statusCode ?? 0, body: text === '' ? undefined : JSON.parse(text) }));
    });
    req.on('error', reject);
    req.end();
  });
}
