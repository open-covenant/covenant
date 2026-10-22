import { createHash, randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { ComputeJournal } from './compute-journal.js';
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
  /** Must be a durable mounted path in production; never contains workspace URLs. */
  journalPath?: string;
}

// Hard ceiling on any operator setting: the beta token's own spend cap.
const TOKEN_CAP_USDC_MICROS = 10_000_000;
export const MIN_DURATION_SECS = 300;
const API_TIMEOUT_MS = 60_000;
const DEFAULT_API_URL = 'https://compute.opencovenant.org';
// http puts the bearer token on the wire in cleartext, so it is allowed only
// where the wire is the host itself. URL keeps the brackets on an IPv6 hostname.
const LOOPBACK_HOSTS = new Set(['127.0.0.1', '[::1]', 'localhost']);
// Reap runs in the run's finally, ahead of artifact capture, the ledger commit
// and the concurrency slot release, so it gets a much shorter budget.
const REAP_TIMEOUT_MS = 20_000;
const POLL_INTERVAL_MS = 250;
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
    ...(env.COMPUTE_JOURNAL_PATH ? { journalPath: env.COMPUTE_JOURNAL_PATH } : {}),
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

export interface ComputeReceipt {
  runtime_secs: number;
  charged_usdc_micros: number;
  refunded_usdc_micros: number;
  id?: string;
  job_id?: string;
  app_id?: string;
  provider?: string;
  provisioning_secs?: number;
  provisioning_usdc_micros?: number;
  commitment?: string;
  transaction?: string | null;
}

export interface ComputeJob {
  id: string;
  status: string;
  offer_id: string;
  maximum_usdc_micros: number;
  access_url: string | null;
  error: string | null;
  receipt: ComputeReceipt | null;
}

export interface ComputeSessionOptions {
  runKey: string;
  /** Prevent a new paid request when the dispatcher has stopped this run. */
  signal?: AbortSignal;
  maxUsdcMicros?: number;
  maxDurationSecs?: number;
  maxLaunches?: number;
}

export interface ComputeSettlement {
  job_id: string;
  status: string;
  maximum_usdc_micros: number;
  receipt: ComputeReceipt;
}

export interface ComputeCleanupReport {
  runKey: string;
  settled: ComputeSettlement[];
  pendingJobIds: string[];
  pendingLaunchKeys: string[];
  complete: boolean;
}

interface Reservation {
  key: string;
  plan: {
    app: typeof GPU_WORKSPACE_APP;
    offer: Offer;
    duration_secs: number;
    maximum_usdc_micros: number;
  };
  deadline: number;
  jobId?: string;
  status?: string;
  receipt?: ComputeReceipt;
  rejected?: boolean;
}

interface JournalRun {
  runKey: string;
  closed: boolean;
  maxUsdcMicros: number;
  maxDurationSecs: number;
  maxLaunches: number;
  reservations: Reservation[];
}

// A launch refused outright, by the provider or as a stale quote, starts nothing
// and charges nothing, so it does not use up the run's launch allowance; up to
// this many refusals are retried on the next-cheapest machine.
const MAX_DECLINED_LAUNCHES = 2;
const launched = (run: JournalRun): number => run.reservations.filter((r) => !r.rejected).length;
const declinedByProvider = (job: ComputeJob): boolean =>
  job.status === 'failed' &&
  job.receipt?.provider === 'control-plane' &&
  job.receipt.charged_usdc_micros === 0 &&
  job.receipt.refunded_usdc_micros === job.maximum_usdc_micros;

const quoteMaximum = (ratePerHour: number, durationSecs: number): number =>
  Math.ceil((ratePerHour * durationSecs) / 3_600);
const whole = (n: unknown): n is number =>
  typeof n === 'number' && Number.isSafeInteger(n) && n >= 0;
const validOffer = (o: unknown): o is Offer => {
  const v = o as Offer | null;
  return (
    !!v &&
    typeof v === 'object' &&
    typeof v.id === 'string' &&
    JOB_ID.test(v.id) &&
    typeof v.online === 'boolean' &&
    whole(v.rate_usdc_micros_per_hour) &&
    !!v.gpu &&
    typeof v.gpu.model === 'string' &&
    whole(v.gpu.vram_mib)
  );
};

function validReceipt(receipt: unknown, maximum: number, jobId: string): receipt is ComputeReceipt {
  const r = receipt as ComputeReceipt | null;
  return (
    !!r &&
    typeof r === 'object' &&
    whole(r.runtime_secs) &&
    whole(r.charged_usdc_micros) &&
    whole(r.refunded_usdc_micros) &&
    r.charged_usdc_micros <= maximum &&
    r.refunded_usdc_micros <= maximum &&
    r.charged_usdc_micros + r.refunded_usdc_micros === maximum &&
    (r.job_id === undefined || r.job_id === jobId) &&
    ['id', 'app_id', 'provider', 'commitment'].every((key) => {
      const value = (r as unknown as Record<string, unknown>)[key];
      return value === undefined || (typeof value === 'string' && value.length <= 512);
    }) &&
    (r.transaction === undefined ||
      r.transaction === null ||
      (typeof r.transaction === 'string' && r.transaction.length <= 512)) &&
    (r.provisioning_secs === undefined || whole(r.provisioning_secs)) &&
    (r.provisioning_usdc_micros === undefined || whole(r.provisioning_usdc_micros))
  );
}

/** Persist only receipt fields that cannot contain workspace access credentials. */
function safeReceipt(receipt: ComputeReceipt): ComputeReceipt {
  const result: ComputeReceipt = {
    runtime_secs: receipt.runtime_secs,
    charged_usdc_micros: receipt.charged_usdc_micros,
    refunded_usdc_micros: receipt.refunded_usdc_micros,
  };
  for (const key of [
    'id',
    'job_id',
    'app_id',
    'provider',
    'commitment',
    'transaction',
    'provisioning_secs',
    'provisioning_usdc_micros',
  ] as const) {
    const value = receipt[key];
    if (value !== undefined) Object.assign(result, { [key]: value });
  }
  return result;
}

function validRun(value: unknown): value is JournalRun {
  const v = value as JournalRun | null;
  return (
    !!v &&
    typeof v === 'object' &&
    typeof v.runKey === 'string' &&
    typeof v.closed === 'boolean' &&
    whole(v.maxUsdcMicros) &&
    v.maxUsdcMicros > 0 &&
    whole(v.maxDurationSecs) &&
    v.maxDurationSecs >= MIN_DURATION_SECS &&
    whole(v.maxLaunches) &&
    v.maxLaunches > 0 &&
    Array.isArray(v.reservations) &&
    launched(v) <= v.maxLaunches &&
    v.reservations.length <= v.maxLaunches + MAX_DECLINED_LAUNCHES &&
    v.reservations.every(
      (r) =>
        typeof r.key === 'string' &&
        whole(r.deadline) &&
        (r.rejected === undefined || typeof r.rejected === 'boolean') &&
        !!r.plan &&
        validOffer(r.plan.offer) &&
        whole(r.plan.duration_secs) &&
        r.plan.duration_secs >= MIN_DURATION_SECS &&
        r.plan.duration_secs <= v.maxDurationSecs &&
        whole(r.plan.maximum_usdc_micros) &&
        r.plan.maximum_usdc_micros ===
          quoteMaximum(r.plan.offer.rate_usdc_micros_per_hour, r.plan.duration_secs) &&
        (r.jobId === undefined || (typeof r.jobId === 'string' && JOB_ID.test(r.jobId))) &&
        (r.receipt === undefined ||
          (!!r.jobId &&
            TERMINAL.has(r.status ?? '') &&
            validReceipt(r.receipt, r.plan.maximum_usdc_micros, r.jobId))),
    ) &&
    v.reservations.reduce((sum, r) => sum + r.plan.maximum_usdc_micros, 0) <= v.maxUsdcMicros
  );
}

export class ComputeError extends Error {
  constructor(
    message: string,
    readonly code?: string,
  ) {
    super(message);
  }
}

function asJob(body: unknown): ComputeJob {
  const job = body as ComputeJob | null;
  if (!job || typeof job !== 'object') throw new ComputeError('control plane returned no job');
  if (
    typeof job.id !== 'string' ||
    !JOB_ID.test(job.id) ||
    typeof job.status !== 'string' ||
    !['funding', 'provisioning', 'running', 'stopping', 'cancelling', ...TERMINAL].includes(
      job.status,
    ) ||
    typeof job.offer_id !== 'string' ||
    !whole(job.maximum_usdc_micros) ||
    !(job.access_url === null || typeof job.access_url === 'string') ||
    !(job.error === null || typeof job.error === 'string')
  ) {
    throw new ComputeError('control plane returned a malformed job');
  }
  return job;
}

// Shared by sessions in this process. The mounted journal has exactly one
// gateway writer; its synchronous disk transactions cannot interleave.
const sessionLocks = new Map<string, Promise<unknown>>();

/** A reservation is irrevocable for this run, including uncertain launch outcomes. */
export class ComputeSession {
  private readonly journal: ComputeJournal<JournalRun>;
  private readonly journalKey: string;
  private readonly lockKey: string;
  private readonly options: Omit<Required<ComputeSessionOptions>, 'signal'>;
  private readonly signal?: AbortSignal;
  // When this gateway first saw each job running. Compute starts the booking
  // clock at readiness, which is never later than this observation.
  private readonly readySince = new Map<string, number>();

  constructor(
    private readonly cfg: ComputeConfig,
    options?: ComputeSessionOptions,
  ) {
    this.signal = options?.signal;
    this.options = {
      runKey: options?.runKey ?? randomUUID(),
      maxUsdcMicros: options?.maxUsdcMicros ?? cfg.maxUsdcMicros,
      maxDurationSecs: options?.maxDurationSecs ?? cfg.maxDurationSecs,
      maxLaunches: options?.maxLaunches ?? cfg.maxLaunches,
    };
    for (const field of ['maxUsdcMicros', 'maxDurationSecs', 'maxLaunches'] as const) {
      if (
        !whole(this.options[field]) ||
        this.options[field] < 1 ||
        this.options[field] > cfg[field]
      ) {
        throw new ComputeError(`invalid per-run GPU ${field}`);
      }
    }
    if (this.options.maxDurationSecs < MIN_DURATION_SECS)
      throw new ComputeError('GPU booking duration must be at least 300 seconds');
    if (!this.options.runKey || this.options.runKey.length > 512)
      throw new ComputeError('invalid GPU run key');
    this.journal = new ComputeJournal(cfg.journalPath, validRun);
    this.journalKey = createHash('sha256').update(this.options.runKey).digest('hex');
    this.lockKey = `${this.journal.path ?? 'memory'}:${this.journalKey}`;
    this.journal.update(
      this.journalKey,
      (existing) =>
        existing ?? {
          ...this.options,
          closed: false,
          reservations: [],
        },
    );
  }

  private current(): JournalRun {
    const run = this.journal.entries()[this.journalKey];
    if (!run || run.runKey !== this.options.runKey)
      throw new ComputeError('GPU journal run is missing');
    return run;
  }

  private update(change: (run: JournalRun) => void): JournalRun {
    return this.journal.update(this.journalKey, (run) => {
      if (!run) throw new ComputeError('GPU journal run is missing');
      change(run);
      return run;
    });
  }

  private async locked<T>(task: () => Promise<T>): Promise<T> {
    const previous = sessionLocks.get(this.lockKey) ?? Promise.resolve();
    const result = previous.catch(() => {}).then(task);
    sessionLocks.set(this.lockKey, result);
    try {
      return await result;
    } finally {
      if (sessionLocks.get(this.lockKey) === result) sessionLocks.delete(this.lockKey);
    }
  }

  committedUsd(): number {
    return (
      this.current().reservations.reduce((sum, r) => sum + r.plan.maximum_usdc_micros, 0) /
      1_000_000
    );
  }

  inspect(): ComputeCleanupReport {
    const run = this.current();
    const settled: ComputeSettlement[] = [];
    const pendingJobIds: string[] = [];
    const pendingLaunchKeys: string[] = [];
    for (const r of run.reservations) {
      if (r.rejected) continue;
      if (r.receipt && r.jobId && TERMINAL.has(r.status ?? '')) {
        settled.push({
          job_id: r.jobId,
          status: r.status!,
          maximum_usdc_micros: r.plan.maximum_usdc_micros,
          receipt: r.receipt,
        });
      } else if (r.jobId) pendingJobIds.push(r.jobId);
      else pendingLaunchKeys.push(r.key);
    }
    return {
      runKey: run.runKey,
      settled,
      pendingJobIds,
      pendingLaunchKeys,
      complete: pendingJobIds.length === 0 && pendingLaunchKeys.length === 0,
    };
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
      signal: AbortSignal.timeout(Math.max(1, timeoutMs)),
    });
    const body = (await res.json().catch(() => null)) as unknown;
    if (res.ok) return body;
    const error = (body as { error?: { code?: unknown } } | null)?.error;
    const code =
      error &&
      typeof error === 'object' &&
      typeof error.code === 'string' &&
      /^[a-z0-9_]{1,80}$/.test(error.code)
        ? error.code
        : `http_${res.status}`;
    // Provider error messages may contain URLs or credentials; do not echo them.
    throw new ComputeError(`compute request failed: ${code}`, code);
  }

  private async offers(): Promise<Offer[]> {
    const body = await this.api('/v1/offers');
    if (!Array.isArray(body) || body.some((o) => !validOffer(o)))
      throw new ComputeError('control plane returned a malformed offer list');
    // The complete offer must match the control plane catalog. Reject credential-
    // shaped fields rather than accidentally persisting them in its immutable plan.
    if (/"(?:access_url|authorization|api_token|token|password)"\s*:/i.test(JSON.stringify(body)))
      throw new ComputeError('control plane offer contains unexpected credentials');
    return body
      .filter((o: Offer) => o.online && o.gpu.vram_mib >= GPU_WORKSPACE_APP.min_vram_mib)
      .sort((a: Offer, b: Offer) => a.rate_usdc_micros_per_hour - b.rate_usdc_micros_per_hour);
  }

  async launch(durationSecs?: number): Promise<ComputeJob> {
    const duration = this.bookingDuration(durationSecs);
    return this.locked(async () => {
      if (this.signal?.aborted)
        throw new ComputeError('GPU run was aborted; no new booking is allowed');
      const run = this.current();
      if (run.closed) throw new ComputeError('GPU run is closed; no further bookings are allowed');
      const uncertain = run.reservations.find((r) => !r.jobId && !r.rejected);
      if (uncertain) {
        const committed = run.reservations.reduce((sum, r) => sum + r.plan.maximum_usdc_micros, 0);
        if (committed > this.options.maxUsdcMicros || launched(run) > this.options.maxLaunches) {
          throw new ComputeError(
            'GPU reservation exceeds the current run authorization; reconciliation is required',
          );
        }
        if (uncertain.deadline <= Date.now())
          throw new ComputeError(
            'GPU launch outcome is uncertain and its retry deadline has passed',
          );
        if (duration !== uncertain.plan.duration_secs)
          throw new ComputeError(
            'GPU launch outcome is uncertain; only the original booking can be retried',
          );
        return this.submit(uncertain);
      }
      const maxLaunches = Math.min(run.maxLaunches, this.options.maxLaunches);
      for (;;) {
        const exhausted = () =>
          this.current().reservations.length >= run.maxLaunches + MAX_DECLINED_LAUNCHES;
        let job: ComputeJob;
        try {
          job = await this.launchOnce(duration, maxLaunches);
        } catch (error) {
          // A stale quote is refused before any booking; submit() marked it declined.
          if (error instanceof ComputeError && error.code === 'stale_offer' && !exhausted())
            continue;
          throw error;
        }
        if (!declinedByProvider(job) || exhausted()) return job;
        // Nothing started and nothing was charged: try the next machine.
        this.update((current) => {
          current.reservations.find((r) => r.jobId === job.id)!.rejected = true;
        });
      }
    });
  }

  private async launchOnce(duration: number, maxLaunches: number): Promise<ComputeJob> {
    const run = this.current();
    if (launched(run) >= maxLaunches)
      throw new ComputeError(
        `launch cap reached: this run may launch ${workspaceCount(maxLaunches)}`,
      );
    const declined = new Set(
      run.reservations.filter((r) => r.rejected).map((r) => r.plan.offer.id),
    );
    const committed = run.reservations.reduce((sum, r) => sum + r.plan.maximum_usdc_micros, 0);
    const remaining = Math.min(run.maxUsdcMicros, this.options.maxUsdcMicros) - committed;
    const offer = (await this.offers()).find(
      (o) =>
        !declined.has(o.id) && quoteMaximum(o.rate_usdc_micros_per_hour, duration) <= remaining,
    );
    if (!offer)
      throw new ComputeError(
        `no offer fits the ${remaining} micro-USDC left in this run's GPU budget for ${duration}s`,
      );
    let reservation!: Reservation;
    this.update((current) => {
      // Recheck atomically after the non-billable offer request.
      if (this.signal?.aborted)
        throw new ComputeError('GPU run was aborted; no new booking is allowed');
      if (
        current.closed ||
        launched(current) >= maxLaunches ||
        current.reservations.length >= current.maxLaunches + MAX_DECLINED_LAUNCHES
      )
        throw new ComputeError('GPU run is closed or its launch cap is reached');
      const maximum = quoteMaximum(offer.rate_usdc_micros_per_hour, duration);
      const committed = current.reservations.reduce(
        (sum, r) => sum + r.plan.maximum_usdc_micros,
        0,
      );
      if (committed + maximum > Math.min(current.maxUsdcMicros, this.options.maxUsdcMicros))
        throw new ComputeError('GPU run budget is fully committed');
      reservation = {
        key: `gpu-${this.journalKey}-${current.reservations.length}`,
        plan: {
          app: GPU_WORKSPACE_APP,
          offer,
          duration_secs: duration,
          maximum_usdc_micros: maximum,
        },
        deadline: Date.now() + duration * 1_000,
      };
      current.reservations.push(reservation);
    });
    // The disk transaction above completes (including fsync) before this POST.
    return this.submit(reservation);
  }

  private async submit(reservation: Reservation): Promise<ComputeJob> {
    if (this.signal?.aborted)
      throw new ComputeError('GPU run was aborted; no new booking is allowed');
    try {
      const raw = await this.api('/v1/jobs', {
        method: 'POST',
        body: JSON.stringify(reservation.plan),
        headers: { 'Idempotency-Key': reservation.key },
      });
      // Preserve a syntactically valid ID even when another response field is
      // malformed: cleanup must still have a chance to locate the paid job.
      const id = (raw as { id?: unknown } | null)?.id;
      if (typeof id === 'string' && JOB_ID.test(id))
        this.update((run) => {
          run.reservations.find((r) => r.key === reservation.key)!.jobId = id;
        });
      const job = this.observe(reservation.key, raw);
      if (TERMINAL.has(job.status) && !job.receipt)
        throw new ComputeError('GPU job is terminal but its final receipt is still pending');
      return job;
    } catch (error) {
      // Only documented pre-submit failures prove no booking exists. Any timeout,
      // 5xx, malformed success or unknown error retains the reservation forever.
      if (
        error instanceof ComputeError &&
        [
          'stale_offer',
          'insufficient_funds',
          'invalid_maximum_usdc_micros',
          'invalid_duration',
        ].includes(error.code ?? '')
      ) {
        this.update((run) => {
          const r = run.reservations.find((r) => r.key === reservation.key)!;
          if (!r.jobId) r.rejected = true;
        });
      }
      throw error;
    }
  }

  private bookingDuration(requested?: number): number {
    if (requested === undefined)
      return Math.min(
        defaultDurationSecs(this.cfg),
        this.options.maxDurationSecs,
        this.current().maxDurationSecs,
      );
    if (!Number.isFinite(requested) || requested <= 0)
      throw new ComputeError('duration_secs must be a positive number of seconds');
    const secs = Math.floor(requested);
    if (secs < MIN_DURATION_SECS)
      throw new ComputeError(`duration_secs must be at least ${MIN_DURATION_SECS} seconds`);
    return Math.min(secs, this.options.maxDurationSecs, this.current().maxDurationSecs);
  }

  private owned(jobId: string): Reservation {
    if (typeof jobId !== 'string' || !JOB_ID.test(jobId)) throw new ComputeError('not a job id');
    const r = this.current().reservations.find((r) => r.jobId === jobId);
    if (!r) throw new ComputeError(`job ${jobId} was not launched by this run`);
    return r;
  }

  private observe(key: string, raw: unknown): ComputeJob {
    const job = asJob(raw);
    const r = this.current().reservations.find((r) => r.key === key)!;
    if (
      (r.jobId && r.jobId !== job.id) ||
      job.offer_id !== r.plan.offer.id ||
      job.maximum_usdc_micros !== r.plan.maximum_usdc_micros
    )
      throw new ComputeError('control plane job does not match this booking');
    if (job.receipt !== null && !validReceipt(job.receipt, r.plan.maximum_usdc_micros, job.id))
      throw new ComputeError('control plane returned an invalid GPU receipt');
    this.update((run) => {
      const entry = run.reservations.find((r) => r.key === key)!;
      entry.jobId = job.id;
      entry.status = job.status;
      if (TERMINAL.has(job.status) && job.receipt) entry.receipt = safeReceipt(job.receipt);
    });
    if (job.status === 'running' && job.access_url && !this.readySince.has(job.id))
      this.readySince.set(job.id, Date.now());
    return job;
  }

  /**
   * Waits for a job this run launched to accept work. Returns its workspace
   * address, which stays inside the gateway, and the booked window left.
   */
  async awaitRunning(
    jobId: string,
    untilMs: number,
    pollMs = 5_000,
  ): Promise<{ accessUrl: string; readySinceMs: number; durationSecs: number }> {
    const durationSecs = this.owned(jobId).plan.duration_secs;
    for (;;) {
      const job = await this.status(jobId);
      if (job.status === 'running' && job.access_url)
        return {
          accessUrl: job.access_url,
          readySinceMs: this.readySince.get(job.id) ?? Date.now(),
          durationSecs,
        };
      if (!['funding', 'provisioning'].includes(job.status))
        throw new ComputeError(`GPU job ${jobId} is ${job.status} and cannot run commands`);
      const remaining = untilMs - Date.now();
      if (remaining <= 0 || this.signal?.aborted)
        throw new ComputeError(`GPU job ${jobId} is still ${job.status}; try again shortly`);
      await delay(Math.min(pollMs, remaining), undefined, { signal: this.signal }).catch(
        () => undefined,
      );
    }
  }

  async status(jobId: string): Promise<ComputeJob> {
    const r = this.owned(jobId);
    const job = this.observe(r.key, await this.api(`/v1/jobs/${encodeURIComponent(jobId)}`));
    if (TERMINAL.has(job.status) && !job.receipt)
      throw new ComputeError('GPU job is terminal but its final receipt is still pending');
    return job;
  }

  async cancel(jobId: string, timeoutMs = API_TIMEOUT_MS): Promise<ComputeJob> {
    const r = this.owned(jobId);
    const deadline = Date.now() + timeoutMs;
    const path = `/v1/jobs/${encodeURIComponent(jobId)}`;
    let job = this.observe(r.key, await this.api(path, { method: 'DELETE' }, timeoutMs));
    while (!TERMINAL.has(job.status) || !job.receipt) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new ComputeError(`GPU cleanup pending for job ${jobId}`);
      await delay(Math.min(POLL_INTERVAL_MS, remaining));
      if (Date.now() >= deadline) throw new ComputeError(`GPU cleanup pending for job ${jobId}`);
      job = this.observe(r.key, await this.api(path, {}, Math.max(1, deadline - Date.now())));
    }
    return job;
  }

  private async cancelUnknown(key: string, timeoutMs: number): Promise<void> {
    // This endpoint seals even an absent key against a delayed launch POST.
    // A lookup/404 alone cannot prove that an ambiguous booking is safe to release.
    const raw = await this.api(
      `/v1/jobs/by-key/${encodeURIComponent(key)}`,
      { method: 'DELETE' },
      timeoutMs,
    );
    const result = raw as Record<string, unknown> | null;
    if (
      !result ||
      typeof result !== 'object' ||
      Array.isArray(result) ||
      result.idempotency_key !== key ||
      result.cancellation_recorded !== true ||
      !Object.hasOwn(result, 'job')
    )
      throw new ComputeError('control plane returned an invalid booking cancellation');
    if (result.job !== null) {
      // Validate the job, quote and receipt before attaching any returned ID.
      this.observe(key, result.job);
      return;
    }
    this.update((run) => {
      const reservation = run.reservations.find((r) => r.key === key);
      if (!run.closed || !reservation || reservation.jobId || reservation.receipt)
        throw new ComputeError('GPU booking changed during cancellation');
      // Retain the plan, committed maximum and consumed launch slot permanently.
      reservation.rejected = true;
    });
  }

  async reap(timeoutMs = REAP_TIMEOUT_MS): Promise<ComputeCleanupReport> {
    // Close before waiting for an in-flight launch. It cannot start another
    // booking while cleanup waits for the first POST to reveal its job ID.
    this.update((run) => {
      run.closed = true;
    });
    return this.locked(async () => {
      const deadline = Date.now() + timeoutMs;
      const pending = this.inspect();
      await Promise.allSettled([
        ...pending.pendingJobIds.map((id) => this.cancel(id, timeoutMs)),
        ...pending.pendingLaunchKeys.map(async (key) => {
          await this.cancelUnknown(key, timeoutMs);
          const reservation = this.current().reservations.find((r) => r.key === key)!;
          const remaining = deadline - Date.now();
          if (reservation.jobId && !reservation.receipt && remaining > 0)
            await this.cancel(reservation.jobId, remaining);
        }),
      ]);
      return this.inspect();
    });
  }

  static async recover(cfg: ComputeConfig): Promise<{ runs: ComputeCleanupReport[] }> {
    if (!cfg.journalPath) return { runs: [] };
    const journal = new ComputeJournal<JournalRun>(cfg.journalPath, validRun);
    const runs: ComputeCleanupReport[] = [];
    for (const run of Object.values(journal.entries())) {
      const session = new ComputeSession(cfg, { runKey: run.runKey });
      // Never replay POST here: seal/cancel unknown keys, then settle known IDs.
      runs.push(await session.reap());
    }
    return { runs };
  }
}
