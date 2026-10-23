import { isAbsolute } from 'node:path';
import type { ComputeConfig } from './compute.js';
import type { BackendId, GpuPolicy } from './types.js';

export const SMOKE_GPU_POLICY: GpuPolicy = {
  max_usdc_micros: 200000,
  duration_secs: 300,
  max_launches: 1,
};

export function parseGpuPolicy(value: unknown): GpuPolicy | undefined {
  if (value === undefined) return undefined;
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('invalid GPU policy');
  const policy = value as Record<string, unknown>;
  if (
    Object.keys(policy).length !== 3 ||
    policy.max_usdc_micros !== 200000 ||
    policy.duration_secs !== 300 ||
    policy.max_launches !== 1
  ) {
    throw new Error(
      'GPU policy must authorize exactly one 300-second booking capped at 200000 micro-USDC',
    );
  }
  return { ...SMOKE_GPU_POLICY };
}

export function gpuAvailable(opts: {
  backend: BackendId;
  authToken?: string;
  compute: ComputeConfig | null;
  persistentRuns: boolean;
  recovered: boolean;
  env?: NodeJS.ProcessEnv;
}): boolean {
  const env = opts.env ?? process.env;
  return Boolean(
    env.COMPUTE_GPU_ENABLED === '1' &&
    env.COMPUTE_DURABLE_STORAGE === '1' &&
    opts.backend === 'anthropic' &&
    opts.authToken &&
    opts.authToken.length >= 32 &&
    opts.persistentRuns &&
    opts.recovered &&
    opts.compute?.journalPath &&
    isAbsolute(opts.compute.journalPath) &&
    env.LEDGER_PATH &&
    isAbsolute(env.LEDGER_PATH) &&
    opts.compute.maxUsdcMicros >= 200000 &&
    opts.compute.maxDurationSecs >= 300 &&
    opts.compute.maxLaunches >= 1,
  );
}

export function smokeComputeConfig(cfg: ComputeConfig): ComputeConfig {
  return { ...cfg, maxUsdcMicros: 200000, maxDurationSecs: 300, maxLaunches: 1 };
}
