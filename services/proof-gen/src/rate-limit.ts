import type IORedis from 'ioredis';

// Redis-backed token bucket. Atomic check-and-increment via a Lua script so
// two API replicas behind a load balancer share one limit per agent. The
// in-memory Map this replaces leaked 2× the configured limit per replica.

const SCRIPT = `
local key = KEYS[1]
local limit = tonumber(ARGV[1])
local window_ms = tonumber(ARGV[2])

local count = tonumber(redis.call('GET', key) or '0')
if count >= limit then
  local ttl = redis.call('PTTL', key)
  if ttl < 0 then ttl = window_ms end
  return { 0, ttl }
end

local new_count = redis.call('INCR', key)
if new_count == 1 then
  redis.call('PEXPIRE', key, window_ms)
end
return { 1, 0 }
`;

export type RateLimitOutcome = { ok: true } | { ok: false; retry_after: number };

export function rateLimitKey(agentDid: string): string {
  return `proof-gen:rl:${agentDid}`;
}

export type RateLimitDeps = {
  connection: IORedis;
  limit: number;
  windowMs: number;
};

export async function checkRateLimit(
  agentDid: string,
  deps: RateLimitDeps,
): Promise<RateLimitOutcome> {
  const key = rateLimitKey(agentDid);
  try {
    const raw = (await deps.connection.eval(
      SCRIPT,
      1,
      key,
      String(deps.limit),
      String(deps.windowMs),
    )) as [number, number];
    const allowed = raw[0] === 1;
    if (allowed) return { ok: true };
    return { ok: false, retry_after: Math.max(1, Math.ceil(raw[1] / 1000)) };
  } catch (err) {
    // Fail closed on Redis errors — a broken store should not allow
    // unlimited requests. Surface as rate_limited with a 1s retry so the
    // caller backs off and the operator notices via /metrics or logs.
    throw new RateLimitBackendError(err instanceof Error ? err.message : String(err));
  }
}

export class RateLimitBackendError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'RateLimitBackendError';
  }
}
