import type { IncomingMessage } from 'node:http';
import { describe, expect, it } from 'vitest';
import {
  ActivityStreams,
  BoundedTokenBuckets,
  PublicAdmission,
  RateLimitError,
  requestScheme,
  requestSource,
} from './admission.js';
import { loadConfig } from './config.js';

describe('public admission limits', () => {
  it('refills bounded per-source token buckets and returns a retry delay', () => {
    let now = 1_000;
    const buckets = new BoundedTokenBuckets(100, () => now);
    for (let index = 0; index < 6; index += 1) buckets.consume('quote', '192.0.2.1');
    expect(() => buckets.consume('quote', '192.0.2.1')).toThrow(RateLimitError);
    try {
      buckets.consume('quote', '192.0.2.1');
    } catch (cause) {
      expect(cause).toMatchObject({ retryAfterSeconds: 10 });
    }
    now += 10_000;
    expect(() => buckets.consume('quote', '192.0.2.1')).not.toThrow();
  });

  it('shares a bounded overflow bucket instead of allocating unbounded sources', () => {
    const buckets = new BoundedTokenBuckets(1, () => 1_000);
    buckets.consume('quote', '192.0.2.1');
    for (let index = 0; index < 6; index += 1) {
      buckets.consume('quote', `198.51.100.${index + 1}`);
    }
    expect(() => buckets.consume('quote', '203.0.113.1')).toThrow(RateLimitError);
  });

  it.each([
    ['account_jobs' as const, 30],
    ['account_repositories' as const, 12],
    ['api_auth' as const, 60],
    ['api_tokens' as const, 10],
    ['payment_status' as const, 12],
    ['repository_connect' as const, 6],
    ['repository_issues' as const, 10],
  ])('bounds GitHub-backed %s requests per source', (route, capacity) => {
    const buckets = new BoundedTokenBuckets(100, () => 1_000);
    for (let index = 0; index < capacity; index += 1) {
      buckets.consume(route, '192.0.2.1');
    }
    expect(() => buckets.consume(route, '192.0.2.1')).toThrow(RateLimitError);
    expect(() => buckets.consume(route, '192.0.2.2')).not.toThrow();
  });

  it.each([
    ['preflight' as const, 10],
    ['account_jobs' as const, 30],
    ['account_repositories' as const, 12],
    ['api_tokens' as const, 10],
    ['payment_status' as const, 12],
    ['repository_connect' as const, 6],
    ['repository_issues' as const, 10],
  ])('bounds GitHub-backed %s requests per account across sources', (route, capacity) => {
    const secret = 'p'.repeat(32);
    const admission = new PublicAdmission(
      loadConfig({
        MIZUKI_WEB_PROXY_SECRET: secret,
        MIZUKI_RATE_LIMIT_MAX_SOURCES: '100',
      }),
    );
    const request = (index: number) =>
      ({
        headers: {
          'x-mizuki-client-ip': `198.51.100.${index + 1}`,
          'x-mizuki-proxy-secret': secret,
        },
        socket: { remoteAddress: '10.0.0.3' },
      }) as unknown as IncomingMessage;

    for (let index = 0; index < capacity; index += 1) {
      admission.consumeAccount(route, request(index), '42');
    }
    expect(() => admission.consumeAccount(route, request(capacity), '42')).toThrow(RateLimitError);
    expect(() => admission.consumeAccount(route, request(capacity + 1), '99')).not.toThrow();
  });

  it('trusts only the overwritten Cloudflare address on Render when enabled', () => {
    const request = {
      headers: {
        'cf-connecting-ip': '198.51.100.4',
        'x-forwarded-for': '203.0.113.9, 198.51.100.4',
      },
      socket: { remoteAddress: '::ffff:10.0.0.3' },
    } as IncomingMessage;
    expect(requestSource(request, 0)).toBe('10.0.0.3');
    expect(requestSource(request, 1)).toBe('198.51.100.4');

    request.headers['cf-connecting-ip'] = 'spoofed';
    expect(requestSource(request, 1)).toBe('10.0.0.3');
    delete request.headers['cf-connecting-ip'];
    expect(requestSource(request, 1)).toBe('10.0.0.3');
  });

  it('uses authenticated web identity and the validated Render source', () => {
    const secret = 'p'.repeat(32);
    const request = {
      headers: {
        'cf-connecting-ip': '192.0.2.4',
        'x-forwarded-for': '203.0.113.99, 192.0.2.4',
        'x-mizuki-client-ip': '198.51.100.7',
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': secret,
      },
      socket: { remoteAddress: '10.0.0.3' },
    } as unknown as IncomingMessage;

    expect(requestSource(request, 1, secret)).toBe('198.51.100.7');
    expect(requestScheme(request, 1, secret)).toBe('https');

    request.headers['x-mizuki-proxy-secret'] = 'x'.repeat(32);
    request.headers['x-forwarded-proto'] = 'http';
    expect(requestSource(request, 1, secret)).toBe('192.0.2.4');
    expect(requestScheme(request, 1, secret)).toBe('http');

    request.headers['x-mizuki-proxy-secret'] = 'é'.repeat(32);
    expect(requestSource(request, 1, secret)).toBe('192.0.2.4');
  });

  it('does not let caller-controlled XFF rotate Render admission buckets', () => {
    const admission = new PublicAdmission(loadConfig({ MIZUKI_TRUSTED_PROXY_HOPS: '1' }));
    const request = (index: number) =>
      ({
        headers: {
          'cf-connecting-ip': '192.0.2.4',
          'x-forwarded-for': `203.0.113.${index}, 192.0.2.4`,
        },
        socket: { remoteAddress: '10.0.0.3' },
      }) as unknown as IncomingMessage;

    for (let index = 0; index < 6; index += 1) admission.consume('quote', request(index));
    expect(() => admission.consume('quote', request(6))).toThrow(RateLimitError);
  });

  it('does not trust a proxy secret shorter than 32 UTF-8 bytes', () => {
    const shortSecret = 'é'.repeat(15);
    const request = {
      headers: {
        'cf-connecting-ip': '192.0.2.4',
        'x-forwarded-for': '203.0.113.99, 192.0.2.4',
        'x-forwarded-proto': 'http',
        'x-mizuki-client-ip': '198.51.100.7',
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': shortSecret,
      },
      socket: { remoteAddress: '10.0.0.3' },
    } as unknown as IncomingMessage;

    expect(requestSource(request, 1, shortSecret)).toBe('192.0.2.4');
    expect(requestScheme(request, 1, shortSecret)).toBe('http');

    const validSecret = 'é'.repeat(16);
    request.headers['x-mizuki-proxy-secret'] = validSecret;
    expect(requestSource(request, 1, validSecret)).toBe('198.51.100.7');
    expect(requestScheme(request, 1, validSecret)).toBe('https');
  });

  it('keeps authenticated web callers in separate rate-limit buckets', () => {
    const secret = 'p'.repeat(32);
    const admission = new PublicAdmission(
      loadConfig({
        MIZUKI_TRUSTED_PROXY_HOPS: '1',
        MIZUKI_WEB_PROXY_SECRET: secret,
      }),
    );
    const request = (clientIp: string) =>
      ({
        headers: {
          'x-mizuki-client-ip': clientIp,
          'x-mizuki-proxy-secret': secret,
        },
        socket: { remoteAddress: '10.0.0.3' },
      }) as unknown as IncomingMessage;

    for (let index = 0; index < 6; index += 1) {
      admission.consume('quote', request('198.51.100.7'));
    }
    expect(() => admission.consume('quote', request('198.51.100.8'))).not.toThrow();
  });

  it('caps total and per-source activity streams and releases exactly once', () => {
    const streams = new ActivityStreams(2, 1);
    const releaseA = streams.acquire('192.0.2.1');
    expect(() => streams.acquire('192.0.2.1')).toThrow(RateLimitError);
    const releaseB = streams.acquire('192.0.2.2');
    expect(() => streams.acquire('192.0.2.3')).toThrow(RateLimitError);
    releaseA();
    releaseA();
    const releaseC = streams.acquire('192.0.2.3');
    releaseB();
    releaseC();
  });
});
