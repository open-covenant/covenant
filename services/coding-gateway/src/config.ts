import { parseE2bEgressPolicy } from './egress-policy.js';

/** Gateway configuration and model pricing, all overridable by env. */

const model = process.env.CODER_MODEL ?? 'claude-sonnet-4-6';
const usePodBaseUrl = process.env.USEPOD_BASE_URL ?? 'https://api.usepod.ai';
const usePodMaxInputPriceMicrounits = boundedInteger(
  process.env.USEPOD_MAX_INPUT_PRICE_MICROUNITS,
  200_000,
  'USEPOD_MAX_INPUT_PRICE_MICROUNITS',
  1,
  100_000_000,
);
const usePodMaxOutputPriceMicrounits = boundedInteger(
  process.env.USEPOD_MAX_OUTPUT_PRICE_MICROUNITS,
  400_000,
  'USEPOD_MAX_OUTPUT_PRICE_MICROUNITS',
  1,
  100_000_000,
);
const usePodInputPrice = usdPerMillionPrice(
  process.env.USEPOD_INPUT_USD_PER_MILLION,
  '0.2',
  'USEPOD_INPUT_USD_PER_MILLION',
);
const usePodOutputPrice = usdPerMillionPrice(
  process.env.USEPOD_OUTPUT_USD_PER_MILLION,
  '0.4',
  'USEPOD_OUTPUT_USD_PER_MILLION',
);
const usePodMinimumBalance = atomicUnits(process.env.USEPOD_MIN_BALANCE, '1', 'USEPOD_MIN_BALANCE');
const perRunUsdMax = positiveNumber(process.env.CODER_PER_RUN_USD_MAX, 2, 'CODER_PER_RUN_USD_MAX');
const sandboxWorstCaseUsdPerSec = boundedPositiveNumber(
  process.env.CODER_E2B_WORST_CASE_USD_PER_SEC,
  0.0002,
  'CODER_E2B_WORST_CASE_USD_PER_SEC',
  0.01,
);
const sandboxTariffRef = tariffReference(
  process.env.CODER_E2B_TARIFF_REF,
  'development-unverified',
  'CODER_E2B_TARIFF_REF',
);
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
const e2bEgressAllowlist = parseE2bEgressPolicy(process.env.E2B_EGRESS_ALLOW);
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
  usePodBaseUrl,
  usePodMaxInputPriceMicrounits,
  usePodMaxOutputPriceMicrounits,
  usePodInputUsdPerMillion: usePodInputPrice.usd,
  usePodOutputUsdPerMillion: usePodOutputPrice.usd,
  usePodMinimumBalance,
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
  perRunUsdMax,

  maxConcurrent: positiveInt(process.env.CODER_MAX_CONCURRENT, 2, 'CODER_MAX_CONCURRENT'),

  // Per-run wall-clock ceiling. The gateway aborts a run after this many ms;
  // the E2B microVM uses the same value as its self-destruct backstop, so a
  // gateway crash mid-run still has a hard end. The ledger records the same
  // deadline for audit but charges unresolved reservations in full on restart.
  wallMs: positiveInt(process.env.CODER_WALL_MS, 600_000, 'CODER_WALL_MS'),

  // Pinned operator ceiling, not a provider billing receipt. The installed E2B
  // SDK exposes resource metrics but no authoritative cost receipt, so every
  // attempted E2B run is charged for the full wall-clock reservation at this
  // rate. Production must explicitly pin both the rate and its evidence ref.
  sandboxWorstCaseUsdPerSec,
  sandboxTariffRef,
  e2bTemplateId: optionalTemplateId(process.env.E2B_TEMPLATE_ID),
  e2bExpectedCpuCount: boundedInteger(
    process.env.E2B_EXPECTED_CPU_COUNT,
    4,
    'E2B_EXPECTED_CPU_COUNT',
    1,
    64,
  ),
  e2bExpectedMemoryMb: boundedInteger(
    process.env.E2B_EXPECTED_MEMORY_MB,
    4096,
    'E2B_EXPECTED_MEMORY_MB',
    128,
    262_144,
  ),
  e2bEgressAllowlist,

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

  // Operator IPs bypass only the per-IP throttle. USD caps remain hard.
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
    ['CODER_E2B_WORST_CASE_USD_PER_SEC', Boolean(env.CODER_E2B_WORST_CASE_USD_PER_SEC)],
    ['CODER_E2B_TARIFF_REF', Boolean(env.CODER_E2B_TARIFF_REF)],
    ['E2B_TEMPLATE_ID', Boolean(env.E2B_TEMPLATE_ID?.trim())],
    ['E2B_EXPECTED_CPU_COUNT', Boolean(env.E2B_EXPECTED_CPU_COUNT)],
    ['E2B_EXPECTED_MEMORY_MB', Boolean(env.E2B_EXPECTED_MEMORY_MB)],
    ['CODER_MODEL', Boolean(env.CODER_MODEL)],
    ['USEPOD_INPUT_USD_PER_MILLION', Boolean(env.USEPOD_INPUT_USD_PER_MILLION)],
    ['USEPOD_OUTPUT_USD_PER_MILLION', Boolean(env.USEPOD_OUTPUT_USD_PER_MILLION)],
    ['USEPOD_MAX_INPUT_PRICE_MICROUNITS', Boolean(env.USEPOD_MAX_INPUT_PRICE_MICROUNITS)],
    ['USEPOD_MAX_OUTPUT_PRICE_MICROUNITS', Boolean(env.USEPOD_MAX_OUTPUT_PRICE_MICROUNITS)],
    ['USEPOD_MIN_BALANCE', Boolean(env.USEPOD_MIN_BALANCE)],
    ['LEDGER_PATH', Boolean(env.LEDGER_PATH?.startsWith('/'))],
    ['RUN_STORE_PATH', Boolean(env.RUN_STORE_PATH?.startsWith('/'))],
  ] as const;
  const missing: string[] = required.filter(([, present]) => !present).map(([name]) => name);
  if ((env.CODER_BACKEND ?? 'anthropic') !== 'usepod') {
    missing.push('CODER_BACKEND=usepod');
  }
  if (env.USEPOD_MODEL) missing.push('USEPOD_MODEL must be unset; use CODER_MODEL');
  if (env.E2B_TEMPLATE) missing.push('E2B_TEMPLATE must be unset; use immutable E2B_TEMPLATE_ID');
  if (env.E2B_TEMPLATE_ID !== undefined) {
    try {
      optionalTemplateId(env.E2B_TEMPLATE_ID);
    } catch (cause) {
      missing.push((cause as Error).message);
    }
  }
  try {
    const url = new URL(env.USEPOD_BASE_URL ?? '');
    if (
      url.origin !== 'https://api.usepod.ai' ||
      url.protocol !== 'https:' ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      url.pathname !== '/'
    ) {
      missing.push('USEPOD_BASE_URL must be exactly https://api.usepod.ai');
    }
  } catch {
    missing.push('USEPOD_BASE_URL must be exactly https://api.usepod.ai');
  }
  if (env.USEPOD_MIN_BALANCE !== undefined) {
    try {
      atomicUnits(env.USEPOD_MIN_BALANCE, '', 'USEPOD_MIN_BALANCE');
    } catch {
      missing.push('USEPOD_MIN_BALANCE must be positive whole USDC microunits');
    }
  }
  if (env.CODER_E2B_WORST_CASE_USD_PER_SEC !== undefined) {
    try {
      boundedPositiveNumber(
        env.CODER_E2B_WORST_CASE_USD_PER_SEC,
        0.0002,
        'CODER_E2B_WORST_CASE_USD_PER_SEC',
        0.01,
      );
    } catch (cause) {
      missing.push((cause as Error).message);
    }
  }
  if (env.CODER_E2B_TARIFF_REF !== undefined) {
    try {
      tariffReference(env.CODER_E2B_TARIFF_REF, '', 'CODER_E2B_TARIFF_REF');
    } catch (cause) {
      missing.push((cause as Error).message);
    }
  }
  for (const [name, raw, fallback, min, max] of [
    ['E2B_EXPECTED_CPU_COUNT', env.E2B_EXPECTED_CPU_COUNT, 4, 1, 64],
    ['E2B_EXPECTED_MEMORY_MB', env.E2B_EXPECTED_MEMORY_MB, 4096, 128, 262_144],
  ] as const) {
    try {
      boundedInteger(raw, fallback, name, min, max);
    } catch (cause) {
      missing.push((cause as Error).message);
    }
  }
  let inputEstimate: number | undefined;
  let outputEstimate: number | undefined;
  try {
    inputEstimate = usdPerMillionPrice(
      env.USEPOD_INPUT_USD_PER_MILLION,
      '0.2',
      'USEPOD_INPUT_USD_PER_MILLION',
    ).microunits;
  } catch (cause) {
    missing.push((cause as Error).message);
  }
  try {
    outputEstimate = usdPerMillionPrice(
      env.USEPOD_OUTPUT_USD_PER_MILLION,
      '0.4',
      'USEPOD_OUTPUT_USD_PER_MILLION',
    ).microunits;
  } catch (cause) {
    missing.push((cause as Error).message);
  }
  const maxInputPrice = boundedInteger(
    env.USEPOD_MAX_INPUT_PRICE_MICROUNITS,
    200_000,
    'USEPOD_MAX_INPUT_PRICE_MICROUNITS',
    1,
    100_000_000,
  );
  const maxOutputPrice = boundedInteger(
    env.USEPOD_MAX_OUTPUT_PRICE_MICROUNITS,
    400_000,
    'USEPOD_MAX_OUTPUT_PRICE_MICROUNITS',
    1,
    100_000_000,
  );
  if (inputEstimate !== undefined && inputEstimate < maxInputPrice) {
    missing.push('USEPOD_INPUT_USD_PER_MILLION understates the input price ceiling');
  }
  if (outputEstimate !== undefined && outputEstimate < maxOutputPrice) {
    missing.push('USEPOD_OUTPUT_USD_PER_MILLION understates the output price ceiling');
  }
  let egress = new Set<string>();
  try {
    egress = new Set(parseE2bEgressPolicy(env.E2B_EGRESS_ALLOW));
  } catch (cause) {
    missing.push((cause as Error).message);
  }
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

function boundedPositiveNumber(
  raw: string | undefined,
  fallback: number,
  name: string,
  max: number,
): number {
  const value = positiveNumber(raw, fallback, name);
  if (value > max) throw new Error(`${name} must be no more than ${max}`);
  return value;
}

function tariffReference(raw: string | undefined, fallback: string, name: string): string {
  if (raw === undefined || raw === '') {
    if (fallback) return fallback;
    throw new Error(`${name} must be a content-addressed evidence reference`);
  }
  const value = raw.trim();
  try {
    const url = new URL(value);
    if (
      url.protocol === 'https:' &&
      !url.username &&
      !url.password &&
      /^#sha256=[a-f0-9]{64}$/i.test(url.hash)
    ) {
      return value;
    }
  } catch {
    // Report the same operator-facing contract below.
  }
  throw new Error(`${name} must be a fetchable HTTPS reference ending in #sha256=<digest>`);
}

function optionalTemplateId(raw: string | undefined): string | undefined {
  const value = raw?.trim();
  if (!value) return undefined;
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{5,127}$/.test(value)) {
    throw new Error('E2B_TEMPLATE_ID must be an immutable provider template identifier');
  }
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

function boundedInteger(
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

function usdPerMillionPrice(
  raw: string | undefined,
  fallback: string,
  name: string,
): { usd: number; microunits: number } {
  const value = (raw ?? fallback).trim();
  if (!/^\d{1,3}(?:\.\d{1,6})?$/.test(value)) {
    throw new Error(`${name} must be a decimal with no more than six fractional digits`);
  }
  const [whole, fraction = ''] = value.split('.');
  const microunits = BigInt(whole!) * 1_000_000n + BigInt(fraction.padEnd(6, '0'));
  if (microunits <= 0n || microunits > 100_000_000n) {
    throw new Error(`${name} must be greater than zero and no more than 100`);
  }
  return { usd: Number(microunits) / 1_000_000, microunits: Number(microunits) };
}

function atomicUnits(raw: string | undefined, fallback: string, name: string): string {
  const value = raw === undefined || raw === '' ? fallback : raw;
  if (!/^[1-9]\d{0,47}$/.test(value)) {
    throw new Error(`${name} must be a positive whole number of USDC microunits`);
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
  ...((process.env.CODER_BACKEND ?? 'anthropic') === 'usepod'
    ? {
        [model]: {
          input: usePodInputPrice.usd,
          output: usePodOutputPrice.usd,
          cacheRead: 0,
          cacheWrite: 0,
        },
      }
    : {}),
};
