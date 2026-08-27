import type { ComputeApi } from './api';
import type {
  ComputeApp,
  ComputeJob,
  ComputeOffer,
  ComputeReceipt,
  LaunchPlan,
  LaunchRequest,
} from './domain';
import { trustClasses } from './domain';

const apps: ComputeApp[] = [
  {
    id: 'gpu-workspace',
    name: 'GPU Workspace',
    summary: 'A bounded CUDA and Jupyter workspace on a dedicated GPU.',
    kind: 'workspace',
    availability: 'available',
    image:
      'docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10',
    min_vram_mib: 16_384,
    min_trust: 'open',
    default_duration_secs: 1_800,
    max_duration_secs: 21_600,
    default_max_usdc_micros: 500_000,
  },
  {
    id: 'comfyui',
    name: 'ComfyUI',
    summary: 'Create images with a visual generative workflow.',
    kind: 'image',
    availability: 'preview',
    image: null,
    min_vram_mib: 16_384,
    min_trust: 'open',
    default_duration_secs: 1_800,
    max_duration_secs: 14_400,
    default_max_usdc_micros: 500_000,
  },
  {
    id: 'open-webui',
    name: 'Open WebUI',
    summary: 'Run an open-model chat session on a dedicated GPU.',
    kind: 'chat',
    availability: 'preview',
    image: null,
    min_vram_mib: 16_384,
    min_trust: 'open',
    default_duration_secs: 3_600,
    max_duration_secs: 21_600,
    default_max_usdc_micros: 1_000_000,
  },
];

const offers: ComputeOffer[] = [
  {
    id: 'offer-demo-l40s',
    gpu: { model: 'L40S', vram_mib: 49_152, cuda_major: 12 },
    rate_usdc_micros_per_hour: 222_000,
    trust_class: 'open',
    online: true,
  },
  {
    id: 'offer-demo-a6000',
    gpu: { model: 'RTX A6000', vram_mib: 49_152, cuda_major: 12 },
    rate_usdc_micros_per_hour: 260_000,
    trust_class: 'isolated',
    online: true,
  },
];

type DemoJob = ComputeJob & {
  created_at: number;
};

const jobs = new Map<string, DemoJob>();
const idempotentJobs = new Map<string, string>();

function clone<T>(value: T): T {
  return structuredClone(value);
}

function quote(request: LaunchRequest): LaunchPlan {
  const app = apps.find((candidate) => candidate.id === request.app_id);
  if (!app || app.availability !== 'available') {
    throw new Error('This app is not available yet.');
  }
  if (request.duration_secs <= 0 || request.duration_secs > app.max_duration_secs) {
    throw new Error('The requested duration is outside the app limit.');
  }

  const offer = offers.find(
    (candidate) =>
      candidate.online &&
      candidate.gpu.vram_mib >= app.min_vram_mib &&
      trustClasses.indexOf(candidate.trust_class) >=
        trustClasses.indexOf(request.min_trust ?? app.min_trust),
  );
  if (!offer) throw new Error('No compatible GPU is currently online.');

  const maximum = Math.ceil(
    (offer.rate_usdc_micros_per_hour * request.duration_secs) / 3_600,
  );
  if (maximum > request.max_usdc_micros) {
    throw new Error('No compatible GPU fits within this allowance cap.');
  }

  return {
    app: clone(app),
    offer: clone(offer),
    duration_secs: request.duration_secs,
    maximum_usdc_micros: maximum,
  };
}

function receipt(job: DemoJob, status: 'completed' | 'cancelled'): ComputeReceipt {
  const runtime = Math.max(1, Math.floor((Date.now() - job.created_at) / 1_000));
  const charged =
    status === 'cancelled'
      ? Math.min(job.maximum_usdc_micros, Math.ceil((222_000 * runtime) / 3_600))
      : job.maximum_usdc_micros;

  return {
    id: `receipt-${job.id}`,
    job_id: job.id,
    app_id: job.app_id,
    provider: 'demo',
    runtime_secs: runtime,
    charged_usdc_micros: charged,
    refunded_usdc_micros: job.maximum_usdc_micros - charged,
    commitment: `demo-${job.id}-commitment`,
    transaction: null,
  };
}

function currentJob(id: string): DemoJob {
  const job = jobs.get(id);
  if (!job) throw new Error('Job not found.');

  const age = Date.now() - job.created_at;
  if (job.status === 'funding' && age > 700) job.status = 'provisioning';
  if (job.status === 'provisioning' && age > 1_600) {
    job.status = 'running';
    job.access_ready = true;
  }

  return job;
}

export function createDemoApi(): ComputeApi {
  return {
    async runtimeStatus() {
      return {
        state: 'connected',
        endpoint_label: 'Local simulation',
        message: 'No workload or payment is created in demo mode.',
        authentication: { source: 'none' },
        token_required: false,
      };
    },
    async configureSessionToken() {
      return { source: 'none' };
    },
    async clearSessionToken() {
      return { source: 'none' };
    },
    async listApps() {
      return clone(apps);
    },
    async listOffers() {
      return clone(offers);
    },
    async listJobs() {
      return clone(Array.from(jobs.values(), (job) => currentJob(job.id)));
    },
    async planJob(request) {
      return clone(quote(request));
    },
    async launchJob(request, idempotencyKey) {
      const existing = idempotentJobs.get(idempotencyKey);
      if (existing) return clone(currentJob(existing));

      const plan = quote(request);
      const id = `demo-${crypto.randomUUID().slice(0, 12)}`;
      const job: DemoJob = {
        id,
        app_id: plan.app.id,
        offer_id: plan.offer.id,
        status: 'funding',
        maximum_usdc_micros: plan.maximum_usdc_micros,
        access_ready: false,
        error: null,
        receipt: null,
        created_at: Date.now(),
      };
      jobs.set(id, job);
      idempotentJobs.set(idempotencyKey, id);
      return clone(job);
    },
    async getJob(id) {
      return clone(currentJob(id));
    },
    async cancelJob(id) {
      const job = currentJob(id);
      if (!['completed', 'cancelled', 'failed'].includes(job.status)) {
        job.status = 'cancelled';
        job.access_ready = false;
        job.receipt = receipt(job, 'cancelled');
      }
      return clone(job);
    },
    async openAccessUrl(id) {
      const job = currentJob(id);
      if (job.status !== 'running' || !job.access_ready) {
        throw new Error('Workspace access is not ready.');
      }
      window.open('https://workspace.example.test', '_blank', 'noopener,noreferrer');
    },
    async openJupyterSetupGuide() {
      window.open(
        'https://docs.vast.ai/guides/instances/connect/jupyter',
        '_blank',
        'noopener,noreferrer',
      );
    },
  };
}
