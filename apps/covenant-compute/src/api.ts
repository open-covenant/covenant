import { invoke } from '@tauri-apps/api/core';

import { createDemoApi } from './demo';
import type {
  AuthenticationMetadata,
  ComputeApp,
  ComputeJob,
  ComputeOffer,
  LaunchPlan,
  LaunchRequest,
  RuntimeStatus,
} from './domain';

export interface ComputeApi {
  runtimeStatus(): Promise<RuntimeStatus>;
  configureSessionToken(token: string): Promise<AuthenticationMetadata>;
  clearSessionToken(): Promise<AuthenticationMetadata>;
  listApps(): Promise<ComputeApp[]>;
  listOffers(): Promise<ComputeOffer[]>;
  listJobs(): Promise<ComputeJob[]>;
  planJob(request: LaunchRequest, idempotencyKey: string): Promise<LaunchPlan>;
  launchJob(request: LaunchRequest, idempotencyKey: string): Promise<ComputeJob>;
  getJob(id: string): Promise<ComputeJob>;
  cancelJob(id: string): Promise<ComputeJob>;
  openAccessUrl(id: string): Promise<void>;
  openJupyterSetupGuide(): Promise<void>;
}

const tauriApi: ComputeApi = {
  runtimeStatus: () => invoke<RuntimeStatus>('runtime_status'),
  configureSessionToken: (token) =>
    invoke<AuthenticationMetadata>('configure_session_token', { token }),
  clearSessionToken: () =>
    invoke<AuthenticationMetadata>('clear_session_token'),
  listApps: () => invoke<ComputeApp[]>('list_apps'),
  listOffers: () => invoke<ComputeOffer[]>('list_offers'),
  listJobs: () => invoke<ComputeJob[]>('list_jobs'),
  planJob: (request, idempotencyKey) =>
    invoke<LaunchPlan>('plan_job', { request, idempotencyKey }),
  launchJob: (request, idempotencyKey) =>
    invoke<ComputeJob>('launch_job', { request, idempotencyKey }),
  getJob: (id) => invoke<ComputeJob>('get_job', { id }),
  cancelJob: (id) => invoke<ComputeJob>('cancel_job', { id }),
  openAccessUrl: (id) => invoke<void>('open_access_url', { id }),
  openJupyterSetupGuide: () => invoke<void>('open_jupyter_setup_guide'),
};

export const isDemoMode = import.meta.env.VITE_COMPUTE_DEMO === 'true';
export const computeApi: ComputeApi = isDemoMode ? createDemoApi() : tauriApi;
