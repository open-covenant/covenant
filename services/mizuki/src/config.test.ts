import { describe, expect, it } from 'vitest';
import { assertBootConfig, assertLiveConfig, liveConfigIssues, loadConfig } from './config.js';

describe('updater configuration', () => {
  it('uses the canary-qualified development routes', () => {
    expect(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' })).toMatchObject({
      usePodImplementationModel: 'deepseek-v3.2',
      usePodModel: 'deepseek-v4-flash',
    });
  });

  it('requires the updater URL and token together', () => {
    expect(() => loadConfig({ MIZUKI_UPDATER_URL: 'http://updater:8793' })).toThrow(
      'must be configured together',
    );
    expect(() => loadConfig({ MIZUKI_UPDATER_TOKEN: 't'.repeat(32) })).toThrow(
      'must be configured together',
    );
  });

  it('rejects malformed refund authority seeds', () => {
    expect(() => loadConfig({ MIZUKI_JOB_AUTHORITY_SEED: 'not-a-seed' })).toThrow('32-byte seed');
    expect(
      loadConfig({ MIZUKI_JOB_AUTHORITY_SEED: Buffer.alloc(32, 9).toString('base64') })
        .jobAuthoritySeed,
    ).toBe(Buffer.alloc(32, 9).toString('base64'));
  });

  it('loads bounded polling settings for an authenticated updater', () => {
    const config = loadConfig({
      MIZUKI_UPDATER_URL: 'updater:8793/',
      MIZUKI_UPDATER_TOKEN: 't'.repeat(32),
      MIZUKI_UPDATER_TIMEOUT_MS: '1200',
      MIZUKI_UPDATER_POLL_INTERVAL_MS: '5000',
    });
    expect(config).toMatchObject({
      updaterUrl: 'http://updater:8793',
      updaterToken: 't'.repeat(32),
      updaterTimeoutMs: 1200,
      updaterPollIntervalMs: 5000,
    });
  });

  it('keeps updater readiness inside the outer readiness deadline', () => {
    expect(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }).updaterTimeoutMs).toBe(15_000);
    expect(() =>
      loadConfig({
        MIZUKI_UPDATER_URL: 'http://updater:8793',
        MIZUKI_UPDATER_TOKEN: 't'.repeat(32),
        MIZUKI_UPDATER_TIMEOUT_MS: '20000',
        MIZUKI_READINESS_TIMEOUT_MS: '20000',
      }),
    ).toThrow('must be shorter');
  });

  it('bounds proxy, rate-limit, and activity stream settings', () => {
    expect(() => loadConfig({ MIZUKI_TRUSTED_PROXY_HOPS: '2' })).toThrow('between 0 and 1');
    expect(() => loadConfig({ MIZUKI_RATE_LIMIT_MAX_SOURCES: '99' })).toThrow(
      'between 100 and 100000',
    );
    expect(() => loadConfig({ MIZUKI_SSE_MAX_CONNECTIONS: '0' })).toThrow('between 1 and 1000');
    expect(() =>
      loadConfig({
        MIZUKI_SSE_MAX_CONNECTIONS: '2',
        MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE: '3',
      }),
    ).toThrow('must not exceed the global limit');
    expect(
      loadConfig({
        MIZUKI_TRUSTED_PROXY_HOPS: '0',
        MIZUKI_RATE_LIMIT_MAX_SOURCES: '5000',
        MIZUKI_SSE_MAX_CONNECTIONS: '50',
        MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE: '2',
        MIZUKI_SSE_IDLE_TIMEOUT_MS: '60000',
      }),
    ).toMatchObject({
      trustedProxyHops: 0,
      trustedProxyConfigured: true,
      rateLimitMaxSources: 5000,
      sseMaxConnections: 50,
      sseMaxConnectionsPerSource: 2,
      sseIdleTimeoutMs: 60000,
    });
  });

  it('enforces bounded readiness freshness', () => {
    expect(() =>
      loadConfig({ MIZUKI_READINESS_REFRESH_MS: '60000', MIZUKI_READINESS_MAX_AGE_MS: '30000' }),
    ).toThrow('must not be shorter');
    expect(() =>
      loadConfig({ MIZUKI_READINESS_TIMEOUT_MS: '120000', MIZUKI_READINESS_MAX_AGE_MS: '90000' }),
    ).toThrow('must not exceed');
    expect(
      loadConfig({
        MIZUKI_READINESS_REFRESH_MS: '20000',
        MIZUKI_READINESS_MAX_AGE_MS: '60000',
        MIZUKI_READINESS_TIMEOUT_MS: '10000',
      }),
    ).toMatchObject({
      readinessRefreshMs: 20000,
      readinessMaxAgeMs: 60000,
      readinessTimeoutMs: 10000,
      escrowReadinessMinLamports: '1000000000',
    });
    expect(() => loadConfig({ MIZUKI_ESCROW_READINESS_MIN_LAMPORTS: '0' })).toThrow(
      'positive atomic amount',
    );
  });

  it('enforces a zero-authority shadow boot boundary', () => {
    const shadow = {
      MIZUKI_RUNTIME_ROLE: 'shadow',
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_REQUIRE_GITHUB_APP: '0',
      MIZUKI_DATABASE_URL: 'postgres://mizuki:secret@database/mizuki_shadow',
      MIZUKI_ADMIN_TOKEN: 'a'.repeat(32),
    };

    const config = loadConfig(shadow);
    expect(config.runtimeRole).toBe('shadow');
    expect(() => assertBootConfig(config)).not.toThrow();
    expect(() => loadConfig({ ...shadow, MIZUKI_PAYMENT_MODE: 'live' })).toThrow(
      'MIZUKI_PAYMENT_MODE=mock',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_REQUIRE_GITHUB_APP: '1' })).toThrow(
      'MIZUKI_REQUIRE_GITHUB_APP=0',
    );
    expect(() => loadConfig({ ...shadow, USEPOD_API_KEY: '' })).toThrow('USEPOD_API_KEY');
    expect(() => loadConfig({ ...shadow, MIZUKI_CODING_GATEWAY_TOKEN: 'c'.repeat(32) })).toThrow(
      'MIZUKI_CODING_GATEWAY_TOKEN',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_POLICY_SIGNER_TOKEN: 'p'.repeat(32) })).toThrow(
      'MIZUKI_POLICY_SIGNER_TOKEN',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_GITHUB_APP_ID: '123' })).toThrow(
      'MIZUKI_GITHUB_APP_ID',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_UPDATER_TOKEN: 'u'.repeat(32) })).toThrow(
      'MIZUKI_UPDATER_TOKEN',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_RELEASE_PROBE_TOKEN: 'r'.repeat(32) })).toThrow(
      'MIZUKI_RELEASE_PROBE_TOKEN',
    );
    expect(() => loadConfig({ ...shadow, MIZUKI_FUTURE_CUSTODY_TOKEN: '' })).toThrow(
      'MIZUKI_FUTURE_CUSTODY_TOKEN',
    );
  });

  it('rejects incomplete shadow storage and invalid runtime roles', () => {
    expect(() => loadConfig({ MIZUKI_RUNTIME_ROLE: 'candidate' })).toThrow('MIZUKI_RUNTIME_ROLE');
    const config = loadConfig({
      MIZUKI_RUNTIME_ROLE: 'shadow',
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_REQUIRE_GITHUB_APP: '0',
    });
    expect(() => assertBootConfig(config)).toThrow('MIZUKI_DATABASE_URL, MIZUKI_ADMIN_TOKEN');
  });
});

describe('live configuration', () => {
  const complete = {
    MIZUKI_PAYMENT_MODE: 'live',
    MIZUKI_DATABASE_URL: 'postgres://mizuki:secret@database/mizuki',
    MIZUKI_PUBLIC_BASE_URL: 'https://api.example.com',
    MIZUKI_WEB_ORIGIN: 'https://mizuki.example.com',
    MIZUKI_TRUSTED_PROXY_HOPS: '1',
    MIZUKI_X402_FACILITATOR: 'https://facilitator.example.com',
    MIZUKI_PAY_TO: '11111111111111111111111111111111',
    MIZUKI_ESCROW_REFUND_TO: 'So11111111111111111111111111111111111111112',
    MIZUKI_POLICY_SIGNER_URL: 'http://signer:8792',
    MIZUKI_POLICY_SIGNER_TOKEN: 'p'.repeat(32),
    MIZUKI_JOB_AUTHORITY_SEED: Buffer.alloc(32, 7).toString('base64'),
    MIZUKI_ADMIN_TOKEN: 'a'.repeat(32),
    MIZUKI_RELEASE_PROBE_TOKEN: 'd'.repeat(32),
    MIZUKI_CODING_GATEWAY_URL: 'http://gateway:8642',
    MIZUKI_CODING_GATEWAY_TOKEN: 'c'.repeat(32),
    USEPOD_API_KEY: 'usepod-key',
    USEPOD_BASE_URL: 'https://api.usepod.ai',
    USEPOD_MODEL: 'coder-route',
    USEPOD_REVIEW_MODEL: 'reviewer-route',
    USEPOD_INPUT_USD_PER_MILLION: '0.2',
    USEPOD_OUTPUT_USD_PER_MILLION: '0.4',
    USEPOD_MAX_INPUT_PRICE_MICROUNITS: '200000',
    USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '400000',
    USEPOD_MIN_BALANCE: '2000000',
    MIZUKI_BOUNTY_REVIEW_MAX_COST_MICROUNITS: '50000',
    MIZUKI_GITHUB_APP_ID: '1234',
    MIZUKI_GITHUB_PRIVATE_KEY: '-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----',
    MIZUKI_GITHUB_CLIENT_ID: 'client-id',
    MIZUKI_GITHUB_CLIENT_SECRET: 'g'.repeat(32),
    MIZUKI_GITHUB_WEBHOOK_SECRET: 'w'.repeat(32),
    MIZUKI_SESSION_SECRET: 's'.repeat(32),
    MIZUKI_WEB_PROXY_SECRET: 'p'.repeat(32),
    MIZUKI_UPDATER_URL: 'http://updater:8793',
    MIZUKI_UPDATER_TOKEN: 'u'.repeat(32),
  };

  it('accepts the complete fail-closed production contract', () => {
    expect(() => assertLiveConfig(loadConfig(complete))).not.toThrow();
  });

  it('rejects a production updater timeout shorter than the application probe budget', () => {
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, MIZUKI_UPDATER_TIMEOUT_MS: '8000' })),
    ).toThrow('MIZUKI_UPDATER_TIMEOUT_MS must be at least 15000');
  });

  it('rejects missing custody, GitHub, route, and durable-store settings', () => {
    expect(() => assertLiveConfig(loadConfig({ MIZUKI_PAYMENT_MODE: 'live' }))).toThrow(
      'live Mizuki configuration is incomplete',
    );
  });

  it('boots only with durable storage and admin authority while readiness stays incomplete', () => {
    const config = loadConfig({
      MIZUKI_PAYMENT_MODE: 'live',
      MIZUKI_DATABASE_URL: 'postgres://mizuki:secret@database/mizuki',
      MIZUKI_ADMIN_TOKEN: 'a'.repeat(32),
    });

    expect(() => assertBootConfig(config)).not.toThrow();
    expect(liveConfigIssues(config)).toEqual(
      expect.arrayContaining(['MIZUKI_PUBLIC_BASE_URL', 'MIZUKI_POLICY_SIGNER_TOKEN']),
    );
    expect(() => assertLiveConfig(config)).toThrow('live Mizuki configuration is incomplete');
    expect(() => assertBootConfig(loadConfig({ MIZUKI_PAYMENT_MODE: 'live' }))).toThrow(
      'MIZUKI_DATABASE_URL, MIZUKI_ADMIN_TOKEN',
    );
  });

  it('requires separate coder and reviewer routes', () => {
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, USEPOD_REVIEW_MODEL: 'coder-route' })),
    ).toThrow('must differ');
  });

  it('keeps USDC refunds and SOL escrow returns on distinct signer custody addresses', () => {
    expect(() =>
      assertLiveConfig(
        loadConfig({ ...complete, MIZUKI_ESCROW_REFUND_TO: complete.MIZUKI_PAY_TO }),
      ),
    ).toThrow(/must differ/);
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, MIZUKI_ESCROW_REFUND_TO: 'not-an-address' })),
    ).toThrow(/MIZUKI_ESCROW_REFUND_TO/);
  });

  it('rejects tokenized base URLs and cost estimates below the request ceilings', () => {
    expect(() =>
      assertLiveConfig(
        loadConfig({ ...complete, USEPOD_BASE_URL: 'https://api.usepod.ai/proxy/exposed/v1' }),
      ),
    ).toThrow(/exactly https:\/\/api\.usepod\.ai/);
    expect(() =>
      assertLiveConfig(
        loadConfig({
          ...complete,
          USEPOD_MAX_OUTPUT_PRICE_MICROUNITS: '500000',
          USEPOD_OUTPUT_USD_PER_MILLION: '0.4',
        }),
      ),
    ).toThrow(/understates the output price ceiling/);
    for (const invalid of ['NaN', 'Infinity', '1e-1', '-0.2', '0', '0.1234567']) {
      expect(() => loadConfig({ ...complete, USEPOD_INPUT_USD_PER_MILLION: invalid })).toThrow(
        /USEPOD_INPUT_USD_PER_MILLION/,
      );
    }
  });

  it('pins the live provider origin and requires an explicit funded floor', () => {
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, USEPOD_BASE_URL: 'https://relay.example' })),
    ).toThrow(/exactly https:\/\/api\.usepod\.ai/);
    expect(() => assertLiveConfig(loadConfig({ ...complete, USEPOD_MIN_BALANCE: '0' }))).toThrow(
      /positive decimal/,
    );
  });

  it('keeps credentialed dependencies on single-label private service addresses', () => {
    for (const [name, value] of [
      ['MIZUKI_CODING_GATEWAY_URL', 'https://gateway.example.com'],
      ['MIZUKI_POLICY_SIGNER_URL', 'https://signer.example.com'],
      ['MIZUKI_UPDATER_URL', 'https://updater.example.com'],
    ]) {
      expect(() => assertLiveConfig(loadConfig({ ...complete, [name]: value }))).toThrow(name);
    }
  });

  it('requires explicit Render proxy trust in live mode', () => {
    const { MIZUKI_TRUSTED_PROXY_HOPS: _, ...withoutProxy } = complete;
    expect(() => assertLiveConfig(loadConfig(withoutProxy))).toThrow('MIZUKI_TRUSTED_PROXY_HOPS');
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, MIZUKI_TRUSTED_PROXY_HOPS: '0' })),
    ).toThrow('MIZUKI_TRUSTED_PROXY_HOPS=1');
  });

  it('requires authenticated web proxy forwarding in live mode', () => {
    const { MIZUKI_WEB_PROXY_SECRET: _, ...withoutProxySecret } = complete;
    expect(() => assertLiveConfig(loadConfig(withoutProxySecret))).toThrow(
      'MIZUKI_WEB_PROXY_SECRET',
    );
  });

  it('requires a valid capability payout wallet when ClawPump earnings are enabled', () => {
    expect(() =>
      assertLiveConfig(loadConfig({ ...complete, CLAWPUMP_AGENT_ID: 'mizuki-agent' })),
    ).toThrow('CLAWPUMP_PAYOUT_WALLET');
    expect(() =>
      assertLiveConfig(
        loadConfig({
          ...complete,
          CLAWPUMP_AGENT_ID: 'mizuki-agent',
          CLAWPUMP_PAYOUT_WALLET: complete.MIZUKI_ESCROW_REFUND_TO,
        }),
      ),
    ).not.toThrow();
  });
});
