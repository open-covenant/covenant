import type { IncomingMessage } from 'node:http';
import { describe, expect, it } from 'vitest';
import {
  ActivityStreams,
  BoundedTokenBuckets,
  RateLimitError,
  requestSource,
} from './admission.js';

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

  it('ignores forwarding headers unless proxy hops are explicit and fails safe on bad data', () => {
    const request = {
      headers: { 'x-forwarded-for': '198.51.100.4, 10.0.0.2' },
      socket: { remoteAddress: '::ffff:10.0.0.3' },
    } as IncomingMessage;
    expect(requestSource(request, 0)).toBe('10.0.0.3');
    expect(requestSource(request, 2)).toBe('198.51.100.4');

    request.headers['x-forwarded-for'] = 'spoofed, 10.0.0.2';
    expect(requestSource(request, 2)).toBe('10.0.0.3');
    request.headers['x-forwarded-for'] = '198.51.100.4';
    expect(requestSource(request, 3)).toBe('10.0.0.3');
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
