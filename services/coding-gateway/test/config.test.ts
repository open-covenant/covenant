import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

const ENV_KEYS = [
  'CODER_IP_MAX_PER_IP',
  'CODER_IP_REFILL_MS',
  'TRUSTED_PROXY_HOPS',
  'CODER_EXEMPT_IPS',
  'CODER_READINESS_REFRESH_MS',
  'CODER_READINESS_MAX_AGE_MS',
  'CODER_READINESS_TIMEOUT_MS',
  'USEPOD_MIN_BALANCE',
] as const;

const savedEnv: Partial<Record<(typeof ENV_KEYS)[number], string | undefined>> = {};

describe('config env validation', () => {
  beforeEach(() => {
    for (const k of ENV_KEYS) savedEnv[k] = process.env[k];
  });

  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (savedEnv[k] === undefined) delete process.env[k];
      else process.env[k] = savedEnv[k];
    }
  });

  // Each test re-imports config so module-load-time validation runs
  // against the current env. Vitest caches imports per worker, so we
  // reset the module registry between tests.
  async function loadConfig(): Promise<typeof import('../src/config.js').config> {
    vi.resetModules();
    const { config } = await import('../src/config.js');
    return config;
  }

  it('accepts an explicit zero opt-out', async () => {
    process.env['CODER_IP_MAX_PER_IP'] = '0';
    const c = await loadConfig();
    expect(c.ipMaxPerIp).toBe(0);
  });

  it('refuses a typo that would silently disable the per-IP gate', async () => {
    // `Number('true')` is NaN, and `NaN > 0` is false — the previous
    // code would silently disable the gate. The validator must reject
    // at boot so the operator notices the typo.
    process.env['CODER_IP_MAX_PER_IP'] = 'true';
    await expect(loadConfig()).rejects.toThrow(/CODER_IP_MAX_PER_IP/);
  });

  it('refuses a negative TRUSTED_PROXY_HOPS instead of silently defaulting', async () => {
    process.env['TRUSTED_PROXY_HOPS'] = '-1';
    await expect(loadConfig()).rejects.toThrow(/TRUSTED_PROXY_HOPS/);
  });

  it('refuses a fractional CODER_IP_REFILL_MS to keep ms math integer-clean', async () => {
    process.env['CODER_IP_REFILL_MS'] = '1.5';
    await expect(loadConfig()).rejects.toThrow(/CODER_IP_REFILL_MS/);
  });

  it("treats an empty env value as 'unset' and uses the default", async () => {
    process.env['CODER_IP_MAX_PER_IP'] = '';
    const c = await loadConfig();
    expect(c.ipMaxPerIp).toBe(1);
  });

  it('enforces bounded readiness freshness', async () => {
    process.env['CODER_READINESS_REFRESH_MS'] = '60000';
    process.env['CODER_READINESS_MAX_AGE_MS'] = '30000';
    await expect(loadConfig()).rejects.toThrow(/must not be shorter/);

    process.env['CODER_READINESS_REFRESH_MS'] = '20000';
    process.env['CODER_READINESS_MAX_AGE_MS'] = '60000';
    process.env['CODER_READINESS_TIMEOUT_MS'] = '10000';
    await expect(loadConfig()).resolves.toMatchObject({
      readinessRefreshMs: 20000,
      readinessMaxAgeMs: 60000,
      readinessTimeoutMs: 10000,
    });
  });

  describe('exemptIps (CODER_EXEMPT_IPS) parsing', () => {
    it('parses a comma-separated exempt list into a Set', async () => {
      process.env['CODER_EXEMPT_IPS'] = '10.0.0.1,10.0.0.2';
      const c = await loadConfig();
      expect(c.exemptIps.size).toBe(2);
      expect(c.exemptIps.has('10.0.0.1')).toBe(true);
      expect(c.exemptIps.has('10.0.0.2')).toBe(true);
    });

    it('tolerates whitespace around entries so a hand-edited list still matches clean IPs', async () => {
      // Operators editing the env var won't be byte-perfect; trim() keeps
      // "10.0.0.1, 10.0.0.2 " usable. Drop trim and the leading space on the
      // second entry survives into the Set, so has("10.0.0.2") misses it and
      // that operator IP is silently blocked by the daily cap it was meant
      // to bypass.
      process.env['CODER_EXEMPT_IPS'] = '10.0.0.1, 10.0.0.2 ';
      const c = await loadConfig();
      expect(c.exemptIps.has('10.0.0.1')).toBe(true);
      expect(c.exemptIps.has('10.0.0.2')).toBe(true);
    });

    it('drops empty entries from trailing commas', async () => {
      process.env['CODER_EXEMPT_IPS'] = '10.0.0.1,,';
      const c = await loadConfig();
      expect(c.exemptIps.size).toBe(1);
      expect(c.exemptIps.has('10.0.0.1')).toBe(true);
      expect(c.exemptIps.has('')).toBe(false);
    });

    it('unset env yields an empty exempt set', async () => {
      delete process.env['CODER_EXEMPT_IPS'];
      const c = await loadConfig();
      expect(c.exemptIps.size).toBe(0);
    });
  });

  it('requires the production sandbox, auth, model, and persistent stores', async () => {
    const { assertProductionConfig } = await import('../src/config.js');
    expect(() => assertProductionConfig({ NODE_ENV: 'production' })).toThrow(/CODER_AUTH_TOKEN/);
    expect(() =>
      assertProductionConfig({
        NODE_ENV: 'production',
        CODER_BACKEND: 'usepod',
        CODER_AUTH_TOKEN: 'a'.repeat(32),
        CODER_MODEL: 'deepseek-v3.2',
        USEPOD_API_KEY: 'key',
        USEPOD_BASE_URL: 'https://api.usepod.ai',
        USEPOD_MAX_INPUT_PRICE_MICROUNITS: '200000',
        USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '400000',
        USEPOD_MIN_BALANCE: '2000000',
        E2B_API_KEY: 'key',
        E2B_TEMPLATE: 'mizuki-coder',
        E2B_EGRESS_ALLOW: 'github.com,codeload.github.com,registry.npmjs.org',
        LEDGER_PATH: '/var/data/ledger.json',
        RUN_STORE_PATH: '/var/data/runs.json',
      }),
    ).not.toThrow();
  });

  it('uses one production route variable for requests, readiness, and cost receipts', async () => {
    const { assertProductionConfig } = await import('../src/config.js');
    const base = {
      NODE_ENV: 'production',
      CODER_BACKEND: 'usepod',
      CODER_AUTH_TOKEN: 'a'.repeat(32),
      USEPOD_API_KEY: 'key',
      USEPOD_BASE_URL: 'https://api.usepod.ai',
      USEPOD_MAX_INPUT_PRICE_MICROUNITS: '200000',
      USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '400000',
      USEPOD_MIN_BALANCE: '2000000',
      E2B_API_KEY: 'key',
      E2B_TEMPLATE: 'mizuki-coder',
      E2B_EGRESS_ALLOW: 'github.com,codeload.github.com',
      LEDGER_PATH: '/var/data/ledger.json',
      RUN_STORE_PATH: '/var/data/runs.json',
    };

    expect(() => assertProductionConfig({ ...base, USEPOD_MODEL: 'route-a' })).toThrow(
      /CODER_MODEL/,
    );
    expect(() =>
      assertProductionConfig({
        ...base,
        CODER_MODEL: 'route-a',
        USEPOD_MODEL: 'route-b',
      }),
    ).toThrow(/USEPOD_MODEL must be unset/);
  });

  it('rejects unsafe proxy configuration and understated price estimates', async () => {
    const { assertProductionConfig } = await import('../src/config.js');
    const complete = {
      NODE_ENV: 'production',
      CODER_BACKEND: 'usepod',
      CODER_AUTH_TOKEN: 'a'.repeat(32),
      CODER_MODEL: 'deepseek-v3.2',
      USEPOD_API_KEY: 'key',
      USEPOD_BASE_URL: 'https://api.usepod.ai',
      USEPOD_MAX_INPUT_PRICE_MICROUNITS: '200000',
      USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '400000',
      USEPOD_MIN_BALANCE: '2000000',
      E2B_API_KEY: 'key',
      E2B_TEMPLATE: 'mizuki-coder',
      E2B_EGRESS_ALLOW: 'github.com,codeload.github.com',
      LEDGER_PATH: '/var/data/ledger.json',
      RUN_STORE_PATH: '/var/data/runs.json',
    };

    expect(() =>
      assertProductionConfig({
        ...complete,
        USEPOD_BASE_URL: 'https://api.usepod.ai/proxy/exposed/v1',
      }),
    ).toThrow(/exactly https:\/\/api\.usepod\.ai/);
    expect(() =>
      assertProductionConfig({
        ...complete,
        USEPOD_MAX_INPUT_PRICE_MICROUNITS: '300000',
        USEPOD_INPUT_USD_PER_MILLION: '0.2',
      }),
    ).toThrow(/understates the input price ceiling/);
  });

  it('pins the production provider origin and requires an explicit funded floor', async () => {
    const { assertProductionConfig } = await import('../src/config.js');
    const complete = {
      NODE_ENV: 'production',
      CODER_BACKEND: 'usepod',
      CODER_AUTH_TOKEN: 'a'.repeat(32),
      CODER_MODEL: 'deepseek-v3.2',
      USEPOD_API_KEY: 'key',
      USEPOD_BASE_URL: 'https://api.usepod.ai',
      USEPOD_MAX_INPUT_PRICE_MICROUNITS: '200000',
      USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '400000',
      USEPOD_MIN_BALANCE: '2000000',
      E2B_API_KEY: 'key',
      E2B_TEMPLATE: 'mizuki-coder',
      E2B_EGRESS_ALLOW: 'github.com,codeload.github.com',
      LEDGER_PATH: '/var/data/ledger.json',
      RUN_STORE_PATH: '/var/data/runs.json',
    };

    expect(() =>
      assertProductionConfig({ ...complete, USEPOD_BASE_URL: 'https://relay.example' }),
    ).toThrow(/exactly https:\/\/api\.usepod\.ai/);
    expect(() => assertProductionConfig({ ...complete, USEPOD_MIN_BALANCE: '0' })).toThrow(
      /positive decimal/,
    );
  });
});
