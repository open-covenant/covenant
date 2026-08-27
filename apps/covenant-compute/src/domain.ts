export type AppKind = 'workspace' | 'image' | 'chat' | 'agent';
export type AppAvailability = 'available' | 'preview';
export type TrustClass = 'open' | 'isolated' | 'attested' | 'confidential';
export type RuntimeConnection = 'connected' | 'degraded' | 'offline';
export type AuthenticationSource = 'none' | 'environment' | 'session';
export type JobStatus =
  | 'funding'
  | 'provisioning'
  | 'running'
  | 'stopping'
  | 'completed'
  | 'cancelled'
  | 'failed';

export interface RuntimeStatus {
  state: RuntimeConnection;
  endpoint_label: string | null;
  message: string | null;
  authentication: AuthenticationMetadata;
  token_required: boolean;
}

export interface AuthenticationMetadata {
  source: AuthenticationSource;
}

export interface ComputeApp {
  id: string;
  name: string;
  summary: string;
  kind: AppKind;
  availability: AppAvailability;
  image: string | null;
  min_vram_mib: number;
  min_trust: TrustClass;
  default_duration_secs: number;
  max_duration_secs: number;
  default_max_usdc_micros: number;
}

export interface GpuSpec {
  model: string;
  vram_mib: number;
  cuda_major: number;
}

export interface ComputeOffer {
  id: string;
  gpu: GpuSpec;
  rate_usdc_micros_per_hour: number;
  trust_class: TrustClass;
  online: boolean;
}

export interface LaunchRequest {
  app_id: string;
  duration_secs: number;
  max_usdc_micros: number;
  min_trust?: TrustClass;
}

export interface LaunchPlan {
  app: ComputeApp;
  offer: ComputeOffer;
  duration_secs: number;
  maximum_usdc_micros: number;
}

export interface ComputeReceipt {
  id: string;
  job_id: string;
  app_id: string;
  provider: string;
  runtime_secs: number;
  charged_usdc_micros: number;
  refunded_usdc_micros: number;
  commitment: string;
  transaction: string | null;
}

export interface ComputeJob {
  id: string;
  app_id: string;
  offer_id: string;
  status: JobStatus;
  maximum_usdc_micros: number;
  access_ready: boolean;
  error: string | null;
  receipt: ComputeReceipt | null;
}

export const trustClasses: readonly TrustClass[] = [
  'open',
  'isolated',
  'attested',
  'confidential',
];

export const terminalStatuses: ReadonlySet<JobStatus> = new Set([
  'completed',
  'cancelled',
  'failed',
]);

export function formatUsdc(micros: number): string {
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 2,
    maximumFractionDigits: micros < 100_000 ? 4 : 2,
  }).format(micros / 1_000_000);
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3_600) return `${Math.ceil(seconds / 60)} min`;

  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.ceil((seconds % 3_600) / 60);
  return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
}

export function formatVram(mib: number): string {
  const gib = mib / 1_024;
  return `${Number.isInteger(gib) ? gib : gib.toFixed(1)} GB`;
}

export function formatTrust(value: TrustClass): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

export function shortId(value: string, visible = 7): string {
  if (value.length <= visible * 2 + 1) return value;
  return `${value.slice(0, visible)}…${value.slice(-visible)}`;
}

export function isValidAccessUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'https:' || (url.protocol === 'http:' && url.hostname === '127.0.0.1');
  } catch {
    return false;
  }
}

export function errorMessage(error: unknown): string {
  if (typeof error === 'string' && error.trim()) return error;
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'object' && error !== null && 'message' in error) {
    const message = Reflect.get(error, 'message');
    if (typeof message === 'string' && message.trim()) return message;
  }
  return 'The runtime returned an unexpected error.';
}

export function showPrivateBetaAccess(
  status: RuntimeStatus,
  demoMode: boolean,
): boolean {
  return (
    !demoMode &&
    (status.token_required || status.authentication.source === 'session')
  );
}
