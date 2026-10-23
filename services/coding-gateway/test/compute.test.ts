import { afterEach, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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

const folders: string[] = [];
const journalConfig = () => {
  const folder = mkdtempSync(join(tmpdir(), 'gpu-journal-'));
  folders.push(folder);
  return {
    ...CFG,
    maxLaunches: 1,
    maxDurationSecs: 300,
    journalPath: join(folder, 'journal.json'),
  };
};

function mockFetch(handler: (url: string, init?: RequestInit) => unknown | Promise<unknown>) {
  const fn = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    const body = await handler(String(url), init);
    if (body instanceof Response) return body;
    return new Response(JSON.stringify(body), { status: 200 });
  });
  vi.stubGlobal('fetch', fn);
  return fn;
}

type WireJob = Record<string, any>;
function mockMarket(
  options: {
    rate?: number;
    offers?: WireJob[];
    post?: (plan: WireJob, key: string) => unknown | Promise<unknown>;
    cancel?: (job: WireJob) => unknown | Promise<unknown>;
    cancelByKey?: (key: string) => unknown | Promise<unknown>;
    get?: (job: WireJob) => unknown | Promise<unknown>;
  } = {},
) {
  const jobs = new Map<string, WireJob>();
  const plans: WireJob[] = [];
  const keys: string[] = [];
  const fetch = mockFetch(async (url, init) => {
    if (url.endsWith('/v1/offers'))
      return options.offers ?? [offer('a', options.rate ?? 1_200_000)];
    if (url.includes('/v1/jobs/by-key/') && init?.method === 'DELETE') {
      const key = decodeURIComponent(url.split('/').pop()!);
      if (options.cancelByKey) return options.cancelByKey(key);
      return new Response('{}', { status: 404 });
    }
    if (url.endsWith('/v1/jobs') && init?.method === 'POST') {
      const plan = JSON.parse(String(init.body));
      const key = (init.headers as Record<string, string>)['Idempotency-Key'];
      plans.push(plan);
      keys.push(key);
      if (options.post) return options.post(plan, key);
      const job = {
        id: `j${jobs.size + 1}`,
        status: 'running',
        offer_id: plan.offer.id,
        maximum_usdc_micros: plan.maximum_usdc_micros,
        access_url: 'https://workspace.test/?token=secret-workspace',
        error: null,
        receipt: null,
      };
      jobs.set(job.id, job);
      return job;
    }
    const job = jobs.get(url.split('/').pop()!);
    if (!job) throw new Error('unexpected URL');
    if (init?.method === 'DELETE') return options.cancel ? options.cancel(job) : settledJob(job);
    return options.get ? options.get(job) : job;
  });
  return { fetch, plans, keys, jobs };
}
function settledJob(job: WireJob, receipt: Record<string, unknown> = {}) {
  return {
    ...job,
    status: 'cancelled',
    access_url: null,
    receipt: {
      job_id: job.id,
      runtime_secs: 12,
      charged_usdc_micros: 4_000,
      refunded_usdc_micros: job.maximum_usdc_micros - 4_000,
      ...receipt,
    },
  };
}

afterEach(() => {
  for (const folder of folders.splice(0)) rmSync(folder, { recursive: true, force: true });
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

describe('bounded durable ComputeSession', () => {
  it('books one five-minute workspace with an exact quote below the total cap', async () => {
    const market = mockMarket();
    const session = new ComputeSession(journalConfig(), { runKey: 'dispatch-one' });
    expect((await session.launch(3600)).id).toBe('j1');
    expect(market.plans[0]).toMatchObject({ duration_secs: 300, maximum_usdc_micros: 100_000 });
    expect(session.committedUsd()).toBe(0.1);
    await expect(session.launch()).rejects.toThrow('this run may launch 1 GPU workspace');
    expect(market.plans).toHaveLength(1);
  });

  it('cannot spend the refund on another workspace', async () => {
    mockMarket();
    const session = new ComputeSession(journalConfig(), { runKey: 'cancelled-once' });
    await session.launch();
    await session.cancel('j1');
    expect(session.committedUsd()).toBe(0.1);
    await expect(session.launch()).rejects.toThrow('launch cap reached');
  });

  it('reserves the quote durably before POST and never journals access credentials', async () => {
    const cfg = journalConfig();
    mockMarket({
      post: (plan) => {
        const saved = readFileSync(cfg.journalPath, 'utf8');
        expect(saved).toContain('100000');
        expect(saved).toContain('dispatch-reserved');
        return {
          id: 'j1',
          offer_id: 'a',
          maximum_usdc_micros: plan.maximum_usdc_micros,
          status: 'running',
          access_url: 'https://test/?token=secret-workspace',
          error: null,
          receipt: null,
        };
      },
    });
    await new ComputeSession(cfg, { runKey: 'dispatch-reserved' }).launch();
    const saved = readFileSync(cfg.journalPath, 'utf8');
    expect(saved).not.toContain('secret-workspace');
    expect(saved).not.toContain('access_url');
    expect(saved).not.toContain('beta-token');
  });

  it('serializes simultaneous launches from separate sessions sharing a run key', async () => {
    const market = mockMarket();
    const cfg = journalConfig();
    const first = new ComputeSession(cfg, { runKey: 'concurrent-run' });
    const second = new ComputeSession(cfg, { runKey: 'concurrent-run' });
    const results = await Promise.allSettled([first.launch(), second.launch()]);
    expect(results.filter((r) => r.status === 'fulfilled')).toHaveLength(1);
    expect(market.plans).toHaveLength(1);
  });

  it('replays an ambiguous launch with the same key and exact plan across restart', async () => {
    let attempts = 0;
    const market = mockMarket({
      post: (plan) => {
        if (++attempts === 1) throw new Error('network timeout after provider accepted');
        return {
          id: 'recovered',
          status: 'running',
          offer_id: 'a',
          maximum_usdc_micros: plan.maximum_usdc_micros,
          access_url: null,
          error: null,
          receipt: null,
        };
      },
    });
    const cfg = journalConfig();
    const initial = new ComputeSession(cfg, { runKey: 'uncertain-run' });
    await expect(initial.launch()).rejects.toThrow('network timeout');
    expect(initial.committedUsd()).toBe(0.1);
    const restarted = new ComputeSession(cfg, { runKey: 'uncertain-run' });
    await expect(restarted.launch()).resolves.toMatchObject({ id: 'recovered' });
    expect(market.keys[0]).toBe(market.keys[1]);
    expect(market.plans[0]).toEqual(market.plans[1]);
    await expect(restarted.launch()).rejects.toThrow('launch cap reached');
  });

  it('never creates or replays an unknown booking during recovery or after cleanup', async () => {
    const market = mockMarket({
      post: () => {
        throw new Error('timeout');
      },
    });
    const cfg = journalConfig();
    const session = new ComputeSession(cfg, { runKey: 'unknown' });
    await expect(session.launch()).rejects.toThrow('timeout');
    const report = await ComputeSession.recover(cfg);
    expect(report.runs[0]).toMatchObject({
      runKey: 'unknown',
      complete: false,
      pendingJobIds: [],
    });
    expect(report.runs[0].pendingLaunchKeys).toHaveLength(1);
    expect(market.plans).toHaveLength(1);
    await expect(new ComputeSession(cfg, { runKey: 'unknown' }).launch()).rejects.toThrow('closed');
  });

  it('does not replay a reservation after its booking deadline', async () => {
    const market = mockMarket({
      post: () => {
        throw new Error('timeout');
      },
    });
    const cfg = journalConfig();
    await expect(new ComputeSession(cfg, { runKey: 'expired' }).launch()).rejects.toThrow(
      'timeout',
    );
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 301_000);
    await expect(new ComputeSession(cfg, { runKey: 'expired' }).launch()).rejects.toThrow(
      'deadline',
    );
    expect(market.plans).toHaveLength(1);
  });

  it('never exceeds the cumulative budget with a larger configured launch setting', async () => {
    const market = mockMarket({ rate: 1_800_000 });
    const session = new ComputeSession(
      { ...journalConfig(), maxLaunches: 2 },
      { runKey: 'aggregate' },
    );
    await session.launch();
    await expect(session.launch()).rejects.toThrow('50000 micro-USDC left');
    expect(market.plans).toHaveLength(1);
    expect(session.committedUsd()).toBe(0.15);
  });

  it('rejects an expensive quote without making a paid request', async () => {
    const market = mockMarket({ rate: 3_000_000 });
    await expect(
      new ComputeSession(journalConfig(), { runKey: 'pricey' }).launch(),
    ).rejects.toThrow('GPU budget');
    expect(market.plans).toHaveLength(0);
  });

  it.each([0, -1, NaN, Infinity, 60, 299])(
    'rejects unsupported duration %s before requests',
    async (duration) => {
      const market = mockMarket();
      await expect(
        new ComputeSession(journalConfig(), { runKey: 'duration' }).launch(duration),
      ).rejects.toThrow();
      expect(market.fetch).not.toHaveBeenCalled();
    },
  );

  it('rejects per-run bounds above the operator limits', () => {
    expect(
      () => new ComputeSession(journalConfig(), { runKey: 'too-high', maxUsdcMicros: 200_001 }),
    ).toThrow('invalid per-run');
  });

  it('fails closed on a corrupt journal rather than resetting the spend counter', () => {
    const cfg = journalConfig();
    writeFileSync(cfg.journalPath, '{"version":1,"runs":{"bad":{}}}');
    expect(() => new ComputeSession(cfg, { runKey: 'corrupt' })).toThrow('invalid GPU journal');
  });

  it('rejects malformed offers and retains a valid ID from a malformed paid response', async () => {
    mockFetch(() => [{ ...offer('a', 100), rate_usdc_micros_per_hour: -1 }]);
    await expect(
      new ComputeSession(journalConfig(), { runKey: 'bad-offer' }).launch(),
    ).rejects.toThrow('malformed offer');
    mockMarket({ post: () => ({ id: 'known-malformed', status: 'running' }) });
    const session = new ComputeSession(journalConfig(), { runKey: 'bad-job' });
    await expect(session.launch()).rejects.toThrow('malformed job');
    expect(session.inspect().pendingJobIds).toEqual(['known-malformed']);
  });
});

describe('launches the provider refuses', () => {
  const refused = (plan: WireJob, id: string) => ({
    id,
    status: 'failed',
    offer_id: plan.offer.id,
    maximum_usdc_micros: plan.maximum_usdc_micros,
    access_url: null,
    error: 'provider_rejected',
    receipt: {
      job_id: id,
      runtime_secs: 0,
      charged_usdc_micros: 0,
      refunded_usdc_micros: plan.maximum_usdc_micros,
      provider: 'control-plane',
      commitment: `control-plane:launch-rejected:${id}`,
    },
  });
  const offers = [
    offer('a', 404_445),
    offer('b', 407_408),
    offer('c', 407_408),
    offer('d', 611_112),
  ];

  it('moves to the next machine without using the launch allowance', async () => {
    let posts = 0;
    const market = mockMarket({
      offers,
      post: (plan) =>
        ++posts === 1
          ? refused(plan, 'declined-1')
          : {
              id: 'accepted-2',
              status: 'provisioning',
              offer_id: plan.offer.id,
              maximum_usdc_micros: plan.maximum_usdc_micros,
              access_url: null,
              error: null,
              receipt: null,
            },
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'refused-once' });
    const job = await session.launch(300);
    expect(job).toMatchObject({ id: 'accepted-2', offer_id: 'b', status: 'provisioning' });
    expect(market.plans.map((plan) => plan.offer.id)).toEqual(['a', 'b']);
    expect(new Set(market.keys).size).toBe(2);
    expect(session.inspect()).toMatchObject({ pendingJobIds: ['accepted-2'], settled: [] });
    // The accepted launch is the run's one launch.
    await expect(session.launch(300)).rejects.toThrow(/launch cap reached/);
  });

  it('stops after two refusals and reports the last one', async () => {
    const market = mockMarket({
      offers,
      post: (plan, key) => refused(plan, `declined-${key.slice(-1)}`),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'refused-always' });
    const job = await session.launch(300);
    expect(job.status).toBe('failed');
    expect(market.plans.map((plan) => plan.offer.id)).toEqual(['a', 'b', 'c']);
    expect(session.inspect().settled).toHaveLength(1);
    expect(session.inspect().settled[0]!.receipt.charged_usdc_micros).toBe(0);
    await expect(session.launch(300)).rejects.toThrow(/launch cap reached/);
    expect(market.plans).toHaveLength(3);
  });

  it('moves past a stale quote the control plane refuses before booking', async () => {
    let posts = 0;
    const market = mockMarket({
      offers,
      post: (plan) =>
        ++posts === 1
          ? new Response(JSON.stringify({ error: { code: 'stale_offer' } }), { status: 409 })
          : {
              id: 'accepted-after-stale',
              status: 'provisioning',
              offer_id: plan.offer.id,
              maximum_usdc_micros: plan.maximum_usdc_micros,
              access_url: null,
              error: null,
              receipt: null,
            },
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'stale-once' });
    expect((await session.launch(300)).offer_id).toBe('b');
    expect(market.plans.map((plan) => plan.offer.id)).toEqual(['a', 'b']);
  });

  it('gives up on stale quotes after two retries', async () => {
    const market = mockMarket({
      offers,
      post: () => new Response(JSON.stringify({ error: { code: 'stale_offer' } }), { status: 409 }),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'stale-always' });
    await expect(session.launch(300)).rejects.toThrow(/stale_offer/);
    expect(market.plans).toHaveLength(3);
  });

  it('does not retry a failure that charged anything', async () => {
    const market = mockMarket({
      offers,
      post: (plan) => ({
        ...refused(plan, 'charged-1'),
        receipt: {
          ...refused(plan, 'charged-1').receipt,
          provider: 'vast',
          charged_usdc_micros: 100,
          refunded_usdc_micros: plan.maximum_usdc_micros - 100,
        },
      }),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'charged-failure' });
    expect((await session.launch(300)).status).toBe('failed');
    expect(market.plans).toHaveLength(1);
  });
});

describe('cleanup and receipt settlement', () => {
  it('does not delay known-job cleanup behind an unknown-key request', async () => {
    let release!: () => void;
    const cancelled = new Promise<void>((resolve) => {
      release = resolve;
    });
    let posts = 0;
    const known = {
      id: 'already-known',
      status: 'running',
      offer_id: 'a',
      maximum_usdc_micros: 100_000,
      access_url: null,
      error: null,
      receipt: null,
    };
    const market = mockMarket({
      post: () => {
        if (++posts === 1) return known;
        throw new Error('second response lost');
      },
      cancel: (job) => {
        release();
        return settledJob(job);
      },
      cancelByKey: async (key) => {
        await cancelled;
        return { idempotency_key: key, cancellation_recorded: true, job: null };
      },
    });
    market.jobs.set(known.id, known);
    const session = new ComputeSession(
      { ...journalConfig(), maxLaunches: 2 },
      { runKey: 'parallel-cleanup' },
    );
    await session.launch();
    await expect(session.launch()).rejects.toThrow('second response lost');
    expect(await session.reap()).toMatchObject({ complete: true, settled: [{ job_id: known.id }] });
    expect(market.plans).toHaveLength(2);
    expect(session.committedUsd()).toBe(0.2);
  });

  it('recovers a lost launch response by key and settles its receipt without another POST', async () => {
    let accepted!: WireJob;
    const market = mockMarket({
      post: (plan) => {
        accepted = {
          id: 'lost-response-job',
          status: 'stopping',
          offer_id: plan.offer.id,
          maximum_usdc_micros: plan.maximum_usdc_micros,
          access_url: null,
          error: null,
          receipt: null,
        };
        throw new Error('response lost after acceptance');
      },
      cancelByKey: (key) => ({
        idempotency_key: key,
        cancellation_recorded: true,
        job: accepted,
      }),
    });
    const cfg = journalConfig();
    const session = new ComputeSession(cfg, { runKey: 'lost-response' });
    await expect(session.launch()).rejects.toThrow('response lost');
    market.jobs.set(accepted.id, accepted);
    const recovered = await ComputeSession.recover(cfg);
    expect(recovered.runs[0]).toMatchObject({
      complete: true,
      pendingLaunchKeys: [],
      pendingJobIds: [],
      settled: [{ job_id: accepted.id, receipt: { charged_usdc_micros: 4_000 } }],
    });
    expect(market.plans).toHaveLength(1);
    const lookup = market.fetch.mock.calls.find(([url]) => String(url).includes('/by-key/'))!;
    expect(lookup[0]).toBe(`${cfg.apiUrl}/v1/jobs/by-key/${market.keys[0]}`);
    expect(lookup[1]).toMatchObject({
      method: 'DELETE',
      headers: { Authorization: 'Bearer beta-token' },
    });
    const restarted = new ComputeSession(cfg, { runKey: 'lost-response' });
    expect(restarted.committedUsd()).toBe(0.1);
    await expect(restarted.launch()).rejects.toThrow('closed');
  });

  it('accepts only a sealed absence, preserves the consumed booking and survives restart', async () => {
    const market = mockMarket({
      post: () => {
        throw new Error('unknown delivery');
      },
      cancelByKey: (key) => ({ idempotency_key: key, cancellation_recorded: true, job: null }),
    });
    const cfg = journalConfig();
    const session = new ComputeSession(cfg, { runKey: 'sealed-absent' });
    await expect(session.launch()).rejects.toThrow('unknown delivery');
    expect(await session.reap()).toMatchObject({
      complete: true,
      settled: [],
      pendingLaunchKeys: [],
      pendingJobIds: [],
    });
    const saved = Object.values(
      JSON.parse(readFileSync(cfg.journalPath, 'utf8')).runs,
    )[0] as WireJob;
    expect(saved).toMatchObject({ closed: true, maxLaunches: 1, maxUsdcMicros: 200_000 });
    expect(saved.reservations).toHaveLength(1);
    expect(saved.reservations[0]).toMatchObject({
      rejected: true,
      key: market.keys[0],
      plan: market.plans[0],
    });
    const restarted = new ComputeSession(cfg, { runKey: 'sealed-absent' });
    expect(restarted.committedUsd()).toBe(0.1);
    await expect(restarted.launch()).rejects.toThrow('closed');
    expect((await ComputeSession.recover(cfg)).runs[0].complete).toBe(true);
    expect(market.plans).toHaveLength(1);
    expect(
      market.fetch.mock.calls.filter(([url]) => String(url).includes('/by-key/')),
    ).toHaveLength(1);
  });

  it.each([
    ['missing seal', (key: string) => ({ idempotency_key: key, job: null })],
    [
      'false seal',
      (key: string) => ({ idempotency_key: key, cancellation_recorded: false, job: null }),
    ],
    [
      'string seal',
      (key: string) => ({ idempotency_key: key, cancellation_recorded: 'true', job: null }),
    ],
    ['wrong key', () => ({ idempotency_key: 'other-key', cancellation_recorded: true, job: null })],
    ['missing job', (key: string) => ({ idempotency_key: key, cancellation_recorded: true })],
    [
      'malformed job',
      (key: string) => ({
        idempotency_key: key,
        cancellation_recorded: true,
        job: { id: 'untrusted-id' },
      }),
    ],
    ['empty body', () => new Response(null, { status: 204 })],
    ['not found', () => new Response('{}', { status: 404 })],
    ['no authentication', () => new Response('{}', { status: 401 })],
    ['wrong owner', () => new Response('{}', { status: 403 })],
    [
      'timeout',
      () => {
        throw new Error('timeout');
      },
    ],
  ])(
    'retains an unknown booking on %s and safely retries background cancellation',
    async (_name, reply) => {
      let valid = false;
      const market = mockMarket({
        post: () => {
          throw new Error('lost response');
        },
        cancelByKey: (key) =>
          valid ? { idempotency_key: key, cancellation_recorded: true, job: null } : reply(key),
      });
      const cfg = journalConfig();
      const session = new ComputeSession(cfg, { runKey: 'invalid-key-cancellation' });
      await expect(session.launch()).rejects.toThrow('lost response');
      const before = JSON.parse(readFileSync(cfg.journalPath, 'utf8'));
      Object.values(before.runs).forEach((run) => {
        (run as WireJob).closed = true;
      });
      expect(await session.reap()).toMatchObject({
        complete: false,
        pendingJobIds: [],
        settled: [],
      });
      expect(session.inspect().pendingLaunchKeys).toEqual(market.keys);
      expect(JSON.parse(readFileSync(cfg.journalPath, 'utf8'))).toEqual(before);
      valid = true;
      expect(await session.reap()).toMatchObject({ complete: true, settled: [] });
      expect(market.plans).toHaveLength(1);
    },
  );

  it.each([
    { offer_id: 'different-offer' },
    { maximum_usdc_micros: 99_999 },
    { receipt: { runtime_secs: 1, charged_usdc_micros: 1, refunded_usdc_micros: 1 } },
  ])(
    'does not attach an ID from a key result that mismatches its booking: %j',
    async (mismatch) => {
      const market = mockMarket({
        post: () => {
          throw new Error('lost response');
        },
        cancelByKey: (key) => ({
          idempotency_key: key,
          cancellation_recorded: true,
          job: {
            id: 'untrusted-id',
            status: 'cancelled',
            offer_id: 'a',
            maximum_usdc_micros: 100_000,
            access_url: null,
            error: null,
            receipt: null,
            ...mismatch,
          },
        }),
      });
      const session = new ComputeSession(journalConfig(), { runKey: 'mismatched-key-job' });
      await expect(session.launch()).rejects.toThrow('lost response');
      expect(await session.reap()).toMatchObject({
        complete: false,
        pendingLaunchKeys: market.keys,
        pendingJobIds: [],
        settled: [],
      });
      expect(
        market.fetch.mock.calls.filter(([url]) => String(url).endsWith('/untrusted-id')),
      ).toHaveLength(0);
    },
  );

  it('polls stopping until a terminal receipt conserves the booked maximum', async () => {
    let polls = 0;
    mockMarket({
      cancel: (job) => ({ ...job, status: 'stopping' }),
      get: (job) =>
        ++polls === 1 ? { ...job, status: 'cancelled', receipt: null } : settledJob(job),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'stopping' });
    await session.launch();
    const result = await session.reap();
    expect(polls).toBe(2);
    expect(result).toMatchObject({
      complete: true,
      pendingJobIds: [],
      pendingLaunchKeys: [],
      settled: [
        {
          job_id: 'j1',
          maximum_usdc_micros: 100_000,
          receipt: { charged_usdc_micros: 4_000, refunded_usdc_micros: 96_000 },
        },
      ],
    });
    expect(await session.reap()).toEqual(result);
  });

  it('retains a nonterminal job after the cleanup deadline', async () => {
    mockMarket({
      cancel: (job) => ({ ...job, status: 'stopping' }),
      get: (job) => ({ ...job, status: 'stopping' }),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'still-stopping' });
    await session.launch();
    expect(await session.reap(10)).toMatchObject({
      complete: false,
      pendingJobIds: ['j1'],
      settled: [],
    });
  });

  it.each([
    { charged_usdc_micros: 100_001, refunded_usdc_micros: 0 },
    { charged_usdc_micros: -1, refunded_usdc_micros: 100_001 },
    { charged_usdc_micros: 4_000, refunded_usdc_micros: 95_999 },
    { runtime_secs: 1.5 },
    { job_id: 'someone-elses-job' },
    { commitment: { access_url: 'https://secret-workspace/' } },
  ])('never reports an invalid final receipt as settled: %j', async (receipt) => {
    mockMarket({ cancel: (job) => settledJob(job, receipt) });
    const session = new ComputeSession(journalConfig(), { runKey: 'invalid-receipt' });
    await session.launch();
    expect(await session.reap()).toMatchObject({
      complete: false,
      pendingJobIds: ['j1'],
      settled: [],
    });
  });

  it('does not treat terminal status without a receipt as successful settlement', async () => {
    mockMarket({
      cancel: (job) => ({ ...job, status: 'cancelled', receipt: null }),
      get: (job) => ({ ...job, status: 'cancelled', receipt: null }),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'missing-receipt' });
    await session.launch();
    expect(await session.reap(10)).toMatchObject({ complete: false, pendingJobIds: ['j1'] });
    await expect(session.status('j1')).rejects.toThrow('receipt is still pending');
  });

  it('recovers and settles a known workspace after restart without another POST', async () => {
    const market = mockMarket();
    const cfg = journalConfig();
    await new ComputeSession(cfg, { runKey: 'restart' }).launch();
    const recovered = await ComputeSession.recover(cfg);
    expect(recovered.runs[0]).toMatchObject({
      runKey: 'restart',
      complete: true,
      settled: [{ job_id: 'j1' }],
    });
    expect(market.plans).toHaveLength(1);
    await expect(new ComputeSession(cfg, { runKey: 'restart' }).launch()).rejects.toThrow('closed');
  });

  it('waits for an in-flight POST then cancels the returned job', async () => {
    let resolvePost!: (value: unknown) => void;
    let started!: () => void;
    const start = new Promise<void>((resolve) => {
      started = resolve;
    });
    mockFetch((url, init) => {
      if (url.endsWith('/v1/offers')) return [offer('a', 1_200_000)];
      if (init?.method === 'POST') {
        started();
        return new Promise((resolve) => {
          resolvePost = resolve;
        });
      }
      if (init?.method === 'DELETE')
        return settledJob({
          id: 'inflight',
          offer_id: 'a',
          maximum_usdc_micros: 100_000,
          access_url: null,
          error: null,
        });
      throw new Error('unexpected request');
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'inflight' });
    const launch = session.launch();
    await start;
    const reap = session.reap();
    resolvePost({
      id: 'inflight',
      status: 'running',
      offer_id: 'a',
      maximum_usdc_micros: 100_000,
      access_url: null,
      error: null,
      receipt: null,
    });
    await launch;
    expect(await reap).toMatchObject({ complete: true, settled: [{ job_id: 'inflight' }] });
    await expect(session.launch()).rejects.toThrow('closed');
  });

  it.each(['../../v1/offers', '', 'j1/../../v1/admin', 'unowned'])(
    'blocks invalid and unowned IDs %s',
    async (id) => {
      const market = mockMarket();
      const session = new ComputeSession(journalConfig(), { runKey: 'ownership' });
      await expect(session.status(id)).rejects.toThrow();
      await expect(session.cancel(id)).rejects.toThrow();
      expect(market.fetch).not.toHaveBeenCalled();
    },
  );

  it('rejects responses bound to a different booking', async () => {
    mockMarket({ get: (job) => ({ ...job, id: 'other-run' }) });
    const session = new ComputeSession(journalConfig(), { runKey: 'wrong-id' });
    await session.launch();
    await expect(session.status('j1')).rejects.toThrow('does not match this booking');
  });
});

describe('launch response and authorization edge cases', () => {
  it('never reports a terminal launch without a receipt as success', async () => {
    mockMarket({
      post: (plan) => ({
        id: 'terminal-missing',
        status: 'failed',
        offer_id: 'a',
        maximum_usdc_micros: plan.maximum_usdc_micros,
        access_url: null,
        error: null,
        receipt: null,
      }),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'terminal-launch' });
    await expect(session.launch()).rejects.toThrow('receipt is still pending');
    expect(session.inspect()).toMatchObject({
      complete: false,
      pendingJobIds: ['terminal-missing'],
    });
  });

  it('does not replay a pending reservation above a tightened authorization cap', async () => {
    const market = mockMarket({
      post: () => {
        throw new Error('ambiguous');
      },
    });
    const cfg = journalConfig();
    await expect(new ComputeSession(cfg, { runKey: 'tightened' }).launch()).rejects.toThrow(
      'ambiguous',
    );
    await expect(
      new ComputeSession(cfg, { runKey: 'tightened', maxUsdcMicros: 50_000 }).launch(),
    ).rejects.toThrow('current run authorization');
    expect(market.plans).toHaveLength(1);
  });

  it('does not discard an uncertain reservation after a non-specific server error', async () => {
    mockMarket({
      post: () =>
        new Response(
          JSON.stringify({
            error: { code: 'busy', message: 'https://workspace/?secret=do-not-log' },
          }),
          { status: 500 },
        ),
    });
    const session = new ComputeSession(journalConfig(), { runKey: 'server-error' });
    await expect(session.launch()).rejects.toThrow('compute request failed: busy');
    expect(session.inspect().pendingLaunchKeys).toHaveLength(1);
    expect(session.committedUsd()).toBe(0.1);
  });

  it('retains ownership after an empty DELETE reply so recovery can retry cleanup', async () => {
    const cfg = journalConfig();
    let deletes = 0;
    mockMarket({
      cancel: (job) => (++deletes === 1 ? new Response(null, { status: 204 }) : settledJob(job)),
    });
    const session = new ComputeSession(cfg, { runKey: 'empty-delete' });
    await session.launch();
    expect(await session.reap()).toMatchObject({ complete: false, pendingJobIds: ['j1'] });
    expect((await ComputeSession.recover(cfg)).runs[0]).toMatchObject({
      complete: true,
      pendingJobIds: [],
    });
    expect(deletes).toBe(2);
  });
});

describe('dispatch cancellation before GPU submission', () => {
  it('prevents a fresh or ambiguous POST after dispatcher abort', async () => {
    const market = mockMarket({
      post: () => {
        throw new Error('timeout');
      },
    });
    const controller = new AbortController();
    const session = new ComputeSession(journalConfig(), {
      runKey: 'abort-retry',
      signal: controller.signal,
    });
    await expect(session.launch()).rejects.toThrow('timeout');
    controller.abort();
    await expect(session.launch()).rejects.toThrow('aborted');
    expect(market.plans).toHaveLength(1);
  });

  it('rechecks abort after a pending offer lookup before reserving or paying', async () => {
    const controller = new AbortController();
    const fetched = mockFetch((url) => {
      expect(url).toContain('/v1/offers');
      controller.abort();
      return [offer('a', 1_200_000)];
    });
    const session = new ComputeSession(journalConfig(), {
      runKey: 'abort-offers',
      signal: controller.signal,
    });
    await expect(session.launch()).rejects.toThrow('aborted');
    expect(fetched).toHaveBeenCalledTimes(1);
    expect(session.committedUsd()).toBe(0);
  });
});
