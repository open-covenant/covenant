import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ComputeError,
  ComputeSession,
  computeConfigFromEnv,
  computeSummary,
  defaultDurationSecs,
  probeComputeControlPlane,
} from '../src/compute.js';
import { ConfigError } from '../src/config-error.js';

const CFG = {
  apiUrl: 'https://compute.test',
  apiToken: 'beta-token',
  maxUsdcMicros: 200_000,
  maxDurationSecs: 1_800,
  maxLaunches: 4,
};

// Cheapest online offer observed on the live market, in micro-USDC per GPU-hour.
const CHEAPEST_RATE_PER_HOUR = 377_186;

const offer = (id: string, rate: number) => ({
  id,
  gpu: { model: 'RTX 4090', vram_mib: 49_140 },
  rate_usdc_micros_per_hour: rate,
  online: true,
});

const job = (id: string, status = 'provisioning', extra: Record<string, unknown> = {}) => ({
  id,
  status,
  offer_id: 'vast:1:1',
  maximum_usdc_micros: 188_593,
  access_url: null,
  error: null,
  receipt: null,
  ...extra,
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

/** One always-online offer, jobs named j1, j2, ... in launch order. */
function mockMarket(rate = 370_519) {
  let launched = 0;
  return mockFetch((url, init) => {
    if (url.endsWith('/v1/offers')) return [offer('a', rate)];
    if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job(`j${++launched}`);
    if (init?.method === 'DELETE') return job(url.split('/').pop()!, 'cancelled');
    throw new Error(`unexpected ${url}`);
  });
}

const silenceReapLog = () => vi.spyOn(console, 'error').mockImplementation(() => {});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('computeConfigFromEnv', () => {
  it('is disabled without a token', () => {
    expect(computeConfigFromEnv({} as NodeJS.ProcessEnv)).toBeNull();
  });

  it('reads bounds from the environment', () => {
    const cfg = computeConfigFromEnv({
      COMPUTE_API_TOKEN: 't',
      COMPUTE_MAX_USDC_MICROS: '50000',
      COMPUTE_MAX_DURATION_SECS: '600',
      COMPUTE_MAX_LAUNCHES: '2',
    } as unknown as NodeJS.ProcessEnv)!;
    expect(cfg.maxUsdcMicros).toBe(50_000);
    expect(cfg.maxDurationSecs).toBe(600);
    expect(cfg.maxLaunches).toBe(2);
  });

  it('applies the documented defaults', () => {
    const cfg = computeConfigFromEnv({ COMPUTE_API_TOKEN: 't' } as NodeJS.ProcessEnv)!;
    expect(cfg).toMatchObject({
      apiUrl: 'https://compute.opencovenant.org',
      maxUsdcMicros: 200_000,
      maxDurationSecs: 1_800,
      maxLaunches: 1,
    });
  });

  // The three bounds are one budget. A default set whose launch count cannot be
  // reached at market rates reads as a broken feature, not a spend ceiling.
  it('funds every default launch at the market rate the defaults were sized for', () => {
    const cfg = computeConfigFromEnv({ COMPUTE_API_TOKEN: 't' } as NodeJS.ProcessEnv)!;
    const perLaunch = Math.ceil((CHEAPEST_RATE_PER_HOUR * defaultDurationSecs(cfg)) / 3_600);
    expect(perLaunch * cfg.maxLaunches).toBeLessThanOrEqual(cfg.maxUsdcMicros);
  });

  it('states the accepted range instead of an opt-out that also fails to boot', () => {
    for (const value of ['-5000', '0']) {
      expect(() =>
        computeConfigFromEnv({
          COMPUTE_API_TOKEN: 't',
          COMPUTE_MAX_USDC_MICROS: value,
        } as NodeJS.ProcessEnv),
      ).toThrow(
        'COMPUTE_MAX_USDC_MICROS=' +
          JSON.stringify(value) +
          ' is not valid: set it to a whole number between 1 and 10000000',
      );
    }
  });

  it('rejects a configuration error as a ConfigError so the entry point can print it alone', () => {
    expect(() =>
      computeConfigFromEnv({
        COMPUTE_API_TOKEN: 't',
        COMPUTE_MAX_LAUNCHES: '99',
      } as NodeJS.ProcessEnv),
    ).toThrow(ConfigError);
  });

  // A silent Number() cast turned every one of these into a cap that either
  // fails open (9e9 = $9,000) or fails every launch with "cap of NaN" while
  // the tool stays advertised.
  it.each([
    ['COMPUTE_MAX_USDC_MICROS', 'oops'],
    ['COMPUTE_MAX_USDC_MICROS', '10000001'],
    ['COMPUTE_MAX_USDC_MICROS', '9e9'],
    ['COMPUTE_MAX_USDC_MICROS', '0'],
    ['COMPUTE_MAX_DURATION_SECS', '-1'],
    ['COMPUTE_MAX_DURATION_SECS', '30'],
    ['COMPUTE_MAX_DURATION_SECS', '86400'],
    ['COMPUTE_MAX_LAUNCHES', '0'],
    ['COMPUTE_MAX_LAUNCHES', '2.5'],
  ])('refuses %s=%s at boot', (name, value) => {
    expect(() =>
      computeConfigFromEnv({ COMPUTE_API_TOKEN: 't', [name]: value } as NodeJS.ProcessEnv),
    ).toThrow(new RegExp(name));
  });
});

describe('COMPUTE_API_URL', () => {
  const parse = (COMPUTE_API_URL: string) =>
    computeConfigFromEnv({ COMPUTE_API_TOKEN: 't', COMPUTE_API_URL } as NodeJS.ProcessEnv)!;

  it('normalizes a valid https origin', () => {
    expect(parse('https://compute.example/').apiUrl).toBe('https://compute.example');
    expect(parse('  https://compute.example  ').apiUrl).toBe('https://compute.example');
  });

  // Left to request time, a bad origin fails only after the run has already
  // paid for model tokens.
  it.each(['not-a-url', 'compute.opencovenant.org', ''])('refuses %s at boot', (value) => {
    expect(() => parse(value)).toThrow(/COMPUTE_API_URL/);
  });

  it('refuses a cleartext control plane that would put the bearer token on the wire', () => {
    expect(() => parse('http://compute.opencovenant.org')).toThrow(/must use https/);
  });

  it('allows http for loopback so local development still works', () => {
    expect(parse('http://127.0.0.1:8080').apiUrl).toBe('http://127.0.0.1:8080');
    expect(parse('http://localhost:8080').apiUrl).toBe('http://localhost:8080');
    expect(parse('http://[::1]:8080').apiUrl).toBe('http://[::1]:8080');
  });

  it('refuses embedded credentials, a query, or a fragment', () => {
    for (const value of [
      'https://user:pass@compute.example',
      'https://compute.example?token=x',
      'https://compute.example#x',
    ]) {
      expect(() => parse(value)).toThrow(/plain origin/);
    }
  });
});

describe('probeComputeControlPlane', () => {
  const cfg = { ...CFG, apiUrl: 'https://compute.test' };

  it('reads the non-billable offer list with the configured token', async () => {
    const fetched = mockFetch((url, init) => {
      expect(url).toBe('https://compute.test/v1/offers');
      expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer beta-token');
      return [offer('a', 377_186)];
    });
    await expect(probeComputeControlPlane(cfg)).resolves.toBeUndefined();
    expect(fetched).toHaveBeenCalledTimes(1);
  });

  it('rejects a token the control plane will not accept', async () => {
    mockFetch(() => new Response('{}', { status: 401 }));
    await expect(probeComputeControlPlane(cfg)).rejects.toThrow(/HTTP 401/);
  });

  it('rejects a response that is not an offer list', async () => {
    mockFetch(() => ({ offers: [] }));
    await expect(probeComputeControlPlane(cfg)).rejects.toThrow(/malformed offer list/);
  });
});

describe('computeSummary', () => {
  it('is disabled with no configuration', () => {
    expect(computeSummary(null)).toEqual({ enabled: false });
  });

  it('publishes the effective bounds and the control plane host', () => {
    expect(computeSummary({ ...CFG, maxUsdcMicros: 250_000, maxDurationSecs: 900 })).toEqual({
      enabled: true,
      budgetUsd: 0.25,
      maxLaunches: 4,
      maxDurationSecs: 900,
      defaultDurationSecs: 900,
      controlPlane: 'compute.test',
    });
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
    expect(session.committedUsd()).toBeCloseTo(0.18526, 6);
  });

  it('rejects when no offer fits the budget', async () => {
    mockFetch((url) => {
      if (url.endsWith('/v1/offers')) return [offer('pricey', 9_000_000)];
      throw new Error('should not launch');
    });
    await expect(new ComputeSession(CFG).launch()).rejects.toThrow(/GPU budget/);
  });

  it('retries past a stale offer under one idempotency key', async () => {
    const keys: string[] = [];
    let posts = 0;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519), offer('b', 380_000)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') {
        keys.push((init.headers as Record<string, string>)['Idempotency-Key']!);
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
    // One logical launch, one key: a POST that succeeds server-side but times
    // out here must dedupe instead of billing a workspace reap cannot see.
    expect(new Set(keys).size).toBe(1);
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

  it('defaults to the app booking window rather than the configured maximum', async () => {
    let plan: Record<string, unknown> | undefined;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 100_000)];
      if (init?.method === 'POST') {
        plan = JSON.parse(String(init.body));
        return job('j1');
      }
      throw new Error(`unexpected ${url}`);
    });
    await new ComputeSession({ ...CFG, maxDurationSecs: 3_600 }).launch();
    expect(plan!.duration_secs).toBe(1_800);
  });

  it.each([0, -5, Number.NaN, Number.POSITIVE_INFINITY])(
    'refuses duration_secs=%s instead of booking the maximum',
    async (duration) => {
      const fetchMock = mockMarket();
      await expect(new ComputeSession(CFG).launch(duration)).rejects.toThrow(/positive number/);
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  it('refuses a duration below the minimum booking window', async () => {
    const fetchMock = mockMarket();
    await expect(new ComputeSession(CFG).launch(30)).rejects.toThrow(/at least 60 seconds/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('floors a fractional duration', async () => {
    let plan: Record<string, unknown> | undefined;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (init?.method === 'POST') {
        plan = JSON.parse(String(init.body));
        return job('j1');
      }
      throw new Error(`unexpected ${url}`);
    });
    await new ComputeSession(CFG).launch(600.7);
    expect(plan!.duration_secs).toBe(600);
  });

  it('spends the cap across the whole run, not per job', async () => {
    const fetchMock = mockMarket();
    const session = new ComputeSession(CFG);
    await session.launch();
    // 185_260 of 200_000 committed; nothing left for a second booking.
    await expect(session.launch()).rejects.toThrow(/14740 micro-USDC left/);
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
  });

  it('does not return budget when a job is cancelled', async () => {
    mockMarket();
    const session = new ComputeSession(CFG);
    await session.launch();
    await session.cancel('j1');
    await expect(session.launch()).rejects.toThrow(/GPU budget/);
    expect(session.committedUsd()).toBeCloseTo(0.18526, 6);
  });

  it('stops at the launch cap even when the budget would allow more', async () => {
    mockMarket(3_600);
    const session = new ComputeSession({ ...CFG, maxUsdcMicros: 10_000_000, maxLaunches: 2 });
    await session.launch();
    await session.launch();
    await expect(session.launch()).rejects.toThrow(/launch cap reached/);
  });

  it('agrees with itself on the singular, since one launch is the default cap', async () => {
    mockMarket(3_600);
    const session = new ComputeSession({ ...CFG, maxUsdcMicros: 10_000_000, maxLaunches: 1 });
    await session.launch();
    await expect(session.launch()).rejects.toThrow('this run may launch 1 GPU workspace');
  });

  it('reports the control plane message, not just the status code', async () => {
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (init?.method === 'POST') {
        return new Response(
          JSON.stringify({ error: { code: 'insufficient_funds', message: 'balance too low' } }),
          { status: 402 },
        );
      }
      throw new Error(`unexpected ${url}`);
    });
    await expect(new ComputeSession(CFG).launch()).rejects.toThrow(
      /insufficient_funds: balance too low/,
    );
  });

  it('rejects a malformed offer list instead of throwing a raw TypeError', async () => {
    mockFetch(() => ({ offers: [] }));
    const call = new ComputeSession(CFG).launch();
    await expect(call).rejects.toBeInstanceOf(ComputeError);
    await expect(call).rejects.toThrow(/malformed offer list/);
  });

  it('sends Content-Type only on requests that carry a body', async () => {
    const fetchMock = mockMarket();
    const session = new ComputeSession(CFG);
    await session.launch();
    await session.cancel('j1');
    const headersFor = (method?: string) =>
      fetchMock.mock.calls
        .filter(([, init]) => init?.method === method)
        .map(([, init]) => init!.headers as Record<string, string>);
    expect(headersFor('POST')[0]).toHaveProperty('Content-Type', 'application/json');
    expect(headersFor('DELETE')[0]).not.toHaveProperty('Content-Type');
    expect(headersFor(undefined)[0]).not.toHaveProperty('Content-Type');
  });
});

describe('ComputeSession job routes', () => {
  it.each(['../../v1/admin/tokens', '../offers?all=true', 'j1/../../v1/offers', ''])(
    'refuses to build a job route from %s',
    async (jobId) => {
      const fetchMock = mockMarket();
      const session = new ComputeSession(CFG);
      await expect(session.status(jobId)).rejects.toThrow(/not a job id/);
      await expect(session.cancel(jobId)).rejects.toThrow(/not a job id/);
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  it('refuses a job this run did not launch', async () => {
    mockMarket();
    const session = new ComputeSession(CFG);
    await session.launch();
    await expect(session.status('j9')).rejects.toThrow(/was not launched by this run/);
    await expect(session.cancel('j9')).rejects.toThrow(/was not launched by this run/);
  });

  it('keeps a cancelled job pollable for its receipt', async () => {
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('j1');
      if (init?.method === 'DELETE') return job('j1', 'cancelled');
      return job('j1', 'cancelled', {
        receipt: { runtime_secs: 12, charged_usdc_micros: 1_235, refunded_usdc_micros: 184_025 },
      });
    });
    const session = new ComputeSession(CFG);
    await session.launch();
    await session.cancel('j1');
    const after = await session.status('j1');
    expect(after.receipt).toMatchObject({ charged_usdc_micros: 1_235 });
  });

  it('returns a failed job with its cause instead of throwing http_200', async () => {
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('j1');
      return job('j1', 'failed', { error: 'gpu fell offline' });
    });
    const session = new ComputeSession(CFG);
    await session.launch();
    expect(await session.status('j1')).toMatchObject({
      status: 'failed',
      error: 'gpu fell offline',
    });
  });

  it('cancels a job that carries error text', async () => {
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('j1');
      return job('j1', 'cancelled', { error: 'gpu fell offline' });
    });
    const session = new ComputeSession(CFG);
    await session.launch();
    expect(await session.cancel('j1')).toMatchObject({ status: 'cancelled' });
  });

  it('rejects an empty cancel response instead of returning null to the model', async () => {
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('j1');
      return new Response(null, { status: 204 });
    });
    const session = new ComputeSession(CFG);
    await session.launch();
    const call = session.cancel('j1');
    await expect(call).rejects.toBeInstanceOf(ComputeError);
    await expect(call).rejects.toThrow(/no job/);
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

  it('keeps retrying a job that only reports cancelling', async () => {
    const log = silenceReapLog();
    let deletes = 0;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 370_519)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job('slow');
      if (init?.method === 'DELETE')
        return job('slow', ++deletes === 1 ? 'cancelling' : 'cancelled');
      throw new Error(`unexpected ${url}`);
    });

    const session = new ComputeSession(CFG);
    await session.launch();
    expect(await session.reap()).toEqual([]);
    expect(log).toHaveBeenCalledWith(expect.stringContaining('slow'));
    expect(await session.reap()).toEqual(['slow']);
    expect(deletes).toBe(2);
  });

  it('reports the jobs it could not cancel and keeps reaping the rest', async () => {
    const log = silenceReapLog();
    const names = ['stuck', 'clean'];
    let launched = 0;
    let stuckDeletes = 0;
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 3_600)];
      if (url.endsWith('/v1/jobs') && init?.method === 'POST') return job(names[launched++]!);
      if (init?.method === 'DELETE') {
        const id = url.split('/').pop()!;
        if (id !== 'stuck') return job(id, 'cancelled');
        stuckDeletes += 1;
        return new Response(JSON.stringify({ error: { code: 'busy' } }), { status: 500 });
      }
      throw new Error(`unexpected ${url}`);
    });

    const session = new ComputeSession({ ...CFG, maxUsdcMicros: 10_000_000 });
    await session.launch();
    await session.launch();
    expect(await session.reap()).toEqual(['clean']);
    expect(log).toHaveBeenCalledWith(expect.stringContaining('stuck'));
    // The unconfirmed job stays tracked, so a later reap tries it again.
    expect(await session.reap()).toEqual([]);
    expect(stuckDeletes).toBe(2);
  });
});
