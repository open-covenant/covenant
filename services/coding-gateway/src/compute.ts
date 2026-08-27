import { randomUUID } from 'node:crypto';

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
  maxUsdcMicros: number;
  maxDurationSecs: number;
}

export function computeConfigFromEnv(env: NodeJS.ProcessEnv = process.env): ComputeConfig | null {
  const apiToken = env.COMPUTE_API_TOKEN;
  if (!apiToken) return null;
  return {
    apiUrl: (env.COMPUTE_API_URL ?? 'https://compute.opencovenant.org').replace(/\/$/, ''),
    apiToken,
    maxUsdcMicros: Number(env.COMPUTE_MAX_USDC_MICROS ?? 200_000),
    maxDurationSecs: Number(env.COMPUTE_MAX_DURATION_SECS ?? 1_800),
  };
}

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

export class ComputeError extends Error {}

/**
 * Per-run compute session: every launched job is tracked so the run's end
 * (success or failure) can reap whatever the model left behind — a leaked
 * workspace bills until its own deadline.
 */
export class ComputeSession {
  private readonly launched = new Set<string>();

  constructor(private readonly cfg: ComputeConfig) {}

  private async api(path: string, init: RequestInit = {}): Promise<unknown> {
    const res = await fetch(`${this.cfg.apiUrl}${path}`, {
      ...init,
      headers: {
        Authorization: `Bearer ${this.cfg.apiToken}`,
        'Content-Type': 'application/json',
        ...(init.headers ?? {}),
      },
      signal: AbortSignal.timeout(60_000),
    });
    const body = (await res.json().catch(() => null)) as
      | { error?: { code?: string; message?: string } }
      | ComputeJob
      | Offer[]
      | null;
    if (!res.ok || (body && typeof body === 'object' && 'error' in body && body.error)) {
      const err = body && 'error' in (body as object) ? (body as { error?: { code?: string } }).error : undefined;
      throw new ComputeError(err?.code ?? `http_${res.status}`);
    }
    return body;
  }

  async offers(): Promise<Offer[]> {
    const offers = (await this.api('/v1/offers')) as Offer[];
    return offers
      .filter((o) => o.online)
      .sort((a, b) => a.rate_usdc_micros_per_hour - b.rate_usdc_micros_per_hour);
  }

  /**
   * Launch the cheapest offer whose quote fits the per-run cap. Offers drift
   * continuously, so refetch-and-submit and retry past stale offers.
   */
  async launch(durationSecs?: number): Promise<ComputeJob> {
    const duration = Math.min(
      durationSecs && durationSecs > 0 ? durationSecs : this.cfg.maxDurationSecs,
      this.cfg.maxDurationSecs,
    );
    let lastError = 'no_offer_within_budget';
    for (let attempt = 0; attempt < 3; attempt++) {
      const offers = await this.offers();
      const affordable = offers.filter(
        (o) => quoteMaximum(o.rate_usdc_micros_per_hour, duration) <= this.cfg.maxUsdcMicros,
      );
      if (affordable.length === 0) {
        throw new ComputeError(
          `no offer fits the budget cap of ${this.cfg.maxUsdcMicros} micro-USDC for ${duration}s`,
        );
      }
      for (const offer of affordable.slice(0, 3)) {
        const plan = {
          app: GPU_WORKSPACE_APP,
          offer,
          duration_secs: duration,
          maximum_usdc_micros: quoteMaximum(offer.rate_usdc_micros_per_hour, duration),
        };
        try {
          const job = (await this.api('/v1/jobs', {
            method: 'POST',
            body: JSON.stringify(plan),
            headers: { 'Idempotency-Key': randomUUID() },
          })) as ComputeJob;
          this.launched.add(job.id);
          return job;
        } catch (e) {
          lastError = (e as Error).message;
          if (lastError !== 'stale_offer') throw e;
        }
      }
    }
    throw new ComputeError(lastError);
  }

  async status(jobId: string): Promise<ComputeJob> {
    return (await this.api(`/v1/jobs/${jobId}`)) as ComputeJob;
  }

  async cancel(jobId: string): Promise<ComputeJob> {
    const job = (await this.api(`/v1/jobs/${jobId}`, { method: 'DELETE' })) as ComputeJob;
    this.launched.delete(jobId);
    return job;
  }

  /** Cancel every job this run launched and did not already cancel. */
  async reap(): Promise<string[]> {
    const reaped: string[] = [];
    for (const id of [...this.launched]) {
      try {
        const job = await this.cancel(id);
        if (job.status === 'cancelled' || job.status === 'completed') reaped.push(id);
      } catch {
        // Best effort: the control plane's own deadline still bounds the spend.
      }
    }
    return reaped;
  }
}
