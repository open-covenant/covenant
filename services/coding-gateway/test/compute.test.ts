import { afterEach, describe, expect, it, vi } from 'vitest';
import { ComputeSession, computeConfigFromEnv } from '../src/compute.js';

const CFG = {
  apiUrl: 'https://compute.test',
  apiToken: 'beta-token',
  maxUsdcMicros: 200_000,
  maxDurationSecs: 1_800,
};

const offer = (id: string, rate: number) => ({
  id,
  gpu: { model: 'RTX 4090', vram_mib: 49_140 },
  rate_usdc_micros_per_hour: rate,
  online: true,
});

const job = (id: string, status = 'provisioning') => ({
  id,
  status,
  offer_id: 'vast:1:1',
  maximum_usdc_micros: 188_593,
  access_url: null,
  error: null,
  receipt: null,
});

function mockFetch(handler: (url: string, init?: RequestInit) => unknown) {
  const fn = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    const body = handler(String(url), init);
    if (body instanceof Response) return body;
    return new Response(JSON.stringify(body), { status: 200 });
  });
  vi.stubGlobal('fetch', fn);
  return fn;
}

afterEach(() => vi.unstubAllGlobals());

describe('computeConfigFromEnv', () => {
  it('is disabled without a token', () => {
    expect(computeConfigFromEnv({} as NodeJS.ProcessEnv)).toBeNull();
  });

  it('reads bounds from the environment', () => {
    const cfg = computeConfigFromEnv({
      COMPUTE_API_TOKEN: 't',
      COMPUTE_MAX_USDC_MICROS: '50000',
      COMPUTE_MAX_DURATION_SECS: '600',
    } as unknown as NodeJS.ProcessEnv)!;
    expect(cfg.maxUsdcMicros).toBe(50_000);
    expect(cfg.maxDurationSecs).toBe(600);
  });
});

describe('ComputeSession.launch', () => {
  it('launches the cheapest affordable offer with the exact quote', async () => {
    let plan: Record<string, unknown> | undefined;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('b', 720_000), offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') {
        plan = JSON.parse(String(init.body));
        return job('j1');
      }
      throw new Error(`unexpected ${url}`);
    });

    const session = new ComputeSession(CFG);
    const launched = await session.launch();
    expect(launched.id).toBe('j1');
    expect((plan!.offer as { id: string }).id).toBe('a');
    // ceil(370519 * 1800 / 3600)
    expect(plan!.maximum_usdc_micros).toBe(185_260);
  });

  it('rejects when no offer fits the budget', async () => {
    mockFetch((url) => {
      if (url.endsWith('/v1/offers')) return [offer('pricey', 9_000_000)];
      throw new Error('should not launch');
    });
    await expect(new ComputeSession(CFG).launch()).rejects.toThrow(/budget cap/);
  });

  it('retries past a stale offer', async () => {
    let posts = 0;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519), offer('b', 380_000)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') {
        posts += 1;
        if (posts === 1) {
          return new Response(JSON.stringify({ error: { code: 'stale_offer' } }), { status: 409 });
        }
        return job('j2');
      }
      throw new Error(`unexpected ${url}`);
    });

    const launched = await new ComputeSession(CFG).launch();
    expect(launched.id).toBe('j2');
    expect(posts).toBe(2);
  });

  it('clamps the requested duration to the configured maximum', async () => {
    let plan: Record<string, unknown> | undefined;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (init?.method === 'POST') {
        plan = JSON.parse(String(init.body));
        return job('j3');
      }
      throw new Error(`unexpected ${url}`);
    });
    await new ComputeSession(CFG).launch(7_200);
    expect(plan!.duration_secs).toBe(1_800);
  });
});

describe('ComputeSession.reap', () => {
  it('cancels launched jobs that were not cancelled during the run', async () => {
    const cancelled: string[] = [];
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('leak');
      if (init?.method === 'DELETE') {
        cancelled.push(url.split('/').pop()!);
        return job('leak', 'cancelled');
      }
      throw new Error(`unexpected ${url}`);
    });

    const session = new ComputeSession(CFG);
    await session.launch();
    const reaped = await session.reap();
    expect(cancelled).toEqual(['leak']);
    expect(reaped).toEqual(['leak']);
    // Idempotent: nothing left to reap.
    expect(await session.reap()).toEqual([]);
  });

  it('does not reap jobs the model already cancelled', async () => {
    const deletes: string[] = [];
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('done');
      if (init?.method === 'DELETE') {
        deletes.push(url.split('/').pop()!);
        return job('done', 'cancelled');
      }
      throw new Error(`unexpected ${url}`);
    });

    const session = new ComputeSession(CFG);
    await session.launch();
    await session.cancel('done');
    await session.reap();
    expect(deletes).toEqual(['done']);
  });
});
