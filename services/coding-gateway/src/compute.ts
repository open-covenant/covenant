import { randomUUID } from 'node:crypto';
import { boundedInteger } from './config.js';
import { ConfigError } from './config-error.js';

/**
 * Client for the Covenant Compute control plane (compute.opencovenant.org).
 *
 * The gateway holds the beta token and enforces the spend bounds; the model
 * only sees the tool surface, so a dispatched task can rent a bounded GPU
 * workspace without the credential or the budget ever entering the sandbox.
 */

export interface ComputeConfig {
  apiUrl: string;
  apiToken: string;
  /** Whole-run budget: every launch commits its booking maximum against it. */
  maxUsdcMicros: number;
  maxDurationSecs: number;
  maxLaunches: number;
}

// Hard ceiling on any operator setting: the beta token's own spend cap.
const TOKEN_CAP_USDC_MICROS = 10_000_000;
export const MIN_DURATION_SECS = 60;
const API_TIMEOUT_MS = 60_000;
const DEFAULT_API_URL = 'https://compute.opencovenant.org';
// http puts the bearer token on the wire in cleartext, so it is allowed only
// where the wire is the host itself. URL keeps the brackets on an IPv6 hostname.
const LOOPBACK_HOSTS = new Set(['127.0.0.1', '[::1]', 'localhost']);
// Reap runs in the run's finally, ahead of artifact capture, the ledger commit
// and the concurrency slot release, so it gets a much shorter budget.
const REAP_TIMEOUT_MS = 5_000;
const JOB_ID = /^[A-Za-z0-9:_-]{1,128}$/;
const TERMINAL = new Set(['cancelled', 'completed', 'failed']);

// Must byte-match the control plane's built-in catalog entry: launch plans are
// validated by strict equality against the released catalog.
const GPU_WORKSPACE_APP = {
  id: 'gpu-workspace',
  name: 'GPU Workspace',
  summary: 'Open a bounded CUDA and Jupyter workspace on a dedicated GPU.',
  kind: 'workspace',
  availability: 'available',
  image:
    'docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10',
  min_vram_mib: 16_384,
  min_trust: 'open',
  default_duration_secs: 1_800,
  max_duration_secs: 21_600,
  default_max_usdc_micros: 500_000,
};

export function computeConfigFromEnv(env: NodeJS.ProcessEnv = process.env): ComputeConfig | null {
  const apiToken = env.COMPUTE_API_TOKEN;
  if (!apiToken) return null;
  return {
    apiUrl: computeApiUrl(env.COMPUTE_API_URL),
    apiToken,
    // The three bounds are one budget, not three: at the market's cheapest
    // rate, budget / (rate x duration) is how many launches actually fit. The
    // defaults ($0.20, 1800s, 1 launch) fund exactly one default booking at up
    // to 400000 micro-USDC per GPU-hour.
    maxUsdcMicros: boundedInteger(
      env.COMPUTE_MAX_USDC_MICROS,
      200_000,
      'COMPUTE_MAX_USDC_MICROS',
      1,
      TOKEN_CAP_USDC_MICROS,
    ),
    maxDurationSecs: boundedInteger(
      env.COMPUTE_MAX_DURATION_SECS,
      1_800,
      'COMPUTE_MAX_DURATION_SECS',
      MIN_DURATION_SECS,
      GPU_WORKSPACE_APP.max_duration_secs,
    ),
    maxLaunches: boundedInteger(env.COMPUTE_MAX_LAUNCHES, 1, 'COMPUTE_MAX_LAUNCHES', 1, 20),
  };
}

/**
 * Parsed at boot like every other endpoint knob, because an unusable value
 * otherwise boots healthy and surfaces only once a run has already paid for
 * model tokens.
 */
function computeApiUrl(raw: string | undefined): string {
  const value = (raw ?? DEFAULT_API_URL).trim();
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ConfigError(
      `COMPUTE_API_URL=${JSON.stringify(value)} is not a URL: set it to an origin such as ${DEFAULT_API_URL}`,
    );
  }
  if (
    url.protocol !== 'https:' &&
    !(url.protocol === 'http:' && LOOPBACK_HOSTS.has(url.hostname))
  ) {
    throw new ConfigError(
      `COMPUTE_API_URL=${JSON.stringify(value)} must use https. The compute bearer token is sent on every request, so http is accepted only for 127.0.0.1, ::1, and localhost.`,
    );
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new ConfigError(
      `COMPUTE_API_URL=${JSON.stringify(value)} must be a plain origin with no embedded credentials, query, or fragment`,
    );
  }
  return `${url.origin}${url.pathname}`.replace(/\/+$/, '');
}

/** Non-billable control-plane read: proves the origin resolves and the token authenticates. */
export async function probeComputeControlPlane(
  cfg: ComputeConfig,
  timeoutMs = 15_000,
): Promise<void> {
  const response = await fetch(`${cfg.apiUrl}/v1/offers`, {
    headers: { Authorization: `Bearer ${cfg.apiToken}` },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!response.ok) {
    throw new Error(`compute control plane readiness failed with HTTP ${response.status}`);
  }
  if (!Array.isArray(await response.json().catch(() => null))) {
    throw new Error('compute control plane returned a malformed offer list');
  }
}

export const defaultDurationSecs = (cfg: ComputeConfig): number =>
  Math.min(GPU_WORKSPACE_APP.default_duration_secs, cfg.maxDurationSecs);

export const workspaceCount = (n: number): string => `${n} GPU workspace${n === 1 ? '' : 's'}`;

/** Effective GPU bounds for /v1/capabilities and the boot banner. */
export function computeSummary(cfg: ComputeConfig | null): Record<string, unknown> {
  if (!cfg) return { enabled: false };
  return {
    enabled: true,
    budgetUsd: cfg.maxUsdcMicros / 1_000_000,
    maxLaunches: cfg.maxLaunches,
    maxDurationSecs: cfg.maxDurationSecs,
    defaultDurationSecs: defaultDurationSecs(cfg),
    controlPlane: new URL(cfg.apiUrl).host,
  };
}

// Read once at boot so a malformed COMPUTE_* value fails the process, not the
// first run that reaches for a GPU.
export const computeConfig = computeConfigFromEnv();

interface Offer {
  id: string;
  gpu: { model: string; vram_mib: number };
  rate_usdc_micros_per_hour: number;
  online: boolean;
}

export interface ComputeJob {
  id: string;
  status: string;
  offer_id: string;
  maximum_usdc_micros: number;
  access_url: string | null;
  error: string | null;
  receipt: {
    runtime_secs: number;
    charged_usdc_micros: number;
    refunded_usdc_micros: number;
  } | null;
}

const quoteMaximum = (ratePerHour: number, durationSecs: number): number =>
  Math.ceil((ratePerHour * durationSecs) / 3_600);

export class ComputeError extends Error {
  constructor(
    message: string,
    readonly code?: string,
  ) {
    super(message);
  }
}

function errorField(body: unknown): { code?: string; message?: string } | undefined {
  if (!body || typeof body !== 'object') return undefined;
  const err = (body as { error?: unknown }).error;
  if (typeof err === 'string') return { message: err };
  return err && typeof err === 'object' ? (err as { code?: string; message?: string }) : undefined;
}

function asJob(body: unknown): ComputeJob {
  const job = body as ComputeJob | null;
  if (!job || typeof job !== 'object') throw new ComputeError('control plane returned no job');
  if (typeof job.id !== 'string' || typeof job.status !== 'string') {
    throw new ComputeError('control plane returned a malformed job');
  }
  return job;
}

/**
 * Per-run compute session. It owns the run's USDC budget and every job the run
 * launched, so the end of the run (success or failure) can reap whatever the
 * model left behind. A leaked workspace bills until its own deadline.
 */
export class ComputeSession {
  /** Jobs that may still be billing. */
  private readonly launched = new Set<string>();
  /** Jobs confirmed terminal: still pollable for a receipt, never reaped. */
  private readonly settled = new Set<string>();
  private launches = 0;
  private committedMicros = 0;

  constructor(private readonly cfg: ComputeConfig) {}

  /** Worst-case USDC this run has committed. Cancelling never gives it back. */
  committedUsd(): number {
    return this.committedMicros / 1_000_000;
  }

  private async api(
    path: string,
    init: { method?: string; body?: string; headers?: Record<string, string> } = {},
    timeoutMs = API_TIMEOUT_MS,
  ): Promise<unknown> {
    const res = await fetch(`${this.cfg.apiUrl}${path}`, {
      ...init,
      headers: {
        Authorization: `Bearer ${this.cfg.apiToken}`,
        ...(init.body === undefined ? {} : { 'Content-Type': 'application/json' }),
        ...init.headers,
      },
      signal: AbortSignal.timeout(timeoutMs),
    });
    const body = (await res.json().catch(() => null)) as unknown;
    if (res.ok) return body;
    // `error` is a legitimate field on a 200 job, so only a non-2xx is a failure.
    const err = errorField(body);
    throw new ComputeError(
      [err?.code ?? `http_${res.status}`, err?.message].filter(Boolean).join(': '),
      err?.code,
    );
  }

  private async offers(): Promise<Offer[]> {
    const body = await this.api('/v1/offers');
    if (!Array.isArray(body)) {
      throw new ComputeError('control plane returned a malformed offer list');
    }
    return (body as Offer[])
      .filter((o) => o?.online)
      .sort((a, b) => a.rate_usdc_micros_per_hour - b.rate_usdc_micros_per_hour);
  }

  /**
   * Launch the cheapest offer whose quote fits what is left of the run budget.
   * Offers drift continuously, so each attempt refetches the market and retries
   * past stale ones under a single idempotency key: a POST that succeeded at the
   * control plane but timed out here must dedupe, or it leaves an untracked
   * workspace billing where reap cannot see it.
   */
  async launch(durationSecs?: number): Promise<ComputeJob> {
    const duration = this.bookingDuration(durationSecs);
    if (this.launches >= this.cfg.maxLaunches) {
      throw new ComputeError(
        `launch cap reached: this run may launch ${workspaceCount(this.cfg.maxLaunches)}`,
      );
    }
    const remaining = this.cfg.maxUsdcMicros - this.committedMicros;
    if (remaining <= 0) {
      throw new ComputeError(
        `this run's GPU budget of ${this.cfg.maxUsdcMicros} micro-USDC is fully committed`,
      );
    }
    const idempotencyKey = randomUUID();
    let lastError: ComputeError | undefined;
    for (let attempt = 0; attempt < 3; attempt++) {
      const affordable = (await this.offers()).filter(
        (o) => quoteMaximum(o.rate_usdc_micros_per_hour, duration) <= remaining,
      );
      if (affordable.length === 0) {
        throw new ComputeError(
          `no offer fits the ${remaining} micro-USDC left in this run's GPU budget for ${duration}s; try a shorter duration_secs`,
        );
      }
      for (const offer of affordable.slice(0, 3)) {
        const maximum = quoteMaximum(offer.rate_usdc_micros_per_hour, duration);
        const plan = {
          app: GPU_WORKSPACE_APP,
          offer,
          duration_secs: duration,
          maximum_usdc_micros: maximum,
        };
        try {
          const job = asJob(
            await this.api('/v1/jobs', {
              method: 'POST',
              body: JSON.stringify(plan),
              headers: { 'Idempotency-Key': idempotencyKey },
            }),
          );
          this.launches += 1;
          this.committedMicros += maximum;
          this.launched.add(job.id);
          return job;
        } catch (e) {
          if (!(e instanceof ComputeError) || e.code !== 'stale_offer') throw e;
          lastError = e;
        }
      }
    }
    throw new ComputeError(
      `every offer went stale before the workspace launched (${lastError?.message ?? 'stale_offer'})`,
      lastError?.code,
    );
  }

  private bookingDuration(requested?: number): number {
    if (requested === undefined) return defaultDurationSecs(this.cfg);
    if (!Number.isFinite(requested) || requested <= 0) {
      throw new ComputeError('duration_secs must be a positive number of seconds');
    }
    const secs = Math.floor(requested);
    if (secs < MIN_DURATION_SECS) {
      throw new ComputeError(`duration_secs must be at least ${MIN_DURATION_SECS} seconds`);
    }
    return Math.min(secs, this.cfg.maxDurationSecs);
  }

  async status(jobId: string): Promise<ComputeJob> {
    return asJob(await this.api(this.jobPath(jobId)));
  }

  async cancel(jobId: string, timeoutMs = API_TIMEOUT_MS): Promise<ComputeJob> {
    const job = asJob(await this.api(this.jobPath(jobId), { method: 'DELETE' }, timeoutMs));
    if (TERMINAL.has(job.status)) {
      this.launched.delete(jobId);
      this.settled.add(jobId);
    }
    return job;
  }

  /**
   * Two bounds on a model-supplied id: the shape keeps a relative path from
   * walking out of /v1/jobs into any other route the gateway token can reach,
   * and the ownership check keeps one run off a concurrent run's workspaces,
   * whose access_url is a live credential.
   */
  private jobPath(jobId: string): string {
    if (!JOB_ID.test(jobId)) {
      throw new ComputeError(`not a job id: ${JSON.stringify(jobId.slice(0, 64))}`);
    }
    if (!this.launched.has(jobId) && !this.settled.has(jobId)) {
      throw new ComputeError(`job ${jobId} was not launched by this run`);
    }
    return `/v1/jobs/${encodeURIComponent(jobId)}`;
  }

  /** Cancel every job this run launched and did not already cancel. */
  async reap(): Promise<string[]> {
    const ids = [...this.launched];
    if (ids.length === 0) return [];
    const results = await Promise.allSettled(ids.map((id) => this.cancel(id, REAP_TIMEOUT_MS)));
    const reaped: string[] = [];
    results.forEach((result, i) => {
      const id = ids[i]!;
      if (result.status === 'fulfilled' && TERMINAL.has(result.value.status)) {
        reaped.push(id);
        return;
      }
      // Anything unconfirmed stays tracked and bills to its own deadline. Log
      // the id only: the failure body can carry the workspace credential.
      console.error(`gpu_workspace: job ${id} did not confirm cancellation`);
    });
    return reaped;
  }
}
