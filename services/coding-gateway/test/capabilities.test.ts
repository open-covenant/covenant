import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Server } from 'node:http';

const token = 'c'.repeat(32);
const ENV_KEYS = [
  'CODER_AUTH_TOKEN',
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
});

/** Boot the gateway on an ephemeral port and read one endpoint from it. */
async function get(path: string): Promise<unknown> {
  const { server } = (await import('../src/server.js')) as { server: Server };
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('gateway did not bind');
  try {
    const response = await fetch(`http://127.0.0.1:${address.port}${path}`, {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status).toBe(200);
    return await response.json();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((cause) => (cause ? reject(cause) : resolve()));
    });
  }
}

describe('GET /v1/capabilities', () => {
  it('reports the GPU feature off when no compute token is configured', async () => {
    await expect(get('/v1/capabilities')).resolves.toMatchObject({
      features: { run_submission: true, gpu_workspace: false },
      compute: { enabled: false },
      sandbox: { provider: 'local' },
    });
  });

  // Otherwise an operator can only confirm the feature is on by paying for a
  // run that reaches for a GPU.
  it('reports the GPU feature and its effective bounds when it is configured', async () => {
    process.env.COMPUTE_API_TOKEN = 'operator-token';
    process.env.COMPUTE_API_URL = 'https://compute.test';
    process.env.COMPUTE_MAX_USDC_MICROS = '250000';
    process.env.COMPUTE_MAX_DURATION_SECS = '900';
    process.env.COMPUTE_MAX_LAUNCHES = '2';

    await expect(get('/v1/capabilities')).resolves.toMatchObject({
      features: { gpu_workspace: true },
      compute: {
        enabled: true,
        budgetUsd: 0.25,
        maxLaunches: 2,
        maxDurationSecs: 900,
        defaultDurationSecs: 900,
        controlPlane: 'compute.test',
      },
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
