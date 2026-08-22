/** Gateway configuration and model pricing, all overridable by env. */

const model = process.env.CODER_MODEL ?? 'claude-sonnet-4-6';
const readinessRefreshMs = boundedDuration(
  process.env.CODER_READINESS_REFRESH_MS,
  120_000,
  'CODER_READINESS_REFRESH_MS',
  10_000,
  300_000,
);
const readinessMaxAgeMs = boundedDuration(
  process.env.CODER_READINESS_MAX_AGE_MS,
  300_000,
  'CODER_READINESS_MAX_AGE_MS',
  30_000,
  600_000,
);
const readinessTimeoutMs = boundedDuration(
  process.env.CODER_READINESS_TIMEOUT_MS,
  20_000,
  'CODER_READINESS_TIMEOUT_MS',
  1_000,
  60_000,
);
if (readinessMaxAgeMs < readinessRefreshMs) {
  throw new Error('CODER_READINESS_MAX_AGE_MS must not be shorter than the refresh interval');
}
if (readinessTimeoutMs > readinessMaxAgeMs) {
  throw new Error('CODER_READINESS_TIMEOUT_MS must not exceed the maximum evidence age');
}

export const config = {
  authToken: process.env.CODER_AUTH_TOKEN,
  backend: (process.env.CODER_BACKEND ?? 'anthropic') as 'anthropic' | 'openai' | 'usepod',
  // Public default is Sonnet 4.6 — strong coding at ~5x lower cost than Opus,
  // so the $200/mo budget survives public traffic. Set CODER_MODEL to
  // claude-opus-4-7 for a gated top-quality tier.
  model,
  // Public (Sonnet) default is "low": on open-ended build prompts ("make a
  // Next.js app with X") high effort burns minutes on a single upfront
  // thinking block before the first tool call — measured ~3min — which reads
  // as a hang. Low gets to the first action in seconds and still drives a
  // real scaffold→install→build loop. The gated Opus tier keeps "xhigh".
  // Override per-deploy with CODER_EFFORT.
  effort: (process.env.CODER_EFFORT ?? (model.includes('opus') ? 'xhigh' : 'low')) as
    | 'low'
    | 'medium'
    | 'high'
    | 'xhigh'
    | 'max',

  // Spend caps (USD). Daily is a rate limit on the monthly bucket; both hard.
  dailyUsd: positiveNumber(process.env.CODER_DAILY_USD, 6, 'CODER_DAILY_USD'),
  monthlyUsd: positiveNumber(process.env.CODER_MONTHLY_USD, 200, 'CODER_MONTHLY_USD'),
  perRunUsdMax: positiveNumber(process.env.CODER_PER_RUN_USD_MAX, 2, 'CODER_PER_RUN_USD_MAX'),

  maxConcurrent: positiveInt(process.env.CODER_MAX_CONCURRENT, 2, 'CODER_MAX_CONCURRENT'),

  // Per-run wall-clock ceiling. The gateway aborts a run after this many ms;
  // the E2B microVM uses the same value as its self-destruct backstop, so a
  // gateway crash mid-run still has a hard end. Also the reservation horizon
  // the spend ledger persists for warm recovery on restart.
  wallMs: positiveInt(process.env.CODER_WALL_MS, 600_000, 'CODER_WALL_MS'),

  // Operator-supplied E2B estimate; reconcile it against live sandbox receipts.
  sandboxUsdPerSec: nonNegativeNumber(
    process.env.CODER_SANDBOX_USD_PER_SEC,
    0.0001,
    'CODER_SANDBOX_USD_PER_SEC',
  ),

  // Per-IP admission gate. With CODER_MAX_CONCURRENT=2 a single anonymous
  // client can otherwise occupy both slots and burn the entire daily cap
  // alone before Turnstile / edge gates land — one in-flight per IP is
  // the cheapest stop-gap. Set CODER_IP_MAX_PER_IP=0 to disable
  // explicitly (typos like `=true` would otherwise silently disable
  // through Number() → NaN → NaN > 0 false).
  ipMaxPerIp: nonNegativeInt(process.env.CODER_IP_MAX_PER_IP, 1, 'CODER_IP_MAX_PER_IP'),
  ipRefillMs: nonNegativeInt(process.env.CODER_IP_REFILL_MS, 60_000, 'CODER_IP_REFILL_MS'),
  // How many trusted proxies sit between the gateway and the public
  // internet. The right-most TRUSTED_PROXY_HOPS entries of
  // X-Forwarded-For are honored; everything left is treated as client-
  // controlled and ignored. Default 0 (use the socket peer) — safe for
  // any deployment but collapses every visitor behind shared NAT to one
  // address. Picking too large lets a client rotate IPs via the header.
  trustedProxyHops: nonNegativeInt(process.env.TRUSTED_PROXY_HOPS, 0, 'TRUSTED_PROXY_HOPS'),

  // Operator-allowlisted IPs that bypass the per-IP bucket AND the
  // daily/monthly USD spend caps. The kill-switch, the concurrency cap,
  // and observability still apply, so an exempt run still shows on
  // `/v1/budget`. Comma-separated list, IPv4 / IPv6 / bracketless. Set
  // to the operator's own IP so the public daily-cap exhaustion (which
  // is intentionally low) doesn't block diagnostic / development
  // traffic from the people maintaining the deployment.
  exemptIps: parseIpSet(process.env.CODER_EXEMPT_IPS),

  readinessRefreshMs,
  readinessMaxAgeMs,
  readinessTimeoutMs,
} as const;

export function assertProductionConfig(env: NodeJS.ProcessEnv = process.env): void {
  if (env.NODE_ENV !== 'production') return;
  const required = [
    ['CODER_AUTH_TOKEN', env.CODER_AUTH_TOKEN && env.CODER_AUTH_TOKEN.length >= 32],
    ['USEPOD_API_KEY', Boolean(env.USEPOD_API_KEY)],
    ['E2B_API_KEY', Boolean(env.E2B_API_KEY)],
    ['CODER_MODEL', Boolean(env.CODER_MODEL)],
    ['LEDGER_PATH', Boolean(env.LEDGER_PATH?.startsWith('/'))],
    ['RUN_STORE_PATH', Boolean(env.RUN_STORE_PATH?.startsWith('/'))],
  ] as const;
  const missing: string[] = required.filter(([, present]) => !present).map(([name]) => name);
  if ((env.CODER_BACKEND ?? 'anthropic') !== 'usepod') {
    missing.push('CODER_BACKEND=usepod');
  }
  if (env.USEPOD_MODEL) missing.push('USEPOD_MODEL must be unset; use CODER_MODEL');
  const egress = new Set(
    (env.E2B_EGRESS_ALLOW ?? '')
      .split(',')
      .map((value) => value.trim().toLowerCase())
      .filter(Boolean),
  );
  for (const host of ['github.com', 'codeload.github.com']) {
    if (!egress.has(host)) missing.push(`E2B_EGRESS_ALLOW:${host}`);
  }
  if (missing.length > 0) {
    throw new Error(`production gateway configuration is incomplete: ${missing.join(', ')}`);
  }
}

/**
 * Parse a non-negative integer env value, falling back to `fallback` on
 * an absent value but **refusing** garbage (negative, NaN, fractional)
 * — a silent `Number()` cast would let a typo like
 * `CODER_IP_MAX_PER_IP=true` (→ NaN → `NaN > 0` false) bypass the
 * per-IP gate without any visible signal. Loud-fail at boot is the
 * cheaper failure mode for a security-sensitive control.
 */
function nonNegativeInt(raw: string | undefined, fallback: number, name: string): number {
  if (raw === undefined || raw === '') return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
    throw new Error(
      `${name}=${JSON.stringify(raw)} is not a non-negative integer — refusing to boot with a silently-disabled control. Set ${name}=0 to opt out explicitly.`,
    );
  }
  return n;
}

function positiveInt(raw: string | undefined, fallback: number, name: string): number {
  const value = nonNegativeInt(raw, fallback, name);
  if (value === 0) throw new Error(`${name} must be greater than zero`);
  return value;
}

function positiveNumber(raw: string | undefined, fallback: number, name: string): number {
  const value = nonNegativeNumber(raw, fallback, name);
  if (value === 0) throw new Error(`${name} must be greater than zero`);
  return value;
}

function boundedDuration(
  raw: string | undefined,
  fallback: number,
  name: string,
  min: number,
  max: number,
): number {
  const value = positiveInt(raw, fallback, name);
  if (value < min || value > max) throw new Error(`${name} must be between ${min} and ${max}`);
  return value;
}

function nonNegativeNumber(raw: string | undefined, fallback: number, name: string): number {
  if (raw === undefined || raw === '') return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) {
    throw new Error(`${name} must be a non-negative finite number`);
  }
  return value;
}

/**
 * Parse a comma-separated IP allowlist. Empty / unset yields an empty
 * set; whitespace and trailing-comma typos are tolerated so an operator
 * editing the env var doesn't have to be byte-perfect.
 */
function parseIpSet(raw: string | undefined): ReadonlySet<string> {
  if (!raw) return new Set();
  return new Set(
    raw
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean),
  );
}

/** USD per 1M tokens, by model. cacheRead ~0.1x input, cacheWrite ~1.25x input. */
export const PRICING: Record<
  string,
  { input: number; output: number; cacheRead: number; cacheWrite: number }
> = {
  'claude-opus-4-7': { input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25 },
  'claude-sonnet-4-6': { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 },
  'claude-haiku-4-5': { input: 1, output: 5, cacheRead: 0.1, cacheWrite: 1.25 },
  'deepseek-v3.2': {
    input: Number(process.env.USEPOD_INPUT_USD_PER_MILLION ?? 0.2),
    output: Number(process.env.USEPOD_OUTPUT_USD_PER_MILLION ?? 0.4),
    cacheRead: 0,
    cacheWrite: 0,
  },
};
