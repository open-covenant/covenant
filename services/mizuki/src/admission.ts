import { isIP } from 'node:net';
import type { IncomingMessage } from 'node:http';
import type { Config } from './config.js';

export type PublicRoute =
  | 'quote'
  | 'oauth_start'
  | 'oauth_callback'
  | 'wallet_challenge'
  | 'wallet_verify'
  | 'bounty_wallet_proof'
  | 'bounty_claim'
  | 'bounty_pr'
  | 'bounty_dispute'
  | 'job';

type Policy = {
  capacity: number;
  windowMs: number;
};

type Bucket = {
  tokens: number;
  refilledAt: number;
  seenAt: number;
};

type SourceBuckets = {
  routes: Map<PublicRoute, Bucket>;
  seenAt: number;
};

const policies: Record<PublicRoute, Policy> = {
  quote: { capacity: 6, windowMs: 60_000 },
  oauth_start: { capacity: 10, windowMs: 60_000 },
  oauth_callback: { capacity: 10, windowMs: 60_000 },
  wallet_challenge: { capacity: 8, windowMs: 60_000 },
  wallet_verify: { capacity: 8, windowMs: 60_000 },
  bounty_wallet_proof: { capacity: 6, windowMs: 60_000 },
  bounty_claim: { capacity: 6, windowMs: 60_000 },
  bounty_pr: { capacity: 6, windowMs: 60_000 },
  bounty_dispute: { capacity: 4, windowMs: 60_000 },
  job: { capacity: 8, windowMs: 60_000 },
};

export class PublicAdmission {
  readonly streams: ActivityStreams;
  private readonly buckets: BoundedTokenBuckets;
  private readonly trustedProxyHops: number;

  constructor(config: Config) {
    this.trustedProxyHops = config.trustedProxyHops ?? 0;
    this.buckets = new BoundedTokenBuckets(config.rateLimitMaxSources ?? 10_000);
    this.streams = new ActivityStreams(
      config.sseMaxConnections ?? 100,
      config.sseMaxConnectionsPerSource ?? 3,
    );
  }

  source(req: IncomingMessage): string {
    return requestSource(req, this.trustedProxyHops);
  }

  consume(route: PublicRoute, req: IncomingMessage): void {
    this.buckets.consume(route, this.source(req));
  }
}

export class BoundedTokenBuckets {
  private readonly sources = new Map<string, SourceBuckets>();
  private readonly overflow = new Map<PublicRoute, Bucket>();

  constructor(
    private readonly maxSources: number,
    private readonly now: () => number = Date.now,
  ) {
    if (!Number.isInteger(maxSources) || maxSources < 1) {
      throw new Error('rate-limit source capacity must be a positive integer');
    }
  }

  consume(route: PublicRoute, source: string): void {
    const now = this.now();
    const policy = policies[route];
    let sourceBuckets = this.sources.get(source);
    if (!sourceBuckets) {
      this.prune(now);
      if (this.sources.size < this.maxSources) {
        sourceBuckets = { routes: new Map(), seenAt: now };
        this.sources.set(source, sourceBuckets);
      }
    }

    const bucket = sourceBuckets
      ? (sourceBuckets.routes.get(route) ?? freshBucket(policy, now))
      : (this.overflow.get(route) ?? freshBucket(policy, now));
    if (sourceBuckets) {
      sourceBuckets.routes.set(route, bucket);
      sourceBuckets.seenAt = now;
    } else {
      this.overflow.set(route, bucket);
    }

    refill(bucket, policy, now);
    bucket.seenAt = now;
    if (bucket.tokens < 1) {
      const refillPerMs = policy.capacity / policy.windowMs;
      throw new RateLimitError(Math.max(1, Math.ceil((1 - bucket.tokens) / refillPerMs / 1_000)));
    }
    bucket.tokens -= 1;
  }

  private prune(now: number): void {
    const idleWindowMs = Math.max(...Object.values(policies).map((policy) => policy.windowMs));
    for (const [source, buckets] of this.sources) {
      if (now - buckets.seenAt >= idleWindowMs) this.sources.delete(source);
    }
  }
}

export class ActivityStreams {
  private total = 0;
  private readonly sources = new Map<string, number>();

  constructor(
    private readonly maxTotal: number,
    private readonly maxPerSource: number,
  ) {
    if (maxTotal < 1 || maxPerSource < 1 || maxPerSource > maxTotal) {
      throw new Error('invalid activity stream limits');
    }
  }

  acquire(source: string): () => void {
    const active = this.sources.get(source) ?? 0;
    if (this.total >= this.maxTotal || active >= this.maxPerSource) {
      throw new RateLimitError(5);
    }
    this.total += 1;
    this.sources.set(source, active + 1);

    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.total -= 1;
      const remaining = (this.sources.get(source) ?? 1) - 1;
      if (remaining === 0) this.sources.delete(source);
      else this.sources.set(source, remaining);
    };
  }
}

export class RateLimitError extends Error {
  constructor(readonly retryAfterSeconds: number) {
    super('request rate limit exceeded');
  }
}

export function requestSource(req: IncomingMessage, trustedProxyHops: number): string {
  const direct = normalizedIp(req.socket.remoteAddress) ?? 'unknown';
  if (trustedProxyHops === 0) return direct;

  const value = firstHeader(req, 'x-forwarded-for');
  if (!value) return direct;
  const forwarded = value.split(',').map((item) => normalizedIp(item.trim()));
  if (forwarded.some((item) => item === undefined)) return direct;

  const chain = [...(forwarded as string[]), direct];
  const index = chain.length - trustedProxyHops - 1;
  return index >= 0 ? chain[index] : direct;
}

function freshBucket(policy: Policy, now: number): Bucket {
  return { tokens: policy.capacity, refilledAt: now, seenAt: now };
}

function refill(bucket: Bucket, policy: Policy, now: number): void {
  const elapsed = Math.max(0, now - bucket.refilledAt);
  bucket.tokens = Math.min(
    policy.capacity,
    bucket.tokens + elapsed * (policy.capacity / policy.windowMs),
  );
  bucket.refilledAt = now;
}

function normalizedIp(value: string | undefined): string | undefined {
  if (!value) return undefined;
  let candidate = value.trim();
  if (candidate.startsWith('[') && candidate.includes(']')) {
    candidate = candidate.slice(1, candidate.indexOf(']'));
  }
  const zone = candidate.indexOf('%');
  if (zone >= 0) candidate = candidate.slice(0, zone);
  if (candidate.startsWith('::ffff:') && isIP(candidate.slice(7)) === 4) {
    candidate = candidate.slice(7);
  }
  return isIP(candidate) ? candidate.toLowerCase() : undefined;
}

function firstHeader(req: IncomingMessage, name: string): string | undefined {
  const value = req.headers[name];
  return Array.isArray(value) ? value[0] : value;
}
