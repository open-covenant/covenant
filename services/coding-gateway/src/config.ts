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

  // Rough E2B sandbox rate; tune once coder-07 reports real sandbox-seconds.
  sandboxUsdPerSec: Number(process.env.CODER_SANDBOX_USD_PER_SEC ?? 0.0001),
} as const;

/** USD per 1M tokens, by model. cacheRead ~0.1x input, cacheWrite ~1.25x input. */
export const PRICING: Record<
  string,
  { input: number; output: number; cacheRead: number; cacheWrite: number }
> = {
  "claude-opus-4-7": { input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25 },
  "claude-sonnet-4-6": { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 },
  "claude-haiku-4-5": { input: 1, output: 5, cacheRead: 0.1, cacheWrite: 1.25 },
};
