import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Server } from 'node:http';

const token = 'c'.repeat(32);
const nativeFetch = globalThis.fetch;
const ENV_KEYS = [
  'NODE_ENV',
  'CODER_AUTH_TOKEN',
  'E2B_API_KEY',
  'COMPUTE_API_TOKEN',
  'COMPUTE_API_URL',
  'COMPUTE_MAX_USDC_MICROS',
  'COMPUTE_MAX_DURATION_SECS',
  'COMPUTE_MAX_LAUNCHES',
] as const;
const saved: Partial<Record<(typeof ENV_KEYS)[number], string | undefined>> = {};

beforeEach(() => {
  for (const key of ENV_KEYS) {
    saved[key] = process.env[key];
    delete process.env[key];
  }
  process.env.NODE_ENV = 'test';
  process.env.CODER_AUTH_TOKEN = token;
  vi.resetModules();
});

afterEach(() => {
  for (const key of ENV_KEYS) {
    if (saved[key] === undefined) delete process.env[key];
    else process.env[key] = saved[key];
  }
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/** Boot the gateway on an ephemeral port without starting its background work. */
async function withGateway<T>(check: (origin: string) => Promise<T>): Promise<T> {
  const { server } = (await import('../src/server.js')) as { server: Server };
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('gateway did not bind');
  try {
    return await check(`http://127.0.0.1:${address.port}`);
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((cause) => (cause ? reject(cause) : resolve()));
    });
  }
}

async function get(path: string): Promise<unknown> {
  return withGateway(async (origin) => {
    const response = await nativeFetch(`${origin}${path}`, {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status).toBe(200);
    return response.json();
  });
}

describe('GET /v1/capabilities', () => {
  it('answers an anonymous legacy probe with empty liveness and no provider work', async () => {
    const { GatewayReadiness } = await import('../src/readiness.js');
    const { LocalSandboxProvider } = await import('../src/sandbox/local.js');
    const { E2bSandboxProvider } = await import('../src/sandbox/e2b.js');
    const { ComputeSession } = await import('../src/compute.js');
    const backends = await import('../src/backends/index.js');
    const unexpectedWork = new Error('liveness must not perform provider work');
    const readiness = vi
      .spyOn(GatewayReadiness.prototype, 'check')
      .mockRejectedValue(unexpectedWork);
    const localSandbox = vi
      .spyOn(LocalSandboxProvider.prototype, 'create')
      .mockRejectedValue(unexpectedWork);
    const e2bSandbox = vi
      .spyOn(E2bSandboxProvider.prototype, 'create')
      .mockRejectedValue(unexpectedWork);
    const launch = vi.spyOn(ComputeSession.prototype, 'launch').mockRejectedValue(unexpectedWork);
    const backend = vi.spyOn(backends, 'selectBackend').mockImplementation(() => {
      throw unexpectedWork;
    });
    const providerFetch = vi.fn<typeof fetch>().mockRejectedValue(unexpectedWork);
    vi.stubGlobal('fetch', providerFetch);

    await withGateway(async (origin) => {
      const response = await nativeFetch(`${origin}/v1/capabilities`);
      expect(response.status).toBe(204);
      expect(await response.text()).toBe('');
      expect(response.headers.get('cache-control')).toBe('private, no-store');
      expect(response.headers.get('vary')).toBe('Authorization');
      expect(response.headers.get('content-type')).toBeNull();
    });

    for (const operation of [readiness, localSandbox, e2bSandbox, launch, backend, providerFetch]) {
      expect(operation).not.toHaveBeenCalled();
    }
  });

  it.each(['', 'Bearer', 'Bearer wrong-token', 'Basic wrong-token'])(
    'rejects a present but invalid Authorization header %j',
    async (authorization) => {
      await withGateway(async (origin) => {
        const response = await nativeFetch(`${origin}/v1/capabilities`, {
          headers: { authorization },
        });
        expect(response.status).toBe(401);
        await expect(response.json()).resolves.toEqual({ error: 'unauthorized' });
      });
    },
  );

  it('preserves anonymous capability JSON for local development without an auth token', async () => {
    delete process.env.CODER_AUTH_TOKEN;
    await withGateway(async (origin) => {
      const response = await nativeFetch(`${origin}/v1/capabilities`);
      expect(response.status).toBe(200);
      await expect(response.json()).resolves.toMatchObject({
        features: { run_submission: true, gpu_workspace: false },
        compute: { enabled: false },
        sandbox: { provider: 'local' },
      });
    });
  });

  it('does not grant the liveness exception to other methods or neighboring paths', async () => {
    await withGateway(async (origin) => {
      for (const method of ['HEAD', 'POST', 'PUT', 'DELETE', 'OPTIONS']) {
        const response = await nativeFetch(`${origin}/v1/capabilities`, { method });
        expect(response.status, method).toBe(401);
        await response.text();
      }
      for (const path of [
        '/v1/capabilities/',
        '/v1/capabilities/other',
        '/v1/capabilities-other',
      ]) {
        const response = await nativeFetch(`${origin}${path}`);
        expect(response.status, path).toBe(401);
        await expect(response.json()).resolves.toEqual({ error: 'unauthorized' });
      }
    });
  });

  it.each([undefined, 'Bearer wrong-token'])(
    'keeps readiness, budget, and every run route protected with Authorization %j',
    async (authorization) => {
      await withGateway(async (origin) => {
        const routes = [
          ['GET', '/readyz'],
          ['GET', '/v1/budget'],
          ['POST', '/v1/runs'],
          ['GET', '/v1/run-sessions/unknown'],
          ['GET', '/v1/runs/unknown'],
          ['GET', '/v1/runs/unknown/artifacts'],
          ['GET', '/v1/runs/unknown/files'],
          ['GET', '/v1/runs/unknown/events'],
          ['POST', '/v1/runs/unknown/stop'],
        ];
        for (const [method, path] of routes) {
          const response = await nativeFetch(`${origin}${path}`, {
            method,
            headers: authorization === undefined ? {} : { authorization },
          });
          expect(response.status, `${method} ${path}`).toBe(401);
          await expect(response.json()).resolves.toEqual({ error: 'unauthorized' });
        }
      });
    },
  );

  it('reports the GPU feature off when no compute token is configured', async () => {
    await expect(get('/v1/capabilities')).resolves.toMatchObject({
      features: { run_submission: true, gpu_workspace: false },
      compute: { enabled: false },
      sandbox: { provider: 'local' },
    });
  });

  // Otherwise an operator can only confirm the feature is on by paying for a
  // run that reaches for a GPU.
  it('does not enable GPU merely because a token and broad operator limits exist', async () => {
    process.env.COMPUTE_API_TOKEN = 'operator-token';
    process.env.COMPUTE_API_URL = 'https://compute.test';
    process.env.COMPUTE_MAX_USDC_MICROS = '250000';
    process.env.COMPUTE_MAX_DURATION_SECS = '900';
    process.env.COMPUTE_MAX_LAUNCHES = '2';

    await expect(get('/v1/capabilities')).resolves.toMatchObject({
      features: { gpu_workspace: false },
      compute: { enabled: false },
    });
  });
});

describe('GET /v1/budget', () => {
  it('rounds USD to six decimals so no float artifact reaches an operator', async () => {
    // 600s x $0.0002/s is 0.12000000000000001 in binary floating point.
    const budget = (await get('/v1/budget')) as {
      dailyUsd: number;
      accounting: { sandbox: { maximumPerRunUsd: number } };
    };
    expect(budget.accounting.sandbox.maximumPerRunUsd).toBe(0.12);
    expect(budget.dailyUsd).toBe(0);
  });
});
