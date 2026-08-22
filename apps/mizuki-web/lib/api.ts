import 'server-only';
import { demoActivity, demoBounties, demoCapabilities, demoMetrics, demoTreasury } from './demo';
import type {
  ActivityEvent,
  Bounty,
  Capability,
  Job,
  Loadable,
  Metrics,
  Overview,
  Treasury,
} from './types';

const timeoutMs = 8_000;

export function getApiBaseUrl(): string {
  return (process.env.MIZUKI_API_URL || 'http://127.0.0.1:8787').replace(/\/$/, '');
}

export function isDemoMode(): boolean {
  return process.env.MIZUKI_DEMO_MODE === '1';
}

async function request<T>(path: string): Promise<T> {
  const signal = AbortSignal.timeout(timeoutMs);
  const response = await fetch(`${getApiBaseUrl()}${path}`, {
    signal,
    cache: 'no-store',
    headers: { accept: 'application/json' },
  });
  if (!response.ok) {
    throw new Error(`Mizuki API returned ${response.status}`);
  }
  return (await response.json()) as T;
}

function list<T>(
  value: T[] | { items?: T[]; data?: T[]; bounties?: T[]; events?: T[]; capabilities?: T[] },
): T[] {
  if (Array.isArray(value)) return value;
  return value.items ?? value.data ?? value.bounties ?? value.events ?? value.capabilities ?? [];
}

async function load<T>(
  reader: () => Promise<T>,
  demo: T,
  empty: (value: T) => boolean,
): Promise<Loadable<T>> {
  if (isDemoMode()) return { status: empty(demo) ? 'empty' : 'ready', data: demo, demo: true };
  try {
    const data = await reader();
    return { status: empty(data) ? 'empty' : 'ready', data };
  } catch (cause) {
    return {
      status: 'error',
      error: cause instanceof Error ? cause.message : 'The Mizuki API is unavailable',
    };
  }
}

export async function getMetrics(): Promise<Loadable<Metrics>> {
  return load(
    () => request<Metrics>('/v1/metrics'),
    demoMetrics,
    () => false,
  );
}

export async function getBounties(): Promise<Loadable<Bounty[]>> {
  return load(
    async () =>
      list(await request<Bounty[] | { items?: Bounty[]; bounties?: Bounty[] }>('/v1/bounties')),
    demoBounties,
    (items) => items.length === 0,
  );
}

export async function getBounty(id: string): Promise<Loadable<Bounty>> {
  const fixture = demoBounties.find((item) => item.id === id) ?? demoBounties[0];
  return load(
    () => request<Bounty>(`/v1/bounties/${encodeURIComponent(id)}`),
    fixture,
    () => false,
  );
}

export async function getTreasury(): Promise<Loadable<Treasury>> {
  return load(
    () => request<Treasury>('/v1/treasury'),
    demoTreasury,
    () => false,
  );
}

export async function getCapabilities(): Promise<Loadable<Capability[]>> {
  return load(
    async () =>
      list(
        await request<Capability[] | { items?: Capability[]; capabilities?: Capability[] }>(
          '/v1/capabilities',
        ),
      ),
    demoCapabilities,
    (items) => items.length === 0,
  );
}

export async function getActivity(): Promise<Loadable<ActivityEvent[]>> {
  return load(
    async () =>
      list(
        await request<ActivityEvent[] | { items?: ActivityEvent[]; events?: ActivityEvent[] }>(
          '/v1/activity',
        ),
      ),
    demoActivity,
    (items) => items.length === 0,
  );
}

export async function getJob(id: string): Promise<Loadable<Job>> {
  return load(
    () => request<Job>(`/v1/jobs/${encodeURIComponent(id)}`),
    {
      id,
      state: 'validating',
      issueUrl: 'https://github.com/public-tools/release-workflows/issues/184',
      class: 'standard',
      priceAtomic: '10000000',
      paymentTransaction: '3taZ74j1ArW3u8C9GMMNTfXsY7bGmRksYbFgrwEa3aAa',
      changedFiles: ['src/workflow/normalize.ts', 'src/workflow/normalize.test.ts'],
      validations: [
        { command: 'pnpm test', exitCode: 0 },
        { command: 'pnpm typecheck', exitCode: 0 },
      ],
      variableRouteCostEstimateUsd: 2.04,
      costCoverage: {
        included: [
          'gateway_model_token_rate_estimate',
          'gateway_sandbox_runtime_estimate',
          'reviewer_model_token_rate_estimate',
        ],
        excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
      },
      createdAt: '2026-08-22T16:31:00.000Z',
      updatedAt: '2026-08-22T16:40:00.000Z',
    },
    () => false,
  );
}

export async function getOverview(): Promise<Overview> {
  const [metrics, bounties, treasury, capabilities, activity] = await Promise.all([
    getMetrics(),
    getBounties(),
    getTreasury(),
    getCapabilities(),
    getActivity(),
  ]);
  return { metrics, bounties, treasury, capabilities, activity };
}
