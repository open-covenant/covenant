/** Gateway configuration and model pricing, all overridable by env. */

const model = process.env.CODER_MODEL ?? "claude-sonnet-4-6";

export const config = {
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
  effort: (process.env.CODER_EFFORT ?? (model.includes("opus") ? "xhigh" : "low")) as
    | "low"
    | "medium"
    | "high"
    | "xhigh"
    | "max",

  // Spend caps (USD). Daily is a rate limit on the monthly bucket; both hard.
  dailyUsd: Number(process.env.CODER_DAILY_USD ?? 6),
  monthlyUsd: Number(process.env.CODER_MONTHLY_USD ?? 200),
  perRunUsdMax: Number(process.env.CODER_PER_RUN_USD_MAX ?? 2),

  maxConcurrent: Number(process.env.CODER_MAX_CONCURRENT ?? 2),

  // Per-run wall-clock ceiling. The gateway aborts a run after this many ms;
  // the E2B microVM uses the same value as its self-destruct backstop, so a
  // gateway crash mid-run still has a hard end. Also the reservation horizon
  // the spend ledger persists for warm recovery on restart.
  wallMs: Number(process.env.CODER_WALL_MS ?? 600_000),

  // Rough E2B sandbox rate; tune once coder-07 reports real sandbox-seconds.
  sandboxUsdPerSec: Number(process.env.CODER_SANDBOX_USD_PER_SEC ?? 0.0001),

  // Per-IP admission gate. With CODER_MAX_CONCURRENT=2 a single anonymous
  // client can otherwise occupy both slots and burn the entire daily cap
  // alone before Turnstile / edge gates land — one in-flight per IP is
  // the cheapest stop-gap. Set CODER_IP_MAX_PER_IP=0 to disable
  // explicitly (typos like `=true` would otherwise silently disable
  // through Number() → NaN → NaN > 0 false).
  ipMaxPerIp: nonNegativeInt(process.env.CODER_IP_MAX_PER_IP, 1, "CODER_IP_MAX_PER_IP"),
  ipRefillMs: nonNegativeInt(process.env.CODER_IP_REFILL_MS, 60_000, "CODER_IP_REFILL_MS"),
  // How many trusted proxies sit between the gateway and the public
  // internet. The right-most TRUSTED_PROXY_HOPS entries of
  // X-Forwarded-For are honored; everything left is treated as client-
  // controlled and ignored. Default 0 (use the socket peer) — safe for
  // any deployment but collapses every visitor behind shared NAT to one
  // address. Picking too large lets a client rotate IPs via the header.
  trustedProxyHops: nonNegativeInt(process.env.TRUSTED_PROXY_HOPS, 0, "TRUSTED_PROXY_HOPS"),
} as const;

/**
 * Parse a non-negative integer env value, falling back to `fallback` on
 * an absent value but **refusing** garbage (negative, NaN, fractional)
 * — a silent `Number()` cast would let a typo like
 * `CODER_IP_MAX_PER_IP=true` (→ NaN → `NaN > 0` false) bypass the
 * per-IP gate without any visible signal. Loud-fail at boot is the
 * cheaper failure mode for a security-sensitive control.
 */
function nonNegativeInt(raw: string | undefined, fallback: number, name: string): number {
  if (raw === undefined || raw === "") return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
    throw new Error(
      `${name}=${JSON.stringify(raw)} is not a non-negative integer — refusing to boot with a silently-disabled control. Set ${name}=0 to opt out explicitly.`,
    );
  }
  return n;
}

/** USD per 1M tokens, by model. cacheRead ~0.1x input, cacheWrite ~1.25x input. */
export const PRICING: Record<
  string,
  { input: number; output: number; cacheRead: number; cacheWrite: number }
> = {
  "claude-opus-4-7": { input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25 },
  "claude-sonnet-4-6": { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 },
  "claude-haiku-4-5": { input: 1, output: 5, cacheRead: 0.1, cacheWrite: 1.25 },
};
